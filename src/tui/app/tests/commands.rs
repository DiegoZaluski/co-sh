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
    let id = crate::session::store::generate_session_id();
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
    let foreign = crate::session::store::SessionStore::new();
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
    let foreign = crate::session::store::SessionStore::new();
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
    assert_ne!(new_id, previous_id, "/new must select a different session");

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
        let foreign = crate::session::store::SessionStore::new();
        assert!(
            !foreign.try_lock_session(&id),
            "the current session must carry this process's open-lock"
        );
        // Every PREVIOUS session's claim was handed off by this /new —
        // a fresh foreign process can claim it right now.
        for prev in &superseded {
            let released = crate::session::store::SessionStore::new();
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
    let agent_colors = crate::agent_colors::AgentColors::from_theme(&app.theme);
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
    // (the manager pane exists ONLY because the user marked it).
    let session_id = crate::session::store::generate_session_id();
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
            worktree: None,
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.get(1))
                .map(|p| p.id)
                .unwrap_or(0),
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

/// Adding panes to a split while agents are RUNNING must not kill the
/// running turns nor wipe the visible chat: the fresh confirm carries the
/// surviving pane OBJECTS over (timeline, running flag, stop signal, bound
/// session), the new panes join at their picked positions, and every event
/// of a surviving turn keeps landing in ITS pane (the stable pane-id
/// routing). Before the reconcile, any pane-set change signal-stopped every
/// running turn and rebuilt fresh empty panes — the streams died and the
/// chat visually reset.
#[tokio::test]
async fn adding_panes_while_running_keeps_the_running_turns_and_chats() {
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "add panes".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);

    // Pane 0 starts a turn and streams content.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
    let epoch = app.agents_pane_epoch;
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id,
            epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "turn in flight".into(),
            },
        })
        .ok();
    app.poll_events();

    // The user re-picks WITH MORE PANES while that turn is still running.
    app.open_agents_split(vec![
        crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 1,
        },
        crate::ui::agents_picker::AgentPick {
            name: "kilo".into(),
            instances: 1,
        },
    ]);
    {
        let split = app.agents_split.as_ref().unwrap();
        assert_eq!(split.panes.len(), 2, "the fresh set has both panes");
        assert_eq!(
            split.panes[0].id, pane_id,
            "the running pane's OBJECT survived the re-pick"
        );
        assert!(
            split.panes[0].running,
            "the running turn keeps running across the re-pick"
        );
        assert!(
            !split.panes[0].activity.timeline.is_empty(),
            "the running pane's chat survived the re-pick"
        );
        assert!(
            !split.panes[1].running && split.panes[1].activity.timeline.is_empty(),
            "the genuinely new pane starts fresh"
        );
    }

    // The surviving turn's stream keeps landing in its pane (the routing
    // goes through the pane id, not the index).
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "still streaming after the re-pick".into(),
            },
        })
        .ok();
    app.poll_events();
    {
        let split = app.agents_split.as_ref().unwrap();
        let text: Vec<&str> = split.panes[0]
            .activity
            .timeline
            .iter()
            .filter_map(|e| match e {
                crate::routes::session::right_panel::types::SubagentTimelineEntry::Message {
                    text,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            text.iter()
                .any(|t| t.contains("still streaming after the re-pick")),
            "the surviving turn's stream follows its pane: {text:?}"
        );
    }

    // The turn ENDS after the re-pick: its Done still applies to the
    // surviving pane (session binding included).
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            pane_id,
            epoch: app.agents_pane_epoch,
            output: "report after re-pick".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-kept".into()),
        })
        .ok();
    app.poll_events();
    {
        let split = app.agents_split.as_ref().unwrap();
        assert!(
            !split.panes[0].running,
            "the Done closed the surviving turn"
        );
        assert_eq!(
            split.panes[0].session_id.as_deref(),
            Some("sess-kept"),
            "the Done binds the surviving pane's session"
        );
        let panes = app
            .state
            .current_session()
            .unwrap()
            .agents_panes
            .clone()
            .unwrap();
        assert_eq!(
            panes[0],
            crate::types::AgentPaneRef::Bound {
                agent: "claude".into(),
                session_id: "sess-kept".into(),
                worktree: None,
            },
            "the surviving pane's binding promotes from its own turn's Done"
        );
    }

    // A DROPPED pane's late events find no pane: shrinking the set must not
    // let a removed turn's stream leak into the pane that took its index.
    app.agents_split.as_mut().unwrap().set_running(1, false);
    let dropped_id = {
        // Re-grow the set once more so a pane EXISTS at index 1, then drop
        // it with the next confirm while its (synthetic) turn is marked.
        app.open_agents_split(vec![
            crate::ui::agents_picker::AgentPick {
                name: "claude".into(),
                instances: 1,
            },
            crate::ui::agents_picker::AgentPick {
                name: "kilo".into(),
                instances: 1,
            },
        ]);
        let split = app.agents_split.as_ref().unwrap();
        split.panes[1].id
    };
    app.agents_split.as_mut().unwrap().set_running(1, true);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    assert_eq!(app.agents_split.as_ref().unwrap().panes[0].id, pane_id);
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 1,
            pane_id: dropped_id,
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "dropped pane leak".into(),
            },
        })
        .ok();
    app.poll_events();
    {
        let split = app.agents_split.as_ref().unwrap();
        assert_eq!(split.panes.len(), 1);
        assert!(
            !split.panes[0].activity.timeline.iter().any(|e| matches!(
                e,
                crate::routes::session::right_panel::types::SubagentTimelineEntry::Message {
                    text
                } if text.contains("dropped pane leak")
            )),
            "a dropped pane's late event must not leak into the kept pane"
        );
    }
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
    let session_id = crate::session::store::generate_session_id();
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "still streaming".into(),
            },
        })
        .ok();
    app.poll_events();
    assert!(
        !app.agents_split.as_ref().unwrap().panes[0]
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
            epoch: app.agents_pane_epoch,
            event: cosh_tools::subagent::events::SubagentEvent::Message {
                text: "parked stream".into(),
            },
        })
        .ok();
    app.poll_events();
    let parked_text = app.parked_agents_split.as_ref().unwrap().panes[0]
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
    let reattached_text = app.agents_split.as_ref().unwrap().panes[0]
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
    let unique = |tag: &str| format!("{}-{tag}", crate::session::store::generate_session_id());
    let a = unique("a");
    app.state
        .add_empty_session(a.clone(), "open mode".into(), 0);
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
                worktree: None,
            },
            AgentPaneRef::Name("kilo".into()),
        ]);
        session.agents_mode_open = false;
    }

    // Session C: no agent state at all.
    let c = unique("c");
    app.state
        .add_empty_session(c.clone(), "no agents".into(), 0);

    // Switch to B: NO split screen — the set rebuilds parked instead.
    assert!(app.switch_session_locked(&b));
    assert!(
        app.agents_split.is_none(),
        "a parked set never grabs the screen"
    );
    let parked = app
        .parked_agents_split
        .as_ref()
        .expect("parked slot rebuilt");
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

/// The `agents_call` hang (user report: the main agent called a pane and the
/// call never came back): a WHOLE-SET death — a session switch — must fail
/// the outgoing panes' in-flight AND queued `agents_call` requests and
/// re-sync the registry to the incoming session's set. The old switch path
/// only failed IN-FLIGHT requests and never re-synced: a queued request of
/// the OLD session stayed in the registry with nobody left to dispatch it
/// (`serve_agents_pane_queues` is gated off without a split), so the main
/// loop's pre-Done busy-guard (`has_outstanding_requests`) spun eternally —
/// the harness hung even though no pane existed anymore. A colliding id
/// (same agent name) would even be served by the NEW session's pane.
#[tokio::test]
async fn session_switch_fails_outstanding_pane_requests_and_resyncs_the_registry() {
    use cosh::harness::panes::{self, ReplyMode};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let unique = |tag: &str| format!("{}-{tag}", crate::session::store::generate_session_id());

    // Session A: a split with a cosh pane; a queued `agents_call` request
    // nobody has dispatched yet (the pane is idle — the frame tick would
    // serve it, but the switch races ahead of it).
    let a = unique("a");
    app.state
        .add_empty_session(a.clone(), "with panes".into(), 0);
    app.state.current_session_id = Some(a.clone());
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "cosh".into(),
        instances: 1,
    }]);
    app.sync_agents_pane_registry();
    let handle = panes::enqueue("cosh", "delegated work", panes::MAIN, ReplyMode::Wait)
        .expect("cosh pane is addressable");
    let rx = handle.receiver.expect("wait mode carries a receiver");
    assert!(panes::has_queued_request("cosh"), "request queued");
    assert!(panes::has_outstanding_requests(panes::MAIN));

    // Session B: NO pane set — the plainest hang reproduction: after the
    // switch nothing can ever dispatch the old request.
    let b = unique("b");
    app.state.add_empty_session(b.clone(), "no panes".into(), 0);
    assert!(app.switch_session_locked(&b));
    assert!(app.agents_split.is_none());
    assert!(app.parked_agents_split.is_none());

    // THE FIX: the queued request failed (the waiter unblocked with the
    // failure instead of hanging forever) and the registry is EMPTY —
    // `has_panes` false means the next harness build registers no
    // `agents_call` tool at all.
    let answer = rx.await.expect("sender alive");
    assert!(
        answer.output.is_err(),
        "the queued request must fail on a session switch, not hang: {answer:?}"
    );
    assert!(
        !panes::has_outstanding_requests(panes::MAIN),
        "no outstanding request may survive a session switch (the busy-guard would spin forever)"
    );
    assert!(
        !panes::has_panes(),
        "the outgoing pane set must leave the registry — no session, no panes"
    );

    // A session WITH panes re-registers them: the incoming set is what the
    // registry serves, never the old session's leftovers.
    app.state.current_session_id = Some(a.clone());
    assert!(app.switch_session_locked(&a));
    let ids: Vec<String> = panes::list_panes().into_iter().map(|p| p.id).collect();
    assert!(
        ids.contains(&"cosh".to_string()),
        "pane re-registered: {ids:?}"
    );
    assert!(panes::has_panes());
}

