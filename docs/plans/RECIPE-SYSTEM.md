# Recipe System Design

> Replace the current template system with a full recipe library — meeting-type-aware prompts that fully control extraction output.

## Problem

The current template system only appends `additional_instructions` to a fixed extraction prompt. This means:
- Every meeting produces the same rigid 6-section output (KEY POINTS, DECISIONS, ACTION ITEMS, etc.)
- No way to define custom sections per meeting type
- No shared user context (role, company, domain terminology)
- No auto-selection based on calendar events
- No in-app management UI

Granola-style recipes solve this: each recipe IS the full prompt, defining exactly what sections to extract and how to format them.

## Design

### Architecture

```
~/.minutes/
├── profile.md              ← Shared context (who you are, injected into all recipes)
├── recipes/
│   ├── manager-1on1.md     ← Full recipe files
│   ├── team-leads.md
│   ├── daily-standup.md
│   ├── first-meeting.md
│   ├── working-session.md  ← Default fallback
│   └── ...
```

### Profile (`~/.minutes/profile.md`)

A separate file defining shared context injected into every recipe. Keeps recipes DRY — you update your role once, not in every recipe.

```markdown
---
name: Davy Guo
role: Sr. Product Manager
company: Lingraphica
team: Team Elysian
focus: Practice App (speech therapy)
---

Lingraphica builds AAC devices and apps for adults with aphasia and other communication challenges. Davy works on Team Elysian, focused on the Practice App (speech therapy).

Optimize for traceability and recall. Preserve decisions, commitments, objections, risks, and context needed to reconstruct what happened.
```

The profile frontmatter provides structured fields for programmatic use (e.g. displaying "Davy Guo — Sr. PM" in settings). The body is the prose injected into prompts.

### Recipe File Format

Each recipe is a markdown file with YAML frontmatter. The body IS the prompt (not documentation — the entire body gets sent to the LLM).

```markdown
---
name: Manager 1:1
slug: manager-1on1
version: 1.0.0
description: Weekly 1:1 with manager. Coaching, priorities, leadership context.
triggers:
  calendar_keywords: ["1:1 Katie", "Katie Driscoll"]
  attendees: ["katie.driscoll@lingraphica.com"]
priority: 10
fallback: false
---

This is Davy's weekly 1:1 with his manager. Prioritize coaching, priority shifts,
leadership context, commitments, and support-level changes.

The meeting follows EOS Level 10 format. Goals use support levels: S1 FYI, S2 input
wanted, S3 decision needed, S4 urgent.

## Rules
- Use only # and ## headings.
- Prefer topic-based bullets over chronological summaries.
...

## Output Sections

# TLDR
Use 1-3 bullets...

# Meeting Context
Use 1-4 bullets...

# Coaching and Feedback
For each coaching moment:
## [Coaching topic]
- What Katie flagged: [behavior, habit, framing, or risk].
...
```

### Frontmatter Schema (v2)

```yaml
# Required
name: string              # Human-readable display name
slug: string              # Unique identifier (lowercase, hyphens)
version: string           # Semver

# Optional
description: string       # One-liner for recipe list UI
triggers:                 # Auto-selection rules
  calendar_keywords: []   # Match against calendar event title (case-insensitive substring)
  attendees: []           # Match if any attendee email is in the event
  regex: string           # Advanced: regex against event title
priority: number          # Higher = preferred when multiple recipes match (default: 0)
fallback: bool            # If true, used when no other recipe matches (default: false)
language: string          # Override summary language (default: inherit from profile/config)
```

### Selection Logic

```
1. Calendar match: event title/attendees → triggers.calendar_keywords / triggers.attendees
2. Manual override: user picks a recipe in the app UI before/during/after recording
3. LLM fallback: if no match and no manual pick, send first 500 words of transcript
   to a fast model to classify meeting type → pick best recipe
4. Default: recipe with `fallback: true` (e.g. "working-session")
```

The selection is recorded in the meeting's YAML frontmatter as `recipe: <slug>` so reprocessing uses the same recipe.

### Prompt Assembly

When the summarizer runs, the final prompt sent to the LLM is:

