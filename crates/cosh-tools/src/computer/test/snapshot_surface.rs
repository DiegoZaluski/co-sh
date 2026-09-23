//! Tests for snapshot rendering and shell-surface targeting against the mock provider.
#[cfg(test)]
/// Shell-surface dispatch tests — same mock fixture as the wait tests
/// (Taskbar surface at `MOCK_SHELL_PID`), driving `snapshot_surface`
/// directly through its provider seam.
mod snapshot_surface_tests {
    use std::time::Duration;

    use xa11y::{ShellSurface, ShellSurfaceKind, mock};

    use crate::computer::snapshot::{DEFAULT_MAX_DEPTH, snapshot_surface};
    use crate::computer::types::{ComputerSnapshot, SnapshotFormat};

    fn taskbar() -> xa11y::ShellSurface {
        ShellSurface::by_kind_with(
            mock::build_provider(),
            ShellSurfaceKind::Taskbar,
            Duration::ZERO,
        )
        .expect("mock fixture carries a taskbar")
    }

    /// A surface-rooted snapshot without a selector outlines the WHOLE
    /// surface and reports the surface's own identity (name + shell pid),
    /// not an app's.
    #[test]
    fn surface_root_reports_surface_identity() {
        let input = ComputerSnapshot::default();
        let out = snapshot_surface(
            &input,
            taskbar(),
            Some(DEFAULT_MAX_DEPTH as usize),
            SnapshotFormat::default(),
        )
        .expect("mock taskbar must snapshot");
        assert_eq!(out.app, "Taskbar");
        assert_eq!(out.pid, Some(mock::MOCK_SHELL_PID));
        assert!(out.elements >= 1);
        assert!(out.snapshot.contains("Taskbar"), "{}", out.snapshot);
    }

    /// With a selector the SAME locator flow narrows into the surface's
    /// subtree — `nth` (1-based) applies exactly like the app path.
    #[test]
    fn surface_selector_narrows_like_app_path() {
        let input = ComputerSnapshot {
            selector: Some("button".into()),
            nth: Some(1),
            ..ComputerSnapshot::default()
        };
        let out = snapshot_surface(
            &input,
            taskbar(),
            Some(DEFAULT_MAX_DEPTH as usize),
            SnapshotFormat::default(),
        )
        .expect("mock taskbar carries buttons");
        assert_eq!(out.app, "Taskbar");
        assert_eq!(out.elements, 1, "nth(1) picks exactly one match");
    }

    /// A selector that matches nothing under the surface is an honest
    /// error naming the surface — same contract as the app path.
    #[test]
    fn surface_selector_miss_is_honest_error() {
        let input = ComputerSnapshot {
            selector: Some("text_field[name='definitely-not-here']".into()),
            ..ComputerSnapshot::default()
        };
        let err = snapshot_surface(
            &input,
            taskbar(),
            Some(DEFAULT_MAX_DEPTH as usize),
            SnapshotFormat::default(),
        )
        .expect_err("missing selector must fail");
        assert!(!err.is_empty());
    }
}

#[cfg(test)]
mod snapshot_render_tests {
    use crate::computer::snapshot::{render_tree, write_outline};
    use crate::computer::types::SnapshotFormat;
    use crate::computer::types::{ElementStates, StateNode, ToggleState};

    /// A node with only default states renders as bare `role name=value` —
    /// no state tokens, keeping the common case one short line.
    #[test]
    fn default_states_render_nothing() {
        let node = StateNode {
            role: "button".into(),
            name: Some("OK".into()),
            value: None,
            states: ElementStates::default(),
            children: vec![],
        };
        assert_eq!(write_outline_str(&node), "button name=\"OK\"\n");
    }

