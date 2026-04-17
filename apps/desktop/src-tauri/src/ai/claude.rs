//! Claude CLI backend.
//!
//! Shells out to the `claude` CLI that the user already has installed
//! (their Claude Pro/Max subscription pays for usage). We never set
//! `ANTHROPIC_API_KEY` — if the user has it exported, we unset it for the
//! subprocess so the CLI authenticates via the subscription session.
//! Otherwise we'd silently burn their API credits instead of using the
//! subscription they're paying for.
//!
//! Wire formats we parse:
//!
//!   `claude -p "<prompt>" --output-format json`
//!     Single JSON object on stdout with a `result` string. Used by ask().
//!
//!   `claude -p "<prompt>" --output-format stream-json --verbose`
//!     One JSON object per line. Types include:
//!       { "type": "system", "subtype": "init", ... }  (discarded)
//!       { "type": "assistant", "message": { "content": [{"type":"text","text":"..."}] } }
//!       { "type": "result", "result": "final answer", ... }
//!     We emit the per-chunk text deltas for streaming.

use std::process::Stdio;

use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::{AiBackend, AiMode, AiRequest, AiResponse};

/// Timeouts. The CLI can take a while on long prompts; 60s is generous but
/// not so long the UI looks hung forever if something's wrong.
const ASK_TIMEOUT_SECS: u64 = 60;

pub struct ClaudeCliBackend {
    /// Path to the `claude` binary. Defaults to "claude" (PATH lookup);
    /// config can override (e.g. for users who installed via a non-
    /// standard location). Phase 5b config plumbing sets this.
    pub binary: String,
}

impl Default for ClaudeCliBackend {
    fn default() -> Self {
        Self {
            binary: "claude".to_string(),
        }
    }
}

#[async_trait]
impl AiBackend for ClaudeCliBackend {
    fn id(&self) -> &'static str {
        "claude-cli"
    }

    fn display_name(&self) -> &'static str {
        "Claude"
    }

    async fn is_available(&self) -> bool {
        // `claude --version` is cheap and doesn't hit the network or
        // auth layer. If the binary isn't on PATH, Command spawn fails.
        match Command::new(&self.binary)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    }

    async fn ask(&self, req: AiRequest) -> Result<AiResponse, String> {
        let prompt = build_prompt(&req);
        let mut cmd = base_command(&self.binary);
        cmd.arg("-p").arg(&prompt);
        cmd.arg("--output-format").arg("json");

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(ASK_TIMEOUT_SECS),
            cmd.output(),
        )
        .await
        .map_err(|_| "Claude CLI timed out".to_string())?
        .map_err(|e| format!("Claude CLI spawn failed: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // The CLI emits JSON on stdout even in the error path, so parse
        // stdout first and prefer its message. Only fall back to the raw
        // status + stderr if stdout isn't parseable (CLI not installed,
        // killed by signal, etc).
        //
        // `api_error_status` comes across as an int when HTTP failed (401,
        // 429, 500) and as a null on success. We custom-deserialize it to
        // a display-friendly string regardless of shape.
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Envelope {
            result: String,
            is_error: bool,
            #[serde(deserialize_with = "de_flexible_string")]
            api_error_status: Option<String>,
            subtype: Option<String>,
        }

        match serde_json::from_str::<Envelope>(stdout.trim()) {
            Ok(env) => {
                if env.is_error || !output.status.success() {
                    // The CLI's error message goes in `result`. Other
                    // diagnostic bits (subtype, api_error_status) come
                    // through too so we can tell auth issues from
                    // rate-limits from transport errors.
                    let hint = env
                        .subtype
                        .filter(|s| s != "success")
                        .map(|s| format!(" [{s}]"))
                        .unwrap_or_default();
                    let api = env
                        .api_error_status
                        .map(|s| format!(" (api status: {s})"))
                        .unwrap_or_default();
                    let msg = if env.result.is_empty() {
                        format!("Claude CLI failed{hint}{api}")
                    } else {
                        format!("{}{hint}{api}", env.result)
                    };
                    return Err(msg);
                }
                Ok(AiResponse {
                    text: env.result.trim().to_string(),
                    backend: Self::default().id().to_string(),
                })
            }
            Err(parse_err) => {
                // Non-JSON output means the CLI barfed before it could
                // emit its envelope. Include both stdout and stderr so
                // the user has enough to diagnose (missing auth, bad
                // flag, killed by signal, etc).
                Err(format!(
                    "Claude CLI exited {} (unparseable output). \
                     stdout: {} ;; stderr: {} ;; parse: {parse_err}",
                    output.status,
                    stdout.trim(),
                    stderr.trim(),
                ))
            }
        }
    }

    fn stream(&self, req: AiRequest) -> BoxStream<'static, Result<String, String>> {
        let prompt = build_prompt(&req);
        let binary = self.binary.clone();

        // tokio channel → async stream. We spawn a task that runs the CLI
        // and pumps per-line deltas; the returned stream just consumes.
        let (tx, rx) = mpsc::unbounded_channel::<Result<String, String>>();

        tokio::spawn(async move {
            let mut cmd = base_command(&binary);
            cmd.arg("-p").arg(&prompt);
            cmd.arg("--output-format").arg("stream-json");
            cmd.arg("--verbose"); // stream-json requires --verbose per docs
            cmd.stdout(Stdio::piped());
            cmd.stderr(Stdio::piped());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Err(format!("Claude CLI spawn failed: {e}")));
                    return;
                }
            };
            let stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    let _ = tx.send(Err("Claude CLI: stdout handle missing".into()));
                    return;
                }
            };

            let mut reader = BufReader::new(stdout).lines();
            let mut last_text = String::new();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<StreamEvent>(trimmed) {
                    Ok(StreamEvent::Assistant { message }) => {
                        // Text deltas: each event carries the CURRENT full
                        // assistant message in some CLI versions. We compute
                        // the diff against `last_text` so the UI sees true
                        // deltas. If the content is actually incremental
                        // (newer CLI behavior) the diff == the new content
                        // and this still works.
                        let current = message.text();
                        let delta = if current.len() >= last_text.len()
                            && current.starts_with(&last_text)
                        {
                            current[last_text.len()..].to_string()
                        } else {
                            // Non-append update (rare) — emit whole thing and
                            // let the frontend replace.
                            current.clone()
                        };
                        last_text = current;
                        if !delta.is_empty() && tx.send(Ok(delta)).is_err() {
                            // Receiver dropped; stop pumping.
                            let _ = child.kill().await;
                            return;
                        }
                    }
                    Ok(StreamEvent::Result { result, is_error }) => {
                        if is_error {
                            let _ = tx.send(Err(result));
                        }
                        // Don't emit the final text — it duplicates whatever
                        // we've already streamed via assistant events.
                        break;
                    }
                    Ok(StreamEvent::Other) => {
                        // system/init/tool_use etc — ignore for chat use.
                    }
                    Err(_) => {
                        // Non-JSON line (shouldn't happen with stream-json,
                        // but guard against CLI oddities).
                        continue;
                    }
                }
            }
            // Drain process to avoid zombies.
            let _ = child.wait().await;
        });

        UnboundedReceiverStream::new(rx).boxed()
    }
}

