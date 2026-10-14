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

/// The TODO completion strike-through animation: a todo that flips to
/// `completed` arms a fresh frame counter (hold 2 frames, sweep 12), repeated
/// `set_todos` calls with the same list keep it ticking, the already-struck
/// items never replay it, and `/new`'s panel reset drops everything.
#[tokio::test]
async fn todo_completion_strike_animation_frames() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    use crate::routes::session::right_panel::types::{TODO_STRIKE_TOTAL_FRAMES, TodoItem};

    let mut app = App::new("/tmp".to_string());
    let todo = |status: &str, content: &str| TodoItem {
        status: status.to_string(),
        content: content.to_string(),
    };

    // Rehydration: the first update already carries the item as completed —
    // it must strike immediately (permanent marker), never animate.
    app.state
        .right_panel
        .set_todos(vec![todo("completed", "done before we saw it")]);
    let panel = &mut app.state.right_panel;
    assert!(
        !panel.todo_strike_animating(),
        "rehydrated completed todo must not replay the animation"
    );
    assert_eq!(panel.todo_strike_frame(0), None, "permanent strike");

    // The primary flow: an item the panel saw PENDING flips to completed —
    // it must arm a fresh counter (hold phase first), not snap to permanent.
    app.state
        .right_panel
        .set_todos(vec![todo("in_progress", "flip me")]);
    let panel = &mut app.state.right_panel;
    assert!(
        panel.todo_strike_frame(0).is_none(),
        "pending item has no counter"
    );
    app.state
        .right_panel
        .set_todos(vec![todo("completed", "flip me")]);
    let panel = &mut app.state.right_panel;
    assert_eq!(
        panel.todo_strike_frame(0),
        Some(0),
        "pending→completed arms a fresh hold-phase counter"
    );
    assert!(panel.todo_strike_animating());

    // Duplicate contents are independent: two identical items completing in
    // the same update must each get their own counter, not share one.
    app.state.right_panel.set_todos(vec![
        todo("in_progress", "Same"),
        todo("in_progress", "Same"),
    ]);
    let panel = &mut app.state.right_panel;
    assert!(panel.todo_strike_frame(0).is_none());
    assert!(panel.todo_strike_frame(1).is_none());
    app.state
        .right_panel
        .set_todos(vec![todo("completed", "Same"), todo("completed", "Same")]);
    let panel = &mut app.state.right_panel;
    assert_eq!(
        panel.todo_strike_frame(0),
        Some(0),
        "first duplicate animates"
    );
    assert_eq!(
        panel.todo_strike_frame(1),
        Some(0),
        "second duplicate animates independently"
    );

    // A completed item's counter must not leak onto a different item that
    // slides into its slot after a removal.
    app.state
        .right_panel
        .set_todos(vec![todo("in_progress", "a"), todo("completed", "b")]);
    let panel = &mut app.state.right_panel;
    assert_eq!(panel.todo_strike_frame(1), Some(0));
    app.state
        .right_panel
        .set_todos(vec![todo("completed", "b"), todo("completed", "a")]);
    let panel = &mut app.state.right_panel;
    // "b" carried its fresh counter through the shift; "a" completed in this
    // update also arms fresh — no stale/aliased counters either way.
    assert_eq!(panel.todo_strike_frame(0), Some(0));
    assert_eq!(panel.todo_strike_frame(1), Some(0));

    // Live completion: a pending todo flips to completed → animation arms.
    // "old" is a NEW item arriving already completed in this same update —
    // from the tool's point of view it was just completed too, so it animates
    // as well (only the panel's very first snapshot strikes instantly).
    app.state.right_panel.set_todos(vec![
        todo("in_progress", "sweep me"),
        todo("completed", "old"),
    ]);
    let panel = &mut app.state.right_panel;
    let first = panel.todo_strike_frame(1).expect("strike just armed");
    assert_eq!(
        first, 0,
        "fresh counter starts at 0: hold phase, sweep not started yet"
    );
    assert!(
        panel.todo_strike_frame(1).is_some(),
        "newly-completed item in a later update animates too"
    );
    assert!(panel.todo_strike_animating());

    // The every-update repetition with the same list must keep the same
    // running counter alive, not re-arm (which would restart the sweep).
    app.state.right_panel.set_todos(vec![
        todo("in_progress", "sweep me"),
        todo("completed", "old"),
    ]);
    let panel = &mut app.state.right_panel;
    assert_eq!(
        panel.todo_strike_frame(1),
        Some(first),
        "unchanged list keeps its counter"
    );

    // Advancing past the total finishes the animation: the counter is
    // dropped and the strike becomes permanent.
    panel.advance_todo_strikes(TODO_STRIKE_TOTAL_FRAMES + 1);
    assert!(
        !panel.todo_strike_animating(),
        "finished animation is dropped"
    );
    assert_eq!(panel.todo_strike_frame(0), None, "permanent strike");

    // An item removed from every update forgets its seen-content key, so a
    // much later re-completion of the same text animates again.
    app.state
        .right_panel
        .set_todos(vec![todo("in_progress", "sweep me")]);
    app.state
        .right_panel
        .set_todos(vec![todo("completed", "old")]);
    let panel = &mut app.state.right_panel;
    assert!(
        panel.todo_strike_frame(0).is_some(),
        "re-completion of a forgotten key animates again"
    );

    // `/new` resets the panel wholesale: no strike state may survive.
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    let panel = &app.state.right_panel;
    assert!(
        !panel.todo_strike_animating(),
        "/new drops the strike animation state"
    );
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
    // The selected session is FRESH: empty transcript. (Without the `home`
    // feature the app boots into a live session, and `/new` ids are
    // millisecond timestamps — a same-millisecond collision replaces the
    // boot session in the LRU instead of growing it, so the count itself is
    // not a portable invariant; the freshness of the selected session is.)
    let selected = app
        .state
        .current_session()
        .expect("the selected session exists");
    assert!(
        selected.messages.is_empty(),
        "the selected session is a fresh one"
    );
}

