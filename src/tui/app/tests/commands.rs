use super::{App, HOME_LOCK, isolate_home};
use crate::ui::dialogs::DialogType;
use crossterm::event::KeyCode;

fn slash_cmd(name: &str) -> crate::ui::slash_menu::SlashCommand {
    crate::ui::slash_menu::SlashCommand {
        name: name.into(),
        desc: String::new(),
    }
}

#[tokio::test]
async fn compaction_progress_targets_the_running_box_and_final_output_replaces_status() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use crate::types::{Message, MessageRole, Part, TextPart};
    use cosh::harness::events::{LlmCompactionEvent, LlmCompactionPhase};
    let mut app = App::new("/tmp".to_string());
    let id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(id.clone(), "compaction".into(), 0);
    app.state.current_session_id = Some(id);
    app.handle_llm_compaction_event(LlmCompactionEvent::Started);
    app.state
        .current_session_mut()
        .unwrap()
        .messages
        .push(Message {
            id: "later-input".into(),
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: "keep raw".into(),
                synthetic: false,
            })],
            created_at: 0,
            agent: None,
            model: None,
        });
    app.handle_llm_compaction_event(LlmCompactionEvent::Progress {
        phase: LlmCompactionPhase::Mapping,
        completed: 2,
        total: 4,
    });
    let Part::Compaction(part) = &app.state.current_session().unwrap().messages[0].parts[0] else {
        panic!()
    };
    assert_eq!(part.text, "Mapping segments: 2/4");
    app.handle_llm_compaction_event(LlmCompactionEvent::OutputStarted);
    app.handle_llm_compaction_token("validated checkpoint");
    app.handle_llm_compaction_event(LlmCompactionEvent::Finished);
    let Part::Compaction(part) = &app.state.current_session().unwrap().messages[0].parts[0] else {
        panic!()
    };
    assert_eq!(part.text, "validated checkpoint");
    assert!(!part.is_running());
}

/// Regression: Enter on `/toolcall` in the slash menu must open the
/// mode picker dialog. It used to fall into the generic branch (fill the
/// prompt with "/toolcall ") because the dispatch lived only in the
/// unreachable `None` keymap branch, not in the handler that actually
/// intercepts Enter.
#[tokio::test]
async fn slash_toolcall_command_opens_tool_call_dialog() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "toolcall".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(
        app.is_tool_call_dialog_visible(),
        "selecting /toolcall must open the native|inline picker"
    );
    assert!(!app.slash_menu.visible, "slash menu closes after Enter");
}

/// Generic slash commands still fill the prompt instead of opening a
/// dialog (the fallback branch of `run_slash_command`).
#[tokio::test]
async fn slash_unknown_command_fills_prompt() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "nonexistent".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert_eq!(app.prompt_view.input, "/nonexistent ");
    assert!(!app.dialog.visible());
    assert!(!app.slash_menu.visible);
}

/// `/new` creates and selects a fresh session straight from the prompt —
/// no detour through Home.
#[tokio::test]
async fn slash_new_creates_and_selects_a_fresh_session() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    assert!(app.state.current_session_id.is_none());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(
        app.state.current_session_id.is_some(),
        "a session was selected"
    );
    assert!(
        matches!(app.mode(), crate::app::AppMode::Session),
        "the app flips into Session mode"
    );
    assert!(!app.slash_menu.visible);
    assert!(
        app.state.session_cache.len() == 1,
        "exactly one new session exists"
    );
}

/// REGRESSION: `/new` must start the fresh session with a CLEAN right
/// panel. The panel lives on AppState (not inside the Session model), so
/// without the reset in `start_new_session` the old session's todos,
/// PTY/subagent sessions, panel focus and scroll state leaked into the new
/// session (the panel/Esc switch paths already reset it — this path
/// didn't).
#[tokio::test]
async fn slash_new_resets_right_panel_state() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // Simulate a used panel from the "old" session: todos, a PTY session,
    // panel focus and a manually scrolled-away flag.
    app.state
        .right_panel
        .set_todos(vec![crate::routes::session::right_panel::types::TodoItem {
            status: "pending".into(),
            content: "old session todo".into(),
        }]);
    app.state
        .right_panel
        .start_pty("echo old".to_string(), None);
    app.state
        .right_panel
        .complete_last_pty("old output".to_string());
    app.state.right_panel.panel_focus =
        Some(crate::routes::session::right_panel::types::PanelFocus::Bash);
    app.state.right_panel.user_scrolled_away = true;
    assert!(app.state.right_panel.has_content());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(
        app.state.current_session_id.is_some(),
        "a session was selected"
    );
    let panel = &app.state.right_panel;
    assert!(
        !panel.has_content(),
        "todos and PTY sessions from the old session must not leak"
    );
    assert!(
        panel.pty_sessions.is_empty() && panel.todos.is_empty(),
        "no old-session entries survive the /new reset"
    );
    assert!(
        panel.panel_focus.is_none(),
        "panel focus from the old session must not leak"
    );
    assert!(
        !panel.user_scrolled_away,
        "scroll state from the old session must not leak"
    );
    assert!(
        !panel.bash_history_mode,
        "bash history mode from the old session must not leak"
    );
}