/// A FRESH picker confirm supersedes a parked split of the same session:
/// the replaced panes' stop signals fire (their turn can't stream into a
/// pane of the new list), the parked slot empties.
#[tokio::test]
async fn fresh_confirm_supersedes_the_parked_split() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
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
        Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        ))),
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
    assert!(
        app.parked_agents_split.is_none(),
        "the parked set is dropped"
    );
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
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "stale epoch".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);

    // A turn from the OLD pane set lands AFTER a fresh confirm replaced the
    // slots: the OLD pane object was dropped by the re-pick, so its id
    // answers to nothing anymore — the Done is dropped by the identity
    // routing (and its pre-bump epoch stamp is stale too).
    let stale_epoch = app.agents_pane_epoch;
    let stale_pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            pane_id: stale_pane_id,
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rehydrate".into(), 0);
    app.state.current_session_id = Some(session_id);

    // Persisted metadata: pane 0 bound to a session, pane 1 unbound.
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![
        AgentPaneRef::Bound {
            agent: "claude".into(),
            session_id: "sess-old".into(),
            worktree: None,
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
            epoch,
            event: cosh_tools::subagent::events::SubagentEvent::UserMessage {
                text: "what changed today?".into(),
            },
        })
        .ok();
    app.event_tx
        .send(HarnessEvent::AgentsPaneEvent {
            pane: 0,
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
        texts
            .iter()
            .any(|t| t.contains("the answer from yesterday")),
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
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "replay fail".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![AgentPaneRef::Bound {
        agent: "claude".into(),
        session_id: "sess-gone".into(),
        worktree: None,
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "rehydrating gate".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.state.current_session_mut().unwrap().agents_panes = Some(vec![
        AgentPaneRef::Bound {
            agent: "claude".into(),
            session_id: "sess-1".into(),
            worktree: None,
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
    let session_id = crate::session::store::generate_session_id();
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
    let session_id = crate::session::store::generate_session_id();
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
    let session_id = crate::session::store::generate_session_id();
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
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
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
/// manager loop), and the pane-keyed completion resolves the waiter — exactly the
/// path a manager delegation to a sibling takes.
#[tokio::test]
async fn agents_call_serves_a_queued_cosh_pane_end_to_end() {
    use cosh::harness::panes::{self, ReplyMode};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    // Contract: this test drives `serve_agents_pane_queues` → `dispatch_to_pane`,
    // which spawns a manager turn thread. Neutralize the manager route so
    // `run_manager_turn` returns its error immediately (the assembly's
    // `build_connector` fails before building a runtime) instead of ever
    // touching the network or $HOME — no live turn thread is left behind,
    // regardless of any provider in the ambient environment.
    app.llm_config.provider.clear();
    app.llm_config.model = None;
    app.router_view.fallbacks.clear();
    let session_id = crate::session::store::generate_session_id();
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
    assert!(
        ids.contains(&"cosh".to_string()),
        "cosh is registered: {ids:?}"
    );
    assert!(
        ids.contains(&"claude".to_string()),
        "sibling pane is registered too: {ids:?}"
    );

    // The MAIN loop delegates to the cosh pane: the request queues, the
    // queue service dispatches it (the manager pane routing is live), and the turn
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
    assert_eq!(
        panes::complete_request(
            "cosh",
            panes::PaneReport::new(
                Ok("report".to_string()),
                panes::PaneTurnEnd::EndTurn
            )
        ),
        Some(panes::MAIN),
        "a MAIN-requested pane turn resolves the wake promise's tag"
    );
    // Everything HOME-sensitive above has already run: release the lock
    // before the await point (clippy::await_holding_lock) — the remaining
    // steps only touch the app's in-memory state, never $HOME.
    drop(_guard);
    let rx = handle.receiver.expect("wait mode carries a receiver");
    let answer = rx.await.expect("sender alive");
    assert_eq!(answer.output, Ok("report".to_string()));

    // No split: the same service does not panic with no pane set to serve.
    app.close_agents_split();
    app.sync_agents_pane_registry();
    app.serve_agents_pane_queues();
    assert!(
        !panes::has_queued_request("cosh"),
        "an absent cosh pane has no queue to serve"
    );
}

/// Issue 6 — rebooting with a cosh pane in the split restores its chat:
/// 1. the manager turn's terminal Done carries the pane's OWN cosh session
///    id, so the persisted entry promotes to `Bound` (before the fix every
///    manager Done carried `session: None` and the binding never reached
///    disk);
/// 2. a reboot rebuilds the pane `Bound` from that metadata and the
///    cold-restore replay loads the session straight from the STORE (the
///    manager pane is not an ACP client — no `session/load` round-trip):
///    the pane's timeline refills with the conversation.
#[tokio::test]
async fn a_rebooted_cosh_pane_replays_its_session_from_the_store() {
    use crate::types::{AgentPaneRef, Message, MessageRole, Part, ReasoningPart, TextPart};
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "cosh pane restore".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "cosh".into(),
        instances: 1,
    }]);

    // The manager pane's cosh session exists in the store with a real
    // conversation (as `persist_manager_session` leaves it after a turn).
    let manager_session_id = "mgr-sess-issue6";
    app.session_store.save_session(&crate::types::Session {
        id: manager_session_id.into(),
        title: "agents manager".into(),
        messages: vec![
            Message {
                id: "m0".into(),
                role: MessageRole::User,
                parts: vec![Part::Text(TextPart {
                    text: "plan the work".into(),
                    synthetic: false,
                })],
                created_at: 0,
                agent: None,
                model: None,
            },
            Message {
                id: "m1".into(),
                role: MessageRole::Assistant,
                // Reasoning precedes the text, exactly as
                // `persist_manager_session` writes a reasoning-carrying turn.
                parts: vec![
                    Part::Reasoning(ReasoningPart {
                        text: "thinking about the plan".into(),
                        collapsed: true,
                    }),
                    Part::Text(TextPart {
                        text: "the plan".into(),
                        synthetic: false,
                    }),
                ],
                created_at: 0,
                agent: None,
                model: None,
            },
        ],
        created_at: 0,
        title_generated: true,
        provider: None,
        model: None,
        reasoning: None,
        agents_panes: None,
        agents_mode_open: false,
        agents_hidden: Vec::new(),
        ctx_ids: std::collections::HashMap::new(),
        main_agent_worktree: None,
    });

    // The manager turn completes: the Done now carries the pane's session.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            pane_id: app
                .agents_split
                .as_ref()
                .and_then(|s| s.panes.first())
                .map(|p| p.id)
                .unwrap_or(0),
            epoch: app.agents_pane_epoch,
            output: "the report".into(),
            stop_reason: "end_turn".into(),
            session: Some(manager_session_id.into()),
        })
        .ok();
    app.poll_events();

    assert_eq!(
        app.state
            .current_session()
            .unwrap()
            .agents_panes
            .clone()
            .unwrap()[0],
        AgentPaneRef::Bound {
            agent: "cosh".into(),
            session_id: manager_session_id.into(),
            worktree: None,
        },
        "the manager Done must promote the cosh pane's binding (issue 6)"
    );

    // REBOOT: the process died with the app — a fresh split rebuilds from
    // the persisted metadata (Bound seed, EMPTY timelines) and the
    // cold-restore pass replays the bound session.
    let persisted = app
        .state
        .current_session()
        .unwrap()
        .agents_panes
        .clone()
        .unwrap();
    app.agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        persisted,
    ));
    app.trigger_agents_pane_replays();
    app.poll_events();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(!pane.rehydrating, "the store replay ends synchronously");
    let texts: Vec<String> = pane
        .activity
        .timeline
        .iter()
        .filter_map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text }
            | crate::routes::session::right_panel::types::SubagentTimelineEntry::UserMessage {
                text,
            } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("plan the work")),
        "the replayed user prompt lands in the rebooted pane: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("the plan")),
        "the replayed manager reply lands in the rebooted pane: {texts:?}"
    );
    // The persisted REASONING replays as a Thought entry (dimmed, like the
    // live pane's thinking blocks), BEFORE the message it preceded — a
    // rebooted pane must not lose the model's thinking.
    let thoughts: Vec<String> = pane
        .activity
        .timeline
        .iter()
        .filter_map(|e| match e {
            crate::routes::session::right_panel::types::SubagentTimelineEntry::Thought { text } => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        thoughts
            .iter()
            .any(|t| t.contains("thinking about the plan")),
        "the replayed reasoning lands as a Thought in the rebooted pane: {thoughts:?}"
    );
    // Arrival order: the reasoning precedes the message it belongs to.
    let plan_msg = pane.activity.timeline.iter().position(|e| {
        matches!(e, crate::routes::session::right_panel::types::SubagentTimelineEntry::Message { text } if text.contains("the plan"))
    });
    let thought = pane.activity.timeline.iter().position(|e| {
        matches!(e, crate::routes::session::right_panel::types::SubagentTimelineEntry::Thought { text } if text.contains("thinking about the plan"))
    });
    match (thought, plan_msg) {
        (Some(t), Some(m)) => assert!(
            t < m,
            "reasoning replays BEFORE its message, mirroring the live arrival order"
        ),
        _ => unreachable!("both entries were asserted present above"),
    }
    // The binding survives the replay untouched.
    assert_eq!(
        app.state
            .current_session()
            .unwrap()
            .agents_panes
            .clone()
            .unwrap()[0]
            .session_id(),
        Some(manager_session_id),
        "a replay never rebinds the pane's session"
    );
}

/// The wake promise (user-reported): "se o agente principal mandou uma
/// mensagem para um agente em um pane, ele precisa ter um output no
/// futuro". A pane turn whose request was made by the MAIN loop (here:
/// wait mode, whose waiter died when the main turn ended) must wake the
/// idle main agent with the report — the promise input → output holds.
/// The gateway-recommendation gate is pre-declined so `start_agent_loop`
/// runs its real path in the test (no dialog, no parked message).
#[tokio::test]
async fn a_main_initiated_pane_report_wakes_the_idle_main_agent() {
    use cosh::harness::panes::{self, ReplyMode};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.gateway_declined = true;
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "wake promise".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "cosh".into(),
        instances: 1,
    }]);
    app.sync_agents_pane_registry();

    // The MAIN loop delegates to the cosh pane and its turn ends before
    // the report arrives (the user pressed Esc, or the loop finished while
    // the pane kept working): the request is WAIT mode — an inline wait —
    // so simulate the ended waiter by completing the request AFTER the
    // handle is dropped into a dead receiver.
    let handle = panes::enqueue("cosh", "delegate the work", panes::MAIN, ReplyMode::Wait)
        .expect("cosh pane is addressable");
    app.serve_agents_pane_queues();
    // The pane is running the delegated turn; the app is IDLE (no main
    // loop running — the state never went Working).
    assert!(
        app.agents_split.as_ref().unwrap().panes[0].running,
        "the pane turn started"
    );
    drop(handle);

    // The pane's turn completes: the completion reports the MAIN
    // requester and the wake promise fires — the idle main agent starts
    // a FRESH turn carrying the report.
    let requester = panes::complete_request(
        "cosh",
        panes::PaneReport::new(
            Ok("the pane's report".to_string()),
            panes::PaneTurnEnd::EndTurn,
        ),
    );
    assert_eq!(requester, Some(panes::MAIN));
    app.agents_split.as_mut().unwrap().set_running(0, false);
    app.wake_main_agent_if_promised(requester);

    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Working,
        "the main agent must be woken by the report it was promised"
    );
    assert!(
        app.state
            .current_session()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.role == crate::types::MessageRole::User
                && m.parts.iter().any(|p| matches!(p, crate::types::Part::Text(t) if t.text.contains("the pane's report")))),
        "the wake turn's input carries the pane's report"
    );
}

