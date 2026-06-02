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
    /// PID of the currently running claude process (for stop generation).
    pub child_pid: Option<u32>,
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
    /// Generation was stopped by user.
    #[serde(rename = "stopped")]
    Stopped { text: String },
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
    } else {
        // First message in session — inject system prompt so Claude knows it's Recall
        args.push("--append-system-prompt".to_string());
        args.push(build_system_prompt(workspace));
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

    // Store child PID for stop-generation support
    let pid = child.id();
    if let Ok(mut state) = chat_state.lock() {
        state.child_pid = Some(pid);
    }

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

    // Clear child PID
    if let Ok(mut state) = chat_state.lock() {
        state.child_pid = None;
    }

    // Check if the process was killed (SIGTERM = signal 15 on macOS)
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            if sig == 15 || sig == 9 {
                // Process was killed by stop-generation
                app_handle
                    .emit_to("main", "recall:chat", ChatEvent::Stopped { text: full_text.clone() })
                    .ok();
                return Ok(full_text);
            }
        }
    }

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

/// Stop the currently running claude process (if any).
pub fn stop_generation(chat_state: &Arc<Mutex<RecallChatState>>) -> Result<(), String> {
    let pid = chat_state
        .lock()
        .map_err(|_| "Chat state lock failed")?
        .child_pid;

    match pid {
        Some(pid) => {
            #[cfg(unix)]
            {
                // Send SIGTERM for graceful shutdown
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            #[cfg(not(unix))]
            {
                // On Windows, use taskkill
                let _ = Command::new("taskkill")
                    .args(&["/PID", &pid.to_string(), "/F"])
                    .spawn();
            }
            Ok(())
        }
        None => Err("No active generation to stop".to_string()),
    }
}

/// Build the system prompt that gives Claude context about being Recall.
/// Includes a summary of recent meetings if available.
fn build_system_prompt(workspace: &PathBuf) -> String {
    let meetings_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("meetings");

    let mut prompt = String::from(
        "You are Recall, the conversational AI assistant inside Minutes — a privacy-first \
         meeting memory app. The user talks to you about their meetings, decisions, action \
         items, and people they've met with.\n\n\
         Your personality:\n\
         - Concise and direct. No filler.\n\
         - When referencing meetings, cite the date and title.\n\
         - If the user asks about something you don't have context for, say so clearly.\n\
         - Never make up meetings or facts that aren't in the provided context.\n\n",
    );

    // Try to list recent meetings for context
    if meetings_dir.exists() {
        let mut entries: Vec<_> = std::fs::read_dir(&meetings_dir)
            .ok()
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().and_then(|s| s.to_str()) == Some("md")
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Sort by modified time (most recent first)
        entries.sort_by(|a, b| {
            let ma = a.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let mb = b.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            mb.cmp(&ma)
        });

        let recent: Vec<_> = entries.into_iter().take(10).collect();
        if !recent.is_empty() {
            prompt.push_str("Recent meetings (most recent first):\n");
            for entry in &recent {
                let path = entry.path();
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown");
                prompt.push_str(&format!("- {}\n", name));
            }
            prompt.push_str("\nThe user's meetings are stored in: ");
            prompt.push_str(&meetings_dir.display().to_string());
            prompt.push('\n');
        }
    }

    // Note the workspace
    prompt.push_str(&format!(
        "\nYou are running in the Minutes desktop app workspace at: {}\n",
        workspace.display()
    ));

    prompt
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

// ─── Thread Persistence ─────────────────────────────────────────────────────

/// A single message in a persisted thread.
#[derive(Clone, Serialize, Deserialize)]
pub struct ThreadMessage {
    pub role: String,
    pub content: String,
    pub ts: String,
}

/// A persisted thread (stored as JSON on disk).
#[derive(Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub messages: Vec<ThreadMessage>,
}

/// Summary for thread list (without full messages).
#[derive(Clone, Serialize)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub created: String,
    pub updated: String,
    pub message_count: usize,
}

/// Get the threads directory, creating it if needed.
fn threads_dir() -> Result<PathBuf, String> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".minutes/recall/threads");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create threads dir: {}", e))?;
    Ok(dir)
}

/// List all threads (sorted by updated, most recent first).
pub fn list_threads() -> Result<Vec<ThreadSummary>, String> {
    let dir = threads_dir()?;
    let mut threads: Vec<ThreadSummary> = Vec::new();

    let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        if let Ok(thread) = serde_json::from_str::<Thread>(&data) {
            threads.push(ThreadSummary {
                id: thread.id,
                title: thread.title,
                created: thread.created,
                updated: thread.updated,
                message_count: thread.messages.len(),
            });
        }
    }

    threads.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(threads)
}

/// Load a single thread by ID.
pub fn load_thread(id: &str) -> Result<Thread, String> {
    let path = threads_dir()?.join(format!("{}.json", id));
    let data = std::fs::read_to_string(&path)
        .map_err(|_| format!("Thread '{}' not found", id))?;
    serde_json::from_str(&data).map_err(|e| format!("Parse error: {}", e))
}

/// Save (create or update) a thread.
pub fn save_thread(thread: &Thread) -> Result<(), String> {
    let path = threads_dir()?.join(format!("{}.json", thread.id));
    let data = serde_json::to_string_pretty(thread).map_err(|e| e.to_string())?;
    std::fs::write(&path, data).map_err(|e| format!("Write failed: {}", e))?;
    // Set restrictive permissions (0600)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Delete a thread by ID.
pub fn delete_thread(id: &str) -> Result<(), String> {
    let path = threads_dir()?.join(format!("{}.json", id));
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| format!("Delete failed: {}", e))?;
    }
    Ok(())
}

/// Rename a thread.
pub fn rename_thread(id: &str, new_title: &str) -> Result<(), String> {
    let mut thread = load_thread(id)?;
    thread.title = new_title.to_string();
    save_thread(&thread)
}