/// `/new` while the agent loop is working is refused with a toast —
/// selecting a different session mid-run would route the running loop's
/// events into it.
#[tokio::test]
async fn slash_new_refuses_while_agent_is_working() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.status = crate::types::SessionStatus::Working;
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(app.state.current_session_id.is_none(), "no session created");
    assert!(
        app.toast_state
            .current
            .as_ref()
            .is_some_and(|t| t.message.contains("wait for it")),
        "the refusal toast tells the user to wait"
    );
}

/// `/rename` opens the rename dialog prefilled with the current title;
/// editing and pressing Enter applies it to the session (in-memory +
/// panel) and closes the dialog.
#[tokio::test]
async fn slash_rename_edits_and_applies_the_session_title() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.start_new_session();
    let id = app.state.current_session_id.clone().unwrap();
    if let Some(s) = app.state.session_cache.get_mut(&id) {
        s.title = "old title".into();
    }
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "rename".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(
        matches!(
            app.dialog.current().map(|d| &d.dialog_type),
            Some(DialogType::RenameSession { input, cursor_pos })
                if input == "old title" && *cursor_pos == "old title".len()
        ),
        "the dialog opens prefilled with the current title, cursor at end"
    );

    // Clear the field and type a new name, then apply.
    for _ in 0.."old title".len() {
        app.handle_text_input_dialog_key(KeyCode::Backspace);
    }
    for ch in "manual name".chars() {
        app.handle_text_input_dialog_key(KeyCode::Char(ch));
    }
    assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible(), "Enter closes the dialog");
    assert_eq!(
        app.state.session_cache.get(&id).unwrap().title,
        "manual name",
        "the in-memory session title is updated"
    );
}

/// Esc on the rename dialog cancels without touching the title.
#[tokio::test]
async fn slash_rename_esc_cancels_without_changes() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.start_new_session();
    let id = app.state.current_session_id.clone().unwrap();
    if let Some(s) = app.state.session_cache.get_mut(&id) {
        s.title = "keep me".into();
    }
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "rename".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    for ch in "edited".chars() {
        app.handle_text_input_dialog_key(KeyCode::Char(ch));
    }
    app.handle_text_input_dialog_key(KeyCode::Esc);
    assert!(!app.dialog.visible());
    assert_eq!(app.state.session_cache.get(&id).unwrap().title, "keep me");
}

/// An empty (whitespace-only) title applies nothing — Enter just closes,
/// mirroring opencode's prompt behavior.
#[tokio::test]
async fn slash_rename_empty_title_applies_nothing() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.start_new_session();
    let id = app.state.current_session_id.clone().unwrap();
    if let Some(s) = app.state.session_cache.get_mut(&id) {
        s.title = "unchanged".into();
    }
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "rename".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    // Clear everything, then Enter on an empty field.
    for _ in 0.."unchanged".len() {
        app.handle_text_input_dialog_key(KeyCode::Backspace);
    }
    assert!(app.handle_text_input_dialog_key(KeyCode::Enter));
    assert!(!app.dialog.visible(), "Enter still closes the dialog");
    assert_eq!(
        app.state.session_cache.get(&id).unwrap().title,
        "unchanged",
        "an empty title never clears the session name"
    );
}

/// Regression: the slash menu window must SCROLL. With more commands
/// than the 6 visible rows, navigating down used to move the selection
/// past the rendered window — the last commands (e.g. /rename) existed
/// but were invisible and unreachable by arrow keys.
#[tokio::test]
async fn slash_menu_scrolls_so_the_selection_stays_visible() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "/".into();
    app.slash_menu.update(&app.prompt_view.input);
    assert!(
        app.slash_menu.commands.len() > 6,
        "precondition: more commands than the visible window"
    );

    // Navigate to the LAST command (past the bottom of the window).
    for _ in 0..app.slash_menu.commands.len() - 1 {
        app.slash_menu.select_next();
    }
    let selected = app.slash_menu.get_selected_command().unwrap().name.clone();

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    app.slash_menu
        .render(&mut buf, Rect::new(30, 20, 50, 4), &app.theme);

    // Every row of the menu must be scanned: the selected command's name
    // must be drawn somewhere in the buffer.
    let rendered: String = buf.content().iter().map(|c| c.symbol()).collect();
    assert!(
        rendered.contains(&selected),
        "the selected command '{selected}' must be on screen after scrolling down"
    );
}

