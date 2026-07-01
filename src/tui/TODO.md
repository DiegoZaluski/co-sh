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
- [ ] command_palette.rs

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
- [ ] Directory display
- [ ] Connection status
- [ ] LSP count
- [ ] MCP count + status
- [ ] Permissions count

### Home
- [ ] Full ASCII logo
- [ ] Placeholder prompts
- [ ] Session list with active indicator

### System
- [ ] Toast notifications
- [ ] Spinner component
- [ ] Command palette
- [ ] Dialog stack (replace/show/clear)