/// REGRESSION (multi-process session adoption — "single-entry vanished"):
/// a freshly created session used to be selectable but UNLOCKED, so a
/// second cosh process could adopt it through the sidebar switch while the
/// creator kept appending to the same JSONL. The entry funnel must take the
/// per-session open-lock at CREATION time: `/new` (the shared path behind
/// Home's "New session" and the first-message bootstrap) must leave the new
/// session claimed by THIS process and refused to a foreign one.
#[tokio::test]
async fn slash_new_claims_the_fresh_session_lock_against_a_foreign_process() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    let session_id = app
        .state
        .current_session_id
        .clone()
        .expect("a session was selected");

    // A foreign cosh process (independent store, same data dir) must be
    // REFUSED: the claim exists from the moment of creation.
    let foreign = crate::session_store::SessionStore::new();
    assert!(
        !foreign.try_lock_session(&session_id),
        "a session created via /new must be locked against every other process \
         (the multi-process adoption regression)"
    );

    // Esc back to Home must RELEASE the claim — the process no longer
    // holds the session, and keeping the lock would make every other
    // process's attempt fail spuriously.
    app.return_to_home();
    assert!(app.state.current_session_id.is_none());
    assert!(
        foreign.try_lock_session(&session_id),
        "returning to Home must release the open-lock for other processes"
    );
}

/// REGRESSION (stale open-lock on /new): a process that CLAIMED a session
/// (sidebar switch) and then started a NEW session used to keep holding the
/// old session's lock until the process died — every other cosh process
/// trying to open that session was refused with "already open in another
/// process" while nothing here was using it. A claim must be handed off the
/// moment the session stops being this process's selected one: /new (and
/// every other leave-the-session path) releases the previous claim.
#[tokio::test]
async fn slash_new_hands_off_the_previous_session_lock() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());

    // Claim a session the way a sidebar switch does: /new first (which
    // claims its id), then record the id.
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    let previous_id = app
        .state
        .current_session_id
        .clone()
        .expect("a session was selected");
    let foreign = crate::session_store::SessionStore::new();
    assert!(
        !foreign.try_lock_session(&previous_id),
        "precondition: the current session is locked by this process"
    );

    // Start a NEW session while still holding the previous one's claim.
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    });
    let new_id = app
        .state
        .current_session_id
        .clone()
        .expect("a new session was selected");
    assert_ne!(
        new_id, previous_id,
        "/new must select a different session"
    );

    // The reported bug: the previous session's lock stayed with this
    // process after /new. It must be handed off NOW, not at process exit.
    assert!(
        foreign.try_lock_session(&previous_id),
        "starting a new session must release the previous session's \
         open-lock (it stayed held until process death)"
    );
}

/// The `/new` id must never collide with an EXISTING session: ids are
/// millisecond timestamps, and a collision used to replace the old session
/// in the LRU cache. The claim now retries with a suffix. Alongside
/// uniqueness, the lock lifecycle is pinned: the CURRENT session is locked
/// by this process, and every PREVIOUS one has had its claim handed off by
/// the `/new` that superseded it.
#[tokio::test]
async fn slash_new_id_is_unique_per_claim() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let mut ids = std::collections::HashSet::new();
    let mut superseded: Vec<String> = Vec::new();
    for _ in 0..3 {
        app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
            name: "new".into(),
            desc: String::new(),
        });
        let id = app
            .state
            .current_session_id
            .clone()
            .expect("a session was selected");
        assert!(
            ids.insert(id.clone()),
            "two /new sessions in the same millisecond must not share an id"
        );
        // The CURRENT session carries this process's open-lock.
        let foreign = crate::session_store::SessionStore::new();
        assert!(
            !foreign.try_lock_session(&id),
            "the current session must carry this process's open-lock"
        );
        // Every PREVIOUS session's claim was handed off by this /new —
        // a fresh foreign process can claim it right now.
        for prev in &superseded {
            let released = crate::session_store::SessionStore::new();
            assert!(
                released.try_lock_session(prev),
                "a superseded session's open-lock must be handed off when \
                 /new starts the next session, not held until process death"
            );
        }
        superseded.push(id);
    }
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
    // Without the `home` feature the app boots into a live session; the
    // invariant under test is that the refusal keeps the CURRENT session —
    // no new one is created or selected either way.
    let session_count_before = app.state.session_cache.len();
    let previous_id = app.state.current_session_id.clone();
    let cmd = crate::ui::slash_menu::SlashCommand {
        name: "new".into(),
        desc: String::new(),
    };
    app.run_slash_command(&cmd);
    assert_eq!(
        app.state.current_session_id, previous_id,
        "the refusal keeps the current session selected"
    );
    assert_eq!(
        app.state.session_cache.len(),
        session_count_before,
        "no session was created"
    );
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
    let unique_agents = app.state.unique_agents();
    let agent_colors = crate::types::AgentColors::from_theme(&app.theme);
    let mode_color = crate::component::prompt::agent_mode_color(
        &app.theme,
        &agent_colors,
        &unique_agents,
        app.state.mode,
    );
    app.slash_menu
        .render(&mut buf, Rect::new(30, 20, 50, 4), &app.theme, mode_color);

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
    // With `home` the app boots modeless, so the refusal is "No active
    // session."; without it the boot session is live but has no persisted
    // context, so the refusal is "Nothing to compact yet.". Either way the
    // command must refuse through a toast, never start a compaction.
    let message = app
        .toast_state
        .current
        .as_ref()
        .map(|t| t.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains("No active session") || message.contains("Nothing to compact yet"),
        "the refusal surfaces as a toast naming the guard that fired: {message:?}"
    );
}

