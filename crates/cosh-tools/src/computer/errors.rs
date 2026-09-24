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
             accessibility bridge — for a slider/spinner use `set_numeric_value` \
             with `numeric_value`, otherwise click it first (computer_control, \
             the real click is the one reliable focus mover) and type with a \
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
            format!(
                "{head}selector `{selector}` is invalid: {message} — fix the selector syntax and retry (no platform call was made)"
            )
        }
        Error::InvalidActionData { message } => {
            format!(
                "{head}invalid action data: {message} — fix the call arguments and retry (no platform call was made)"
            )
        }
        Error::InvalidConfig { message } => {
            format!(
                "{head}invalid configuration: {message} — fix the environment setting and restart the session"
            )
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

/// Render an APPLICATION-resolution miss (`App::by_name`/`by_pid`), the
/// same rendering [`render`] produces plus the transient-shell hint: a
/// flyout (Quick Settings, Notification Center, a shell context menu) is
/// never an application — it does not appear in `computer_apps` and no app
/// name can reach it, so the fix is surface targeting, not another lookup.
pub fn render_app_miss(tool: &str, err: &Error) -> String {
    let out = render(tool, "resolve application", err);
    match err {
        Error::SelectorNotMatched { .. } => format!(
            "{out} — a name that matches no APPLICATION may belong to a \
             transient shell surface (Quick Settings, Notification Center, \
             a shell menu): those never appear as applications. Target them \
             with `surface` instead (e.g. `\"flyout\"` for an open flyout, \
             `\"menu_bar\"` for menus)"
        ),
        _ => out,
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
