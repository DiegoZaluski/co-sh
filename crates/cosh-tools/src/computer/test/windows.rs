//! Tests for `computer_windows`: validation table + mock-provider listing
//! and window management.
//!
//! The mock provider carries the fixture application ("TestApp") with one
//! window ("Main Window", bounds 100,50 800x600, active + focused) and
//! MODELS window verbs in place: `minimize` flips the state bits,
//! `move_to`/`resize_to` write the bounds, `close` removes the whole
//! subtree — so the post-observation re-list assertions verify the real
//! read-back path, not a fixed fixture.
#[cfg(test)]
mod windows_tests {
    use std::sync::Arc;

    use cosh_sdk::extract_action::{
        ExtractAction, NativeToolCall, StreamAction, ToolSchema,
    };
    use xa11y::mock::build_provider;
    use xa11y::{App, Provider};

    use crate::computer::Computer;
    use crate::computer::types::{
        ComputerWindows, WindowAction, WindowBounds, WindowInfo, WindowManageOutput,
        WindowsResult,
    };
    use crate::computer::windows::{list, manage, validate};

    fn input() -> ComputerWindows {
        ComputerWindows {
            name: Some("TestApp".to_string()),
            pid: None,
            selector: None,
            nth: None,
            action: None,
            x: None,
            y: None,
            width: None,
            height: None,
            timeout_ms: Some(100),
        }
    }

    fn mock_app() -> (Arc<dyn Provider>, App) {
        let provider: Arc<dyn Provider> = build_provider();
        let root = provider.list_apps().expect("mock exposes an app root")[0]
            .clone();
        let app = App::from_data(Arc::clone(&provider), root.clone());
        (provider, app)
    }

    fn expect_listed(
        result: Result<WindowsResult, String>,
        count: Option<usize>,
    ) -> Vec<WindowInfo> {
        let result = result.expect("windows must succeed");
        match result {
            WindowsResult::List(list) => {
                if let Some(count) = count {
                    assert_eq!(list.count, count);
                }
                assert!(!list.app.is_empty(), "app name must echo the source app");
                list.windows
            }
            other => panic!("expected List result, got {other:?}"),
        }
    }

    fn expect_managed(
        result: Result<WindowsResult, String>,
    ) -> Option<WindowInfo> {
        let result = result.expect("windows must succeed");
        match result {
            WindowsResult::Manage(manage) => manage.window,
            other => panic!("expected Manage result, got {other:?}"),
        }
    }

    // ── validation ──────────────────────────────────────────────────

    #[test]
    fn validation_accepts_list_and_payload_pairs() {
        assert!(validate(&input()).is_ok(), "plain list input is valid");

        let move_in = ComputerWindows {
            action: Some(WindowAction::Move),
            x: Some(10),
            y: Some(20),
            ..input()
        };
        assert!(validate(&move_in).is_ok(), "move with x/y is valid");

        let resize_in = ComputerWindows {
            action: Some(WindowAction::Resize),
            width: Some(400),
            height: Some(300),
            ..input()
        };
        assert!(validate(&resize_in).is_ok(), "resize with width/height is valid");
    }

    #[test]
    fn validation_rejects_ambiguous_app_scope() {
        let both = ComputerWindows {
            pid: Some(42),
            ..input()
        };
        assert!(validate(&both).is_err(), "name + pid must be rejected");

        let neither = ComputerWindows {
            name: None,
            pid: None,
            ..input()
        };
        assert!(validate(&neither).is_err(), "no app scope must be rejected");
    }