/// Regression for the "silent summarization death" wedge: an earlier
/// ESC/Interrupt leaves the SHARED stop flag set, and `start_manual_compaction`
/// used to clone it WITHOUT clearing — `compact_on_demand` then raced the
/// still-set flag at every summarizer's pre-connect select, so `/compact`
/// failed silently no matter how often it was retried (the fix clears the
/// flag before the clone, mirroring `start_agent_loop`'s contract).
#[tokio::test]
async fn slash_compact_clears_a_wedged_stop_flag_before_spawning() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // A session with persisted context records, so the command passes its
    // guards and actually spawns the compaction task.
    app.start_new_session();
    app.state.current_session_mut().expect("session").messages = vec![crate::types::Message {
        id: "msg-0".into(),
        role: crate::types::MessageRole::User,
        parts: vec![crate::types::Part::Text(crate::types::TextPart {
            text: "earlier prompt".into(),
            synthetic: false,
        })],
        created_at: 0,
        agent: None,
        model: None,
    }];
    let state: cosh::harness::ContextManagerState = Default::default();
    app.session_store
        .save_session_with_context(&app.state.current_session().unwrap().clone(), &state);

    // The wedge: the previous interaction left the shared flag set.
    app.stop_signal
        .store(true, std::sync::atomic::Ordering::Relaxed);

    let cmd = slash_cmd("compact");
    app.run_slash_command(&cmd);
    // The task started (the guards passed)…
    assert!(
        app.manual_compaction_active,
        "the compaction task must spawn in a valid idle session"
    );
    // …and the flag the task inherits is CLEAR: the pre-connect select in
    // the summarizer must never observe the previous interaction's ESC.
    assert!(
        !app.stop_signal.load(std::sync::atomic::Ordering::Relaxed),
        "start_manual_compaction must clear the shared stop flag before the \
         compaction task clones it — a wedged flag kills /compact silently"
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

/// `/providers` hover-follow (same contract the model picker got): a Move
/// over a provider row moves the highlight WITHOUT executing it, and only
/// the release click opens the provider dialog. Exercises the full App
/// dispatch so the hover arm and the click path cannot drift apart.
#[tokio::test]
async fn providers_hover_moves_selection_and_release_click_opens_dialog() {
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };

    use super::super::AppMode;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.set_test_size(100, 40);
    app.run_slash_command(&slash_cmd("providers"));
    assert!(matches!(app.mode(), AppMode::AddProvider));
    let before = app.add_provider_view.selection.selected_index;

    // Render once so the view captures the list geometry (list_start_y),
    // then hover the SECOND row through the real App dispatch.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| app.render(f, 0.0)).unwrap();

    let row_y = app.add_provider_view.list_start_y + 1;
    app.handle_mouse_event(CMouse {
        kind: CKind::Moved,
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("hover handled");
    assert_eq!(
        app.add_provider_view.selection.selected_index,
        before + 1,
        "hover must follow the mouse to the second row"
    );
    assert!(
        !app.dialog.visible(),
        "a Move must never open the provider dialog"
    );

    // Release click on the same row executes the selection: the provider
    // dialog opens (key input for cloud, URL input for local).
    app.handle_mouse_event(CMouse {
        kind: CKind::Down(CBtn::Left),
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("press handled");
    app.handle_mouse_event(CMouse {
        kind: CKind::Up(CBtn::Left),
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("release handled");
    assert!(
        app.dialog.visible(),
        "the release click must open the provider credential dialog"
    );
}

/// `/tools` hover-follow (same contract `/providers` got): a Move over a
/// tool row moves the highlight WITHOUT toggling it, and only the release
/// click executes the toggle. Exercises the full App dispatch so the hover
/// arm and the click path cannot drift apart.
#[tokio::test]
async fn tools_hover_moves_selection_and_release_click_toggles() {
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };

    use super::super::AppMode;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.set_test_size(100, 40);
    app.run_slash_command(&slash_cmd("tools"));
    assert!(matches!(app.mode(), AppMode::InternalTools));
    let before = app.internal_tools_view.selection.selected_index;

    // Render once so the view captures the list geometry (list_start_y),
    // then hover the SECOND row through the real App dispatch.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| app.render(f, 0.0)).unwrap();

    let row_y = app.internal_tools_view.list_start_y + 1;
    app.handle_mouse_event(CMouse {
        kind: CKind::Moved,
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("hover handled");
    assert_eq!(
        app.internal_tools_view.selection.selected_index,
        before + 1,
        "hover must follow the mouse to the second row"
    );
    assert!(
        !app.internal_tools_view.changed,
        "a Move must never toggle the hovered tool"
    );

    // Release click on the same row executes the selection: the tool under
    // the highlight is toggled (added to the disabled set).
    app.handle_mouse_event(CMouse {
        kind: CKind::Down(CBtn::Left),
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("press handled");
    app.handle_mouse_event(CMouse {
        kind: CKind::Up(CBtn::Left),
        column: 40,
        row: row_y,
        modifiers: KeyModifiers::NONE,
    })
    .expect("release handled");
    assert!(
        app.internal_tools_view.changed,
        "the release click must toggle the highlighted tool"
    );
    assert_eq!(
        app.internal_tools_view.disabled.len(),
        1,
        "exactly the hovered tool must end up disabled"
    );
}

/// A `/agents` pane's FIRST done turn promotes its persisted entry from an
/// unbound `Name` to `Bound { agent, session_id }`, so a reopened session
/// resumes the same agent context. Both panes of the same CLI stay
/// independent (call_ref-keyed binds resolve per pane, never per agent).
#[tokio::test]
async fn agents_pane_done_promotes_the_pane_binding_for_the_next_open() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // One session, split open with a fresh pick list: claude ×2, cosh last
    // (the daemon pane exists ONLY because the user marked it).
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "pane binding".into(), 0);
    // add_empty_session registers the model but does NOT select it — the
    // split persistence writes through the CURRENT session pointer.
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![
        crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 2,
        },
        crate::ui::agents_picker::AgentPick {
            name: "cosh".into(),
            instances: 1,
        },
    ]);
    {
        let session = app.state.current_session().unwrap();
        let panes = session.agents_panes.as_ref().unwrap();
        assert_eq!(
            panes,
            &vec![
                AgentPaneRef::Name("claude".into()),
                AgentPaneRef::Name("claude".into()),
                AgentPaneRef::Name("cosh".into()),
            ],
            "fresh picks persist as unbound Name entries"
        );
    }

    // Pane 0's turn completes: the session id must land in the metadata
    // for THAT pane only — pane 1 (the other claude instance) stays fresh,
    // so both instances keep independent sessions. The pane is dispatched
    // first (running), as every real Done follows a dispatch.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            epoch: app.agents_pane_epoch,
            output: "report one".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-A".into()),
        })
        .ok();
    app.poll_events();

    let panes = app.state.current_session().unwrap().agents_panes.clone();
    let panes = panes.expect("split stays open after a done");
    assert_eq!(
        panes[0],
        AgentPaneRef::Bound {
            agent: "claude".into(),
            session_id: "sess-A".into(),
        },
        "the finished pane promotes to Bound"
    );
    assert_eq!(
        panes[1],
        AgentPaneRef::Name("claude".into()),
        "the sibling claude instance stays unbound"
    );
    assert_eq!(panes[2], AgentPaneRef::Name("cosh".into()));

    // Rebinding pane 0 to a NEW session (a later turn) overwrites — the
    // binding tracks the pane's latest context, not its first. Each turn
    // re-dispatches its pane (running) before the Done lands.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            epoch: app.agents_pane_epoch,
            output: "report two".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-B".into()),
        })
        .ok();
    app.poll_events();
    app.agents_split.as_mut().unwrap().set_running(1, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 1,
            epoch: app.agents_pane_epoch,
            output: "report three".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-C".into()),
        })
        .ok();
    app.poll_events();
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    assert_eq!(
        panes[0].session_id(),
        Some("sess-B"),
        "a later turn moves the binding forward"
    );
    assert_eq!(
        panes[1].session_id(),
        Some("sess-C"),
        "each instance carries its OWN session id"
    );

    // The live panes mirror the binding for their next dispatch.
    let split = app.agents_split.as_ref().unwrap();
    assert_eq!(split.panes[0].session_id.as_deref(), Some("sess-B"));
    assert_eq!(split.panes[1].session_id.as_deref(), Some("sess-C"));
}

