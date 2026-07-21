# RAG Knowledge Base Feature — Complete Plan

## Architecture Overview

New `AppMode::Rag` route accessed from Home menu "RAG". Three sections stacked vertically, each inside a **box** (background color slightly different from the main background, adapting to the theme). Sections are dynamic height based on content.

---

## Section Layout (boxes)

Each section is rendered as a rectangular **box**:
- **Box background**: `background_panel` (or similar subtly different color from `background`) — fills the entire section area
- **Section title**: Has a **background color** (e.g. `primary` or `accent`) covering from the first character to right after the last character of the title text only, not the full line
- **Content**: Inside the box, indented or full-width

Sections:

1. **▓▓Embed Content▓▓** — input bar + content preview (always visible)
2. **▓▓Available Databases▓▓** — toggleable DB list with warning header (always visible)
3. **▓▓Create New Database▓▓** — hidden by default, shown as a **button/option** the user clicks to expand/collapse the creation form

---

## Visual Mockup

```
┌──────────────────────────────────────────────────────────┐
│                                                           │
│  ██Embed Content██  (bg cor do título, resto linha normal)│
│  ┌────────────────────────────────────────────────────┐   │
│  │ [ /path/to/file or https://...           ]         │   │
│  │                                                    │   │
│  │ ┌───Preview─────────────────────────────────────┐  │   │
│  │ │ (scrollable fetched content)                  │  │   │
│  │ │                                                │  │   │
│  │ │                                                │  │   │
│  │ └────────────────────────────────────────────────┘  │   │
│  │        ↑↓ scroll | Enter → embed                   │   │
│  └────────────────────────────────────────────────────┘   │
│                                                           │
│  ██Available Databases██                                 │
│  ┌────────────────────────────────────────────────────┐   │
│  │ ⚠ Selecting many DBs degrades LLM quality.         │   │
│  │ Only enable the relevant ones.                      │   │
│  │                                                     │   │
│  │ ✔ my-docs — Knowledge base about Rust               │   │
│  │ ✗ api-ref — API reference for our services          │   │
│  │ ✔ manual  — Product user manual                     │   │
│  │                                                     │   │
│  │ ↑↓ navigate  space/enter toggle                     │   │
│  └────────────────────────────────────────────────────┘   │
│                                                           │
│  ██Create New Database██ (button, click to expand)       │
│  ┌────────────────────────────────────────────────────┐   │
│  │ Model: [BGESmallENV15                     ▼]      │   │
│  │ Name:  [my-docs                                   │   │
│  │ Desc:  [Knowledge base about Rust API             │   │
│  │                                                   │   │
│  │ [Create Database]                                 │   │
│  └────────────────────────────────────────────────────┘   │
│                                                           │
│  Esc: back to Home / menu navigation                      │
└──────────────────────────────────────────────────────────┘
```

**Key UI behaviors**:
- Each section's content area has a subtle `background_panel` color (theme-aware)
- "Create New Database" section is collapsible — only the title bar / button is visible when collapsed
- When expanded, shows model selector, DB name, DB description inputs
- Content preview box is scrollable with ↑↓ keys, medium fixed height (not full screen)

---

## Data Storage

**DB Registry**: `$DATA_DIR/cosh/rag_dbs.json` (uses `dirs::data_dir()`)
- `~/.local/share/cosh/rag_dbs.json` on Linux
- Contains array of DB configs

**LanceDB data**: `$DATA_DIR/cosh/<db_name>/` — each DB is a directory with LanceDB tables

**Registry struct**:
```json
{
  "name": "my-docs",
  "uri": "/home/user/.local/share/cosh/my-docs",
  "description": "Knowledge base about Rust",
  "embedder": {
    "type": "local",       // "local" | "cloud"
    "model": "BGESmallENV15",
    "vector_dim": 384
  },
  "created_at": 1719000000000
}
```

**Validation**: When embedding into existing DB, verify that current model matches `embedder.model` from registry. If mismatch → reject with toast notification.

---

## Embed Models (selector options)

| Type | Model | Dim | Feature | Env Var |
|------|-------|-----|---------|---------|
| Local | `AllMiniLML6V2` | 384 | `fastembed` | — |
| Local | `BGESmallENV15` | 384 | `fastembed` | — |
| Local | `BGEBaseENV15` | 768 | `fastembed` | — |
| Cloud | `text-embedding-3-small` | 1536 | `cloud` | OPENAI_API_KEY |
| Cloud | `text-embedding-3-large` | 3072 | `cloud` | OPENAI_API_KEY |
| Cloud | `text-embedding-004` | 768 | `cloud` | GEMINI_API_KEY |
| Cloud | `nomic-embed-text` | 768 | `cloud` | OLLAMA_API_KEY |

