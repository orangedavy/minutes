//! Structured Recall chat backend (Phase 2).
//!
//! Spawns `claude --print --output-format stream-json --verbose --include-partial-messages`
//! for each user message and streams structured events back to the frontend.
//! Maintains session continuity via `--resume <session_id>`.

use serde::{Deserialize, Serialize};
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use tauri::Emitter;

/// Persisted chat session state.
#[derive(Default)]
pub struct RecallChatState {
    /// Claude session ID for `--resume` continuity.
    pub session_id: Option<String>,
    /// Workspace directory for the assistant.
    pub workspace: Option<PathBuf>,
}

/// Events emitted to the frontend via Tauri event bus.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum ChatEvent {
    /// A streaming text delta (token-by-token).
    #[serde(rename = "delta")]
    Delta { text: String },
    /// The full response is done.
    #[serde(rename = "done")]
    Done { text: String, session_id: String },
    /// An error occurred.
    #[serde(rename = "error")]
    Error { message: String },
}

/// Minimal JSON parse structs for claude stream-json output.
#[derive(Deserialize)]
struct StreamLine {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    event: Option<StreamEvent>,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    delta: Option<StreamDelta>,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: Option<String>,
}

/// Send a message to Claude and stream the response back via events.
///
/// This is the core function called by `cmd_recall_chat`. It:
/// 1. Finds the claude binary
/// 2. Builds the command with appropriate flags
/// 3. Spawns the process and reads stdout line by line
/// 4. Emits `recall:chat` events for each text delta
/// 5. Stores the session_id for future `--resume` calls
pub fn send_message(
    message: &str,
    agent_binary: &PathBuf,
    workspace: &PathBuf,
    chat_state: &Arc<Mutex<RecallChatState>>,
    app_handle: &tauri::AppHandle,
) -> Result<String, String> {
    let session_id = chat_state
        .lock()
        .map_err(|_| "Chat state lock failed")?
        .session_id
        .clone();

    let mut args = vec![
        "--print".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--include-partial-messages".to_string(),
    ];

    if let Some(ref sid) = session_id {
        args.push("--resume".to_string());
        args.push(sid.clone());
    }

    args.push("-p".to_string());
    args.push(message.to_string());

    // Build a rich PATH for macOS GUI processes (same approach as pty.rs)
    let path_env = build_rich_path();

    let mut child = Command::new(agent_binary)
        .args(&args)
        .current_dir(workspace)
        .env("PATH", &path_env)
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn claude: {}", e))?;

    let stdout = child.stdout.take().ok_or("No stdout")?;
    let reader = std::io::BufReader::new(stdout);

    let mut full_text = String::new();
    let mut new_session_id: Option<String> = None;

    for line_result in reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.is_empty() {
            continue;
        }

        let parsed: StreamLine = match serde_json::from_str(&line) {
            Ok(p) => p,
            Err(_) => continue,
        };

        match parsed.event_type.as_str() {
            "system" => {
                // Capture session_id from init event
                if parsed.subtype.as_deref() == Some("init") {
                    if let Some(sid) = parsed.session_id.clone() {
                        new_session_id = Some(sid);
                    }
                }
            }
            "stream_event" => {
                if let Some(ref event) = parsed.event {
                    if event.event_type == "content_block_delta" {
                        if let Some(ref delta) = event.delta {
                            if delta.delta_type == "text_delta" {
                                if let Some(ref text) = delta.text {
                                    full_text.push_str(text);
                                    app_handle
                                        .emit_to(
                                            "main",
                                            "recall:chat",
                                            ChatEvent::Delta {
                                                text: text.clone(),
                                            },
                                        )
                                        .ok();
                                }
                            }
                        }
                    }
                }
            }
            "result" => {
                // Use the result text as the canonical full response
                if let Some(ref result_text) = parsed.result {
                    full_text = result_text.clone();
                }
                if let Some(sid) = parsed.session_id.clone() {
                    new_session_id = Some(sid);
                }
            }
            _ => {}
        }
    }

    // Wait for process to exit
    let status = child.wait().map_err(|e| format!("Wait failed: {}", e))?;

    if !status.success() && full_text.is_empty() {
        let stderr_msg = child
            .stderr
            .and_then(|mut s| {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut s, &mut buf).ok()?;
                Some(buf)
            })
            .unwrap_or_default();
        let err = if stderr_msg.is_empty() {
            format!("claude exited with code {}", status.code().unwrap_or(-1))
        } else {
            stderr_msg.lines().take(3).collect::<Vec<_>>().join("\n")
        };
        app_handle
            .emit_to("main", "recall:chat", ChatEvent::Error { message: err.clone() })
            .ok();
        return Err(err);
    }

    // Store session_id for continuity
    let final_session_id = new_session_id.unwrap_or_default();
    if !final_session_id.is_empty() {
        if let Ok(mut state) = chat_state.lock() {
            state.session_id = Some(final_session_id.clone());
        }
    }

    // Emit done event
    app_handle
        .emit_to(
            "main",
            "recall:chat",
            ChatEvent::Done {
                text: full_text.clone(),
                session_id: final_session_id,
            },
        )
        .ok();

    Ok(full_text)
}

/// Build a PATH string that includes common agent install locations.
/// GUI apps on macOS get a minimal PATH, so we need to be explicit.
fn build_rich_path() -> String {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    let mut dirs = vec![
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ];

    // Include nvm/fnm managed node paths
    let nvm_dir = std::env::var("NVM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".nvm"));
    if nvm_dir.exists() {
        // Try to find the default node version
        let default_link = nvm_dir.join("alias/default");
        if let Ok(version) = std::fs::read_to_string(&default_link) {
            let node_bin = nvm_dir
                .join("versions/node")
                .join(version.trim())
                .join("bin");
            if node_bin.exists() {
                dirs.insert(0, node_bin);
            }
        }
        // Fallback: current symlink
        let current_bin = nvm_dir.join("current/bin");
        if current_bin.exists() {
            dirs.insert(0, current_bin);
        }
    }

    // Existing PATH entries
    if let Ok(existing) = std::env::var("PATH") {
        for p in existing.split(':') {
            let pb = PathBuf::from(p);
            if !dirs.contains(&pb) {
                dirs.push(pb);
            }
        }
    }

    dirs.iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(":")
}
