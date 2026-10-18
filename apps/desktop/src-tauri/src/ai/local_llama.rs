//! Local Gemma inference via `llama-cpp-2` — Metal on Apple Silicon; on
//! Windows, Vulkan or the best CPU kernel set via ggml backend DLLs that
//! are loaded at runtime (see `load_dynamic_backends`).
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
use llama_cpp_2::model::{LlamaChatMessage, LlamaChatTemplate, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LlamaBackendDevice, LlamaBackendDeviceType};
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

/// ⚠ Field order is load-bearing. Rust drops struct fields in declaration
/// order, and `llama.cpp` calls `abort()` if `llama_backend_free()` runs
/// while any LlamaModel still exists (the model holds an internal raw
/// pointer to the backend). We saw this as a SIGABRT when closing the
/// app during Phase 5b. Ordering here guarantees clean teardown:
///
/// ```text
/// chat_template  (may reference the model)
/// -> model       (holds internal ref to backend)
/// -> backend     (freed last)
/// ```
///
/// Non-Llama fields live at the end because their drop order is
/// irrelevant.
pub struct LocalLlamaBackend {
    /// The model's own chat template, pulled from the GGUF metadata at
    /// load time. Using this instead of a hardcoded Gemma template means
    /// we get whatever format the model was actually trained on — same
    /// thing Ollama does under the hood, without Ollama as a dependency.
    /// Optional because pre-instruction-tuned GGUFs don't ship one.
    chat_template: Option<LlamaChatTemplate>,
    /// Loaded model. LlamaModel is !Sync, so we guard with a Mutex; the
    /// lock is held only for long enough to create a new context (cheap).
    model: Arc<Mutex<LlamaModel>>,
    /// Shared LlamaBackend — init exactly once per process per the crate
    /// docs. Must drop LAST (see field-order comment above).
    backend: Arc<LlamaBackend>,
    /// Path we loaded from. Surfaced in logs + used by tests.
    pub model_path: PathBuf,
    /// Display name used by the `AiBackend::display_name` impl.
    display_label: &'static str,
    /// `llama_supports_gpu_offload()` as seen at load time: a GPU-class
    /// device is registered (Metal on Apple Silicon; on Windows, the
    /// Vulkan DLL loaded and found a driver). `/arcterm-status` shows it
    /// because "is it actually on the GPU?" is the first question on new
    /// hardware, and the answer is otherwise buried in the log.
    gpu_offload: bool,
    /// First GPU-class device, e.g. "Apple M3 Max via Metal" or
    /// "Intel(R) Arc(TM) Graphics via Vulkan". None on CPU-only setups.
    gpu_device: Option<String>,
    /// Thread count for CPU-only inference; `None` leaves llama.cpp's
    /// default when a GPU carries the layers. Decided once at load so the
    /// per-request context creation stays cheap.
    cpu_threads: Option<i32>,
}

impl LocalLlamaBackend {
    /// Load a GGUF model from disk. Blocking on disk I/O + (on Metal)
    /// shader compilation; callers should `spawn_blocking` this if they
    /// care about keeping the async runtime responsive.
    ///
    /// Does NOT verify the on-disk SHA against the registry — only use
    /// this on the boot-critical path where the user is already waiting
    /// on a black window. We trust the 0700 dir + 0600 file perms
    /// applied at download time to prevent drive-by tampering; the
    /// residual risk is a same-uid attacker with file-write capability,
    /// which is a weaker threat than we can address without delaying
    /// window creation by 15–40 s. All user-triggered swap paths
    /// (ai_set_mode lazy-load, ai_set_local_model, model_download
    /// post-load) go through `load_verified` instead.
    pub fn load(path: PathBuf) -> Result<Self, String> {
        // Must precede init: llama_backend_init only sees backends that
        // are already registered. No-op off Windows.
        load_dynamic_backends();
        let backend = Arc::new(
            LlamaBackend::init()
                .map_err(|e| format!("LlamaBackend::init: {e}"))?,
        );

        // Probe once, before the model load, so a failure to find the
        // GPU shows up in the log next to the backend list and not as a
        // mysteriously slow first answer.
        let gpu_offload = backend.supports_gpu_offload();
        let devices = llama_cpp_2::list_llama_ggml_backend_devices();
        for d in &devices {
            log::info!(
                "ggml device {}: {} [{} / {:?}] {} MB",
                d.name,
                d.description,
                d.backend,
                d.device_type,
                d.memory_total / 1_048_576,
            );
        }
        let gpu_device = gpu_device_label(&devices);
        let cpu_threads = cpu_threads_for(gpu_offload);
        log::info!(
            "local llama: gpu_offload={} device={} cpu_threads={}",
            gpu_offload,
            gpu_device.as_deref().unwrap_or("none"),
            cpu_threads.map_or_else(|| "llama.cpp default".to_string(), |n| n.to_string()),
        );

        // n_gpu_layers = 1000 means "offload every layer to the GPU".
        // llama.cpp caps this internally at the model's actual depth,
        // so oversizing here is safe and future-proof.
        let params = LlamaModelParams::default().with_n_gpu_layers(1000);
        let model = LlamaModel::load_from_file(&backend, &path, &params)
            .map_err(|e| format!("load_from_file {}: {e}", path.display()))?;

        // Pull the embedded chat template. `None` = the model's default
        // template name. Models that ship multiple templates (rare) can
        // be queried by name; we don't need that today.
        let chat_template = match model.chat_template(None) {
            Ok(t) => {
                log::info!("local model chat template loaded from GGUF metadata");
                Some(t)
            }
            Err(e) => {
                log::warn!(
                    "no chat template in GGUF ({}); falling back to raw Gemma template",
                    e
                );
                None
            }
        };

        log::info!(
            "local llama model loaded: {} ({} MB)",
            path.display(),
            std::fs::metadata(&path)
                .map(|m| m.len() / 1_048_576)
                .unwrap_or(0),
        );

        // Construction order matches the declared field order on purpose
        // — not strictly required for correctness (that's the DROP order
        // that matters) but keeps init visually aligned with teardown.
        Ok(Self {
            chat_template,
            model: Arc::new(Mutex::new(model)),
            backend,
            model_path: path,
            display_label: "Gemma (local)",
            gpu_offload,
            gpu_device,
            cpu_threads,
        })
    }

