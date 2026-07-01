# TUI Port - Agent Chat

## Core files (done)
- [x] types.rs
- [x] state.rs
- [x] config.rs
- [x] theme.rs
- [x] keymap.rs
- [x] footer.rs
- [x] sidebar.rs
- [x] subagent_footer.rs
- [x] prompt.rs
- [x] permission.rs
- [x] question.rs
- [x] dialogs.rs
- [x] session.rs
- [x] home.rs
- [x] app.rs
- [x] main.rs

## Missing files
- [x] tool_render.rs
- [x] spinner.rs
- [x] toast.rs
- [x] scroll.rs
- [x] markdown.rs
- [x] command_palette.rs

## Missing features

### Session / Chat
- [x] Tool part rendering (shell, glob, read, grep, write, edit, task, webfetch, websearch, apply_patch, todowrite, question, skill, generic)
- [x] Inline tool (icon + text, spinner, status colors)
- [x] Block tool (border box, title, expand/collapse output)
- [x] Reasoning part (collapsible header + markdown body)
- [x] File part (file/directory badge)
- [x] Markdown rendering in text parts
- [x] Diff rendering in edit/apply_patch tools
- [x] Assistant metadata footer (mode · model · duration)
- [x] Agent color per message (border + icon)
- [x] Session scrollbar
- [x] Timestamps toggle (Ctrl+Y)
- [x] Conceal mode toggle (Ctrl+C)
- [x] Thinking mode toggle (Ctrl+T)
- [x] Tool details toggle (Ctrl+D)
- [x] Generic tool output toggle (Ctrl+G)
- [x] Queued message indicator
- [x] Compaction banner

### Prompt
- [x] Agent selector cycling (Tab/Shift+Tab)
- [x] History navigation (Ctrl+Up/Ctrl+Down)
- [x] Placeholder text ("Type a message...")

### Footer
- [x] Directory display
- [x] Connection status (●/○ with color)
- [x] LSP count
- [x] MCP count + status
- [x] Permissions count

### Home
- [x] Full ASCII logo (5-line "cosh" art)
- [x] Placeholder prompts (5 suggested queries)


### System
- [x] Toast notifications (welcome toast, tick-based timeout, variant colors)
- [x] Spinner component (braille spinner in ToolRenderState)
- [x] Command palette (Ctrl+P overlay with filter/select)
- [x] Dialog stack (push/pop/replace/clear, Alert + Confirm)

### Syntax highlighting (new)
- [x] Tree-sitter based syntax highlighting for markdown code blocks
- [x] Rust: keywords, strings, comments, types, functions, numbers
- [x] Python: keywords, strings, comments, functions, numbers
- [x] JavaScript/TypeScript: keywords, strings, comments, functions, numbers
- [x] Code block background (dimmed, dark blue-ish tint)
- [x] Language detection from fenced code block info string
- [x] Extended color palette: keyword (orange), string (green), comment (gray), type (blue), function (purple), number (gold), builtin (cyan)

### Future
- [ ] Session list with active indicator