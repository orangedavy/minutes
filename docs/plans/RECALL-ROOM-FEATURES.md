# Recall Room — Feature Requirements

> Full-featured conversational AI surface built into the Minutes desktop app.
> Press **W** to open. Backed by `claude --print --output-format stream-json`.

---

## Current State (Phase 2–3 shipped)

- ✅ Streaming chat via `claude --print` subprocess
- ✅ Session continuity (`--resume <session_id>`)
- ✅ Two-pane layout: thread rail (168px) + chat main
- ✅ Context strip with chip slots
- ✅ Message bubbles (.rr-msg > .who + .bubble)
- ✅ Textarea input with auto-grow
- ✅ New Thread button (resets session)
- ✅ W / Escape keyboard shortcuts
- ✅ Ask pill integration from home screen

---

## Phase 4: Core Features

### 4.1 — Meeting-Aware System Prompt

**Goal:** Claude automatically knows about the user's recent meetings without manual context selection.

**Behavior:**
- On each message send, the backend builds a system prompt that includes:
  - Summary of the 5 most recent meetings (title, date, participants, key decisions, action items)
  - Any active/overdue action items
  - Today's calendar context (if available)
- The system prompt is passed via `--system-prompt` or prepended to first message in a new session
- Context is refreshed on each new thread (not mid-thread)
- The context strip shows which meetings were auto-included as chips (read-only, non-removable)

**Backend changes (chat.rs):**
- New function `build_system_context()` that reads from `~/meetings/` (using minutes-reader or grep)
- Pass `--system-prompt <file>` to claude CLI (write temp file with context)
- Return list of included meeting filenames to frontend for chip display