    /// Non-default flags render as aria-vocabulary tokens, in fixed order:
    /// disabled, hidden, focused, checked, selected, expanded, editable, busy.
    #[test]
    fn non_default_states_render_as_tokens_in_fixed_order() {
        let states = ElementStates {
            enabled: false,
            visible: false,
            focused: true,
            checked: Some(ToggleState::On),
            selected: true,
            expanded: Some(false),
            editable: true,
            busy: true,
        };
        let node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states,
            children: vec![],
        };
        assert_eq!(
            write_outline_str(&node),
            "checkbox disabled hidden focused checked selected collapsed editable busy\n"
        );
    }

    /// An unchecked checkbox is meaningful (the user wants to know it is
    /// checkable and off), so `Some(Off)` renders `unchecked` — unlike the
    /// absence of checkability (`None`), which renders nothing.
    #[test]
    fn unchecked_is_reported_but_non_checkable_is_silent() {
        let unchecked = ElementStates {
            checked: Some(ToggleState::Off),
            ..ElementStates::default()
        };
        let unchecked_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: unchecked,
            children: vec![],
        };
        assert_eq!(write_outline_str(&unchecked_node), "checkbox unchecked\n");

        let mixed = ElementStates {
            checked: Some(ToggleState::Mixed),
            ..ElementStates::default()
        };
        let mixed_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: mixed,
            children: vec![],
        };
        assert_eq!(write_outline_str(&mixed_node), "checkbox mixed\n");
    }

    /// `json` format serializes the node directly: the flat `states` object
    /// is present with the normalized subset, `checked` is "off" (lowercase
    /// enum) and children nest recursively.
    #[test]
    fn json_format_embeds_states_object() {
        let child = StateNode {
            role: "text_field".into(),
            name: Some("Search".into()),
            value: Some("hi".into()),
            states: ElementStates {
                editable: true,
                ..ElementStates::default()
            },
            children: vec![],
        };
        let root = StateNode {
            role: "window".into(),
            name: Some("Main".into()),
            value: None,
            states: ElementStates::default(),
            children: vec![child],
        };
        let json = render_tree(&SnapshotFormat::Json, &root);
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["role"], "window");
        assert_eq!(value["states"]["enabled"], true);
        assert_eq!(value["states"]["visible"], true);
        assert_eq!(value["states"]["focused"], false);
        // `.get()` (not indexing) so a MISSING key would fail: indexing an
        // absent key also yields Null, which would make this assertion pass
        // even if `checked` were dropped from the serialization entirely.
        assert_eq!(
            value["states"].get("checked"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            value["states"].get("expanded"),
            Some(&serde_json::Value::Null)
        );
        let grandchild = &value["children"][0];
        assert_eq!(grandchild["states"]["editable"], true);
        assert_eq!(
            grandchild["states"].get("checked"),
            Some(&serde_json::Value::Null)
        );
        // ToggleState serializes lowercase.
        let toggled = ElementStates {
            checked: Some(ToggleState::On),
            ..ElementStates::default()
        };
        let toggled_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: toggled,
            children: vec![],
        };
        let toggled_json = render_tree(&SnapshotFormat::Json, &toggled_node);
        let toggled_value: serde_json::Value =
            serde_json::from_str(&toggled_json).expect("valid json");
        assert_eq!(toggled_value["states"]["checked"], "on");
        let mixed_json_value = ElementStates {
            checked: Some(ToggleState::Mixed),
            ..ElementStates::default()
        };
        let mixed_node = StateNode {
            role: "checkbox".into(),
            name: None,
            value: None,
            states: mixed_json_value,
            children: vec![],
        };
        let mixed_json = render_tree(&SnapshotFormat::Json, &mixed_node);
        let mixed_value: serde_json::Value = serde_json::from_str(&mixed_json).expect("valid json");
        assert_eq!(mixed_value["states"]["checked"], "mixed");
    }

    fn write_outline_str(node: &StateNode) -> String {
        let mut out = String::new();
        write_outline(node, 0, &mut out);
        out
    }
}