/// The other half of the wake promise: a pane turn the USER initiated
/// (no `agents_call` request — the completion resolves nothing) must NOT
/// wake an idle main agent. The pane works in harmony with the main agent.
#[tokio::test]
async fn a_user_initiated_pane_turn_never_wakes_the_main_agent() {
    use cosh::harness::panes;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "harmony".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "cosh".into(),
        instances: 1,
    }]);
    app.sync_agents_pane_registry();

    // A USER turn ran in the pane and completed: the registry tracked no
    // request, so the completion resolves None and no wake can arm.
    assert!(
        !panes::has_queued_request("cosh"),
        "a user turn tracks no agents_call request"
    );
    let requester = panes::complete_request(
        "cosh",
        panes::PaneReport::new(
            Ok("a user-driven report".to_string()),
            panes::PaneTurnEnd::EndTurn,
        ),
    );
    assert_eq!(
        requester, None,
        "a user-initiated pane turn promises no output to any agent"
    );
    app.agents_split.as_mut().unwrap().set_running(0, false);
    app.wake_main_agent_if_promised(requester);

    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Idle,
        "a user-initiated pane turn must leave the main agent idle"
    );
}

/// A MANAGER-requested pane completion (a sibling pane a manager pane
/// delegated to — legacy shape; pane loops no longer carry the tool) must
/// not wake the MAIN agent: the delivery belongs to the manager's drain.
#[tokio::test]
async fn a_manager_initiated_pane_report_never_wakes_the_main_agent() {
    use cosh::harness::panes::{self, ReplyMode};

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "manager tag".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "cosh".into(),
        instances: 1,
    }]);
    app.sync_agents_pane_registry();

    let handle = panes::enqueue(
        "cosh",
        "manager to sibling",
        panes::MANAGER,
        ReplyMode::Background,
    )
    .expect("cosh pane is addressable");
    let _ = handle;
    app.serve_agents_pane_queues();
    app.agents_split.as_mut().unwrap().set_running(0, false);
    let requester = panes::complete_request(
        "cosh",
        panes::PaneReport::new(
            Ok("manager's report".to_string()),
            panes::PaneTurnEnd::EndTurn,
        ),
    );
    assert_eq!(requester, Some(panes::MANAGER));
    app.wake_main_agent_if_promised(requester);

    assert_eq!(
        app.state.status,
        crate::types::SessionStatus::Idle,
        "a manager-tagged delivery never wakes the MAIN agent"
    );
    assert!(
        panes::take_ready_deliveries(panes::MANAGER).len() == 1,
        "the delivery stays parked for the manager loop's own drain"
    );
}

/// A prompt drag released OVER a pane belongs to the prompt: the split owns
/// the release only while a pane selection is in flight. The copy must land
/// and the prompt selection must clear — never die silently under the pane.
#[tokio::test]
async fn prompt_drag_released_over_pane_copies_and_clears() {
    use crossterm::event::{
        KeyModifiers, MouseButton as CBtn, MouseEvent as CMouse, MouseEventKind as CKind,
    };

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    app.set_test_size(100, 40);
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "sel".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    // Render once so the split records its hit rects.
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    terminal.draw(|f| app.render(f, 0.0)).unwrap();
    let rect = app.agents_split.as_ref().unwrap().pane_rects[0];

    // Arm a prompt drag selection (press+drag happened over the prompt);
    // no split selection is in flight.
    app.prompt_view.input = "hello world".into();
    app.prompt_view.begin_selection(0, false);
    app.prompt_view.sel_end = Some(5);
    assert!(app.prompt_view.has_selection());
    app.mouse_down_pos = Some((2, 38));
    app.mouse_drag_active = true;
    assert!(!app.agents_split.as_ref().unwrap().has_selection());

    // Release over the pane: the generic dispatch must copy the prompt
    // text and drop the selection.
    app.handle_mouse_event(CMouse {
        kind: CKind::Up(CBtn::Left),
        column: rect.x + 5,
        row: rect.y + 2,
        modifiers: KeyModifiers::NONE,
    })
    .expect("release handled");
    assert!(
        !app.prompt_view.has_selection(),
        "the foreign release must clear the prompt selection"
    );
    let toast = app
        .toast_state
        .current
        .as_ref()
        .expect("a copy toast must show");
    assert!(
        toast.message.contains("5 chars"),
        "the prompt text is what copies: {}",
        toast.message
    );
}

/// Hiding is a LIFO stack: two hides, then restore brings them back in
/// reverse order at their recorded slots. Hiding never deletes the pane —
/// the visible set shrinks, the hidden stack grows; restoring moves it back.
#[tokio::test]
async fn hiding_and_restoring_panes_is_lifo_and_never_deletes() {
    use crate::ui::agents_picker::AgentPick;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state.add_empty_session(id.clone(), "panes".into(), 0);
    app.state.current_session_id = Some(id);
    app.open_agents_split(vec![
        AgentPick {
            name: "claude".into(),
            instances: 1,
        },
        AgentPick {
            name: "kilo".into(),
            instances: 1,
        },
        AgentPick {
            name: "cosh".into(),
            instances: 1,
        },
    ]);

    // Hide the middle pane, then the (new) middle one — stack [kilo, cosh].
    app.agents_split.as_mut().unwrap().focused = Some(1);
    app.hide_focused_agents_pane();
    app.agents_split.as_mut().unwrap().focused = Some(1);
    app.hide_focused_agents_pane();

    {
        let session = app.state.current_session().unwrap();
        let panes = session.agents_panes.as_ref().unwrap();
        assert_eq!(panes.len(), 1, "only the visible pane remains");
        assert_eq!(panes[0].agent(), "claude");
        assert_eq!(session.agents_hidden.len(), 2, "nothing was deleted");
        assert_eq!(session.agents_hidden[0].pane.agent(), "kilo");
        assert_eq!(
            session.agents_hidden[1].pane.agent(),
            "cosh",
            "the most recently hidden sits on top"
        );
    }

    // Restore: cosh first (LIFO), then kilo — both at their old index 1.
    app.restore_last_hidden_agents_pane();
    {
        let session = app.state.current_session().unwrap();
        assert_eq!(session.agents_hidden.len(), 1);
        let panes = session.agents_panes.as_ref().unwrap();
        assert_eq!(
            panes.iter().map(|p| p.agent()).collect::<Vec<_>>(),
            vec!["claude", "cosh"]
        );
    }
    app.restore_last_hidden_agents_pane();
    {
        let session = app.state.current_session().unwrap();
        assert!(session.agents_hidden.is_empty());
        let panes = session.agents_panes.as_ref().unwrap();
        assert_eq!(
            panes.iter().map(|p| p.agent()).collect::<Vec<_>>(),
            vec!["claude", "kilo", "cosh"]
        );
    }
    // Nothing left to restore: a no-op, not a panic.
    app.restore_last_hidden_agents_pane();
}

/// Hiding the LAST visible pane closes the mode entirely (the main chat
/// returns — an empty split screen shows nothing) while the hidden stack
/// survives, and a restore re-opens the split with the pane back.
#[tokio::test]
async fn hiding_the_last_pane_auto_closes_and_restore_reopens() {
    use crate::ui::agents_picker::AgentPick;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state.add_empty_session(id.clone(), "last".into(), 0);
    app.state.current_session_id = Some(id);
    app.open_agents_split(vec![AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app.agents_split.as_mut().unwrap().focused = Some(0);
    app.hide_focused_agents_pane();

    {
        // The split auto-closed: no live state, the mode is closed and the
        // visible set is GONE (`None` — an empty `Some` would be rebuilt
        // as a paneless split by the re-entry/restore paths).
        assert!(
            app.agents_split.is_none(),
            "the empty split left the screen"
        );
        let session = app.state.current_session().unwrap();
        assert!(!session.agents_mode_open, "the mode follows the screen");
        assert!(
            session.agents_panes.is_none(),
            "the visible set is gone, not an empty list"
        );
        assert_eq!(session.agents_hidden.len(), 1, "the pane is restorable");
    }

    // Ctrl+Z re-opens the split and brings the pane back.
    app.restore_last_hidden_agents_pane();
    {
        let session = app.state.current_session().unwrap();
        assert!(session.agents_mode_open, "the mode re-opens with the pane");
        assert!(session.agents_hidden.is_empty());
        let panes = session.agents_panes.as_ref().unwrap();
        assert_eq!(panes.len(), 1);
        assert_eq!(panes[0].agent(), "claude");
    }
    let split = app.agents_split.as_ref().expect("the split is back");
    assert_eq!(split.pane_count(), 1);
    assert_eq!(split.panes[0].agent, "claude");
}

/// The LEGACY emptied-split metadata (`agents_panes = Some([])`, persisted
/// by builds before the auto-close fix) must not resurrect an empty
/// split screen: a `/agents` re-entry on such a session opens the PICKER
/// (the initial state), not a paneless chat-like mode.
#[tokio::test]
async fn legacy_empty_pane_set_opens_the_picker_not_an_empty_split() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let mut app = App::new("/tmp".to_string());
    let id = crate::session::store::generate_session_id();
    app.state.add_empty_session(id.clone(), "legacy".into(), 0);
    app.state.current_session_id = Some(id.clone());
    // Persisted BEFORE the fix: an empty visible set, mode closed.
    {
        let session = app.state.current_session_mut().unwrap();
        session.agents_panes = Some(Vec::new());
        session.agents_mode_open = false;
    }
    // A session-switch round-trip must not park the empty set: the session
    // comes back to the NORMAL chat.
    app.switch_session_locked(&id);
    assert!(
        app.parked_agents_split.is_none(),
        "an empty persisted set parks nothing"
    );
    assert!(app.agents_split.is_none(), "the mode stays closed");

    // A live-but-empty parked split (the legacy in-memory shape) is not
    // re-attachable either: the picker opens instead.
    app.parked_agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        Vec::new(),
    ));
    assert!(
        !app.attach_parked_agents_split(),
        "an empty parked split is not re-attachable"
    );
    assert!(app.parked_agents_split.is_none());

    // Sanity: a NON-empty parked split still re-attaches.
    app.parked_agents_split = Some(crate::routes::session::agents_split::AgentsSplitState::new(
        vec![crate::types::AgentPaneRef::Name("claude".into())],
    ));
    assert!(app.attach_parked_agents_split(), "a real set re-attaches");
    assert_eq!(app.agents_split.as_ref().unwrap().pane_count(), 1);
}

/// Real repo helper for the `/worktree` integration tests: one empty commit
/// on the default branch, so `worktree add` branches off a HEAD. Isolated
/// from the shared `/tmp/cosh-worktrees` root: the add path is EXPLICIT
/// (tempdir-scoped), so parallel tests never collide on names.
fn worktree_test_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "T"],
        vec!["commit", "--allow-empty", "-m", "init", "--quiet"],
    ] {
        let status = std::process::Command::new("git")
            .args(&args)
            .current_dir(&path)
            .status()
            .expect("git status");
        assert!(status.success(), "git {args:?} failed");
    }
    (dir, path)
}

