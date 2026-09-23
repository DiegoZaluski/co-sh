//! Error-rendering component — turns xa11y's classified platform errors
//! into messages that tell the model (and the user reading the trace) what
//! to do next.
//!
//! This is a COMPONENT, not a tool: it owns the `Error` variant → guidance
//! mapping and nothing else. Every computer tool routes its `map_err`
//! layer through [`render`] so a broken platform condition never reaches
//! the model as a bare `Platform error (-1): …` — the error text is the
//! one channel that always gets through, so it must carry the fix.
//!
//! Design (plan phase 7): xa11y already classifies these conditions —
//! `PermissionDenied` carries consent instructions, `SelectorNotMatched`
//! and `Timeout` carry a [`Diagnosis`] — so this layer NEVER flattens that
//! context away; it prepends the tool/context prefix, renders the variant
//! guidance, and appends the diagnosis verbatim.
use xa11y::Error;

/// Render an xa11y error for the model, prefixed by the failing tool and
/// what it was doing (`ctx`, e.g. `"resolve application"`).
///
/// The mapping is total: every [`Error`] variant produces either concrete
/// next-step guidance or — where the error already says exactly what is
/// wrong (`InvalidSelector`, `InvalidActionData`) — the verbatim display
/// plus the fix that applies to the CALL SHAPE (an argument problem is not
/// a platform problem; the model fixes it by editing the call, not by
/// changing system settings).
pub fn render(tool: &str, ctx: &str, err: &Error) -> String {
    let head = format!("{tool}: {ctx}: ");
    match err {
        Error::PermissionDenied { instructions } => format!(
            "{head}permission denied — {instructions} After granting, retry the \
             same call; the consent applies to the whole terminal session"
        ),
        Error::AccessibilityNotEnabled { app, instructions } => {
            format!("{head}accessibility not enabled for {app} — {instructions}")
        }
        Error::SelectorNotMatched {
            selector,
            diagnosis,
        } => {
            let mut out = format!(
                "{head}no element matched `{selector}` — re-capture with \
                 computer_snapshot and match an element that exists NOW \
                 (role[name='…'] is stable; a bare :nth index goes stale \
                 after any UI change)"
            );
            out.push_str(&diagnosis_suffix(diagnosis.as_deref()));
            out
        }
        Error::ElementStale { selector } => format!(
            "{head}element `{selector}` went stale (the UI changed between \
             capture and action) — re-snapshot and act on the fresh tree"
        ),
        Error::ActionNotSupported { action, role } => format!(
            "{head}action `{action}` is not supported on a {role} element — \
             try the action appropriate to its role instead (`press` on \
             buttons, `toggle` on check boxes, `show_menu` on menu items), \
             or act on a parent container"
        ),
        Error::TextValueNotSupported => format!(
            "{head}this element does not accept text input through the \
             accessibility bridge — click it first (computer_control, the \
             real click is the one reliable focus mover) and type with a \
             keyboard step instead"
        ),
        Error::Timeout { elapsed, diagnosis } => {
            let mut out = format!(
                "{head}timed out after {elapsed:.1?} — if the target is slow \
                 to appear, raise `timeout_ms` or add a `wait` step before \
                 this one"
            );
            out.push_str(&diagnosis_suffix(diagnosis.as_deref()));
            out
        }
        Error::InvalidSelector { selector, message } => {
            format!("{head}selector `{selector}` is invalid: {message} — fix the selector syntax and retry (no platform call was made)")
        }
        Error::InvalidActionData { message } => {
            format!("{head}invalid action data: {message} — fix the call arguments and retry (no platform call was made)")
        }
        Error::InvalidConfig { message } => {
            format!("{head}invalid configuration: {message} — fix the environment setting and restart the session")
        }
        Error::NoElementBounds => format!(
            "{head}the element matched but has no on-screen bounds — it may \
             sit in a collapsed container or be virtual; scroll it into \
             view (computer_act `scroll_into_view`) or act on a visible \
             ancestor instead"
        ),
        Error::Unsupported { feature } => format!(
            "{head}`{feature}` has no implementation on this platform/session — a \
             capability limit, not a fixable call error. Input synthesis and \
             screen capture both depend on the session type (native X11, or \
             Wayland with the right portal grants), so pick an approach that \
             fits what this session exposes"
        ),
        Error::Platform { code, message } => format!(
            "{head}platform error ({code}): {message} — a provider-level failure \
             whose meaning depends on the operation (an accessibility-backend \
             fault, a capture or image-processing failure, or an argument the \
             backend rejected); one retry is reasonable, but if unchanged \
             arguments fail the same way, stop and report it to the user \
             instead of looping"
        ),
        // `Error` is #[non_exhaustive]: future variants must still render
        // with the tool prefix and a next-step suggestion, never a bare
        // Display string.
        other => format!(
            "{head}{} — re-capture with computer_snapshot to check the \
             current state before retrying",
            other
        ),
    }
}

/// Render the attached [`Diagnosis`] — verbatim, since xa11y's rendering
/// already carries the condition, last-observed state, near-miss
/// candidates and scope dump (and its Display already opens with its own
/// `; `). Nothing here may flatten it.
fn diagnosis_suffix(diagnosis: Option<&xa11y::Diagnosis>) -> String {
    match diagnosis {
        None => String::new(),
        Some(d) => {
            let rendered = d.to_string();
            if rendered.starts_with(';') {
                format!(" {rendered}")
            } else {
                format!("; {rendered}")
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use std::time::Duration;

    use super::*;
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
        assert!(out.starts_with("computer_snapshot: resolve application: "), "{out}");
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
            diagnosis: Some(Box::new(
                diagnosis().condition("wait for detached").clone(),
            )),
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
        use Error as E;
        let errors: Vec<Error> = vec![
            E::PermissionDenied { instructions: "i".into() },
            E::AccessibilityNotEnabled { app: "a".into(), instructions: "i".into() },
            E::selector_not_matched("s"),
            E::ElementStale { selector: "s".into() },
            E::ActionNotSupported { action: "press".into(), role: xa11y::Role::Button },
            E::TextValueNotSupported,
            E::timeout(Duration::from_secs(1)),
            E::InvalidSelector { selector: "s".into(), message: "m".into() },
            E::InvalidActionData { message: "m".into() },
            E::InvalidConfig { message: "m".into() },
            E::NoElementBounds,
            E::Unsupported { feature: "f".into() },
            E::Platform { code: 0, message: "m".into() },
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