    /// Whether llama.cpp offloads this model's layers to a GPU.
    pub fn gpu_offload(&self) -> bool {
        self.gpu_offload
    }

    /// Description of the GPU in use, when there is one.
    pub fn gpu_device(&self) -> Option<&str> {
        self.gpu_device.as_deref()
    }

    /// SECURITY: `load` + explicit SHA re-verify against the registry
    /// pin. Used by every user-triggered swap path (ai_set_mode lazy-
    /// load, ai_set_local_model, model_download post-load). Adds
    /// ~15–40 s for a 3–8 GB file but the user already initiated the
    /// swap and is waiting on a "loading model" spinner — very
    /// different UX contract from boot. Prevents an attacker with
    /// user-uid file-write access from planting a crafted GGUF that
    /// gets mmap'd on the next mode switch.
    pub fn load_verified(path: PathBuf) -> Result<Self, String> {
        crate::models::verify_integrity(&path)?;
        Self::load(path)
    }

    /// Look up the registry entry matching this model's file on disk, if
    /// any. Lets the status command show a precise "E2B Q4_K_M" label
    /// instead of the generic "Gemma (local)". Returns None for models
    /// loaded from a path outside the registry (future: user-supplied
    /// GGUFs).
    pub fn model_spec(&self) -> Option<&'static crate::models::ModelSpec> {
        let basename = self.model_path.file_name()?.to_str()?;
        crate::models::REGISTRY
            .iter()
            .find(|s| s.filename == basename)
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
        let cpu_threads = self.cpu_threads;

        let (tx, rx) = mpsc::unbounded_channel::<Result<String, String>>();

        // Build the formatted prompt AHEAD of spawn_blocking so template
        // failures surface on this task, not the worker — cleaner error
        // propagation back to the caller.
        let user_content = compose_user_message(&req);
        // Try the GGUF's own template first. If apply fails (Gemma 4's
        // Jinja template uses conditionals that llama.cpp's legacy parser
        // rejects with `ffi error -1`), gracefully fall back to our
        // hardcoded template — same one Phase 5b used successfully for
        // Gemma 3. We do NOT propagate the error because falling back
        // produces usable output; only a logged warning.
        let (prompt, template_aware) = match &self.chat_template {
            Some(tmpl) => {
                let msg = match LlamaChatMessage::new(
                    "user".to_string(),
                    user_content.clone(),
                ) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = tx.send(Err(format!("chat message: {e}")));
                        return UnboundedReceiverStream::new(rx).boxed();
                    }
                };
                let model_guard = model.lock();
                match model_guard.apply_chat_template(tmpl, &[msg], true) {
                    Ok(s) => (s, true),
                    Err(e) => {
                        log::warn!(
                            "apply_chat_template failed ({e}); falling back to \
                             hardcoded Gemma template. This usually means the \
                             model's Jinja uses features llama.cpp's legacy parser \
                             doesn't support."
                        );
                        (hardcoded_gemma_template(&user_content), false)
                    }
                }
            }
            None => (hardcoded_gemma_template(&user_content), false),
        };

        tokio::task::spawn_blocking(move || {
            run_inference(backend, model, prompt, template_aware, cpu_threads, tx);
        });

        UnboundedReceiverStream::new(rx).boxed()
    }
}