*Claude NOT supported (embed returns NotImplemented)*

**Availability logic**: 
- Local models shown only if `fastembed` feature is compiled
- Cloud models shown only if `cloud` feature is compiled **AND** the provider's env var is set

---

## Embed Flow (state machine)

```
[Idle] ──Enter──> [Fetching] ──done──> [Previewing] ──Enter──> [Embedding] ──done──> [Idle]
                     ↑                     │                       │
                   (spinner)           ↑↓ scroll            (spinner + toast)
                     │                     │                       │
                     └──error──> [Idle]    └──error──> [Idle]      └──> toast success
```

1. User types URL or file path in input bar → **Enter**
2. **Fetching**: `SpinnerState` animates while fetching content
   - URL → `web_fetch` (via crate)
   - File path → `fs_read` (read file as text)
3. **Previewing**: Content rendered in scrollable preview box. User can scroll ↑↓
4. User configures Model + DB Name + DB Description in "Create New Database" section
5. User presses **Enter** again (while in Previewing mode) → **Embedding**
   - Create `Embedder` from selected model/type
   - If DB name exists in registry: validate model matches
   - If new DB: create directory + add to registry
   - `Rag::connect(uri, "documents", embedder)` → `rag.ingest(id, content)`
6. Toast notification on success or error

---

## Harness Integration

### Add Recall + Embedder to CoshTools

**`src/harness/tools.rs`**:
```rust
pub struct CoshTools {
    pub recall: Option<Recall>,
    pub embedder: Option<cosh_recall::embed::Embedder>,
}
```

**`format_header_context()`** in `core.rs`:
- If active DBs exist, build `recall_search` tool description
- Call `Recall::with_description(suffix)` where suffix includes:
  - For each active DB: `Name: <name>\nDescription: <desc>\nDB URI: <uri>\nVector Dim: <dim>`
- This tells the LLM what databases are available and what they contain

**Dispatch** in `tools.rs` for `"recall_search"`:
1. Parse `RecallSearchInput` from LLM args (db_uri, table_name, query, limit, etc.)
2. Embed query: `self.embedder.embed(&[&input.query])` → get vector
3. Construct full `RecallSearchInput` with `query_vector`
4. Call `recall::search::search(&full_input)`
5. Return results

---

## RagView State Machine

```rust
enum RagMode {
    Idle,
    Fetching,
    Previewing,
    Embedding,
}

pub struct RagView {
    mode: RagMode,
    input_bar: SearchBar,
    content_preview: String,
    preview_scroll: usize,
    spinner: SpinnerState,
    embed_model: EmbedModel,
    db_name: String,
    db_description: String,
    registry: RagRegistry,
    // DB toggle list
    selected_db_index: usize,
    dbs_scroll_offset: usize,
    active_dbs: HashSet<String>,
    // "Create New Database" expand/collapse
    show_create_db: bool,
}
```

---

## Files to Create/Modify

### New files:
- `src/tui/routes/rag.rs` — RagView struct + rendering + input handling
- `src/tui/routes/rag_registry.rs` — RagDb struct + RagRegistry + JSON persistence

### Modified files:
- `src/tui/routes/home.rs` — Add "RAG" to `MENU_ITEMS` + `HomeAction`
- `src/tui/routes/mod.rs` — Add `pub mod rag; pub mod rag_registry;`
- `src/tui/app.rs` — Add `AppMode::Rag`, `rag_view: RagView`, key handlers
- `src/harness/tools.rs` — Add `Recall` + `Embedder` to `CoshTools`, dispatch `recall_search`
- `src/harness/core.rs` — Dynamic tool description, pass active DBs
- `Cargo.toml` (root) — Add `url` dep, ensure `dirs` dep

### Implementation order:
1. **DB Registry** — Structs + JSON persistence (`rag_registry.rs`)
2. **RagView skeleton** — View struct, modes, Home menu entry, routing (`rag.rs` + `app.rs`)
3. **Fetch flow** — Input bar + content preview with scroll
4. **Embed flow** — Model selector + ingest + validation
5. **DB toggle list** — Like InternalTools + warning header
6. **Harness integration** — Recall + Embedder in CoshTools, dispatch
