//! Local Gemma inference via `llama-cpp-2` (Metal-accelerated on Apple Silicon).
//!
//! The inference loop is CPU-bound (even with GPU layers, there's per-token
//! orchestration) and uses the llama-cpp-2 synchronous API, so we run it on
//! tokio::task::spawn_blocking. Token deltas are pumped through an mpsc
//! channel; the `stream()` impl wraps the receiver into a BoxStream so
//! Tauri command handlers can forward chunks as `ai://chunk` events.
//!
//! Model lifecycle:
//!   - `LocalLlamaBackend::new()` holds the model handle (loaded once;
//!     loading a GGUF from disk takes a couple hundred ms to a few
//!     seconds depending on size). Context is created per-request so
//!     concurrent asks don't share KV cache.
//!   - LlamaBackend::init() is process-global; we construct it lazily
//!     inside new() and keep an Arc.
//!
//! Prompt formatting: Gemma 4 is instruction-tuned and expects its own
//! chat template (`<start_of_turn>user\n…<end_of_turn>\n<start_of_turn>model\n`).
//! We format manually — simpler and more portable than depending on the
//! crate's chat-template helper, which requires tokenizer metadata we
//! don't strictly need here.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{AiBackend, AiMode, AiRequest, AiResponse};

/// How many tokens to generate at most per request. Terminal-scoped
/// answers are short; this cap keeps a pathological case from
/// monopolizing the GPU for minutes.
const MAX_NEW_TOKENS: i32 = 512;

/// Context window for the KV cache. Gemma 4 supports 128K but we're
/// packing a short prompt + short answer — 4K is plenty and keeps
/// memory use modest.
const CTX_SIZE: u32 = 4096;

pub struct LocalLlamaBackend {
    /// Shared LlamaBackend — init exactly once per process per the crate
    /// docs. Arc so the inference task can hold its own reference while
    /// the main thread continues to use it for future requests.
    backend: Arc<LlamaBackend>,
    /// Loaded model. LlamaModel is !Sync, so we guard with a Mutex; the
    /// lock is held only for long enough to create a new context (cheap).
    model: Arc<Mutex<LlamaModel>>,
    /// Path we loaded from. Surfaced in logs + used by tests.
    pub model_path: PathBuf,
    /// Display name used by the `AiBackend::display_name` impl.
    display_label: &'static str,
}

impl LocalLlamaBackend {
    /// Load a GGUF model from disk. Blocking on disk I/O + (on Metal)
    /// shader compilation; callers should `spawn_blocking` this if they
    /// care about keeping the async runtime responsive. Today the only
    /// caller is router bootstrap, which runs before the window opens.
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let backend = Arc::new(
            LlamaBackend::init()
                .map_err(|e| format!("LlamaBackend::init: {e}"))?,
        );

        // n_gpu_layers = 1000 means "offload every layer to the GPU".
        // llama.cpp caps this internally at the model's actual depth,
        // so oversizing here is safe and future-proof.
        let params = LlamaModelParams::default().with_n_gpu_layers(1000);
        let model = LlamaModel::load_from_file(&backend, &path, &params)
            .map_err(|e| format!("load_from_file {}: {e}", path.display()))?;

        log::info!(
            "local llama model loaded: {} ({} MB)",
            path.display(),
            std::fs::metadata(&path)
                .map(|m| m.len() / 1_048_576)
                .unwrap_or(0),
        );

        Ok(Self {
            backend,
            model: Arc::new(Mutex::new(model)),
            model_path: path,
            display_label: "Gemma (local)",
        })
    }
}

#[async_trait]
impl AiBackend for LocalLlamaBackend {
    fn id(&self) -> &'static str {
        "local-gemma"
    }

    fn display_name(&self) -> &'static str {
        self.display_label
    }

    async fn is_available(&self) -> bool {
        // The mere existence of this struct means load() succeeded, so
        // we're trivially available. (If the model file is deleted
        // under our feet later, inference will fail at request time
        // with a clear error.)
        true
    }

    async fn ask(&self, req: AiRequest) -> Result<AiResponse, String> {
        // Full-text answer: drain the stream and concatenate. Same code
        // path as stream(), just collected into a String.
        let mut stream = self.stream(req);
        let mut out = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(delta) => out.push_str(&delta),
                Err(e) => return Err(e),
            }
        }
        Ok(AiResponse {
            text: out.trim().to_string(),
            backend: self.id().to_string(),
        })
    }

    fn stream(&self, req: AiRequest) -> BoxStream<'static, Result<String, String>> {
        let backend = self.backend.clone();
        let model = self.model.clone();
        let prompt = build_prompt(&req);

        let (tx, rx) = mpsc::unbounded_channel::<Result<String, String>>();

        tokio::task::spawn_blocking(move || {
            run_inference(backend, model, prompt, tx);
        });

        UnboundedReceiverStream::new(rx).boxed()
    }
}