```
[Profile body]                          ← from ~/.minutes/profile.md
[Recipe body]                           ← the full recipe prompt (replaces base prompt entirely)

Summarize this transcript:

<transcript>
{transcript}
</transcript>
```

Key change from current system: **the recipe body replaces `build_base_system_prompt()` entirely** when `extends_base: false` (which is the new default for v2 recipes). The old `extends_base: true` behavior is preserved for backwards-compatible Phase 1 templates.

### Data Model Changes

```rust
// New frontmatter (replaces TemplateFrontmatter)
pub struct RecipeFrontmatter {
    pub name: String,
    pub slug: String,
    pub version: String,
    pub description: Option<String>,
    pub triggers: Option<RecipeTriggers>,
    pub priority: i32,              // default 0
    pub fallback: bool,             // default false
    pub language: Option<String>,
    // Deprecated fields kept for backwards compat:
    pub extends_base: Option<bool>,
    pub additional_instructions: Option<String>,
    pub keywords: Option<Vec<String>>,
}

pub struct RecipeTriggers {
    pub calendar_keywords: Vec<String>,
    pub attendees: Vec<String>,
    pub regex: Option<String>,
}

pub struct UserProfile {
    pub name: String,
    pub role: Option<String>,
    pub company: Option<String>,
    pub team: Option<String>,
    pub focus: Option<String>,
    pub body: String,  // The prose context injected into prompts
}
```

### Resolution & Priority

```rust
pub struct RecipeResolver {
    // Loads from:
    // 1. Project-local: .minutes/recipes/
    // 2. User: ~/.minutes/recipes/
    // 3. Bundled: compiled defaults
}

impl RecipeResolver {
    /// Find best recipe for a calendar event
    pub fn match_event(&self, event: &CalendarEvent) -> Option<&Recipe> { ... }

    /// Classify transcript via LLM when no calendar match
    pub fn classify_transcript(&self, first_500_words: &str) -> Option<String> { ... }

    /// Get fallback recipe
    pub fn fallback(&self) -> &Recipe { ... }
}
```

### App UI (Tauri)

The recipe management UI in the desktop app:

1. **Recipe Library screen** — list all recipes (name, description, trigger summary, last used)
2. **Recipe editor** — edit recipe markdown in a code editor pane; preview the sections
3. **Profile editor** — edit your shared context
4. **Recipe picker** — during/after recording, override auto-selection
5. **Detail view integration** — show which recipe was used, allow re-processing with a different recipe

### Migration Path

- Existing Phase 1 templates (`~/.minutes/templates/`) continue to work unchanged
- The system checks `~/.minutes/recipes/` first; if no recipes exist, falls back to templates
- Bundled defaults: ship `working-session` (fallback), `standup`, `1-on-1` as v2 recipes
- CLI: `minutes recipe list`, `minutes recipe new <slug>`, `minutes recipe edit <slug>`
- Reprocessing: `minutes process <file> --recipe <slug>` (replaces `--template`)

### Config Integration

```toml
[profile]
# Points to the profile file (default: ~/.minutes/profile.md)
path = "~/.minutes/profile.md"

[recipes]
# Directory for user recipes (default: ~/.minutes/recipes/)
dir = "~/.minutes/recipes/"
# Default recipe when nothing matches (slug)
default = "working-session"
# Enable LLM classification fallback
auto_classify = true
# Model for classification (fast/cheap)
classify_model = "claude-haiku"
```

### Backwards Compatibility

| Old system | New system |
|---|---|
| `--template <slug>` | `--recipe <slug>` (alias: `--template` still works) |
| `~/.minutes/templates/` | Still checked, treated as `extends_base: true` recipes |
| `additional_instructions` | Still works for old-style templates |
| `build_base_system_prompt()` | Used only when `extends_base: true` or no recipe |

### Phasing

**Phase 1 (this design):** File format, profile, loading, selection logic, CLI commands, prompt assembly. Backend only.

**Phase 2:** Tauri recipe library UI — list, create, edit, preview. Recipe picker in recording flow.

**Phase 3:** LLM auto-classification. Calendar trigger matching (requires `CalendarEvent` to be available at summarization time — it currently is via pipeline context).

**Phase 4:** Recipe analytics (which recipes used most, quality signals), community recipe sharing, recipe versioning/changelog.