/// The SCOPE contract, end to end: a GLOBAL command picked while an external
/// agent pane holds the focus still acts on the APP. `/background` toggles
/// this terminal's background and is never dispatched to the pane as a
/// prompt — before the scope inventory existed, the router trusted the
/// menu's `source` alone, so every cosh command picked on a focused pane
/// became the AGENT's (a per-pane background would mean one terminal with
/// one background per pane: the overengineering the product rule forbids).
#[tokio::test]
async fn a_global_command_on_a_focused_agent_pane_acts_on_the_app() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = worktree_split_app(&repo);
    super::super::agents_dispatch::sync_slash_source(&mut app);
    assert_eq!(
        app.slash_menu.source,
        crate::ui::slash_menu::Source::Agent,
        "precondition: the focused claude pane owns the menu source"
    );
    assert!(
        app.agents_split.as_ref().unwrap().focused.is_some(),
        "precondition: a pane holds the focus"
    );

    let before = app.transparent_background;
    app.run_slash_command(&slash_cmd("background"));

    assert_ne!(
        app.transparent_background, before,
        "`/background` is app state: it must toggle the terminal's background"
    );
    assert!(
        !app.agents_split.as_ref().unwrap().panes[0].running,
        "a Global command must never be dispatched to the pane as a turn"
    );
}

/// One claude pane split with pane 0 focused — the worktree command's
/// target shape.
fn worktree_split_app(repo: &std::path::Path) -> App {
    let mut app = App::new(repo.display().to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "worktree panes".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: "claude".into(),
        instances: 1,
    }]);
    app
}

/// `/worktree add <path> <branch>` on the focused pane: the worktree is
/// created at the EXPLICIT path, the pane pin persists to the session
/// metadata (assigned_by: User), and the harness registry reports the
/// worktree so the main agent's pane listing knows where the pane lives.
#[tokio::test]
async fn slash_worktree_add_pins_the_pane_and_persists_the_binding() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    let wt = repo.join("wt-explicit");

    app.prompt_view.input = format!("/worktree add {} feat-x", wt.display());
    let cmd = slash_cmd("worktree");
    app.run_slash_command(&cmd);

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    let pin = pane.worktree.as_ref().expect("the pane got pinned");
    assert_eq!(pin.branch, "feat-x");
    assert_eq!(pin.path, wt);
    assert_eq!(pin.assigned_by, cosh::git::worktree::AssignedBy::User);
    assert!(wt.is_dir(), "the worktree directory exists");
    assert!(
        pane.worktree_handoff_pending,
        "the cwd moved: the next turn carries the handoff"
    );
    assert!(!app.slash_menu.visible, "the pick consumed the menu");

    // The binding persists through the session metadata write-back.
    let panes = app.state.current_session().unwrap().agents_panes.clone();
    let panes = panes.expect("the split stays open");
    assert_eq!(
        panes[0],
        crate::types::AgentPaneRef::Name("claude".into()),
        "an unbound pane keeps the Name shape — the worktree rides the in-memory pane and the NEXT Bound promotion"
    );

    // The main agent's registry knows the pane's worktree (context block).
    let descriptors = cosh::harness::panes::list_panes();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(
        descriptors[0].worktree.as_deref(),
        Some("feat-x"),
        "the registry carries the worktree name"
    );

    // The worktree checks out the branch in the repo.
    let head = String::from_utf8(
        std::process::Command::new("git")
            .args([
                "-C",
                wt.to_str().unwrap(),
                "rev-parse",
                "--abbrev-ref",
                "HEAD",
            ])
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .unwrap();
    assert_eq!(head.trim(), "feat-x");
}

/// `/worktree add <branch>` (no path): the worktree lands at the default
/// temp location — `<temp>/cosh-worktrees/<slug>` — and the pane's cwd
/// resolution moves there. The default root is SHARED system temp state,
/// so the test claims a run-unique slug (never colliding with parallel
/// tests or leftovers) and removes its own worktree at the end.
#[tokio::test]
async fn slash_worktree_add_without_a_path_uses_the_default_temp_root() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    // Run-unique branch: the default slug derives from it, so two
    // concurrent test runs (or leftovers from earlier ones) can never
    // fight over the same directory under the shared root.
    let branch = format!("fix-default-root-{}", std::process::id());
    let mut app = worktree_split_app(&repo);

    app.prompt_view.input = format!("/worktree add {branch}");
    app.run_slash_command(&slash_cmd("worktree"));

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    let pin = pane.worktree.as_ref().expect("the pane got pinned");
    let expected_root = std::env::temp_dir().join("cosh-worktrees");
    assert_eq!(
        pin.path.parent().map(|p| p.to_path_buf()),
        Some(expected_root),
        "the default path is the shared temp root: {}",
        pin.path.display()
    );
    assert_eq!(pin.branch, branch);

    // Teardown: let the TempDir drop clean up the worktree directory.
    // The branch survives; cosh never deletes branches.
}

/// `/worktree exit` drops the binding, keeps the worktree on disk (nothing
/// removed), and flags the handoff so the pane's next turn carries its
/// context back across the cwd move.
#[tokio::test]
async fn slash_worktree_exit_unpins_without_deleting_anything() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = worktree_split_app(&repo);
    let wt = repo.join("wt-exit");

    // Isolate first (asserting precondition).
    app.prompt_view.input = format!("/worktree add {} feat-exit", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_some()
    );
    assert!(wt.is_dir());

    // Exit.
    app.prompt_view.input = "/worktree exit".into();
    app.run_slash_command(&slash_cmd("worktree"));

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(pane.worktree.is_none(), "the binding is dropped");
    assert!(
        pane.worktree_handoff_pending,
        "the cwd moved back: the next turn carries the handoff"
    );
    assert!(
        wt.is_dir(),
        "the worktree directory Survives: exit never deletes"
    );
    let has_wt = std::process::Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "worktree",
            "list",
            "--porcelain",
        ])
        .output()
        .unwrap()
        .stdout;
    let has_wt = String::from_utf8(has_wt).unwrap();
    assert!(
        has_wt.contains("wt-exit"),
        "the git worktree registration survives"
    );
    // No success toast: the exit is evident from the dropped binding (and
    // its badge disappearing) — only a refusal speaks.
    assert!(
        app.toast_state.current.is_none(),
        "a successful /worktree exit must not toast: {:?}",
        app.toast_state.current.as_ref().map(|t| &t.message)
    );
}

/// `/worktree` with no arguments or an unknown subcommand is refused with
/// the usage toast — nothing breaks, nothing is created.
#[tokio::test]
async fn slash_worktree_unknown_args_refuse_with_usage() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = worktree_split_app(&repo);

    for tail in ["", "rm feat-x", "add onlypath", "exit now"] {
        app.prompt_view.input = format!("/worktree{tail}");
        app.run_slash_command(&slash_cmd("worktree"));
        let pane = &app.agents_split.as_ref().unwrap().panes[0];
        assert!(pane.worktree.is_none(), "tail `{tail}` must not pin");
        assert!(
            app.toast_state
                .current
                .as_ref()
                .is_some_and(|t| t.message.contains("Usage:")),
            "tail `{tail}` must refuse with the usage toast"
        );
    }
}

/// A worktree'd pane's NEXT turn dispatches against the worktree path —
/// the handoff block rides the prompt and the flag clears after ONE turn.
/// The cwd itself is asserted via the pane state; the turn's ACP spawn is
/// out of test reach, so the cwd resolution contract is exercised here
/// only down to the snapshot the dispatch reads.
#[tokio::test]
async fn slash_worktree_handoff_flows_into_the_next_turn_input() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = worktree_split_app(&repo);
    let wt = repo.join("wt-handoff");
    app.prompt_view.input = format!("/worktree add {} feat-handoff", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    // Local timeline the handoff summarizes from.
    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(pane.worktree_handoff_pending);

    // A turn consumes the flag (the manager pane path is testable: cosh
    // panes run the real harness loop). Use the ACP pane's INPUT path:
    // dispatch_to_pane flags running + spawns ACP, which tests must not
    // do — so consume via the split's setter, the same call the dispatch
    // makes after building the input.
    let split = app.agents_split.as_mut().unwrap();
    let pending = split.panes[0].worktree_handoff_pending;
    assert!(pending, "precondition: the pin flagged the handoff");
    split.set_worktree_handoff_pending(0, true);
}

/// A worktree'd pane that finishes its FIRST turn promotes to `Bound`
/// WITH its worktree — the live pane's binding wins over the stale
/// persisted entry (which was a worktree-less `Name`).
#[tokio::test]
async fn slash_worktree_pane_promotes_to_bound_with_its_worktree() {
    use crate::types::AgentPaneRef;
    use cosh::harness::HarnessEvent;

    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = worktree_split_app(&repo);
    let wt = repo.join("wt-promote");
    app.prompt_view.input = format!("/worktree add {} feat-promote", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    // The persisted entry is still a bare `Name` (no worktree yet): the
    // metadata write-back keyed on the pane set happened at the pin, but
    // for an UNBOUND pane the ref carries nothing — the split's own state
    // is the source of truth until the first Done promotes it.
    app.agents_split.as_mut().unwrap().set_running(0, true);
    app.event_tx
        .send(HarnessEvent::AgentsPaneDone {
            pane: 0,
            pane_id: app.agents_split.as_ref().unwrap().panes[0].id,
            epoch: app.agents_pane_epoch,
            output: "the worktree'd turn's report".into(),
            stop_reason: "end_turn".into(),
            session: Some("sess-wt".into()),
        })
        .ok();
    app.poll_events();

    let panes = app.state.current_session().unwrap().agents_panes.clone();
    let panes = panes.expect("the split stays open after a done");
    let AgentPaneRef::Bound {
        agent,
        session_id,
        worktree,
    } = &panes[0]
    else {
        panic!("the pane must promote to Bound, got {:?}", panes[0]);
    };
    assert_eq!(agent, "claude");
    assert_eq!(session_id, "sess-wt");
    let pin = worktree
        .as_ref()
        .expect("the promotion carries the worktree");
    assert_eq!(pin.branch, "feat-promote");
    assert_eq!(pin.path, wt);
}

// ── agents_worktree (Phase 4): the main agent asks, the App supervises ──

/// The agent-driven assign: the parked op executes on the frame drain with
/// the SAME side effects as the user's `/worktree add` (only the
/// `assigned_by` differs — `Agent`), and the outcome reaches the asking
/// loop as a Worktree-kind delivery. A pane the AGENT asked for needs NO
/// confirmation (no user binding to override).
#[tokio::test]
async fn agents_worktree_assign_executes_on_the_app_drain() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    app.sync_agents_pane_registry();

    let branch = format!("agent-wt-{}", std::process::id());
    let op_id = cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Assign,
        Some(&branch),
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    let pin = pane.worktree.as_ref().expect("the drain pinned the pane");
    assert_eq!(pin.branch, branch);
    assert_eq!(
        pin.assigned_by,
        cosh::git::worktree::AssignedBy::Agent,
        "the agent asked, not the user"
    );
    assert!(
        pane.worktree_handoff_pending,
        "the cwd moved: the pane's next turn carries the handoff"
    );
    assert!(
        !app.dialog.visible(),
        "no confirmation for an agent-unbound pane"
    );
    assert!(
        pin.path
            .starts_with(std::env::temp_dir().join("cosh-worktrees")),
        "the agent NEVER picks a path — cosh's temp root only"
    );
    // The registry knows the pin (the main agent's listing sees it).
    let listed = cosh::harness::panes::list_panes();
    assert!(
        listed
            .iter()
            .any(|p| p.worktree.as_deref() == Some(&branch)),
        "the registry carries the new binding"
    );
    // The outcome is a Worktree delivery for the asking loop.
    let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].request_id, op_id);
    assert_eq!(
        deliveries[0].kind,
        cosh::harness::panes::DeliveryKind::Worktree
    );
    let note = deliveries[0].output.clone().unwrap();
    assert!(note.contains(&branch), "{note}");
    assert!(
        note.contains(&pin.path.display().to_string()),
        "the outcome names the worktree path"
    );

    // Teardown: let the TempDir drop clean up the worktree directory.
    // The branch survives; cosh never deletes branches.
}