    #[test]
    fn validation_rejects_mispaired_payloads() {
        let move_without_payload = ComputerWindows {
            action: Some(WindowAction::Move),
            ..input()
        };
        assert!(validate(&move_without_payload).is_err());

        let move_with_size = ComputerWindows {
            action: Some(WindowAction::Move),
            x: Some(10),
            y: Some(20),
            width: Some(400),
            ..input()
        };
        assert!(validate(&move_with_size).is_err());

        let resize_without_payload = ComputerWindows {
            action: Some(WindowAction::Resize),
            ..input()
        };
        assert!(validate(&resize_without_payload).is_err());

        let resize_with_origin = ComputerWindows {
            action: Some(WindowAction::Resize),
            width: Some(400),
            height: Some(300),
            x: Some(10),
            ..input()
        };
        assert!(validate(&resize_with_origin).is_err());
    }

    /// A LONE payload member must never dispatch: `move_to(x, 0)` would
    /// silently slam the window to the top edge. Both pairing rules reject
    /// each half of a payload pair.
    #[test]
    fn validation_rejects_lone_axis_payloads() {
        let move_x_only = ComputerWindows {
            action: Some(WindowAction::Move),
            x: Some(10),
            ..input()
        };
        assert!(
            validate(&move_x_only).is_err(),
            "`move` with `x` but no `y` must be rejected"
        );

        let move_y_only = ComputerWindows {
            action: Some(WindowAction::Move),
            y: Some(20),
            ..input()
        };
        assert!(
            validate(&move_y_only).is_err(),
            "`move` with `y` but no `x` must be rejected"
        );

        let resize_width_only = ComputerWindows {
            action: Some(WindowAction::Resize),
            width: Some(400),
            ..input()
        };
        assert!(
            validate(&resize_width_only).is_err(),
            "`resize` with `width` but no `height` must be rejected"
        );

        let resize_height_only = ComputerWindows {
            action: Some(WindowAction::Resize),
            height: Some(300),
            ..input()
        };
        assert!(
            validate(&resize_height_only).is_err(),
            "`resize` with `height` but no `width` must be rejected"
        );
    }

    /// The manage arm serializes flat TOO (same untagged contract as the
    /// list arm) — a wrapped `{"Manage": …}` envelope would double-package
    /// the JSON the model sees.
    #[test]
    fn serialize_roundtrip_untagged_manage() {
        let manage = WindowsResult::Manage(WindowManageOutput {
            action: "minimize".to_string(),
            window: Some(WindowInfo {
                role: "window".to_string(),
                name: Some("Main Window".to_string()),
                bounds: Some(WindowBounds {
                    x: 100,
                    y: 50,
                    width: 800,
                    height: 600,
                }),
                active: Some(false),
                minimized: Some(true),
                maximized: Some(false),
                fullscreen: Some(false),
            }),
        });
        let json = serde_json::to_string(&manage).expect("serializable");
        assert!(json.contains("\"action\":\"minimize\""), "flat: {json}");
        assert!(
            !json.contains("\"Manage\"") && !json.contains("\"List\""),
            "untagged: {json}"
        );
    }

    #[test]
    fn validation_rejects_payload_without_action() {
        let stray = ComputerWindows {
            x: Some(10),
            y: Some(20),
            ..input()
        };
        assert!(validate(&stray).is_err());
        let stray_size = ComputerWindows {
            width: Some(400),
            height: Some(300),
            ..input()
        };
        assert!(validate(&stray_size).is_err());
    }

    #[test]
    fn validation_rejects_zero_nth_and_nth_without_selector() {
        let zero = ComputerWindows {
            nth: Some(0),
            ..input()
        };
        assert!(validate(&zero).is_err());

        let orphan = ComputerWindows {
            nth: Some(1),
            ..input()
        };
        assert!(validate(&orphan).is_err());
    }

    // ── listing (mock provider) ─────────────────────────────────────

    #[test]
    fn list_reports_every_window_with_geometry_and_state() {
        let (_provider, app) = mock_app();
        let windows = expect_listed(list(&app, &input()), Some(1));
        let only = &windows[0];
        assert_eq!(only.role, "window");
        assert_eq!(only.name.as_deref(), Some("Main Window"));
        // Fixture geometry maps straight through.
        let bounds = only.bounds.clone().expect("the fixture window has bounds");
        assert_eq!((bounds.x, bounds.y), (100, 50));
        assert_eq!((bounds.width, bounds.height), (800, 600));
        // The fixture window is the mock's foreground window.
        assert_eq!(only.active, Some(true));
    }

