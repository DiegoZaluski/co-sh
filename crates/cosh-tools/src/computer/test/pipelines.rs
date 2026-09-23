//! Tests for the pipeline engines: chain validation (act/control/touch), the wait state machine and their report fragments.
#[cfg(test)]
mod act_validation_tests {
    use crate::computer::act::{validate_chain, ACT_CHAIN_MAX_DEPTH};
    use crate::computer::types::ComputerAct;

    fn step() -> ComputerAct {
        ComputerAct::default()
    }

    #[test]
    fn keyboard_steps_pass() {
        let text = ComputerAct {
            text: Some("olá".into()),
            ..step()
        };
        let key = ComputerAct {
            key: Some("enter".into()),
            ..step()
        };
        let chord = ComputerAct {
            key: Some("a".into()),
            held: Some(vec!["ctrl".into()]),
            ..step()
        };
        assert!(validate_chain(&text, ACT_CHAIN_MAX_DEPTH).is_ok());
        assert!(validate_chain(&key, ACT_CHAIN_MAX_DEPTH).is_ok());
        assert!(validate_chain(&chord, ACT_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn key_and_text_together_rejected() {
        let s = ComputerAct {
            key: Some("enter".into()),
            text: Some("hi".into()),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn keyboard_step_with_semantic_field_rejected() {
        let s = ComputerAct {
            key: Some("enter".into()),
            selector: Some("text_field".into()),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("semantic-action step"), "err: {err}");
    }

    #[test]
    fn text_with_held_rejected() {
        let s = ComputerAct {
            text: Some("hi".into()),
            held: Some(vec!["ctrl".into()]),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("`held` is rejected"), "err: {err}");
    }

    #[test]
    fn wait_step_passes() {
        let s = ComputerAct {
            wait: Some(600),
            ..step()
        };
        assert!(validate_chain(&s, ACT_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn wait_with_other_fields_rejected() {
        let s = ComputerAct {
            wait: Some(600),
            selector: Some("text_field".into()),
            name: Some("Obsidian".into()),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("standalone"), "err: {err}");

        let s = ComputerAct {
            wait: Some(600),
            text: Some("hi".into()),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("standalone"), "err: {err}");
    }

    #[test]
    fn wait_zero_and_over_cap_rejected() {
        let s = ComputerAct {
            wait: Some(0),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains(">= 1 ms"), "err: {err}");

        let s = ComputerAct {
            wait: Some(10_001),
            ..step()
        };
        let err = validate_chain(&s, ACT_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("capped at 10000 ms"), "err: {err}");
    }

    #[test]
    fn semantic_then_wait_then_type_passes() {
        // The field-test pattern: act on a menu item that opens an
        // ephemeral field, let the app settle, type into it, submit.
        let enter = ComputerAct {
            key: Some("enter".into()),
            ..step()
        };
        let type_step = ComputerAct {
            text: Some("cosh-test".into()),
            then: Some(Box::new(enter)),
            ..step()
        };
        let wait_step = ComputerAct {
            wait: Some(600),
            then: Some(Box::new(type_step)),
            ..step()
        };
        let press = ComputerAct {
            name: Some("Obsidian".into()),
            selector: Some("menu_item[name='Rename']".into()),
            then: Some(Box::new(wait_step)),
            ..step()
        };
        assert!(validate_chain(&press, ACT_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn chain_depth_cap() {
        fn chain(steps: usize) -> ComputerAct {
            let mut root = step();
            root.key = Some("enter".into());
            for _ in 1..steps {
                let mut wrapper = step();
                wrapper.key = Some("enter".into());
                wrapper.then = Some(Box::new(root));
                root = wrapper;
            }
            root
        }
        assert!(validate_chain(&chain(ACT_CHAIN_MAX_DEPTH), ACT_CHAIN_MAX_DEPTH).is_ok());
        let err = validate_chain(&chain(ACT_CHAIN_MAX_DEPTH + 1), ACT_CHAIN_MAX_DEPTH)
            .unwrap_err();
        assert!(err.contains("exceeds"), "err: {err}");
    }

    #[test]
    fn chain_needs_sim_walks_through_wait_steps() {
        use crate::computer::act::chain_needs_sim;
        // Regression (review finding): a wait step may carry a `then`
        // continuation — the sim decision must look PAST the wait, not
        // stop at it.
        let wait_then_type = ComputerAct {
            wait: Some(600),
            then: Some(Box::new(ComputerAct {
                text: Some("hi".into()),
                ..step()
            })),
            ..step()
        };
        assert!(chain_needs_sim(&wait_then_type), "wait → text needs the sim");

        let type_then_wait = ComputerAct {
            text: Some("hi".into()),
            then: Some(Box::new(ComputerAct {
                wait: Some(600),
                ..step()
            })),
            ..step()
        };
        assert!(chain_needs_sim(&type_then_wait), "text → wait needs the sim");

        let wait_only = ComputerAct {
            wait: Some(600),
            ..step()
        };
        assert!(!chain_needs_sim(&wait_only), "wait-only needs no sim");

        let semantic = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            then: Some(Box::new(ComputerAct {
                wait: Some(600),
                ..step()
            })),
            ..step()
        };
        assert!(!chain_needs_sim(&semantic), "semantic + wait needs no sim");
    }
}

#[cfg(test)]
mod control_validation_tests {
    use crate::computer::control::{validate_chain, CONTROL_CHAIN_MAX_DEPTH};
    use crate::computer::types::ComputerControl;

    fn step() -> ComputerControl {
        ComputerControl::default()
    }

    fn chain(steps: usize) -> ComputerControl {
        // `steps` nested steps (root included); every level carries its own
        // keyboard action so the chain is valid apart from its depth.
        let mut root = step();
        root.key = Some("enter".into());
        for _ in 1..steps {
            let mut wrapper = step();
            wrapper.key = Some("enter".into());
            wrapper.then = Some(Box::new(root));
            root = wrapper;
        }
        root
    }

    #[test]
    fn single_pointer_element_step_passes() {
        let s = ComputerControl {
            app: Some("Safari".into()),
            selector: Some("button[name='OK']".into()),
            ..step()
        };
        assert!(validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn single_coordinate_step_passes() {
        let s = ComputerControl {
            x: Some(10),
            y: Some(20),
            ..step()
        };
        assert!(validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn keyboard_steps_pass() {
        let text = ComputerControl {
            text: Some("olá".into()),
            ..step()
        };
        let key = ComputerControl {
            key: Some("enter".into()),
            ..step()
        };
        let chord = ComputerControl {
            key: Some("a".into()),
            held: Some(vec!["ctrl".into()]),
            ..step()
        };
        assert!(validate_chain(&text, CONTROL_CHAIN_MAX_DEPTH).is_ok());
        assert!(validate_chain(&key, CONTROL_CHAIN_MAX_DEPTH).is_ok());
        assert!(validate_chain(&chord, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn key_and_text_together_rejected() {
        let s = ComputerControl {
            key: Some("enter".into()),
            text: Some("hi".into()),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn keyboard_step_with_pointer_field_rejected() {
        let s = ComputerControl {
            key: Some("enter".into()),
            selector: Some("text_field".into()),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("pointer step"), "err: {err}");
    }

    #[test]
    fn text_with_held_rejected() {
        let s = ComputerControl {
            text: Some("hi".into()),
            held: Some(vec!["ctrl".into()]),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("`held` is rejected"), "err: {err}");
    }

    #[test]
    fn mixed_pipeline_passes() {
        // The canonical pattern: click the field, type, submit.
        let type_step = ComputerControl {
            text: Some("cosh-test".into()),
            ..step()
        };
        let enter_step = ComputerControl {
            key: Some("enter".into()),
            ..step()
        };
        let type_then = ComputerControl {
            then: Some(Box::new(enter_step)),
            ..type_step
        };
        let click = ComputerControl {
            app: Some("Obsidian".into()),
            selector: Some("text_field[name='Untitled']".into()),
            then: Some(Box::new(type_then)),
            ..step()
        };
        assert!(validate_chain(&click, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn chain_depth_cap() {
        assert!(validate_chain(&chain(CONTROL_CHAIN_MAX_DEPTH), CONTROL_CHAIN_MAX_DEPTH).is_ok());
        let err = validate_chain(
            &chain(CONTROL_CHAIN_MAX_DEPTH + 1),
            CONTROL_CHAIN_MAX_DEPTH,
        )
        .unwrap_err();
        assert!(err.contains("exceeds"), "err: {err}");
    }

    #[test]
    fn wait_step_passes() {
        let s = ComputerControl {
            wait: Some(600),
            ..step()
        };
        assert!(validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }

    #[test]
    fn wait_with_other_fields_rejected() {
        let s = ComputerControl {
            wait: Some(600),
            selector: Some("text_field".into()),
            app: Some("Obsidian".into()),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("standalone"), "err: {err}");

        let s = ComputerControl {
            wait: Some(600),
            text: Some("hi".into()),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("standalone"), "err: {err}");
    }

    #[test]
    fn wait_zero_and_over_cap_rejected() {
        let s = ComputerControl {
            wait: Some(0),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains(">= 1 ms"), "err: {err}");

        let s = ComputerControl {
            wait: Some(10_001),
            ..step()
        };
        let err = validate_chain(&s, CONTROL_CHAIN_MAX_DEPTH).unwrap_err();
        assert!(err.contains("capped at 10000 ms"), "err: {err}");
    }

    #[test]
    fn wait_between_click_and_type_passes() {
        // The field-test pattern: click opens an ephemeral rename box, the
        // app needs a beat to create it, then type into it, then submit.
        let enter = ComputerControl {
            key: Some("enter".into()),
            ..step()
        };
        let type_step = ComputerControl {
            text: Some("cosh-test".into()),
            then: Some(Box::new(enter)),
            ..step()
        };
        let wait_step = ComputerControl {
            wait: Some(600),
            then: Some(Box::new(type_step)),
            ..step()
        };
        let click = ComputerControl {
            app: Some("Obsidian".into()),
            selector: Some("menu_item[name='Rename']".into()),
            then: Some(Box::new(wait_step)),
            ..step()
        };
        assert!(validate_chain(&click, CONTROL_CHAIN_MAX_DEPTH).is_ok());
    }
}

#[cfg(test)]
mod touch_validation_tests {
    use crate::computer::touch::validate;
    use crate::computer::types::{ActAction, ComputerAct};

    const TOOL: &str = "computer_act";

    fn step() -> ComputerAct {
        ComputerAct::default()
    }

    #[test]
    fn semantic_step_passes() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("button[name='OK']".into()),
            ..step()
        };
        assert!(validate(&s, TOOL).is_ok());
    }

    #[test]
    fn semantic_step_requires_selector() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`selector`"), "err: {err}");
    }

    #[test]
    fn semantic_step_requires_app_scope() {
        let s = ComputerAct {
            selector: Some("button".into()),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`name`, `pid` or `surface`"), "err: {err}");
    }

    #[test]
    fn name_and_pid_together_rejected() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            pid: Some(42),
            selector: Some("button".into()),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn set_value_requires_payload() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("text_field".into()),
            action: Some(ActAction::SetValue),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`value`"), "err: {err}");
    }

    #[test]
    fn set_numeric_value_requires_payload() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("slider".into()),
            action: Some(ActAction::SetNumericValue),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`numeric_value`"), "err: {err}");
    }

    #[test]
    fn select_text_range_rules() {
        let missing = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("text".into()),
            action: Some(ActAction::SelectText),
            ..step()
        };
        let err = validate(&missing, TOOL).unwrap_err();
        assert!(err.contains("`range`"), "err: {err}");

        let wrong_arity = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("text".into()),
            action: Some(ActAction::SelectText),
            range: Some(vec![1, 2, 3]),
            ..step()
        };
        let err = validate(&wrong_arity, TOOL).unwrap_err();
        assert!(err.contains("exactly 2"), "err: {err}");

        let reversed = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("text".into()),
            action: Some(ActAction::SelectText),
            range: Some(vec![3, 1]),
            ..step()
        };
        let err = validate(&reversed, TOOL).unwrap_err();
        assert!(err.contains("must be <= end"), "err: {err}");
    }

    #[test]
    fn nth_zero_rejected() {
        let s = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            nth: Some(0),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");
    }

    #[test]
    fn semantic_step_with_held_rejected() {
        // Review finding: `held` on a semantic step was silently ignored —
        // a chord belongs to keyboard/pointer steps, so reject it loudly.
        let s = ComputerAct {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            held: Some(vec!["ctrl".into()]),
            ..step()
        };
        let err = validate(&s, TOOL).unwrap_err();
        assert!(err.contains("`held` applies to keyboard and pointer"), "err: {err}");
    }

    #[test]
    fn surface_step_validates_like_app_step() {
        // A surface step is a semantic step: selector required, targeting
        // exclusivity applies (surface + name / surface + pid rejected).
        let ok = ComputerAct {
            surface: Some(crate::computer::types::SurfaceKind::Taskbar),
            selector: Some("button".into()),
            ..step()
        };
        assert!(validate(&ok, TOOL).is_ok());

        let with_name = ComputerAct {
            surface: Some(crate::computer::types::SurfaceKind::Taskbar),
            name: Some("Taskbar".into()),
            selector: Some("button".into()),
            ..step()
        };
        let err = validate(&with_name, TOOL).unwrap_err();
        assert!(err.contains("cannot share a call"), "err: {err}");

        let with_pid = ComputerAct {
            surface: Some(crate::computer::types::SurfaceKind::Taskbar),
            pid: Some(42),
            selector: Some("button".into()),
            ..step()
        };
        let err = validate(&with_pid, TOOL).unwrap_err();
        assert!(err.contains("cannot share a call"), "err: {err}");
    }
}

/// Shell-surface dispatch tests — same mock fixture as the wait tests
/// (Taskbar surface at `MOCK_SHELL_PID`), driving `run_step_on_surface`
/// directly through its provider seam.
#[cfg(test)]
mod touch_surface_tests {
    use std::time::Duration;

    use xa11y::{ShellSurface, ShellSurfaceKind, mock};

    use crate::computer::touch::run_step_on_surface;
    use crate::computer::types::{ActAction, ComputerAct, SurfaceKind};

    const TOOL: &str = "computer_act";

    fn taskbar() -> xa11y::ShellSurface {
        ShellSurface::by_kind_with(mock::build_provider(), ShellSurfaceKind::Taskbar, Duration::ZERO)
            .expect("mock fixture carries a taskbar")
    }

    /// A press on a surface locator reports against the SURFACE's name —
    /// same report shape as the app path (`action `selector` (target)`).
    #[test]
    fn surface_step_report_names_the_surface() {
        let step = ComputerAct {
            surface: Some(SurfaceKind::Taskbar),
            selector: Some("button".into()),
            action: Some(ActAction::Press),
            ..ComputerAct::default()
        };
        let report = run_step_on_surface(&step, taskbar(), Duration::ZERO, TOOL)
            .expect("mock taskbar carries buttons");
        assert!(
            report.starts_with("press `button` (Taskbar)"),
            "report must name the surface target: {report}"
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::computer::types::{ComputerWait, WaitState};
    use std::time::{Duration, Instant};

    use crate::computer::wait::{MAX_WAIT_MS, validate, wait_on_locator, wait_state};
    use xa11y::{ElementState, Locator};
    use xa11y::Provider;
    use xa11y::mock::build_provider;

    fn root_locator(selector: &str) -> Locator {
        let provider = build_provider();
        let provider_dyn: std::sync::Arc<dyn Provider> = provider;
        Locator::new(provider_dyn, None, selector)
    }

    #[test]
    fn validation_requires_selector_and_one_app_scope() {
        let input = ComputerWait::default();
        let err = validate(&input).unwrap_err();
        assert!(err.contains("`selector` is required"), "err: {err}");

        let input = ComputerWait {
            selector: Some("button".into()),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("`name` or `pid`"), "err: {err}");

        let input = ComputerWait {
            name: Some("Safari".into()),
            pid: Some(42),
            selector: Some("button".into()),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("not both"), "err: {err}");
    }

    #[test]
    fn validation_rejects_zero_nth_and_overcap_timeout() {
        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            nth: Some(0),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("1-based"), "err: {err}");

        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            timeout_ms: Some(MAX_WAIT_MS + 1),
            ..Default::default()
        };
        let err = validate(&input).unwrap_err();
        assert!(err.contains("capped at"), "err: {err}");

        // Exactly at the cap is fine.
        let input = ComputerWait {
            name: Some("Safari".into()),
            selector: Some("button".into()),
            timeout_ms: Some(MAX_WAIT_MS),
            ..Default::default()
        };
        assert!(validate(&input).is_ok());
    }

    #[test]
    fn wait_met_immediately_reports_observed_state() {
        // The mock fixture carries a visible, enabled check_box; the
        // default state (`visible`) is met on the first poll.
        let locator = root_locator("check_box");
        let out = wait_on_locator(
            &locator,
            ElementState::Visible,
            Duration::from_secs(2),
            Instant::now(),
        )
        .expect("visible check_box must meet the condition");
        assert!(out.met);
        assert!(out.elapsed_ms < 2_000);
        assert!(out.observed.attached);
        assert_eq!(out.observed.visible, Some(true));
        assert_eq!(out.observed.enabled, Some(true));
    }

    #[test]
    fn wait_detached_met_without_element() {
        // A selector nothing matches: the ABSENCE condition is met on the
        // first poll — the common "spinner went away" shape.
        let locator = root_locator("button[name='Definitely Not There']");
        let out = wait_on_locator(
            &locator,
            ElementState::Detached,
            Duration::from_secs(2),
            Instant::now(),
        )
        .expect("detached is met when nothing matches");
        assert!(out.met);
        assert!(!out.observed.attached);
        assert_eq!(out.observed.visible, None);
    }

    #[test]
    fn wait_timeout_error_carries_diagnosis() {
        // Attached on a selector that never matches: runs out the (tiny)
        // timeout; the error must name the condition and the last
        // observation, not a bare "timeout".
        let locator = root_locator("button[name='Definitely Not There']");
        let err = wait_on_locator(
            &locator,
            ElementState::Attached,
            Duration::from_millis(150),
            Instant::now(),
        )
        .expect_err("attached on a missing element must time out");
        // Phase 7: the wait path routes through the error renderer — the
        // message must carry the tool prefix, the NEXT-STEP guidance
        // (budget, wait step) and the platform's Diagnosis verbatim, so
        // the model sees exactly what the poll loop last saw.
        assert!(err.starts_with("computer_wait: wait for element state: "), "err: {err}");
        assert!(err.contains("timed out after"), "err: {err}");
        assert!(err.contains("timeout_ms"), "missing budget guidance: {err}");
        assert!(err.contains("`wait` step"), "missing wait-step guidance: {err}");
        assert!(
            err.contains("Attached") || err.contains("attached"),
            "timeout error must name the condition: {err}"
        );
        assert!(
            err.contains("last observed"),
            "diagnosis (last observed) must survive verbatim: {err}"
        );
    }

    #[test]
    fn state_mapping_is_total() {
        // Every wire variant maps onto a distinct xa11y state — a missed
        // arm here would silently wait for the wrong condition.
        let pairs = [
            (WaitState::Attached, ElementState::Attached),
            (WaitState::Detached, ElementState::Detached),
            (WaitState::Visible, ElementState::Visible),
            (WaitState::Hidden, ElementState::Hidden),
            (WaitState::Enabled, ElementState::Enabled),
            (WaitState::Disabled, ElementState::Disabled),
            (WaitState::Focused, ElementState::Focused),
            (WaitState::Unfocused, ElementState::Unfocused),
        ];
        for (wire, native) in pairs {
            assert_eq!(wait_state(wire), native);
        }
    }

    #[test]
    fn mock_provider_type_is_usable_via_selector() {
        // Guards the test seam itself: `build_provider` must yield a
        // provider whose locators resolve. If the fixture topology changes
        // (check_box disappearing), the wait tests above fail HERE with a
        // clearer message.
        let provider = build_provider();
        let provider_dyn: std::sync::Arc<dyn Provider> = provider;
        let locator = Locator::new(provider_dyn, None, "check_box");
        assert!(
            locator.count().map(|n| n >= 1).unwrap_or(false),
            "mock fixture lost its check_box — update the wait tests' fixture assumptions"
        );
    }
}

