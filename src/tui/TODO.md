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
- [ ] tool_render.rs
- [ ] spinner.rs
- [ ] toast.rs
- [ ] scroll.rs
- [ ] markdown.rs
- [ ] command_palette.rs

## Missing features

### Session / Chat
- [ ] Tool part rendering (shell, glob, read, grep, write, edit, task, webfetch, websearch, apply_patch, todowrite, question, skill, generic)
- [ ] Inline tool (icon + text, spinner, status colors)
- [ ] Block tool (border box, title, expand/collapse output)
- [ ] Reasoning part (collapsible header + markdown body)
- [ ] File part (file/directory badge)
- [ ] Markdown rendering in text parts
- [ ] Diff rendering in edit/apply_patch tools
- [ ] Assistant metadata footer (mode · model · duration)
- [ ] Agent color per message (border + icon)
- [ ] Session scrollbar
- [ ] Timestamps toggle
- [ ] Conceal mode toggle
- [ ] Thinking mode toggle
- [ ] Tool details toggle
- [ ] Generic tool output toggle
- [ ] Queued message indicator
- [ ] Compaction banner

### Prompt
- [ ] Agent selector cycling
- [ ] History navigation
- [ ] Placeholder text

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