    #[test]
    fn selector_narrows_the_list_to_one_entry() {
        let filtered = ComputerWindows {
            selector: Some("window[name='Main Window']".to_string()),
            ..input()
        };
        let (_provider, app) = mock_app();
        let windows = expect_listed(list(&app, &filtered), Some(1));
        assert_eq!(windows[0].name.as_deref(), Some("Main Window"));
    }

    // ── management (mock provider) ──────────────────────────────────

    #[test]
    fn minimize_changes_state_and_unminimize_restores_it() {
        let (_provider, app) = mock_app();

        let minimized_in = ComputerWindows {
            action: Some(WindowAction::Minimize),
            ..input()
        };
        let after_minimize = expect_managed(manage(&app, &minimized_in, WindowAction::Minimize))
            .expect("minimize re-observes the live window");
        assert_eq!(after_minimize.minimized, Some(true), "post-observation must read the MUTATED state");
        assert_eq!(after_minimize.maximized, Some(false));

        let restored_in = ComputerWindows {
            action: Some(WindowAction::Restore),
            ..input()
        };
        let restored = expect_managed(manage(&app, &restored_in, WindowAction::Restore))
            .expect("restore re-observes the live window");
        assert_eq!(restored.minimized, Some(false));
        assert_eq!(restored.maximized, Some(false));
    }

    #[test]
    fn move_rewrites_bounds_post_observation() {
        let move_in = ComputerWindows {
            action: Some(WindowAction::Move),
            x: Some(300),
            y: Some(220),
            ..input()
        };
        let (_provider, app) = mock_app();
        let moved = expect_managed(manage(&app, &move_in, WindowAction::Move))
            .expect("move re-observes the live window");
        let bounds = moved.bounds.expect("the moved window keeps bounds");
        assert_eq!((bounds.x, bounds.y), (300, 220), "the re-list must reflect the move");
    }

    #[test]
    fn resize_rewrites_bounds_post_observation() {
        let resize_in = ComputerWindows {
            action: Some(WindowAction::Resize),
            width: Some(640),
            height: Some(480),
            ..input()
        };
        let (_provider, app) = mock_app();
        let resized = expect_managed(manage(&app, &resize_in, WindowAction::Resize))
            .expect("resize re-observes the live window");
        let bounds = resized.bounds.expect("the resized window keeps bounds");
        assert_eq!((bounds.width, bounds.height), (640, 480));
    }

    #[test]
    fn close_reports_no_post_state_and_removes_the_window() {
        let close_in = ComputerWindows {
            action: Some(WindowAction::Close),
            selector: Some("window[name='Main Window']".to_string()),
            ..input()
        };
        let (_provider, app) = mock_app();
        let result = manage(&app, &close_in, WindowAction::Close).expect("close must succeed");
        assert!(
            contains_close_with_no_window(&result),
            "close must report Manage output with window: None (the target is gone, \
             so there is no post-state to misreport)"
        );
        // And the mock tree really dropped it: a re-list finds nothing.
        let gone = list(&app, &input()).expect("list after close must succeed");
        match gone {
            WindowsResult::List(output) => {
                assert_eq!(output.count, 0, "a closed window leaves the tree")
            }
            other => panic!("expected List result, got {other:?}"),
        }
    }

    /// Unwrap the untagged enum for the close contract assertion.
    fn contains_close_with_no_window(result: &WindowsResult) -> bool {
        match result {
            WindowsResult::Manage(m) => m.window.is_none(),
            _ => false,
        }
    }