/// Actual inference loop. Runs on a blocking thread; pumps tokens into `tx`.
/// Errors are delivered on the same channel as `Err(...)` items.
///
/// `template_aware` tells us whether `prompt` was produced by the model's
/// own chat template (in which case it already contains any BOS tokens
/// the template expects and we must NOT prepend another one) or by our
/// hardcoded fallback template (which doesn't include BOS, so we add it).
///
/// `cpu_threads` is `Some` only for CPU-only inference (see
/// `cpu_threads_for`); `None` keeps llama.cpp's defaults.
fn run_inference(
    backend: Arc<LlamaBackend>,
    model: Arc<Mutex<LlamaModel>>,
    prompt: String,
    template_aware: bool,
    cpu_threads: Option<i32>,
    tx: mpsc::UnboundedSender<Result<String, String>>,
) {
    let model = model.lock();
    // 1. Context.
    let mut ctx_params = LlamaContextParams::default().with_n_ctx(
        NonZeroU32::new(CTX_SIZE),
    );
    if let Some(n) = cpu_threads {
        // Prompt processing (batch) and generation both get the full
        // physical core count; the batch pass is the one that dominates
        // time-to-first-token with our several-hundred-token context.
        ctx_params = ctx_params.with_n_threads(n).with_n_threads_batch(n);
    }
    let mut ctx = match model.new_context(&backend, ctx_params) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(Err(format!("llama context: {e}")));
            return;
        }
    };

    // 2. Tokenize the prompt. `add_special` lets llama.cpp prepend BOS
    // (Gemma's vocab asks for one); the chat template already emits BOS
    // where the model expects it and double-BOS produces garbage output,
    // so only the hardcoded fallback template gets it. `parse_special`
    // stays on so `<start_of_turn>` markers become control tokens rather
    // than literal text.
    let vocab = model.vocab();
    let tokens = vocab.tokenize(prompt.as_bytes(), !template_aware, true);
    if tokens.is_empty() {
        let _ = tx.send(Err("tokenize: prompt produced no tokens".into()));
        return;
    }
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
    let mut piece_bytes: Vec<u8> = Vec::with_capacity(16);

    while n_cur < n_total {
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);

        if vocab.is_eog(token) {
            break;
        }

        // Detokenize to bytes, then run them through a persistent UTF-8
        // decoder so multi-byte sequences that span token boundaries
        // (common in languages with wide codepoints) don't come out
        // mangled. `special: false` strips <bos>/<eos>/control tokens from
        // output — we only want user-visible text. `lstrip: None` keeps
        // leading spaces inside tokens intact (model naturally emits
        // " word").
        piece_bytes.clear();
        vocab.token_to_piece_into(token, &mut piece_bytes, false, None);
        // decode_to_string never grows the String and silently stops at
        // capacity, so reserve what the decoder says it may need (a lone
        // invalid byte expands to a 3-byte U+FFFD).
        let mut piece = String::new();
        piece.reserve(
            utf8_decoder
                .max_utf8_buffer_length(piece_bytes.len())
                .unwrap_or(piece_bytes.len() * 3 + 4),
        );
        let _ = utf8_decoder.decode_to_string(&piece_bytes, &mut piece, false);
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

/// Pick the device `/arcterm-status` names. GPU-class entries win because
/// those are what llama.cpp offloads to; the CPU entry is always present,
/// and an accelerator entry (BLAS) is not GPU offload — naming either
/// would make the status line claim a GPU on a CPU-only box.
fn gpu_device_label(devices: &[LlamaBackendDevice]) -> Option<String> {
    devices
        .iter()
        .find(|d| {
            matches!(
                d.device_type,
                LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
            )
        })
        .map(|d| format!("{} via {}", d.description, d.backend))
}

/// Thread count for CPU-only inference. llama.cpp defaults to a flat 4
/// threads whatever the machine, which leaves most of a modern laptop
/// idle; physical cores — not SMT siblings, which contend for the same
/// vector units and slow matmul down — is the usual sweet spot. With GPU
/// offload the default stays: the CPU only orchestrates, and macOS
/// behaviour is unchanged.
fn cpu_threads_for(gpu_offload: bool) -> Option<i32> {
    if gpu_offload {
        return None;
    }
    // num_cpus falls back to the logical count (std's
    // available_parallelism) when it cannot read the core topology.
    let physical = num_cpus::get_physical().max(1);
    Some(i32::try_from(physical).unwrap_or(i32::MAX))
}