**Frontend:**
- Auto-populated `.rr-chip.is-meeting` items in the context strip (with calendar icon)
- Chips are informational (show what's included), not interactive for auto-context

---

### 4.2 — Thread Persistence

**Goal:** Threads survive app restarts. Users can browse, resume, and delete past conversations.

**Storage:** `~/.minutes/recall/threads/`
```
~/.minutes/recall/threads/
├── index.json          # Thread list metadata (id, title, created, lastMessage)
└── <thread-id>.json    # Individual thread messages
```

**Thread format (per-thread JSON):**
```json
{
  "id": "thread_abc123",
  "title": "hi there",
  "created": "2026-06-01T10:30:00Z",
  "updated": "2026-06-01T10:35:00Z",
  "session_id": "claude-session-xyz",
  "messages": [
    { "role": "user", "content": "hi there", "ts": "2026-06-01T10:30:00Z" },
    { "role": "assistant", "content": "Hi! How can I help?", "ts": "2026-06-01T10:30:02Z" }
  ]
}
```

**Backend commands (new):**
- `cmd_recall_list_threads()` → returns thread index (id, title, updated, messageCount)
- `cmd_recall_load_thread(id)` → returns full thread JSON
- `cmd_recall_delete_thread(id)` → removes from disk
- `cmd_recall_rename_thread(id, title)` → updates title

**Frontend behavior:**
- On app start: load thread index, populate rail
- On send: append message + response to current thread file
- Thread title: auto-generated from first user message (first 40 chars)
- Rail shows threads sorted by `updated` (most recent first)
- Click thread in rail → load and display its messages, resume its session_id
- Right-click thread → context menu: Rename, Delete, Pin

---

### 4.3 — Thread Deletion

**Goal:** Remove unwanted threads from the rail and disk.

**UI:**
- Swipe-left on thread item in rail → reveals red "Delete" button (mobile pattern at desktop scale)
- OR: right-click → context menu with "Delete thread"
- OR: hover → small × icon appears on thread item
- Confirmation: none needed (threads are cheap, not destructive like meetings)

**Backend:** `cmd_recall_delete_thread(id)` removes the JSON file and updates index.

---

### 4.4 — Stop Generation

**Goal:** Cancel a streaming response mid-generation.

**Backend:**
- Store the child process PID in `RecallChatState`
- New command: `cmd_recall_stop()` → sends SIGTERM to the running claude process
- After kill: emit `ChatEvent::Stopped` so frontend knows it was intentional

**Frontend:**
- While streaming: the Send button transforms into a Stop button (■ square icon)
- Clicking Stop calls `cmd_recall_stop()`
- The partial response is kept in the bubble (not discarded)
- Bubble gets a subtle "(stopped)" indicator

---

### 4.5 — Copy Response

**Goal:** One-click copy of assistant messages.

**UI:**
- On hover over an assistant bubble: a small copy icon (📋) appears top-right of the bubble
- Click → copies bubble text content to clipboard
- Brief "Copied!" tooltip or icon change (checkmark) for 1.5s

**Implementation:** Pure frontend — `navigator.clipboard.writeText(bubble.textContent)`

---

### 4.6 — Markdown Rendering

**Goal:** Render Claude's responses as rich markdown (code blocks, bold, lists, links, tables).

**Library:** Lightweight markdown → HTML renderer. Options:
- **marked** (~40KB) — fast, well-maintained
- **markdown-it** (~100KB) — more plugins
- **Custom minimal parser** — for just code blocks + bold + lists (smallest)

**Recommendation:** Bundle `marked` (minified, inline in index.html or loaded from assets).

**Behavior:**
- Assistant bubbles render markdown on finalize (after streaming completes)
- During streaming: plain text with cursor (avoids flicker from partial markdown)
- On finalize: parse full text → render as HTML inside bubble
- Code blocks get syntax highlighting (highlight.js subset or Prism, ~30KB for common langs)
- Code blocks get a "Copy" button in their top-right corner
- User messages stay plain text (no rendering)

**Security:** Sanitize HTML output (no raw `<script>` injection from Claude responses). Use `marked` with `sanitize: true` or DOMPurify.

---

### 4.7 — File Attachment (Text/Code)

**Goal:** Attach text files to a message so Claude can read them.

**UI:**
- Paperclip icon (📎) next to the textarea in the input row
- Click → native file picker (Tauri `dialog::open`)
- Or: drag-and-drop file onto the input area
- Attached files shown as chips below the textarea (filename + × to remove)
- Max: 5 files per message, 100KB each

**Backend:**
- Read file content, prepend to the message as:
  ```
  <attached_file name="config.toml">
  [contents here]
  </attached_file>

  [user's actual message]
  ```
- Pass as part of the `-p` argument to claude

**Supported types:** `.txt`, `.md`, `.rs`, `.ts`, `.js`, `.py`, `.toml`, `.json`, `.yaml`, `.sh`, `.css`, `.html`, `.swift`, `.c`, `.cpp`, `.h`

---

### 4.8 — Context Chips (Manual Addition)

**Goal:** The "+ Add context" chip lets users manually add specific meetings or tags as context.

**UI flow:**
1. Click "+ Add context" chip
2. Small popover/dropdown appears with:
   - Recent meetings (last 10, from `~/meetings/`)
   - Search input to filter
   - Tags section (extracted from meeting frontmatter)
3. Click a meeting → adds it as a `.rr-chip.is-meeting` in the strip
4. Click a tag → adds as `.rr-chip.is-tag`
5. Each added chip has × to remove

**Backend:**
- `cmd_recall_list_context_options()` → returns recent meetings + tags
- Added context is included in the system prompt for the thread
- Persisted per-thread in the thread JSON (`context: [...]`)

---

## Phase 5: Polish & UX

### 5.1 — Fix Visual Bugs

| Bug | Cause | Fix |
|-----|-------|-----|
| Random green cursor in rail | `.rr-thread.is-active::before` using `float: left` renders awkwardly | Switch to flexbox with a proper left-bar indicator via `border-left` or absolute positioning |
| Thread accent bar too tall | Height 16px with float creates layout weirdness | Use `border-left: 2px solid var(--accent)` on the `.rr-thread.is-active` element directly |
| Empty state shows even after messages | CSS `:empty` pseudo-class counts whitespace nodes | Ensure threadView has no text nodes; or switch to a JS-controlled empty state |

---

### 5.2 — Slash Command Autocomplete

**Goal:** Surface Claude CLI slash commands (`/model`, `/clear`, `/help`, etc.) via autocomplete in the textarea.

**Known commands:**
- `/model` — show/switch model
- `/clear` — clear conversation
- `/help` — show help
- `/compact` — compact conversation
- `/cost` — show token usage

**UI:**
- When user types `/` as the first character, show a floating autocomplete menu above the textarea
- Menu lists available commands with brief descriptions
- Arrow keys to navigate, Enter to select, Escape to dismiss
- Selected command replaces the `/` text in textarea

**Implementation:**
- Pure frontend (static list of known commands)
- Commands are sent as regular messages to Claude (the CLI handles them)
- Some commands (like `/clear`) could be intercepted client-side to also reset the thread

---

### 5.3 — Thinking/Loading State

**Goal:** Show Claude is processing before the first delta arrives.

**UI:**
- After sending, immediately show a "RECALL" message with an animated thinking indicator
- Three-dot pulse animation (⠋⠙⠹ or ···) inside the bubble
- Replaced by real content once first delta arrives
- If no delta after 10s, show "Still thinking…" subtitle

---

### 5.4 — Keyboard Shortcuts (Recall-specific)

| Shortcut | Action |
|----------|--------|
| `Cmd+N` | New thread |
| `Cmd+Backspace` | Delete current thread |
| `Cmd+K` | Focus search/command (future) |
| `Up/Down` (in rail) | Navigate threads |
| `Cmd+C` (on bubble) | Copy focused message |
| `Cmd+Shift+S` | Stop generation |

---

## Implementation Order

| Priority | Feature | Effort | Dependencies |
|----------|---------|--------|--------------|
| P0 | Fix visual bugs (5.1) | S | None |
| P0 | Stop generation (4.4) | M | Backend: store child PID |
| P1 | Thread persistence (4.2) | L | Backend: new commands, disk IO |
| P1 | Thread deletion (4.3) | S | Depends on 4.2 |
| P1 | Markdown rendering (4.6) | M | Bundle marked.js |
| P1 | Copy response (4.5) | S | None |
| P2 | Meeting-aware system prompt (4.1) | L | Backend: reader integration |
| P2 | Thinking state (5.3) | S | None |
| P2 | File attachment (4.7) | M | Backend: file dialog + content injection |
| P3 | Context chips (4.8) | L | Backend: meeting list command |
| P3 | Slash command autocomplete (5.2) | M | Frontend only |
| P3 | Keyboard shortcuts (5.4) | S | Frontend only |

**S** = small (1–2 hours), **M** = medium (half day), **L** = large (full day+)

---

## Architecture Notes

- **claude CLI is the LLM interface** — we don't call APIs directly. All features route through `claude --print`.
- **`--system-prompt <file>`** is how we inject meeting context (temp file written per-thread).
- **`--resume <session_id>`** maintains thread continuity server-side (Claude manages history).
- **Thread storage is local** — `~/.minutes/recall/threads/`. No cloud sync.
- **The child process PID** must be stored in `RecallChatState` for stop-generation to work.
- **Markdown rendering happens only on finalize** — streaming shows raw text to avoid flicker.
- **No images in this phase** — text/code files only for attachments. Images would require multimodal API access that `claude --print` may not support easily.