/// Esc out of the `/agents` mode PARKS the split: the pane objects (streams,
/// drafts, running flags) move to the background slot untouched, the
/// metadata flips to `agents_mode_open: false` while KEEPING the pane set,
/// and a later `/agents` re-attaches the same panes verbatim — a running
/// turn survives the round trip.
#[tokio::test]
async fn parked_agents_split_survives_park_and_re_attach() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "park".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    assert!(app.agents_split.is_some());

    // A turn starts streaming: events land in the live split while the
    // mode is on screen.
    use cosh::harness::HarnessEvent;
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "still streaming".into(),
            },
        })
        .ok();
    app.poll_events();
    assert!(
        !app.agents_split
            .as_ref()
            .unwrap()
            .panes[0]
            .activity
            .timeline
            .is_empty(),
        "the live split consumed the stream"
    );

    // The user leaves the mode: park.
    app.park_agents_split();
    assert!(app.agents_split.is_none(), "the mode left the screen");
    assert!(
        app.parked_agents_split.is_some(),
        "the split objects moved to the parked slot"
    );
    // More events land in the PARKED split (the stream follows the panes).
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "parked stream".into(),
            },
        })
        .ok();
    app.poll_events();
    let parked_text = app
        .parked_agents_split
        .as_ref()
        .unwrap()
        .panes[0]
        .activity
        .timeline
        .iter()
        .filter_map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        parked_text.contains("parked stream"),
        "the parked split keeps consuming the stream: {parked_text:?}"
    );

    // Metadata: mode closed + pane set retained for the pre-marked re-entry.
    {
        let session = app.state.current_session().unwrap();
        assert!(!session.agents_mode_open, "the mode flag dropped");
        assert!(
            session.agents_panes.is_some(),
            "the pane set (and bindings) survived the park"
        );
    }

    // Re-entry: `/agents` re-attaches the SAME objects, verbatim.
    assert!(
        app.attach_parked_agents_split(),
        "a parked split takes the screen back"
    );
    assert!(app.agents_split.is_some());
    assert!(app.parked_agents_split.is_none());
    let reattached_text = app
        .agents_split
        .as_ref()
        .unwrap()
        .panes[0]
        .activity
        .timeline
        .iter()
        .filter_map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        reattached_text.contains("parked stream"),
        "the re-attached split carries the transcribed stream: {reattached_text:?}"
    );
    {
        let session = app.state.current_session().unwrap();
        assert!(session.agents_mode_open, "the mode flag restored");
    }
}