#[cfg(test)]
mod surface_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::computer::surface::{
        kind, resolve_with, surface_label, validate_act, validate_screenshot, validate_snapshot,
    };
    use crate::computer::types::SurfaceKind;
    use crate::computer::types::{ComputerAct, ComputerScreenshot, ComputerSnapshot};
    use xa11y::mock::build_provider;
    use xa11y::{Provider, ShellSurfaceKind};

    fn mock() -> Arc<dyn Provider> {
        build_provider()
    }

    #[test]
    fn kind_mapping_is_total() {
        // Every wire variant maps onto a distinct xa11y kind — a missed arm
        // here would silently target the wrong surface.
        let pairs = [
            (SurfaceKind::MenuBar, ShellSurfaceKind::MenuBar),
            (SurfaceKind::StatusItems, ShellSurfaceKind::StatusItems),
            (SurfaceKind::Taskbar, ShellSurfaceKind::Taskbar),
            (SurfaceKind::Panel, ShellSurfaceKind::Panel),
            (SurfaceKind::Dock, ShellSurfaceKind::Dock),
            (SurfaceKind::Desktop, ShellSurfaceKind::Desktop),
            (SurfaceKind::Flyout, ShellSurfaceKind::Flyout),
            (SurfaceKind::Unknown, ShellSurfaceKind::Unknown),
        ];
        for (wire, native) in pairs {
            assert_eq!(kind(wire), native);
            // Wire spelling must match xa11y's snake_case spelling — the
            // schema's enum values and error messages rely on it.
            assert_eq!(surface_label(wire), native.to_snake_case());
        }
    }

    #[test]
    fn resolve_taskbar_through_mock() {
        // The mock fixture ships a taskbar surface; resolution must yield
        // a handle whose kind and name round-trip.
        let surface = resolve_with(mock(), SurfaceKind::Taskbar, Duration::ZERO)
            .expect("mock fixture carries a taskbar");
        assert_eq!(surface.kind, ShellSurfaceKind::Taskbar);
        assert_eq!(surface.name, "Taskbar");
        // The handle must be usable as a selector root (the flow every
        // consumer relies on).
        let locator = surface.locator("button");
        assert!(locator.count().map(|n| n >= 1).unwrap_or(false));
    }

    #[test]
    fn resolve_missing_kind_is_honest_scope() {
        // The mock ships no dock; a missing kind is an error (scope), not
        // a panic or an empty success.
        let err = resolve_with(mock(), SurfaceKind::Dock, Duration::ZERO)
            .expect_err("mock has no dock surface");
        assert!(err.contains("dock"), "err: {err}");
    }

    #[test]
    fn targeting_exactly_one_of_three() {
        let ok = || validate_snapshot(&ComputerSnapshot::default());
        // Zero targets.
        let err = ok().unwrap_err();
        assert!(
            err.contains("provide `name`, `pid` or `surface`"),
            "err: {err}"
        );
        // Two app scopes.
        let err = validate_snapshot(&ComputerSnapshot {
            name: Some("Safari".into()),
            pid: Some(1),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
        // App + surface mix.
        let err = validate_snapshot(&ComputerSnapshot {
            name: Some("Safari".into()),
            surface: Some(SurfaceKind::MenuBar),
            ..Default::default()
        })
        .unwrap_err();
        assert!(
            err.contains("cannot share a call"),
            "app+surface mix must be rejected: {err}"
        );
        // Exactly one: surface alone is fine (mock-free check).
        assert!(
            validate_snapshot(&ComputerSnapshot {
                surface: Some(SurfaceKind::MenuBar),
                ..Default::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn targeting_act_and_screenshot_match_their_wire_shapes() {
        // act uses `name`; screenshot uses `app` — the error text must
        // mirror each tool's wire spelling.
        let err = validate_act(&ComputerAct::default()).unwrap_err();
        assert!(err.contains("`name`"), "err: {err}");
        let err = validate_act(&ComputerAct {
            name: Some("Safari".into()),
            surface: Some(SurfaceKind::Dock),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("cannot share a call"), "err: {err}");

        // Rootless screenshot is LEGAL (full display / region captures) —
        // the tool's own clauses police those forms; this check only
        // rejects MIXED roots (review round 1, CRITICAL 1).
        assert!(validate_screenshot(&ComputerScreenshot::default()).is_ok());
        let err = validate_screenshot(&ComputerScreenshot {
            app: Some("Safari".into()),
            surface: Some(SurfaceKind::Dock),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("cannot share a call"), "err: {err}");
        // pid + surface mix also rejected.
        let err = validate_screenshot(&ComputerScreenshot {
            pid: Some(1),
            surface: Some(SurfaceKind::Dock),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.contains("cannot share a call"), "err: {err}");
    }
}