    #[test]
    fn action_on_windowless_app_errors() {
        // Close the app's only window first; a subsequent manage call
        // (no selector) has nothing to aim at.
        let close_in = ComputerWindows {
            action: Some(WindowAction::Close),
            ..input()
        };
        let (_provider, app) = mock_app();
        let _ = manage(&app, &close_in, WindowAction::Close).expect("close must succeed");

        let next = ComputerWindows {
            action: Some(WindowAction::Minimize),
            ..input()
        };
        assert!(
            manage(&app, &next, WindowAction::Minimize).is_err(),
            "no window left to act on"
        );
    }

    #[test]
    fn serialize_roundtrip_untagged_list() {
        let (_provider, app) = mock_app();
        let result = list(&app, &input()).expect("list must succeed");
        let json = serde_json::to_string(&result).expect("serializable");
        assert!(json.contains("\"app\":\"TestApp\""), "list serializes flat: {json}");
        // Untagged enum: no `windows`/`manage` discriminator wrapper key.
        assert!(!json.contains("\"List\"") && !json.contains("\"Manage\""), "untagged: {json}");
    }

    /// The model sees the HAND-WRITTEN schema in `description_windows` (the
    /// harness never derives from the Rust structs), and models deliver
    /// numerics as strings ("pid": "4242"). The extractor must repair such
    /// scalars against the hand-written schema so the repaired arguments
    /// deserialize into the real tool input.
    #[test]
    fn stringified_windows_numerics_repair_and_deserialize() {
        let mut ex = ExtractAction::new();
        ex.add_tool(ToolSchema {
            name: "computer_windows".into(),
            input_schema: Computer::new().description_windows["inputSchema"]
                .clone(),
            example_args: None,
        });
        let call = NativeToolCall {
            id: "call_w1".into(),
            name: "computer_windows".into(),
            arguments: r#"{"pid": "4242", "action": "move", "x": "-40", "y": "120"}"#.into(),
            thought_signature: String::new(),
        };
        let repaired = match ex.register_native_call(&call) {
            StreamAction::ToolCall(tc) => tc,
            other => panic!("stringified numerics must repair, got {other:?}"),
        };
        assert_eq!(ex.take_tool_failures(), 0, "repair, not rejection");

        let args = &repaired.arguments;
        assert!(args["pid"].is_i64() && args["pid"] == 4242, "{args:?}");
        assert!(args["x"].is_i64() && args["x"] == -40, "{args:?}");
        assert!(args["y"].is_i64() && args["y"] == 120, "{args:?}");

        // End-to-end: the repaired JSON lands in the REAL deserializer.
        let input: ComputerWindows =
            serde_json::from_value(args.clone()).expect("must deserialize");
        assert_eq!(input.pid, Some(4242));
        assert_eq!(input.x, Some(-40));
        assert_eq!(input.y, Some(120));
        assert_eq!(input.action, Some(WindowAction::Move));
    }

    /// Same repair, `resize` branch: `width`/`height` are string-delivered.
    #[test]
    fn stringified_resize_payload_repairs_and_deserializes() {
        let mut ex = ExtractAction::new();
        ex.add_tool(ToolSchema {
            name: "computer_windows".into(),
            input_schema: Computer::new().description_windows["inputSchema"]
                .clone(),
            example_args: None,
        });
        let call = NativeToolCall {
            id: "call_w2".into(),
            name: "computer_windows".into(),
            arguments: r#"{"name": "Reports", "action": "resize", "width": "800", "height": "600"}"#
                .into(),
            thought_signature: String::new(),
        };
        let repaired = match ex.register_native_call(&call) {
            StreamAction::ToolCall(tc) => tc,
            other => panic!("stringified resize must repair, got {other:?}"),
        };
        assert_eq!(ex.take_tool_failures(), 0, "repair, not rejection");

        let input: ComputerWindows =
            serde_json::from_value(repaired.arguments.clone()).expect("must deserialize");
        assert_eq!(input.name.as_deref(), Some("Reports"));
        assert_eq!(input.action, Some(WindowAction::Resize));
        assert_eq!(input.width, Some(800));
        assert_eq!(input.height, Some(600));
    }
}
