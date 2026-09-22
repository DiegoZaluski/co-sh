//! The left panel: the docked column that coexists with the active route
//! (home or session) and subtracts its width from the main area.
//!
//! Contents — the sessions history list, the usage dashboard and the file
//! explorer — share the panel's width gate and mode enum, which live here:
//! the panel owns its identity; the App only consumes it.

pub mod dashboard;
pub mod explorer_status;
pub mod file_explorer;
pub mod layout;
pub mod sessions;

/// Width of the left panel column. The usage dashboard and the file explorer
/// share the sessions list's width, so geometry is consistent regardless of
/// view. Shared by render and mouse dispatch for exact click hit-testing.
pub const LEFT_PANEL_WIDTH: u16 = 22;

/// Minimum terminal width to show the left panel. Below this width, the panel
/// is auto-hidden to prevent layout conflicts with home/session content.
pub const MIN_WIDTH_FOR_LEFT_PANEL: u16 = 80;

/// What the left panel currently shows. Ctrl+U jumps straight to the usage
/// dashboard; Ctrl+B jumps straight back to the session history — the two
/// keys are NOT a toggle, so a user only ever needs to know the one that
/// shows what they want.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// The session history list (Ctrl+B).
    #[default]
    History,
    /// The usage dashboard (Ctrl+U).
    Dashboard,
    /// The file explorer tree (Ctrl+F).
    Explorer,
}