/// A session switch NEVER grabs the screen back for a PARKED set: the new
/// session's `agents_mode_open: false` rebuilds the manager into the
/// parked slot (`/agents` re-attaches later), while `true` restores the
/// split screen; a session with no set restores nothing. The UI slots are
/// also emptied on switch: running turns of the OLD session stop.
#[tokio::test]
async fn session_switch_restores_the_mode_only_when_it_was_left_open() {
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());

    // Session A: left OPEN → the switch takes the screen back. The ids are
    // suffixed because `generate_session_id` is a bare millisecond
    // timestamp: three rapid calls in one test can collide, and
    // `add_empty_session` would silently overwrite the earlier cache entry.
    let unique = |tag: &str| format!("{}-{tag}", crate::session_store::generate_session_id());
    let a = unique("a");
    app.state.add_empty_session(a.clone(), "open mode".into(), 0);
    app.state.current_session_id = Some(a.clone());
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "kilo".into(),
        instances: 1,
    }]);

    // Session B: parked pane set + flag false (written directly — the shape
    // a session parked and quit carries on disk).
    let b = unique("b");
    app.state
        .add_empty_session(b.clone(), "parked mode".into(), 0);
    // add_empty_session registers the model without selecting it — aim the
    // metadata write at B through the CURRENT session pointer.
    app.state.current_session_id = Some(b.clone());
    {
        let session = app.state.current_session_mut().unwrap();
        session.agents_panes = Some(vec![
            AgentPaneRef::Bound {
                agent: "claude".into(),
                session_id: "sess-old".into(),
            },
            AgentPaneRef::Name("kilo".into()),
        ]);
        session.agents_mode_open = false;
    }

    // Session C: no agent state at all.
    let c = unique("c");
    app.state.add_empty_session(c.clone(), "no agents".into(), 0);

    // Switch to B: NO split screen — the set rebuilds parked instead.
    assert!(app.switch_session_locked(&b));
    assert!(
        app.agents_split.is_none(),
        "a parked set never grabs the screen"
    );
    let parked = app.parked_agents_split.as_ref().expect("parked slot rebuilt");
    assert_eq!(parked.panes.len(), 2);
    // The binding rode along: the Bound entry seeds the pane's resume id.
    assert_eq!(parked.panes[0].agent, "claude");
    assert_eq!(parked.panes[0].session_id.as_deref(), Some("sess-old"));
    assert_eq!(parked.panes[1].agent, "kilo");
    assert!(parked.panes[1].session_id.is_none());
    // ... and the pane set stays persisted (a rebuild must not wipe it).
    assert!(app.state.current_session().unwrap().agents_panes.is_some());

    // Switch to C: nothing agent-related anywhere.
    assert!(app.switch_session_locked(&c));
    assert!(app.agents_split.is_none());
    assert!(app.parked_agents_split.is_none());
    assert!(app.state.current_session().unwrap().agents_panes.is_none());

    // Switch to A: the mode was left OPEN → the split takes the screen back.
    assert!(app.switch_session_locked(&a));
    let split = app.agents_split.as_ref().expect("open mode restored");
    assert_eq!(split.panes.len(), 1);
    assert_eq!(split.panes[0].agent, "kilo");
    assert!(app.parked_agents_split.is_none());
}

/// A FRESH picker confirm supersedes a parked split of the same session:
/// the replaced panes' stop signals fire (their turn can't stream into a
/// pane of the new list), the parked slot empties.
#[tokio::test]
async fn fresh_confirm_supersedes_the_parked_split() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "supersede".into(), 0);
    app.state.current_session_id = Some(session_id);

    // Park a split whose pane carries a stop signal (as a running turn
    // would have armed).
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app.agents_split.as_mut().unwrap().set_stop_signal(
        0,
        Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))),
    );
    app.park_agents_split();
    let parked = app.parked_agents_split.as_ref().unwrap();
    let signal = parked.panes[0].stop_signal.as_ref().unwrap().clone();
    assert!(!signal.load(std::sync::atomic::Ordering::Relaxed));

    // A new confirm replaces it.
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "kilo".into(),
        instances: 1,
    }]);
    assert!(app.parked_agents_split.is_none(), "the parked set is dropped");
    assert!(
        signal.load(std::sync::atomic::Ordering::Relaxed),
        "the parked turn's stop signal fired with it"
    );
}