/// `/compact` while the agent loop is working must be refused with a toast:
/// the running loop owns the context manager, and the re-entry guard stays
/// cleared (no task spawned).
#[tokio::test]
async fn slash_compact_refuses_while_agent_is_working() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.state.status = crate::types::SessionStatus::Working;
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "compact".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(
        !app.manual_compaction_active,
        "no compaction task may start while a loop runs"
    );
    assert!(!app.slash_menu.visible);
    assert!(
        app.toast_state
            .current
            .as_ref()
            .is_some_and(|t| t.message.contains("between messages")),
        "the refusal toast explains when /compact can run"
    );
}

/// `/compact` in an idle session with no persisted context is refused too —
/// there is no timeline snapshot to rebuild the summarizer from.
#[tokio::test]
async fn slash_compact_refuses_without_a_session_context() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "compact".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert!(!app.manual_compaction_active);
    assert!(app.state.current_session_id.is_none());
    assert!(
        app.toast_state.current.is_some(),
        "the refusal surfaces as a toast"
    );
}

/// `/background` toggles the base background between the theme color and the
/// terminal default: ON zeroes the alpha (RGB preserved, so luminance
/// derivations keep working), OFF restores the exact registry color. Each
/// toggle bumps the render generation and surfaces a toast.
#[tokio::test]
async fn slash_background_toggles_and_restores() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let (r, g, b, _) = app.theme_registry.default_theme().background.to_ints();
    assert_eq!(app.config.theme_gen, 0);

    app.run_slash_command(&slash_cmd("background"));
    assert!(app.transparent_background);
    assert_eq!(app.theme.background.to_ints(), (r, g, b, 0));
    assert_eq!(app.config.theme_gen, 1);
    assert!(
        app.toast_state
            .current
            .as_ref()
            .is_some_and(|t| t.message.contains("terminal")),
        "the toast announces the terminal-default background"
    );

    app.run_slash_command(&slash_cmd("background"));
    assert!(!app.transparent_background);
    assert_eq!(app.theme.background.to_ints(), (r, g, b, 255));
    assert_eq!(app.config.theme_gen, 2);
}

/// The transparent-background preference survives a theme switch while it is
/// active — the new theme also gets its base background cleared — and
/// toggling off restores that theme's pristine color.
#[tokio::test]
async fn slash_background_survives_theme_switch() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.setup.appearance.theme = "dracula".to_string();

    app.run_slash_command(&slash_cmd("background"));
    assert!(app.transparent_background);
    let (_, _, _, dracula_a) = app.theme.background.to_ints();
    assert_eq!(dracula_a, 0);

    // A later theme switch (through set_theme, like the dialog does) keeps
    // the preference applied.
    let nord = app.theme_registry.get("nord").cloned().unwrap();
    app.set_theme(&nord);
    let (nr, ng, nb, na) = app.theme.background.to_ints();
    assert_eq!(na, 0, "the new theme inherits the transparent background");

    // Toggling off restores the CURRENT theme's exact registry color.
    app.run_slash_command(&slash_cmd("background"));
    assert_eq!(app.theme.background.to_ints(), (nr, ng, nb, 255));
}

/// The persisted preference loads at startup: a setup.json with
/// `transparent_background: true` starts the app with an alpha-0 background.
#[tokio::test]
async fn background_preference_loads_from_setup_on_startup() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    // isolate_home wipes <scratch>/.config first, so write AFTER it runs.
    let cfg = std::env::temp_dir()
        .join("cosh-hook-test-home")
        .join(".config/cosh");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("setup.json"),
        r#"{"appearance": {"theme": "dracula", "bell_enabled": true, "transparent_background": true}}"#,
    )
    .unwrap();

    let app = App::new("/tmp".to_string());
    assert!(app.transparent_background);
    let (_, _, _, a) = app.theme.background.to_ints();
    assert_eq!(a, 0, "startup applies the persisted /background state");
}

/// The slash menu offers `/background` and filters down to it.
#[tokio::test]
async fn slash_menu_lists_background_command() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.prompt_view.input = "/back".into();
    app.slash_menu.update(&app.prompt_view.input);
    let names: Vec<String> = app
        .slash_menu
        .filtered_indices()
        .into_iter()
        .map(|i| app.slash_menu.commands[i].name.clone())
        .collect();
    assert_eq!(names, vec!["background".to_string()]);
}