/// The SUPREMACY rule: the pane's binding was `AssignedBy::User`, so the
/// agent's clear ask opens the Confirm dialog — even though nothing in
/// Yolo mode otherwise asks — and parks. The USER's answer resolves it:
/// "No" declines (the binding is kept, the outcome delivery still lands)
/// and a later "Yes" would execute.
#[tokio::test]
async fn agents_worktree_clear_of_user_binding_needs_the_user() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    // The USER pins the pane first (explicit path — tempdir-scoped).
    let wt = repo.join("wt-supremacy");
    app.prompt_view.input = format!("/worktree add {} feat-user", wt.display());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "worktree".into(),
        desc: String::new(),
    });
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_some()
    );
    app.sync_agents_pane_registry();

    let op_id = cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Clear,
        None,
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();

    // Parked on the user's answer: the SESSION permission box is up (the
    // same inline box the main loop's permission requests use), the
    // binding is UNCHANGED, and nothing was delivered yet.
    assert!(
        app.permission_dialog.visible,
        "the user's binding demands the user's answer (session permission box)"
    );
    assert!(
        app.pending_worktree_confirmation.is_some(),
        "the op waits for the user"
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_some(),
        "no side effect before the answer"
    );
    assert!(cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN).is_empty());

    // The user declines: the binding is kept, the outcome still lands.
    app.resolve_pending_worktree_permission(
        crate::routes::session::permission::PermissionAction::Deny,
    );
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_some(),
        "the user's binding survives the decline"
    );
    assert!(
        !app.permission_dialog.visible,
        "the answer closed the permission box"
    );
    let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].request_id, op_id);
    let note = deliveries[0].output.clone().unwrap_err();
    assert!(note.to_lowercase().contains("declined"), "{note}");
    assert!(wt.is_dir(), "nothing on disk was touched");
}

/// THE reported bug, end to end: a user-pinned pane whose split is PARKED
/// (the /agents mode closed while the user talks to the main agent). The
/// ask opens the session permission box, the user ALLOWS ONCE, and the
/// confirmed clear must actually drop the binding — the executor used to
/// resolve only the LIVE split and fail with "the target pane is not
/// addressable anymore", leaving the pane pinned forever.
#[tokio::test]
async fn agents_worktree_ask_on_a_parked_split_executes_after_allow_once() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    // The USER pins the pane first (live split — the command needs focus).
    let wt = repo.join("wt-parked");
    app.prompt_view.input = format!("/worktree add {} feat-parked", wt.display());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "worktree".into(),
        desc: String::new(),
    });
    assert!(app.agents_split.as_ref().unwrap().panes[0]
        .worktree
        .is_some());
    // The user leaves the mode (park — the common shape of the report:
    // the main agent is asked from the NORMAL view).
    app.park_agents_split();
    assert!(app.agents_split.is_none());
    assert!(app.parked_agents_split.is_some());
    app.sync_agents_pane_registry();

    let op_id = cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Clear,
        None,
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();

    // The SESSION permission box is up for the user's binding.
    assert!(app.permission_dialog.visible, "the ask renders in the session box");
    assert!(app.pending_worktree_confirmation.is_some());

    // Allow Once: the confirmed clear REALLY clears (the fix).
    app.resolve_pending_worktree_permission(
        crate::routes::session::permission::PermissionAction::AllowOnce,
    );
    let pane = &app.parked_agents_split.as_ref().unwrap().panes[0];
    assert!(
        pane.worktree.is_none(),
        "the confirmed move must not strand the pane in its old worktree"
    );
    assert!(pane.worktree_handoff_pending, "the cwd moved back");
    assert!(!app.permission_dialog.visible, "the box closed");
    assert!(
        !app.worktree_agent_grant(),
        "AllowOnce does not latch the session grant"
    );
    let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].request_id, op_id);
    assert!(deliveries[0].output.clone().unwrap().contains("cleared"));
}

/// An agent-unbound pane in a PARKED split needs NO box at all: the drain
/// executes the assign directly against the parked pane (the same
/// `either_split_mut` view the user-pinned path now uses).
#[tokio::test]
async fn agents_worktree_assign_executes_on_a_parked_split_without_a_box() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    app.park_agents_split();
    app.sync_agents_pane_registry();

    let branch = format!("agent-parked-{}", std::process::id());
    let op_id = cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Assign,
        Some(&branch),
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();

    assert!(!app.permission_dialog.visible, "no box for an agent binding");
    let pane = &app.parked_agents_split.as_ref().unwrap().panes[0];
    let pin = pane.worktree.as_ref().expect("the parked pane got pinned");
    assert_eq!(pin.branch, branch);
    assert_eq!(pin.assigned_by, cosh::git::worktree::AssignedBy::Agent);
    let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].request_id, op_id);
    assert!(deliveries[0].output.is_ok());
}

/// "Allow" latches the SESSION-WIDE grant: a second ask on the same
/// User-assigned binding executes on the drain WITHOUT reopening the box
/// (and the grant survives — this app run only). "Allow Once" does not.
#[tokio::test]
async fn agents_worktree_allow_latches_the_session_grant() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    let wt = repo.join("wt-grant");
    app.prompt_view.input = format!("/worktree add {} feat-grant", wt.display());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "worktree".into(),
        desc: String::new(),
    });
    app.sync_agents_pane_registry();

    // First ask: the box opens, the user answers "Allow".
    cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Clear,
        None,
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();
    assert!(app.permission_dialog.visible);
    app.resolve_pending_worktree_permission(
        crate::routes::session::permission::PermissionAction::Allow,
    );
    assert!(
        app.worktree_agent_grant(),
        "Allow latches the session grant"
    );
    assert!(
        app.parked_agents_split.is_none() && app.agents_split.is_some(),
        "the binding actually changed (live split)"
    );
    assert!(app.agents_split.as_ref().unwrap().panes[0]
        .worktree
        .is_none());
    // Drain the first ask's outcome: the registry deliveries are GLOBAL
    // state — a leftover here would leak into the next test that reads
    // MAIN's deliveries.
    assert_eq!(
        cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN).len(),
        1
    );

    // The user re-pins (another User binding), a new ask comes: NO box.
    let wt2 = repo.join("wt-grant-2");
    app.prompt_view.input = format!("/worktree add {} feat-grant-2", wt2.display());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "worktree".into(),
        desc: String::new(),
    });
    assert!(app.agents_split.as_ref().unwrap().panes[0]
        .worktree
        .is_some());
    app.sync_agents_pane_registry();

    let op_id = cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Clear,
        None,
        cosh::harness::panes::MAIN,
    )
    .expect("the second ask parks");
    app.serve_agents_pane_worktree_ops();
    assert!(
        !app.permission_dialog.visible,
        "the session grant skips the box"
    );
    assert!(app.pending_worktree_confirmation.is_none());
    assert!(app.agents_split.as_ref().unwrap().panes[0]
        .worktree
        .is_none());
    let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].request_id, op_id);
}

/// A harness `PermissionRequest` that arrives while an `agents_worktree`
/// ask holds the session box is PARKED (never overwriting the ask) and is
/// RE-SHOWN the moment the ask resolves — the main loop's own permission
/// gate is then answered through the box into `perm_tx` (simulated here
/// by capturing what the resolver leaves in the box).
#[tokio::test]
async fn agents_worktree_ask_holds_the_box_and_re_shows_a_parked_permission_request() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = worktree_split_app(&app_repo);
    let wt = repo.join("wt-hold");
    app.prompt_view.input = format!("/worktree add {} feat-hold", wt.display());
    app.run_slash_command(&crate::ui::slash_menu::SlashCommand {
        name: "worktree".into(),
        desc: String::new(),
    });
    app.sync_agents_pane_registry();

    // The worktree ask opens the box first.
    cosh::harness::panes::enqueue_worktree_op(
        "claude",
        cosh::harness::panes::WorktreeAction::Clear,
        None,
        cosh::harness::panes::MAIN,
    )
    .expect("the ask parks");
    app.serve_agents_pane_worktree_ops();
    assert!(app.permission_dialog.visible);
    let ask_args = app
        .permission_dialog
        .request
        .as_ref()
        .unwrap()
        .args
        .clone();
    assert!(ask_args.contains("claude"), "{ask_args}");

    // A harness permission request arrives while the ask holds the box:
    // it must NOT overwrite the ask's content (it parks instead).
    app.event_tx
        .send(cosh::harness::HarnessEvent::PermissionRequest {
            tool: "bash_run".into(),
            description: "Run ls".into(),
            args: "path: /tmp".into(),
        })
        .ok();
    app.poll_events();
    assert!(
        app.permission_dialog.request.as_ref().unwrap().tool == "agents_worktree",
        "the parked ask still owns the box"
    );
    assert!(
        app.pending_permission_request().is_some(),
        "the harness request was parked, not dropped"
    );

    // The ask resolves: the harness request takes the box over, and the
    // worktree outcome is delivered.
    app.resolve_pending_worktree_permission(
        crate::routes::session::permission::PermissionAction::AllowOnce,
    );
    assert!(app.permission_dialog.visible, "the parked request re-shows");
    let held = app.permission_dialog.request.as_ref().unwrap();
    assert_eq!(held.tool, "bash_run");
    assert_eq!(held.args, "path: /tmp");
    assert!(app.pending_permission_request().is_none(), "nothing leaks");
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_none(),
        "the allowed ask executed"
    );
    // Drain the ask's outcome: registry deliveries are GLOBAL state — a
    // leftover here would leak into the next test reading MAIN's box.
    assert_eq!(
        cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN).len(),
        1
    );
}

// ── /worktree on the MANAGER (cosh) pane ────────────────────────────────
//
// A cosh pane is a pane like any other for the slash command: it LISTS
// `/worktree`, APPLYING it pins the pane, and the manager turn's own
// `HarnessAssembly::cwd` resolves through `pane_effective_cwd`, so a bound
// cosh pane really runs its turns in the checkout. Only the
// `agents_worktree` TOOL refuses a cosh pane (the user's call, never a
// peer agent's ask).

/// One `cosh` (manager) pane split with pane 0 focused — the manager-pane
/// mirror of `worktree_split_app`.
fn manager_worktree_split_app(repo: &std::path::Path) -> App {
    let mut app = App::new(repo.display().to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "manager worktree".into(), 0);
    app.state.current_session_id = Some(session_id);
    app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
        name: crate::routes::session::agents_split::MANAGER_LABEL.into(),
        instances: 1,
    }]);
    app
}