/// Windows only: register the ggml backend DLLs that ship next to the
/// exe. Nothing is linked statically on that target (`dynamic-backends`
/// in Cargo.toml), so without this call llama.cpp would not even have a
/// CPU backend and every model load would fail. The exe directory is
/// where tauri.windows.conf.json installs the DLLs — it is also Tauri's
/// `resource_dir()` on Windows; `current_exe()` just spares threading an
/// AppHandle through every load path. Once per process: the registry
/// outlives `llama_backend_free`, and loading again would register each
/// device twice.
#[cfg(target_os = "windows")]
fn load_dynamic_backends() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf));
        let Some(dir) = exe_dir else {
            log::warn!("ggml backends: cannot resolve the exe directory; no backends loaded");
            return;
        };
        // load_backends_from_path panics on a non-UTF-8 path. Install dirs
        // live under the user profile, so a profile name with an unpaired
        // surrogate would otherwise take the whole app down.
        if dir.to_str().is_none() {
            log::warn!(
                "ggml backends: exe directory {} is not valid UTF-8; no backends loaded",
                dir.display()
            );
            return;
        }
        llama_cpp_2::llama_backend::load_backends_from_path(&dir);
        log::info!("ggml backends loaded from {}", dir.display());
    });
}

#[cfg(not(target_os = "windows"))]
fn load_dynamic_backends() {}

/// Compose the plain-text content of the single user message we send.
/// This becomes the payload the model's chat template wraps; we DON'T
/// add template markers here — apply_chat_template handles that based
/// on the model's own training format.
fn compose_user_message(req: &AiRequest) -> String {
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
    user
}

/// Fallback: a hand-rolled Gemma 4 chat wrapper. Used only if the GGUF
/// has no embedded chat_template metadata (rare for modern instruction-
/// tuned releases). Kept tightly scoped so it can't accidentally be used
/// when the real template is available.
fn hardcoded_gemma_template(user_content: &str) -> String {
    format!(
        "<start_of_turn>user\n{user_content}<end_of_turn>\n<start_of_turn>model\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use llama_cpp_2::{LlamaBackendDevice, LlamaBackendDeviceType};

    fn device(
        index: usize,
        description: &str,
        backend: &str,
        device_type: LlamaBackendDeviceType,
    ) -> LlamaBackendDevice {
        LlamaBackendDevice {
            index,
            name: format!("{backend}{index}"),
            description: description.to_string(),
            backend: backend.to_string(),
            memory_total: 0,
            memory_free: 0,
            device_type,
        }
    }

    #[test]
    fn gpu_device_label_names_the_gpu_class_device_not_the_cpu() {
        // ggml lists the CPU device first on every platform; the status
        // line must still name the Vulkan iGPU llama.cpp offloads to.
        let devices = [
            device(0, "13th Gen Intel(R) Core(TM) i7", "CPU", LlamaBackendDeviceType::Cpu),
            device(1, "Intel(R) Arc(TM) Graphics", "Vulkan", LlamaBackendDeviceType::IntegratedGpu),
        ];
        assert_eq!(
            gpu_device_label(&devices).as_deref(),
            Some("Intel(R) Arc(TM) Graphics via Vulkan")
        );
    }

    #[test]
    fn gpu_device_label_is_none_without_a_gpu_class_device() {
        // BLAS registers as an accelerator; it is not GPU offload and must
        // not make /arcterm-status claim "GPU: yes".
        let devices = [
            device(0, "AMD Ryzen 5", "CPU", LlamaBackendDeviceType::Cpu),
            device(1, "BLAS", "BLAS", LlamaBackendDeviceType::Accelerator),
        ];
        assert_eq!(gpu_device_label(&devices), None);
    }

    /// Real-model check for the engine bump. Needs a downloaded GGUF;
    /// run by hand with
    /// `cargo test local_inference_smoke -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "loads a multi-GB GGUF from ~/.arcterm/models; manual only"]
    async fn local_inference_smoke() {
        // Surface the ggml device list + GPU probe lines under --nocapture.
        let _ = env_logger::builder().is_test(true).try_init();
        let spec = crate::models::REGISTRY
            .iter()
            .find(|s| s.is_installed())
            .expect("no local model installed");
        let backend = LocalLlamaBackend::load(spec.local_path().expect("model path"))
            .expect("model load");
        eprintln!(
            "model={} gpu_offload={} device={:?} cpu_threads={:?}",
            spec.id, backend.gpu_offload, backend.gpu_device, backend.cpu_threads
        );
        let resp = backend
            .ask(AiRequest {
                prompt: "Reply with exactly one word: pong".to_string(),
                context: None,
                mode: AiMode::Chat,
            })
            .await
            .expect("ask");
        eprintln!("response={:?}", resp.text);
        assert!(!resp.text.is_empty(), "model produced no text");
    }

    #[test]
    fn cpu_threads_are_only_pinned_without_gpu_offload() {
        // With a GPU, llama.cpp's own default stays (macOS unchanged);
        // CPU-only inference gets a positive core count.
        assert_eq!(cpu_threads_for(true), None);
        let n = cpu_threads_for(false).expect("CPU-only path pins a thread count");
        assert!(n >= 1, "thread count must be positive, got {n}");
    }
}
