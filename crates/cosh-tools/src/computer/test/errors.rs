//! Tests for the error renderer: variant mapping, Diagnosis preservation, non_exhaustive wildcard.
#[cfg(test)]
mod render_tests {
    use std::time::Duration;

    use xa11y::Error;

    use crate::computer::errors::render;
    use xa11y::Diagnosis;

    const TOOL: &str = "computer_snapshot";
    const CTX: &str = "resolve application";

    fn diagnosis() -> Diagnosis {
        Diagnosis::new()
            .condition("visible")
            .last_observed("matched button \"Export\" (visible=false, enabled=true)")
            .candidates(vec!["button \"Export All\"".to_string()])
    }

    /// Each actionable variant renders its FIX, not just the failure —
    /// the model must learn what to do next from the error alone.
    #[test]
    fn permission_denied_tells_the_grant_flow() {
        let err = Error::PermissionDenied {
            instructions: "grant in System Settings".into(),
        };
        let out = render(TOOL, CTX, &err);
        assert!(
            out.starts_with("computer_snapshot: resolve application: "),
            "{out}"
        );
        assert!(out.contains("permission denied"), "{out}");
        assert!(out.contains("System Settings"), "{out}");
        assert!(out.contains("retry the same call"), "{out}");
    }

    #[test]
    fn accessibility_not_enabled_names_the_app() {
        let err = Error::AccessibilityNotEnabled {
            app: "Chromium".into(),
            instructions: "relaunch with --force-renderer-accessibility".into(),
        };
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("Chromium"), "{out}");
        assert!(out.contains("--force-renderer-accessibility"), "{out}");
    }

    #[test]
    fn selector_miss_recommends_fresh_capture_and_keeps_diagnosis() {
        let err = Error::SelectorNotMatched {
            selector: "button[name='OK']".into(),
            diagnosis: Some(Box::new(diagnosis())),
        };
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("re-capture"), "{out}");
        assert!(out.contains(":nth index goes stale"), "{out}");
        // The diagnosis must survive verbatim — flattening it away is the
        // exact failure this phase exists to fix.
        assert!(out.contains("visible=false, enabled=true"), "{out}");
        assert!(out.contains("Export All"), "{out}");
    }

    #[test]
    fn selector_miss_without_diagnosis_still_teaches_the_shape() {
        let err = Error::selector_not_matched("button");
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("re-capture"), "{out}");
        assert!(
            !out.contains("last observed"),
            "no empty-diagnosis suffix: {out}"
        );
    }

    #[test]
    fn timeout_advises_budget_and_keeps_diagnosis() {
        let err = Error::Timeout {
            elapsed: Duration::from_millis(3000),
            diagnosis: Some(Box::new(diagnosis().condition("wait for detached").clone())),
        };
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("timed out after 3"), "{out}");
        assert!(out.contains("timeout_ms"), "{out}");
        assert!(out.contains("`wait` step"), "{out}");
        assert!(out.contains("wait for detached"), "{out}");
    }

    #[test]
    fn no_bounds_suggests_scroll_into_view() {
        let out = render(TOOL, CTX, &Error::NoElementBounds);
        assert!(out.contains("no on-screen bounds"), "{out}");
        assert!(out.contains("scroll_into_view"), "{out}");
    }

    #[test]
    fn unsupported_names_the_session_gap() {
        let err = Error::Unsupported {
            feature: "pointer warp".into(),
        };
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("pointer warp"), "{out}");
        assert!(out.contains("X11"), "{out}");
        assert!(out.contains("Wayland"), "{out}");
    }

    #[test]
    fn argument_errors_distinguish_call_shape_from_platform() {
        let selector = Error::InvalidSelector {
            selector: "button, link".into(),
            message: "comma alternation".into(),
        };
        let out = render(TOOL, CTX, &selector);
        assert!(out.contains("fix the selector syntax"), "{out}");
        assert!(out.contains("no platform call was made"), "{out}");

        let data = Error::InvalidActionData {
            message: "duration must be >= 50".into(),
        };
        let out = render(TOOL, CTX, &data);
        assert!(out.contains("fix the call arguments"), "{out}");
        assert!(out.contains("no platform call was made"), "{out}");
    }

    #[test]
    fn platform_error_keeps_the_code_and_names_the_provider_level_failure() {
        let err = Error::Platform {
            code: -1,
            message: "dbus call failed".into(),
        };
        let out = render(TOOL, CTX, &err);
        assert!(out.contains("(-1)"), "{out}");
        assert!(out.contains("dbus call failed"), "{out}");
        // Provider-level (not a target miss): guidance must NOT send the
        // model to re-capture; it frames the failure as operation-dependent
        // and bounds the retry instead of prescribing a target fix.
        assert!(out.contains("provider-level"), "{out}");
        assert!(out.contains("depends on the operation"), "{out}");
        assert!(!out.contains("re-capture"), "{out}");
        assert!(out.contains("stop and report it to the user"), "{out}");
    }

    /// Totality guard: every variant renders SOMETHING useful — a missed
    /// arm would fall to a silent default and strand the model.
    #[test]
    fn every_variant_renders_tool_prefix() {
        use xa11y::Error as E;
        let errors: Vec<Error> = vec![
            E::PermissionDenied {
                instructions: "i".into(),
            },
            E::AccessibilityNotEnabled {
                app: "a".into(),
                instructions: "i".into(),
            },
            E::selector_not_matched("s"),
            E::ElementStale {
                selector: "s".into(),
            },
            E::ActionNotSupported {
                action: "press".into(),
                role: xa11y::Role::Button,
            },
            E::TextValueNotSupported,
            E::timeout(Duration::from_secs(1)),
            E::InvalidSelector {
                selector: "s".into(),
                message: "m".into(),
            },
            E::InvalidActionData {
                message: "m".into(),
            },
            E::InvalidConfig {
                message: "m".into(),
            },
            E::NoElementBounds,
            E::Unsupported {
                feature: "f".into(),
            },
            E::Platform {
                code: 0,
                message: "m".into(),
            },
        ];
        for err in errors {
            let out = render(TOOL, CTX, &err);
            assert!(
                out.starts_with("computer_snapshot: resolve application: "),
                "{out:?} lost the tool prefix"
            );
            assert!(out.len() > 20, "{out:?} is too bare to act on");
        }
    }
}