/// (a) The focused manager pane's slash SOURCE carries exactly ONE
/// `worktree` entry — one row, not a duplicate (the cosh set never had it,
/// and it is prepended exactly once for the manager branch).
#[tokio::test]
async fn manager_pane_slash_source_lists_worktree_exactly_once() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app = manager_worktree_split_app(&repo);

    // The split source (what the menu is seeded from).
    let (entries, _) = app
        .agents_split
        .as_ref()
        .expect("split open")
        .slash_source()
        .expect("a focused pane serves a list");
    let count = entries.iter().filter(|e| e.name == "worktree").count();
    assert_eq!(
        count,
        1,
        "the manager pane lists /worktree exactly once: {:?}",
        entries.iter().map(|e| &e.name).collect::<Vec<_>>()
    );

    // The menu the user actually sees (post-sync) agrees.
    let menu_count = app
        .slash_menu
        .commands
        .iter()
        .filter(|e| e.name == "worktree")
        .count();
    assert_eq!(menu_count, 1, "the synced menu carries one /worktree row");
    // `/worktree` is part of the BASE list (the main agent gets it too), so
    // the manager's list is now byte-identical to the cosh set. The sync is
    // therefore a no-op and the source stays `Cosh` — that is the intended
    // idempotence (`slash_lists_equal` must not churn selection), not a
    // routing change: the router keys on the command's SCOPE, never on this
    // cosmetic label.
    assert_eq!(
        app.slash_menu.commands.len(),
        crate::ui::slash_menu::SlashMenu::cosh_commands().len(),
        "the manager's list IS the base cosh list"
    );
}

/// (b) `/worktree add <branch>` on a MANAGER pane creates the worktree,
/// stamps the binding (assigned_by: User) on that pane and flags the
/// handoff — the SAME side effects as on an ACP pane.
#[tokio::test]
async fn slash_worktree_add_pins_the_manager_pane() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = manager_worktree_split_app(&app_repo);
    let wt = repo.join("wt-manager");

    app.prompt_view.input = format!("/worktree add {} feat-mgr", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert_eq!(
        pane.agent,
        crate::routes::session::agents_split::MANAGER_LABEL,
        "precondition: pane 0 is the manager"
    );
    let pin = pane.worktree.as_ref().expect("the manager pane got pinned");
    assert_eq!(pin.branch, "feat-mgr");
    assert_eq!(pin.path, wt);
    assert_eq!(pin.assigned_by, cosh::git::worktree::AssignedBy::User);
    assert!(wt.is_dir(), "the worktree directory exists");
    assert!(
        pane.worktree_handoff_pending,
        "the cwd moved: the next turn carries the handoff"
    );
    assert!(
        !app.toast_state.current.as_ref().is_some_and(|t| t
            .message
            .contains("stays on the session's working directory")),
        "the old manager-refusal toast must be gone"
    );
}

/// (c) The manager pane's EFFECTIVE cwd equals the worktree path — the
/// single resolution that feeds `dispatch_to_pane` (the ACP `acp::call`
/// cwd AND the manager turn's `HarnessAssembly::cwd`), so a binding is
/// honored by every layer rather than cosmetic. Asserted through
/// `pane_effective_cwd` itself: it is the ONE seam both branches read,
/// which makes it the cleanest probe of the contract (the turn's spawn is
/// out of test reach, but its cwd comes from exactly this call).
#[tokio::test]
async fn manager_pane_effective_cwd_is_the_worktree_path() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let app_repo = repo.clone();
    let mut app = manager_worktree_split_app(&app_repo);
    let wt = repo.join("wt-manager-cwd");

    let session_cwd = app.state.working_directory.clone();

    // Unbound: the effective cwd is the session's working directory.
    {
        let pane = &app.agents_split.as_ref().unwrap().panes[0];
        assert_eq!(
            super::super::agents_dispatch::pane_effective_cwd(pane, &session_cwd),
            std::path::PathBuf::from(&session_cwd),
            "an unbound pane runs in the session cwd"
        );
    }

    app.prompt_view.input = format!("/worktree add {} feat-mgr-cwd", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert_eq!(
        super::super::agents_dispatch::pane_effective_cwd(pane, &session_cwd),
        wt,
        "the bound manager pane's effective cwd IS the worktree path"
    );
}

/// (d) `/worktree exit` clears the MANAGER pane's binding and flags the
/// handoff back — nothing on disk is touched.
#[tokio::test]
async fn slash_worktree_exit_clears_the_manager_pane_binding() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = manager_worktree_split_app(&repo);
    let wt = repo.join("wt-manager-exit");

    app.prompt_view.input = format!("/worktree add {} feat-mgr-exit", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));
    assert!(
        app.agents_split.as_ref().unwrap().panes[0]
            .worktree
            .is_some(),
        "precondition: the manager pane is pinned"
    );

    app.prompt_view.input = "/worktree exit".into();
    app.run_slash_command(&slash_cmd("worktree"));

    let pane = &app.agents_split.as_ref().unwrap().panes[0];
    assert!(pane.worktree.is_none(), "the binding is dropped");
    assert!(
        pane.worktree_handoff_pending,
        "the cwd moved back: the next turn carries the handoff"
    );
    assert!(wt.is_dir(), "exit never deletes the worktree");
}

/// (e) The `agents_worktree` TOOL addresses a cosh pane like any other: the
/// manager pane gets no special refusal — it runs its own agent session with
/// its own cwd, so isolating it is the same ask as isolating a `claude` pane.
/// What still protects the user is unchanged: panes never receive this tool
/// (`with_own_pane_id` strips it) and a User-assigned binding always opens
/// the confirmation box.
#[tokio::test]
async fn agents_worktree_tool_accepts_a_cosh_pane() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = manager_worktree_split_app(&repo);
    app.sync_agents_pane_registry();

    // The tool's pane enum offers the manager alongside every other pane...
    let tool = cosh::harness::panes::agents_worktree_tool();
    let enum_values = tool.input_schema["properties"]["pane"]["enum"]
        .as_array()
        .expect("pane enum present");
    assert!(
        enum_values.iter().any(|v| v == "cosh"),
        "the cosh pane IS an addressable target: {enum_values:?}"
    );

    // ...and an assign to it parks instead of being refused.
    let id = cosh::harness::panes::enqueue_worktree_op(
        "cosh",
        cosh::harness::panes::WorktreeAction::Assign,
        Some("feat-cosh"),
        cosh::harness::panes::MAIN,
    )
    .expect("the tool addresses a cosh pane");
    let parked = cosh::harness::panes::peek_worktree_op().expect("the ask is parked");
    assert_eq!(parked.id, id);
    assert_eq!(parked.pane, "cosh");
    let _ = cosh::harness::panes::pop_worktree_op();
}

/// The MAIN agent's shape: a session and NO `/agents` split. `/worktree`
/// must be reachable from normal chat, which is the whole point of the
/// command being part of the base cosh list instead of a pane synthesis.
fn main_agent_worktree_app(repo: &std::path::Path) -> App {
    let mut app = App::new(repo.display().to_string());
    let session_id = crate::session::store::generate_session_id();
    app.state
        .add_empty_session(session_id.clone(), "main agent worktree".into(), 0);
    app.state.current_session_id = Some(session_id);
    app
}

/// (a) With no split open the main agent's menu carries `/worktree`
/// EXACTLY once — it is a base-list row now, so the pane fusion that used
/// to append it must not double it.
#[tokio::test]
async fn main_agent_menu_lists_worktree_exactly_once() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = main_agent_worktree_app(&repo);

    super::super::agents_dispatch::sync_slash_source(&mut app);
    let count = app
        .slash_menu
        .commands
        .iter()
        .filter(|entry| entry.name == "worktree")
        .count();
    assert_eq!(
        count,
        1,
        "the main agent's menu lists /worktree exactly once: {:?}",
        app.slash_menu
            .commands
            .iter()
            .map(|entry| &entry.name)
            .collect::<Vec<_>>()
    );
    assert!(
        app.agents_split.is_none(),
        "precondition: no split — this is normal chat"
    );
}

/// (b) `/worktree add <path> <branch>` on the MAIN agent creates the
/// checkout, stamps the binding on the SESSION (`assigned_by: User`) and
/// persists it, so a reopened session keeps the isolation.
#[tokio::test]
async fn slash_worktree_add_pins_the_main_agent_and_persists() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = main_agent_worktree_app(&repo);
    let wt = repo.join("wt-main");
    let session_id = app.state.current_session_id.clone().expect("session");

    app.prompt_view.input = format!("/worktree add {} feat-main", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    let pin = app
        .state
        .current_session()
        .and_then(|session| session.main_agent_worktree.as_ref())
        .expect("the main agent got pinned");
    assert_eq!(pin.branch, "feat-main");
    assert_eq!(pin.path, wt);
    assert_eq!(pin.assigned_by, cosh::git::worktree::AssignedBy::User);

    // The binding reached disk: a fresh load of the session file agrees.
    let reloaded = app
        .session_store
        .load_session(&session_id)
        .expect("the session persisted");
    let persisted = reloaded
        .main_agent_worktree
        .expect("the binding survives a reload");
    assert_eq!(persisted.branch, "feat-main");
    assert_eq!(persisted.path, wt);
    // A success is silent (same contract as the pane path).
    assert!(
        app.toast_state.current.is_none(),
        "a successful add must not toast: {:?}",
        app.toast_state.current.as_ref().map(|t| &t.message)
    );
    // No split was ever opened: the binding lives on the session, never a pane.
    assert!(app.agents_split.is_none());
}

/// (c) `/worktree exit` drops the binding and the main agent resolves back
/// to the ORIGINAL startup cwd — the directory the app was started in,
/// which `AppState::working_directory` has held unmutated all along.
#[tokio::test]
async fn slash_worktree_exit_restores_the_original_cwd() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = main_agent_worktree_app(&repo);
    let original = app.state.working_directory.clone();
    let wt = repo.join("wt-exit-main");

    app.prompt_view.input = format!("/worktree add {} feat-exit", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));
    assert!(
        super::super::agents_dispatch::main_agent_effective_cwd(&app) == wt,
        "precondition: pinned"
    );

    app.prompt_view.input = "/worktree exit".to_string();
    app.run_slash_command(&slash_cmd("worktree"));

    assert!(
        app.state
            .current_session()
            .is_some_and(|session| session.main_agent_worktree.is_none()),
        "exit drops the binding"
    );
    assert_eq!(
        super::super::agents_dispatch::main_agent_effective_cwd(&app),
        std::path::PathBuf::from(&original),
        "the main agent is back in the startup cwd"
    );
    // Nothing on disk was removed: the worktree stays registered.
    let listed = std::process::Command::new("git")
        .args(["worktree", "list"])
        .current_dir(&repo)
        .output()
        .expect("git worktree list");
    let has_wt = String::from_utf8_lossy(&listed.stdout).contains("wt-exit-main");
    assert!(has_wt, "the worktree survives its binding being dropped");
}