/// Actual inference loop. Runs on a blocking thread; pumps tokens into `tx`.
/// Errors are delivered on the same channel as `Err(...)` items.
fn run_inference(
    backend: Arc<LlamaBackend>,
    model: Arc<Mutex<LlamaModel>>,
    prompt: String,
    tx: mpsc::UnboundedSender<Result<String, String>>,
) {
    let model = model.lock();
    // 1. Context.
    let ctx_params = LlamaContextParams::default().with_n_ctx(
        NonZeroU32::new(CTX_SIZE),
    );
    let mut ctx = match model.new_context(&backend, ctx_params) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Err(format!("llama context: {e}")));
            return;
        }
    };

    // 2. Tokenize the prompt.
    let tokens = match model.str_to_token(&prompt, AddBos::Always) {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(Err(format!("tokenize: {e}")));
            return;
        }
    };
    let prompt_tokens = tokens.len() as i32;
    let n_total = prompt_tokens + MAX_NEW_TOKENS;

    // 3. Feed the prompt through decode.
    let mut batch = LlamaBatch::new(512, 1);
    for (i, token) in tokens.iter().enumerate() {
        let is_last = i == tokens.len() - 1;
        // sequence 0, pos i, only emit logits for the last prompt token.
        if let Err(e) = batch.add(*token, i as i32, &[0], is_last) {
            let _ = tx.send(Err(format!("batch.add: {e}")));
            return;
        }
    }
    if let Err(e) = ctx.decode(&mut batch) {
        let _ = tx.send(Err(format!("decode prompt: {e}")));
        return;
    }

    // 4. Sampler — greedy-ish. dist() adds a tiny bit of randomness so
    // repeated asks don't give identical answers; greedy() picks the
    // argmax. This is the standard "simple chain" from the crate's
    // examples.
    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::dist(1234),
        LlamaSampler::greedy(),
    ]);

    let mut n_cur = prompt_tokens;
    let mut utf8_decoder = encoding_rs::UTF_8.new_decoder();

    while n_cur < n_total {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        if model.is_eog_token(token) {
            break;
        }

        // Detokenize. The crate returns a `String` directly; the decoder
        // is needed so multi-byte UTF-8 sequences that span token
        // boundaries (common in languages with wide codepoints) don't
        // come out mangled.
        // `special: false` strips <bos>/<eos>/control tokens from output —
        // we only want user-visible text. `lstrip: None` keeps leading
        // spaces inside tokens intact (model naturally emits " word").
        let piece = match model.token_to_piece(token, &mut utf8_decoder, false, None) {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(Err(format!("detokenize: {e}")));
                return;
            }
        };
        if !piece.is_empty() && tx.send(Ok(piece)).is_err() {
            // Consumer dropped — stop generating.
            return;
        }

        // Prepare next batch: one new token.
        batch.clear();
        if let Err(e) = batch.add(token, n_cur, &[0], true) {
            let _ = tx.send(Err(format!("batch.add: {e}")));
            return;
        }
        if let Err(e) = ctx.decode(&mut batch) {
            let _ = tx.send(Err(format!("decode: {e}")));
            return;
        }
        n_cur += 1;
    }
}

/// Wrap the request in Gemma 4's chat template. Reverted to the original
/// Phase 5b prompt shape after the "tuned" variants made things worse.
/// Keeping `to_compact_prompt_block` around in context.rs as a future
/// dial, but not using it: the full context block works as well as
/// compact in practice and gives Claude/Gemma the same grounding.
fn build_prompt(req: &AiRequest) -> String {
    let mut user = String::new();
    if let Some(ctx) = &req.context {
        user.push_str(&ctx.to_prompt_block());
        user.push('\n');
    }
    match req.mode {
        AiMode::Command => {
            user.push_str(
                "Convert the following request into a single shell command \
                 for zsh on macOS. Output ONLY the command, no explanation, \
                 no markdown fences.\n\n",
            );
            user.push_str("Request: ");
        }
        AiMode::Explain => {
            user.push_str(
                "Explain the command or error above. Give a short plain \
                 explanation (2-4 sentences) and then, if applicable, a \
                 suggested fix as a one-line shell command on the last \
                 line in a markdown code block.\n\n",
            );
        }
        AiMode::Chat => {}
    }
    user.push_str(&req.prompt);

    // Gemma 4 chat template.
    format!(
        "<start_of_turn>user\n{user}<end_of_turn>\n<start_of_turn>model\n"
    )
}