/// A `AgentsPaneDone` stamped with a STALE epoch is dropped wholesale: the
/// pane's slot set was replaced or torn down while the turn was being
/// cancelled, so its terminal report must never seed the replacement pane's
/// session id, land in the new pane's transcript, or promote the binding in
/// the CURRENT session's metadata under a same-named pane. A live-epoch
/// Done right after still applies — the gate only filters stale deliveries.
#[tokio::test]
async fn stale_epoch_agents_pane_done_is_dropped() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "stale epoch".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);

    // A turn from the OLD pane set lands AFTER a fresh confirm replaced the
    // slots: re-opening the split bumped the epoch, so the Done's pre-bump
    // stamp is stale.
    let stale_epoch = app.agents_pane_epoch;
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            epoch: stale_epoch,
            output: "stale report".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-old".into()),
        })
        .ok();
    app.poll_events();
    {
        let panes = app
            .state
            .current_session()
            .unwrap()
            .agents_panes
            .clone()
            .unwrap();
        assert_eq!(
            panes[0],
            AgentPaneRef::Name("claude".into()),
            "a stale Done must not promote the pane binding"
        );
    }
    {
        let pane = &app.agents_split.as_ref().unwrap().panes[0];
        assert!(
            pane.session_id.is_none(),
            "the replacement pane must not inherit the cancelled turn's session"
        );
        assert!(
            !pane.activity.timeline.iter().any(|e| matches!(
                e,
                crate::routes::session::right_panel::types::SubagentTimelineEntry::Message {
                    text
                } if text.contains("stale report")
            )),
            "the stale report must not land in the new pane's transcript"
        );
    }

    // Positive control: a CURRENT-epoch Done still applies in full. The
    // pane is dispatched first (running), as every real Done follows a
    // dispatch.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            epoch: app.agents_pane_epoch,
            output: "live report".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-new".into()),
        })
        .ok();
    app.poll_events();
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    assert_eq!(
        panes[0].session_id(),
        Some("sess-new"),
        "a live-epoch Done binds normally"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .activity
            .timeline
            .iter()
            .any(|e| matches!(
                e,
                crate::routes::session::right_panel::types::SubagentTimelineEntry::Message {
                    text
                } if text.contains("live report")
            )),
        "the live report lands in the pane's transcript"
    );
}

/// The cold-restore trigger: a `Bound` pane whose live timeline is EMPTY
/// (the post-reboot state — the process died with the app, only the ACP
/// session id persisted) starts a rehydrate pass the moment the split
/// takes the screen. `Name` panes, panes with content and panes already
/// rehydrating are skipped.
#[tokio::test]
async fn reopening_the_split_rehydrates_bound_panes_with_empty_timelines() {
    use cosh::harness::HarnessEvent;
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rehydrate".into(), 0);
    app.state.current_session_id = Some(session_id);

    // Persisted metadata: pane 0 bound to a session, pane 1 unbound.
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![
        AgentPaneRef::Bound {
            agent: "claude".into(),
            session_id: "sess-old".into(),
        },
        AgentPaneRef::Name("kilo".into()),
    ]);
    app.state.current_session_mut().unwrap().agents_mode_open = true;

    // Restore path (the same arm the session-switch rehydrate uses):
    // rebuild the live split from the persisted pane set.
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));
    app.trigger_agents_pane_replays();

    // In tests the replay pass never spawns a harness — the flag marks
    // the in-flight state the pass would stream through.
    let split = app.agents_split.as_ref().unwrap();
    assert!(
        split.panes[0].rehydrating,
        "the bound pane with an empty timeline starts its replay pass"
    );
    assert!(
        !split.panes[1].rehydrating,
        "an unbound Name pane has nothing to rehydrate"
    );

    // The replayed history lands through the SAME pane-event channel a
    // live turn uses (pane-index + epoch routed) — user and agent sides.
    let epoch = app.agents_pane_epoch;
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch,
            event: cosh_tools::subagent::events::SubagentEvent::UserMessage {
                text: "what changed today?".into(),
            },
        })
        .ok();
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "the answer from yesterday".into(),
            },
        })
        .ok();
    // The terminal event clears the in-flight flag.
    app.event_tx
        .send(HarnessEvent::AgentsPaneReplayed {
            pane: 0,
            epoch,
            ok: true,
            error: String::new(),
        })
        .ok();
    app.poll_events();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(!pane.rehydrating, "the pass ended");
    let texts: Vec<String> = pane
        .activity
        .timeline
        .iter()
        .map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text }
            | crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => text.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("what changed today?")),
        "the replayed user turn lands in the timeline: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("the answer from yesterday")),
        "the replayed agent reply lands in the timeline: {texts:?}"
    );
    // The binding is UNTOUCHED by a replay: the pane still points at the
    // persisted session id (no prompt happened, no new id came back).
    let panes = app.state.current_session().unwrap().agents_panes.clone();
    assert_eq!(
        panes.unwrap()[0].session_id(),
        Some("sess-old"),
        "a replay never rebinds the pane's session"
    );
}

/// A FAILED replay pass (stale id, harness without `loadSession`, spawn
/// failure) clears the in-flight flag and surfaces the reason as a labeled
/// marker in the pane — the pane stays usable and the NEXT prompt falls
/// back to `session/new` through the normal turn path.
#[tokio::test]
async fn a_failed_replay_marks_the_pane_instead_of_silence() {
    use cosh::harness::HarnessEvent;
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "replay fail".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![AgentPaneRef::Bound {
        agent: "claude".into(),
        session_id: "sess-gone".into(),
    }]);
    app.state.current_session_mut().unwrap().agents_mode_open = true;
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));
    app.trigger_agents_pane_replays();
    assert!(app.agents_split.as_ref().unwrap().panes[0].rehydrating);

    let epoch = app.agents_pane_epoch;
    app.event_tx
        .send(HarnessEvent::AgentsPaneReplayed {
            pane: 0,
            epoch,
            ok: false,
            error: "ACP session/load failed: session gone".into(),
        })
        .ok();
    app.poll_events();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(!pane.rehydrating, "a failed pass also ends the wait");
    let texts: Vec<String> = pane
        .activity
        .timeline
        .iter()
        .map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text }
            | crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => text.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("[replay failed]")),
        "the failure is labeled in the pane: {texts:?}"
    );
    // The binding survives a failed pass: the id stays persisted (a later
    // harness restart may resurrect the session; the next prompt's own
    // resume-fallback self-heals a genuinely dead id).
    let panes = app.state.current_session().unwrap().agents_panes.clone();
    assert_eq!(
        panes.unwrap()[0].session_id(),
        Some("sess-gone"),
        "a failed replay never rebinds the pane's session"
    );
}