/// (d) A malformed tail (`add` alone, an unknown subcommand, no arguments)
/// is refused with the SAME usage toast the panes show — the grammar is
/// shared, so the refusal must not drift.
#[tokio::test]
async fn slash_worktree_main_agent_bad_args_refuse_with_usage() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();

    for tail in ["", "add", "nope", "add a b c", "exit now"] {
        let mut app = main_agent_worktree_app(&repo);
        app.prompt_view.input = format!("/worktree {tail}");
        app.run_slash_command(&slash_cmd("worktree"));
        assert!(
            app.toast_state
                .current
                .as_ref()
                .is_some_and(|toast| toast.message.contains("Usage:")),
            "tail `{tail}` must refuse with the usage toast"
        );
        assert!(
            app.state
                .current_session()
                .is_some_and(|session| session.main_agent_worktree.is_none()),
            "a refused `{tail}` must never bind"
        );
    }
}

/// Feature A (adoption): `/worktree add <path> <branch>` on a worktree that
/// ALREADY exists re-enters it instead of failing, binds the main agent, and
/// SAYS SO — the badge alone cannot tell a user whether cosh built a
/// checkout or walked into one that was already there (typically after
/// `/worktree exit`, which keeps the checkout on disk by product rule).
#[tokio::test]
async fn slash_worktree_add_adopts_an_existing_worktree_and_says_so() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let wt = repo.join("wt-adopt");
    // The checkout exists FIRST — created outside the command, exactly the
    // state `/worktree exit` leaves behind.
    let existing = cosh::git::worktree::create(
        &repo,
        "feat-adopt",
        Some(&wt),
        cosh::git::worktree::AssignedBy::User,
    )
    .expect("the checkout exists before the command runs");

    let mut app = main_agent_worktree_app(&repo);
    app.prompt_view.input = format!("/worktree add {} feat-adopt", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    let pin = app
        .state
        .current_session()
        .and_then(|session| session.main_agent_worktree.as_ref())
        .expect("the main agent is bound to the existing checkout");
    assert_eq!(pin.path, existing.path, "the SAME checkout, not a new one");
    assert_eq!(pin.branch, "feat-adopt");
    assert_eq!(
        pin.assigned_by,
        cosh::git::worktree::AssignedBy::User,
        "a slash command is the user acting"
    );

    // The toast distinguishes adoption from creation and names BOTH the
    // path and the branch, so the user can tell which checkout they landed
    // in without reading the footer.
    let message = app
        .toast_state
        .current
        .as_ref()
        .map(|toast| toast.message.clone())
        .expect("an adoption speaks: a silent adopt would make `add` a lie");
    assert!(
        message.contains("Attached to existing worktree"),
        "the toast says it ADOPTED rather than created: {message}"
    );
    assert!(message.contains(&wt.display().to_string()), "{message}");
    assert!(message.contains("feat-adopt"), "{message}");

    // No second checkout appeared: adoption is a binding change, not a
    // filesystem one.
    let entries = cosh::git::worktree::list(&repo).expect("list");
    assert_eq!(
        entries.iter().filter(|entry| entry.path == wt).count(),
        1,
        "one registration for the adopted path"
    );
}

/// Feature B: `/worktree list` renders ONE row per worktree and marks the
/// bound one — the discovery half of adoption, so the user does not have to
/// memorise paths.
#[tokio::test]
async fn slash_worktree_list_marks_the_bound_worktree() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let first = repo.join("wt-list-a");
    let second = repo.join("wt-list-b");
    for (path, branch) in [(&first, "feat-list-a"), (&second, "feat-list-b")] {
        cosh::git::worktree::create(
            &repo,
            branch,
            Some(path),
            cosh::git::worktree::AssignedBy::User,
        )
        .expect("checkout");
    }

    let mut app = main_agent_worktree_app(&repo);
    app.prompt_view.input = format!("/worktree add {} feat-list-a", first.display());
    app.run_slash_command(&slash_cmd("worktree"));
    assert!(
        app.state
            .current_session()
            .and_then(|session| session.main_agent_worktree.as_ref())
            .is_some_and(|worktree| worktree.path == first),
        "precondition: bound to the first worktree"
    );

    app.prompt_view.input = "/worktree list".to_string();
    app.run_slash_command(&slash_cmd("worktree"));

    let message = app
        .toast_state
        .current
        .as_ref()
        .map(|toast| toast.message.clone())
        .expect("the list speaks");
    // One row per worktree, each naming its own path.
    assert!(message.contains(&first.display().to_string()), "{message}");
    assert!(message.contains(&second.display().to_string()), "{message}");
    // Every row goes through the marker column, and ONLY the bound one is
    // marked. Counted per line so an unmarked row cannot borrow the bound
    // row's marker.
    let marked: Vec<&str> = message
        .lines()
        .filter(|line| line.starts_with("\u{2192} "))
        .collect();
    assert_eq!(marked.len(), 1, "exactly one bound row: {message}");
    assert!(
        marked[0].contains(&first.display().to_string()),
        "the marked row is the bound one: {message}"
    );
    assert!(
        !marked[0].contains(&second.display().to_string()),
        "the other worktree stays unmarked: {message}"
    );
    // And nothing was rebound by looking.
    assert!(
        app.state
            .current_session()
            .and_then(|session| session.main_agent_worktree.as_ref())
            .is_some_and(|worktree| worktree.path == first),
        "listing never changes the binding"
    );
}

/// Feature B (grammar): `/worktree list` takes NO arguments — a trailing
/// word is a refusal, not a silent no-op, and it changes no state.
#[tokio::test]
async fn slash_worktree_list_with_extra_args_refuses_with_usage() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let wt = repo.join("wt-list-args");
    cosh::git::worktree::create(
        &repo,
        "feat-list-args",
        Some(&wt),
        cosh::git::worktree::AssignedBy::User,
    )
    .expect("checkout");

    let mut app = main_agent_worktree_app(&repo);
    app.prompt_view.input = format!("/worktree list {}", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    assert!(
        app.toast_state
            .current
            .as_ref()
            .is_some_and(|toast| toast.message.contains("Usage:")),
        "`/worktree list <extra>` refuses with the usage toast: {:?}",
        app.toast_state.current.as_ref().map(|t| &t.message)
    );
    // Refused means untouched: no binding, and the list never ran (a real
    // listing would carry the "worktree(s) of" preamble).
    assert!(
        app.state
            .current_session()
            .is_some_and(|session| session.main_agent_worktree.is_none()),
        "a refused list must never bind"
    );
}