/// Build the `base` command: binary + env sanitation. The key reason this
/// is factored out is Anthropic auth env scrubbing — if the user has stale
/// credentials in any of several vars, the CLI will pick them up and
/// override their logged-in session (→ 401 with "Invalid authentication
/// credentials"). The session stored in ~/.claude wins only when these
/// are all unset.
fn base_command(binary: &str) -> Command {
    let mut cmd = Command::new(binary);
    // Every auth-relevant Anthropic/Claude env var we know of. Being
    // aggressive here costs nothing (the subscription path doesn't need
    // any of these) and fixes a whole class of "works in my terminal but
    // not in ArcTerm" reports from users who have stale tokens lurking
    // in their .zshrc/.zshenv.
    for var in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_TOKEN",
        "ANTHROPIC_SESSION_KEY",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_CUSTOM_HEADERS",
        "CLAUDE_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// Parse a JSON value that may arrive as a string, a number, a bool, or
/// null, and render it as `Option<String>` for display. Used for the
/// CLI's `api_error_status` field (int on error, null on success).
fn de_flexible_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = Value::deserialize(d)?;
    Ok(match v {
        Value::Null => None,
        Value::String(s) if s.is_empty() => None,
        Value::String(s) => Some(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        // Arrays/objects are unlikely here; if they happen we render as
        // JSON so at least the user sees something meaningful.
        other => Some(other.to_string()),
    })
}

/// Combine the structured request into the final prompt text. We prefix
/// the context block so the model can read it before the user's question.
fn build_prompt(req: &AiRequest) -> String {
    let mut out = String::new();
    if let Some(ctx) = &req.context {
        out.push_str(&ctx.to_prompt_block());
        out.push('\n');
    }
    match req.mode {
        AiMode::Command => {
            // Strict constraints so we get an executable line back, not a
            // Markdown code block with commentary. The model still
            // occasionally includes backticks — frontend strips them.
            out.push_str(
                "Convert the following request into a single shell command \
                 for zsh on macOS. Output ONLY the command, no explanation, \
                 no markdown fences, no trailing newline.\n\n",
            );
            out.push_str("Request: ");
        }
        AiMode::Explain => {
            out.push_str(
                "Explain the command or error above. Give a short plain \
                 explanation (2-4 sentences) and then, if applicable, a \
                 suggested fix as a one-line shell command on the last \
                 line in a markdown code block.\n\n",
            );
        }
        AiMode::Chat => {}
    }
    out.push_str(&req.prompt);
    out
}

/// CLI stream-json event shapes. We parse a minimal superset — extra fields
/// are ignored via serde's default behavior.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamEvent {
    Assistant { message: AssistantMessage },
    Result {
        result: String,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

impl AssistantMessage {
    fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
    #[serde(other)]
    Other,
}