/// While a pane is rehydrating, a prompt aimed at it is refused with a
/// toast: the pane's history is still arriving and a dispatch now would
/// interleave with the replay stream (and resume from a half-replayed
/// state). Another pane's prompt is unaffected.
#[tokio::test]
async fn a_prompt_to_a_rehydrating_pane_is_refused_with_a_toast() {
    use crate::types::AgentPaneRef;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rehydrating gate".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![
        AgentPaneRef::Bound {
            agent: "claude".into(),
            session_id: "sess-1".into(),
        },
        AgentPaneRef::Name("kilo".into()),
    ]);
    app.state.current_session_mut().unwrap().agents_mode_open = true;
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));
    app.trigger_agents_pane_replays();

    // Focus pane 0 (rehydrating): the dispatch is refused.
    app.dispatch_to_agents_split("hello?".to_string());
    assert!(
        app.toast_state
            .current
            .as_ref()
            .is_some_and(|t| t.message.contains("Restoring this agent's history")),
        "the refusal toast tells the user to wait"
    );
    assert!(
        !app.agents_split.as_ref().unwrap().panes[0].running,
        "no turn started behind the gate"
    );

    // Focus pane 1 (idle, not rehydrating): the dispatch runs.
    if let Some(split) = app.agents_split.as_mut() {
        split.focused = Some(1);
    }
    app.toast_state.current = None;
    app.dispatch_to_agents_split("go".to_string());
    assert!(
        app.agents_split.as_ref().unwrap().panes[1].running,
        "an idle pane dispatches normally while a sibling rehydrates"
    );
}

/// A user prompt echoes into the pane's timeline as its own turn entry:
/// without the echo the pane shows only the agent's output and the input
/// surfaces only after a reboot's replay pass rebuilds it. A second turn
/// opens a SECOND entry — consecutive tool-less turns must not coalesce
/// into a single row.
#[tokio::test]
async fn a_user_prompt_echoes_into_the_pane_timeline() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "input echo".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes =
        Some(vec![AgentPaneRef::Name("kilo".into())]);
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));

    app.dispatch_to_agents_split("first question".to_string());
    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(pane.running, "the turn started");
    assert!(
        pane.activity.timeline.iter().any(|e| matches!(
            e,
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage { text }
                if text.contains("first question")
        )),
        "the user's prompt is in the timeline: {:?}",
        pane.activity.timeline
    );

    // The turn ends with a tool-less report; the next prompt must open a
    // second entry, not merge into the first one.
    let epoch = app.agents_pane_epoch;
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            epoch,
            output: "first answer".into(),
            stop_reason: "end_turn".into(),
            session: None,
        })
        .ok();
    app.poll_events();
    app.dispatch_to_agents_split("second question".to_string());
    let user_inputs: Vec<String> = app.agents_split.as_ref().unwrap().panes[0]
        .activity
        .timeline
        .iter()
        .filter_map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        user_inputs,
        vec!["first question".to_string(), "second question".to_string()],
        "each turn owns its input entry"
    );
}

/// A `SessionModel` pane event stores the serving pair on the pane (the
/// prompt badge reads it) instead of streaming into the timeline.
#[tokio::test]
async fn a_session_model_event_lands_on_the_pane() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "pane badge".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes =
        Some(vec![AgentPaneRef::Name("kilo".into())]);
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .session_info
            .is_none(),
        "no serving pair before the first report"
    );

    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::SessionModel {
                model: Some("kilo/nvidia/nemotron-3-ultra-550b-a55b:free".into()),
                model_label: Some("Nemotron free".into()),
                provider: Some("nvidia".into()),
                provider_label: Some("Nvidia".into()),
                reasoning: Some("High".into()),
            },
        })
        .ok();
    app.poll_events();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    let info = pane.session_info.as_ref().expect("the report is stored");
    assert_eq!(
        info.model.as_deref(),
        Some("kilo/nvidia/nemotron-3-ultra-550b-a55b:free")
    );
    assert_eq!(info.model_label.as_deref(), Some("Nemotron free"));
    assert_eq!(info.provider.as_deref(), Some("nvidia"));
    assert_eq!(info.provider_label.as_deref(), Some("Nvidia"));
    assert_eq!(info.reasoning.as_deref(), Some("High"));
    assert!(
        pane.activity.timeline.is_empty(),
        "pane chrome never streams into the timeline"
    );
}

/// A `Usage` pane event stores the ACP budget snapshot on the pane's
/// activity (the pane header reads it) instead of streaming into the
/// timeline.
#[tokio::test]
async fn a_usage_event_lands_on_the_pane() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "pane budget".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes =
        Some(vec![AgentPaneRef::Name("kilo".into())]);
    let panes = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        panes,
    ));
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .activity
            .usage
            .is_none(),
        "no budget before the first UsageUpdate"
    );

    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Usage {
                context_window: 100_000,
                tokens_in_context: 9_612,
            },
        })
        .ok();
    app.poll_events();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert_eq!(
        pane.activity.usage,
        Some((100_000, 9_612)),
        "the ACP snapshot is stored as (context_window, tokens_in_context)"
    );
    assert!(
        pane.activity.timeline.is_empty(),
        "the budget never streams into the timeline"
    );
}