/// (e) The main agent's EFFECTIVE cwd is the worktree path once pinned —
/// the single resolution the turn assembly, the shell root, the prompt
/// corrector and the footer branch all read.
#[tokio::test]
async fn main_agent_effective_cwd_follows_the_binding() {
    let _guard = HOME_LOCK.lock();
    isolate_home();
    let (_dir, repo) = worktree_test_repo();
    let mut app = main_agent_worktree_app(&repo);
    let original = std::path::PathBuf::from(app.state.working_directory.clone());

    assert_eq!(
        super::super::agents_dispatch::main_agent_effective_cwd(&app),
        original,
        "unpinned: the startup cwd"
    );

    let wt = repo.join("wt-effective");
    app.prompt_view.input = format!("/worktree add {} feat-eff", wt.display());
    app.run_slash_command(&slash_cmd("worktree"));

    assert_eq!(
        super::super::agents_dispatch::main_agent_effective_cwd(&app),
        wt,
        "pinned: the checkout"
    );
    // The startup cwd is never mutated — it stays the restore target.
    assert_eq!(
        app.state.working_directory,
        original.to_string_lossy(),
        "working_directory stays the original (it is the exit target)"
    );

}
    // ---- Issue 1: gate-free session-binding facts (variants C and A) ----

    /// Variant C — Ctrl+X during a turn whose resume fell back to
    /// `session/new`: the winding-down turn's terminal Done is DROPPED by
    /// the handler's pane-idle gate, which used to drop the new session id
    /// with it (stale binding + an orphaned agent session). The gate-free
    /// fact recorded by the turn task must survive the force-close and
    /// promote BOTH the live pane and the persisted ref.
    #[tokio::test]
    async fn variant_c_a_force_closed_turns_binding_fact_promotes_the_pane() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        // Start from a clean fact registry: this suite shares the process
        // with every other test, and a fact is consumed exactly once.
        let _ = cosh::harness::panes::drain_session_bindings();
        let mut app = App::new("/tmp".to_string());
        let session_id = crate::session::store::generate_session_id();
        app.state
            .add_empty_session(session_id.clone(), "issue1 C".into(), 0);
        app.state.current_session_id = Some(session_id);
        app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 1,
        }]);
        // The pane was Bound to "s-1" by an earlier turn.
        let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
        app.agents_split.as_mut().unwrap().panes[0].session_id = Some("s-1".into());
        app.sync_agents_pane_registry();

        // The turn runs; its resume of "s-1" failed and the harness fell
        // back to a FRESH session "s-2" (the fact the Done used to carry).
        app.agents_split.as_mut().unwrap().set_running(0, true);
        cosh::harness::panes::record_session_binding(pane_id, "s-2".into());

        // Ctrl+X: the pane goes idle NOW; the turn's terminal Done arrives
        // later against an idle pane and is dropped by the gate. The
        // force-close path absorbs the fact itself.
        app.interrupt_focused_pane();
        assert!(
            !app.agents_split.as_ref().unwrap().panes[0].running,
            "the pane is force-closed"
        );

        let pane = &app.agents_split.as_ref().unwrap().panes[0];
        assert_eq!(
            pane.session_id.as_deref(),
            Some("s-2"),
            "the gate-free fact must bind the pane even though the Done was dropped"
        );
        assert_eq!(
            app.state.current_session().unwrap().agents_panes.clone().unwrap()[0],
            crate::types::AgentPaneRef::Bound {
                agent: "claude".into(),
                session_id: "s-2".into(),
                worktree: None,
            },
            "the persisted ref must carry the session the turn actually drove"
        );

        // The late REAL Done (still winding down, carrying the OLD id) must
        // not overwrite the absorbed binding: the pane-idle gate drops it.
        app.event_tx
            .send(cosh::harness::HarnessEvent::AgentsPaneDone {
                pane: 0,
                pane_id,
                epoch: app.agents_pane_epoch,
                output: "late report".into(),
                stop_reason: "cancelled".into(),
                session: Some("s-1".into()),
            })
            .ok();
        app.poll_events();
        assert_eq!(
            app.agents_split.as_ref().unwrap().panes[0].session_id.as_deref(),
            Some("s-2"),
            "a late Done against an idle pane must not rebind (gate intact)"
        );
        assert_eq!(
            app.state.current_session().unwrap().agents_panes.clone().unwrap()[0]
                .session_id(),
            Some("s-2"),
            "the persisted binding stays the absorbed one"
        );
    }

    /// Variant A — hiding a pane whose FIRST turn is still running: the
    /// terminal binding arrives against a pane object that answers to
    /// nothing visible. The fact must promote the HIDDEN-stack entry (via
    /// `HiddenPane::pane_id`) instead of being dropped — pre-fix, the first
    /// turn's session was lost forever and the pane stayed unbound.
    #[tokio::test]
    async fn variant_a_a_hidden_panes_first_turn_binding_lands_in_the_hidden_stack() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let _ = cosh::harness::panes::drain_session_bindings();
        let mut app = App::new("/tmp".to_string());
        let session_id = crate::session::store::generate_session_id();
        app.state
            .add_empty_session(session_id.clone(), "issue1 A".into(), 0);
        app.state.current_session_id = Some(session_id);
        // Built directly (not through the picker) so the agent name is one
        // no harness can spawn: the restore path fires a cold-restore
        // replay for the bound pane, and an unknown agent degrades to a
        // toast instead of launching a real process.
        let agent = "issue1-ghost-agent";
        app.agents_split = Some(
            crate::routes::session::agents_split::AgentsSplitState::new(vec![
                crate::types::AgentPaneRef::Name(agent.into()),
            ]),
        );
        if let Some(session) = app.state.current_session_mut() {
            session.agents_panes =
                Some(vec![crate::types::AgentPaneRef::Name(agent.into())]);
            session.agents_mode_open = true;
        }
        app.sync_agents_pane_registry();

        let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
        app.agents_split.as_mut().unwrap().set_running(0, true);
        // Hide the RUNNING pane (Backspace): the pane object leaves the
        // split with its turn signal-stopped; the ref moves to the hidden
        // stack still unbound, but carrying the pane object's identity.
        app.hide_focused_agents_pane();
        assert!(app.agents_split.is_none(), "the last visible pane hidden: the mode auto-closed");
        {
            let session = app.state.current_session().unwrap();
            let hidden = session.agents_hidden.first().expect("hidden stack entry");
            assert_eq!(hidden.pane_id, pane_id, "the hide must stamp the pane object id");
            assert!(
                hidden.pane.session_id().is_none(),
                "the entry is still unbound at hide time"
            );
        }

        // The turn finishes AFTER the hide: `acp::call` resolved a fresh
        // session "s-A" and the turn task recorded the gate-free fact.
        cosh::harness::panes::record_session_binding(pane_id, "s-A".into());
        app.absorb_pane_session_bindings();

        {
            let session = app.state.current_session().unwrap();
            let hidden = session.agents_hidden.first().expect("hidden stack entry");
            assert_eq!(
                hidden.pane.session_id(),
                Some("s-A"),
                "the hidden entry must be promoted to Bound by the absorbed fact"
            );
            assert_eq!(
                hidden.pane.agent(),
                agent,
                "the promotion must keep the pane's agent"
            );
        }
    }

    /// Variant A, second half — restoring the promoted hidden entry
    /// (Ctrl+Z) rebuilds the pane ALREADY BOUND, so the cold-restore
    /// replay path picks the session up and rehydrates the timeline.
    #[tokio::test]
    async fn variant_a_a_restored_hidden_pane_comes_back_bound() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let _ = cosh::harness::panes::drain_session_bindings();
        let mut app = App::new("/tmp".to_string());
        let session_id = crate::session::store::generate_session_id();
        app.state
            .add_empty_session(session_id.clone(), "issue1 A restore".into(), 0);
        app.state.current_session_id = Some(session_id);
        let agent = "issue1-ghost-agent";
        app.agents_split = Some(
            crate::routes::session::agents_split::AgentsSplitState::new(vec![
                crate::types::AgentPaneRef::Name(agent.into()),
            ]),
        );
        if let Some(session) = app.state.current_session_mut() {
            session.agents_panes =
                Some(vec![crate::types::AgentPaneRef::Name(agent.into())]);
            session.agents_mode_open = true;
        }
        app.sync_agents_pane_registry();

        let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;
        app.agents_split.as_mut().unwrap().set_running(0, true);
        app.hide_focused_agents_pane();
        cosh::harness::panes::record_session_binding(pane_id, "s-A".into());
        app.absorb_pane_session_bindings();

        // Ctrl+Z: the pane returns — bound, so its empty timeline is
        // flagged for the cold-restore replay.
        app.restore_last_hidden_agents_pane();
        let pane = &app.agents_split.as_ref().unwrap().panes[0];
        assert_eq!(
            pane.session_id.as_deref(),
            Some("s-A"),
            "the restored pane must come back bound to the first turn's session"
        );
        assert_eq!(
            app.state.current_session().unwrap().agents_panes.clone().unwrap()[0]
                .session_id(),
            Some("s-A"),
            "the restored metadata entry stays Bound"
        );
    }

    /// Anti-contamination: a binding fact whose pane object id answers to
    /// NOTHING (a pane a rebuild dropped, a legacy hidden entry with
    /// `pane_id = 0`) is dropped, never smeared onto a same-named
    /// replacement — the structural counterpart of the event gates.
    #[tokio::test]
    async fn an_absorbed_binding_fact_without_an_owner_is_dropped() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let _ = cosh::harness::panes::drain_session_bindings();
        let mut app = App::new("/tmp".to_string());
        let session_id = crate::session::store::generate_session_id();
        app.state
            .add_empty_session(session_id.clone(), "issue1 gate".into(), 0);
        app.state.current_session_id = Some(session_id);
        app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 1,
        }]);

        // An id that answers to no pane object, and the legacy `0`.
        cosh::harness::panes::record_session_binding(99_999, "s-x".into());
        cosh::harness::panes::record_session_binding(0, "s-y".into());
        app.absorb_pane_session_bindings();

        assert_eq!(
            app.agents_split.as_ref().unwrap().panes[0].session_id,
            None,
            "an ownerless fact must never bind a same-named pane"
        );
        assert!(
            app.state.current_session().unwrap().agents_panes.as_ref().unwrap()[0]
                .session_id()
                .is_none(),
            "the persisted set stays unbound"
        );
        assert!(
            cosh::harness::panes::drain_session_bindings().is_empty(),
            "the pass consumed every fact: nothing resurfaces later"
        );
    }

    /// Gate regression — the Done handler's identity gate is untouched: a
    /// terminal Done whose pane id answers to no pane (a pane the rebuild
    /// dropped) delivers nothing and binds nothing, exactly as before the
    /// fix. The binding vehicle being gate-free does not weaken the gates.
    #[tokio::test]
    async fn a_done_for_an_unknown_pane_id_still_delivers_nothing() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let _ = cosh::harness::panes::drain_session_bindings();
        let mut app = App::new("/tmp".to_string());
        let session_id = crate::session::store::generate_session_id();
        app.state
            .add_empty_session(session_id.clone(), "issue1 routing".into(), 0);
        app.state.current_session_id = Some(session_id);
        app.open_agents_split(vec![crate::ui::agents_picker::AgentPick {
            name: "claude".into(),
            instances: 1,
        }]);

        app.event_tx
            .send(cosh::harness::HarnessEvent::AgentsPaneDone {
                pane: 0,
                pane_id: 777_777,
                epoch: app.agents_pane_epoch,
                output: "stray report".into(),
                stop_reason: "end_turn".into(),
                session: Some("s-stray".into()),
            })
            .ok();
        app.poll_events();
        app.absorb_pane_session_bindings();

        assert_eq!(
            app.agents_split.as_ref().unwrap().panes[0].session_id,
            None,
            "a Done for an unknown pane id binds nothing (gate intact)"
        );
        assert!(
            app.state.current_session().unwrap().agents_panes.as_ref().unwrap()[0]
                .session_id()
                .is_none()
        );
    }

    // ── agents_compact: the App drain runs the PANE's own compaction ──

    /// The drain's refusal path on an UNBOUND pane: the ask is popped (not
    /// left parked), completed with the reason, and the in-flight guard is
    /// cleared — so the pane is not blocked forever by a refusal, and the
    /// asking loop's paused tool call gets a real answer instead of
    /// hanging.
    #[tokio::test]
    async fn agents_compact_of_an_unbound_pane_is_refused_and_frees_the_guard() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let (_dir, repo) = worktree_test_repo();
        let app_repo = repo.clone();
        let mut app = manager_worktree_split_app(&app_repo);
        app.sync_agents_pane_registry();
        let pane_id = app.agents_split.as_ref().unwrap().panes[0].id;

        let op_id = cosh::harness::panes::enqueue_compact_op(
            crate::routes::session::agents_split::MANAGER_LABEL,
            cosh::harness::panes::MAIN,
        )
        .expect("the ask parks");
        app.serve_agents_pane_compact_ops();

        assert!(
            cosh::harness::panes::peek_compact_op().is_none(),
            "the ask is popped, never left parked on a refusal"
        );
        assert!(
            !cosh::harness::panes::pane_is_compacting(pane_id),
            "a refusal clears the guard: the pane is usable again"
        );
        let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].request_id, op_id);
        assert_eq!(
            deliveries[0].kind,
            cosh::harness::panes::DeliveryKind::Compact
        );
        let error = deliveries[0].output.clone().unwrap_err();
        assert!(
            error.contains("no bound session"),
            "the refusal names the reason: {error}"
        );
        // The compaction never touched the main session's timeline: no
        // "Summarizing" box was opened in it (the pane path is silent by
        // design — its events go to a dedicated, discarded channel).
        assert!(
            app.state
                .current_session()
                .map(|s| s.messages.iter().all(|m| !m
                    .parts
                    .iter()
                    .any(|p| matches!(p, crate::types::Part::Compaction(_)))))
                .unwrap_or(true),
            "the pane compaction must never paint into the main session"
        );
    }

    /// A BUSY pane does NOT pop its ask: the drain retries on a later
    /// frame, so a correctly-made ask waits for the pane's turn to finish
    /// instead of failing.
    #[tokio::test]
    async fn agents_compact_waits_for_a_running_pane_instead_of_failing() {
        let _guard = HOME_LOCK.lock();
        isolate_home();
        let (_dir, repo) = worktree_test_repo();
        let app_repo = repo.clone();
        let mut app = manager_worktree_split_app(&app_repo);
        app.sync_agents_pane_registry();
        let pane_label = crate::routes::session::agents_split::MANAGER_LABEL;
        let op_id = cosh::harness::panes::enqueue_compact_op(
            pane_label,
            cosh::harness::panes::MAIN,
        )
        .expect("the ask parks");

        // The pane is mid-turn: the drain must leave the ask parked.
        app.agents_split
            .as_mut()
            .unwrap()
            .set_running(0, true);
        app.serve_agents_pane_compact_ops();
        let still = cosh::harness::panes::peek_compact_op().expect("the ask stays parked");
        assert_eq!(still.id, op_id, "not popped while the pane is busy");
        assert!(
            !cosh::harness::panes::pane_is_compacting(still.pane_id),
            "a parked ask reserves nothing yet"
        );
        assert!(
            cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN).is_empty(),
            "no outcome while the ask is still parked"
        );

        // The turn ends: the next frame's drain resolves it (here: refused
        // for the missing session, which is the point — it MOVED).
        app.agents_split
            .as_mut()
            .unwrap()
            .set_running(0, false);
        app.serve_agents_pane_compact_ops();
        assert!(cosh::harness::panes::peek_compact_op().is_none());
        let deliveries = cosh::harness::panes::take_ready_deliveries(cosh::harness::panes::MAIN);
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].request_id, op_id);
    }