/// End-to-end `/agents` queue flow with a `cosh` pane. The registry mirrors
/// the split (cosh addressable like any agent), an `agents_call` queued to
/// "cosh" is picked up by the App's queue service (which used to skip the
/// daemon), and the pane-keyed completion resolves the waiter — exactly the
/// path a manager delegation to a sibling takes.
#[tokio::test]
async fn agents_call_serves_a_queued_cosh_pane_end_to_end() {
    use cosh::harness::panes::{self, ReplyMode};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // Contract: this test drives `serve_agents_pane_queues` → `dispatch_to_pane`,
    // which spawns a manager turn thread. Neutralize the daemon connector so
    // `run_manager_turn` returns its error immediately (`daemon_connector`
    // fails before building a runtime) instead of ever touching the network or
    // $HOME — no live turn thread is left behind, regardless of any provider
    // in the ambient environment.
    app.llm_config.provider.clear();
    app.llm_config.model = None;
    app.router_view.fallbacks.clear();
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "cosh queue".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![
        crate::ui::agents_picker::AgentPick {
            name: "cosh".into(),
            instances: 1,
        },
        crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 1,
        },
    ]);
    // Re-affirm the mirror (open already syncs; explicit here so the test
    // fails at the registry assertion, not at the serve step).
    app.sync_agents_pane_registry();

    let ids: Vec<String> = panes::list_panes().into_iter().map(|p| p.id).collect();
    assert!(ids.contains(&"cosh".to_string()), "cosh is registered: {ids:?}");
    assert!(
        ids.contains(&"claude".to_string()),
        "sibling pane is registered too: {ids:?}"
    );

    // The MAIN loop delegates to the cosh pane: the request queues, the
    // queue service dispatches it (the daemon filter is gone), and the turn
    // is running.
    let handle = panes::enqueue("cosh", "do the work", panes::MAIN, ReplyMode::Wait)
        .expect("cosh pane is addressable");
    assert!(panes::has_queued_request("cosh"), "request queued");
    app.serve_agents_pane_queues();
    assert!(
        !panes::has_queued_request("cosh"),
        "the cosh request left the queue: it was served"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0].running,
        "the cosh pane turn started"
    );

    // The turn's end (keyed by pane) resolves the waiter with its report.
    panes::complete_request("cosh", Ok("report".to_string()));
    let rx = handle.receiver.expect("wait mode carries a receiver");
    let answer = rx.await.expect("sender alive");
    assert_eq!(answer, Ok("report".to_string()));

    // No split: the same service does not panic with no pane set to serve.
    app.close_agents_split();
    app.sync_agents_pane_registry();
    app.serve_agents_pane_queues();
    assert!(
        !panes::has_queued_request("cosh"),
        "an absent cosh pane has no queue to serve"
    );
}

/// Issue 5 — the frame-tick watchdog force-closes a pane whose turn went
/// silent: `running` with NO event for longer than [`PANE_IDLE_WATCHDOG`]
/// means the turn thread died or stalled with no way to report (its Done
/// can never arrive). The pane goes idle, its timeline seals with the
/// watchdog marker, and a turn that is still streaming (recent activity)
/// or idle is never touched.
#[tokio::test]
async fn the_pane_watchdog_force_closes_a_silent_turn() {
    use crate::routes::session::right_panel::types::SubagentTimelineEntry;
    use std::time::Duration;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "watchdog".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 2,
    }]);

    // Pane 0: dispatched, then silent for longer than the watchdog window
    // (backdate its baseline directly — no real 10-minute sleep).
    // Pane 1: dispatched and STREAMING (fresh activity).
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.agents_split.as_mut().unwrap().set_running(1, true);
    let stale = crate::routes::session::agents_split::PANE_IDLE_WATCHDOG + Duration::from_secs(1);
    app.agents_split.as_mut().unwrap().panes[0].last_activity =
        Some(std::time::Instant::now() - stale);

    app.reconcile_pane_watchdog();

    let split = app.agents_split.as_ref().unwrap();
    assert!(
        !split.panes[0].running,
        "the silent pane's turn is force-closed"
    );
    assert!(
        split.panes[0].activity.timeline.iter().any(|e| matches!(
            e,
            SubagentTimelineEntry::Message { text }
                if text.contains("[turn cancelled] the turn stopped responding (watchdog)")
        )),
        "the watchdog seals the timeline with its reason"
    );
    assert!(
        split.panes[1].running,
        "a streaming pane is never touched by the watchdog"
    );
    assert!(
        split.panes[1].activity.timeline.is_empty(),
        "no marker lands on the live pane"
    );

    // An IDLE pane is also untouched (a second run after recovery is a
    // no-op — the recovered pane stays closed and clean).
    app.reconcile_pane_watchdog();
    assert!(!app.agents_split.as_ref().unwrap().panes[0].running);
}

/// Issue 5 — the watchdog runs on the LIVE and the PARKED split alike:
/// a pane that went silent while its split was parked (the user left to
/// the normal view) must recover the same way, or the parked pane's
/// spinner would spin forever behind the scenes.
#[tokio::test]
async fn the_pane_watchdog_covers_the_parked_split() {
    use std::time::Duration;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session_store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "parked watchdog".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.park_agents_split();
    assert!(app.parked_agents_split.is_some());

    // The parked pane goes silent beyond the watchdog window.
    let stale = crate::routes::session::agents_split::PANE_IDLE_WATCHDOG + Duration::from_secs(1);
    app.parked_agents_split.as_mut().unwrap().panes[0].last_activity =
        Some(std::time::Instant::now() - stale);

    app.reconcile_pane_watchdog();

    let parked = app.parked_agents_split.as_ref().unwrap();
    assert!(
        !parked.panes[0].running,
        "a parked pane's dead turn recovers too"
    );
}
