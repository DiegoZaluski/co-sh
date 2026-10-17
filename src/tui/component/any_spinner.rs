//! Style-dispatching agent spinner: every `/spinner` choice behind one
//! surface.
//!
//! The app drives the agent-loop spinner through a single interface
//! (`new`/`advance`/`set_activity`/`set_mode_color`/`update_theme`/`width`
//! /`render`/`reset`/`is_initialized`). This enum dispatches those calls to
//! whichever animation the user picked (`cosh::setup::SpinnerStyle`), so a
//! style switch is a pure decoration change — the loop code never learns
//! which variant is live.

use cosh_tui::core::lib::rgba::RGBA;
use ratatui::buffer::Buffer;

use crate::component::agent_spinner::AgentSpinner;
use crate::component::agent_spinner_bass::AgentSpinnerBass;
use crate::component::agent_spinner_classic::AgentSpinnerClassic;
use crate::component::agent_spinner_crush::AgentSpinnerCrush;
use crate::component::agent_spinner_opencode::AgentSpinnerOpencode;
use crate::theme::Theme;

/// The live agent-loop spinner, whatever `/spinner` style is active.
pub enum AnySpinner {
    /// The bass equalizer (the default): bars on waves + a kick beat.
    Bass(AgentSpinnerBass),
    /// Elder-Futhark runic scramble with the theme gradient.
    Runes(AgentSpinner),
    /// The opencode Knight-Rider scanner sweep (blocks style).
    Opencode(AgentSpinnerOpencode),
    /// The charmbracelet/crush working indicator, ported directly.
    Crush(AgentSpinnerCrush),
    /// The classic braille wheel the in-app busy indicators use.
    Classic(AgentSpinnerClassic),
}

impl AnySpinner {
    /// Build the spinner for `style` with the given label.
    pub fn new(label: &str, theme: &Theme, style: cosh::setup::SpinnerStyle) -> Self {
        match style {
            cosh::setup::SpinnerStyle::Bass => {
                AnySpinner::Bass(AgentSpinnerBass::new(label, theme))
            }
            cosh::setup::SpinnerStyle::Runes => {
                AnySpinner::Runes(AgentSpinner::new(label, theme))
            }
            cosh::setup::SpinnerStyle::Opencode => {
                AnySpinner::Opencode(AgentSpinnerOpencode::new(label, theme))
            }
            cosh::setup::SpinnerStyle::Crush => {
                AnySpinner::Crush(AgentSpinnerCrush::new(label, theme))
            }
            cosh::setup::SpinnerStyle::Classic => {
                AnySpinner::Classic(AgentSpinnerClassic::new(label, theme))
            }
        }
    }

    /// Advance the animation by one frame.
    pub fn advance(&mut self) {
        match self {
            AnySpinner::Bass(s) => s.advance(),
            AnySpinner::Runes(s) => s.advance(),
            AnySpinner::Opencode(s) => s.advance(),
            AnySpinner::Crush(s) => s.advance(),
            AnySpinner::Classic(s) => s.advance(),
        }
    }

    /// Restart the birth animation, preserving the label/activity.
    pub fn reset(&mut self) {
        match self {
            AnySpinner::Bass(s) => s.reset(),
            AnySpinner::Runes(s) => s.reset(),
            AnySpinner::Opencode(s) => s.reset(),
            AnySpinner::Crush(s) => s.reset(),
            AnySpinner::Classic(s) => s.reset(),
        }
    }

    /// Switch the label to the given agent activity (and that status's own
    /// glyph sequence) without disturbing the animation clock.
    pub fn set_activity(&mut self, activity: crate::types::AgentActivity) {
        match self {
            AnySpinner::Bass(s) => s.set_activity(activity),
            AnySpinner::Runes(s) => s.set_activity(activity),
            AnySpinner::Opencode(s) => s.set_activity(activity),
            AnySpinner::Crush(s) => s.set_activity(activity),
            AnySpinner::Classic(s) => s.set_activity(activity),
        }
    }

    /// Update the active prompt-mode colour without resetting the animation.
    pub fn set_mode_color(&mut self, color: RGBA) {
        match self {
            AnySpinner::Bass(s) => s.set_mode_color(color),
            AnySpinner::Runes(s) => s.set_mode_color(color),
            AnySpinner::Opencode(s) => s.set_mode_color(color),
            AnySpinner::Crush(s) => s.set_mode_color(color),
            AnySpinner::Classic(s) => s.set_mode_color(color),
        }
    }

    /// Update the theme colours without resetting the animation.
    pub fn update_theme(&mut self, theme: &Theme) {
        match self {
            AnySpinner::Bass(s) => s.update_theme(theme),
            AnySpinner::Runes(s) => s.update_theme(theme),
            AnySpinner::Opencode(s) => s.update_theme(theme),
            AnySpinner::Crush(s) => s.update_theme(theme),
            AnySpinner::Classic(s) => s.update_theme(theme),
        }
    }

    /// Total width in terminal cells (animation + gap + label + glyph).
    pub fn width(&self) -> usize {
        match self {
            AnySpinner::Bass(s) => s.width(),
            AnySpinner::Runes(s) => s.width(),
            AnySpinner::Opencode(s) => s.width(),
            AnySpinner::Crush(s) => s.width(),
            AnySpinner::Classic(s) => s.width(),
        }
    }

    /// Whether the birth animation has finished.
    pub const fn is_initialized(&self) -> bool {
        match self {
            AnySpinner::Bass(s) => s.is_initialized(),
            AnySpinner::Runes(s) => s.is_initialized(),
            AnySpinner::Opencode(s) => s.is_initialized(),
            AnySpinner::Crush(s) => s.is_initialized(),
            AnySpinner::Classic(s) => s.is_initialized(),
        }
    }

    /// Render the spinner into the buffer at position `(x, y)`.
    pub fn render(&self, buf: &mut Buffer, x: u16, y: u16) {
        match self {
            AnySpinner::Bass(s) => s.render(buf, x, y),
            AnySpinner::Runes(s) => s.render(buf, x, y),
            AnySpinner::Opencode(s) => s.render(buf, x, y),
            AnySpinner::Crush(s) => s.render(buf, x, y),
            AnySpinner::Classic(s) => s.render(buf, x, y),
        }
    }

    /// Test-only view of the current label (every variant keeps one).
    #[cfg(test)]
    pub(crate) fn label_for_test(&self) -> &str {
        match self {
            AnySpinner::Bass(s) => s.label_for_test(),
            AnySpinner::Runes(s) => s.label_for_test(),
            AnySpinner::Opencode(s) => s.label_for_test(),
            AnySpinner::Crush(s) => s.label_for_test(),
            AnySpinner::Classic(s) => s.label_for_test(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosh::setup::SpinnerStyle;

    #[test]
    fn every_style_builds_and_reports_its_own_width() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();

        for style in SpinnerStyle::ALL {
            let mut spinner = AnySpinner::new("Working", &theme, style);
            assert!(spinner.width() > 0, "{style:?} must have width");
            assert!(
                !spinner.is_initialized(),
                "{style:?} must start at the birth window"
            );
            for _ in 0..40 {
                spinner.advance();
            }
            assert!(spinner.is_initialized(), "{style:?} must initialize");
            spinner.set_activity(crate::types::AgentActivity::Searching);
            assert_eq!(spinner.label_for_test(), "searching");
            spinner.set_mode_color(theme.primary);
            spinner.update_theme(&theme);
        }
    }

    #[test]
    fn bass_is_the_default_style() {
        assert_eq!(SpinnerStyle::default(), SpinnerStyle::Bass);
    }

    #[test]
    fn crush_is_its_own_component_not_the_runic_scramble() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();

        let runes = AnySpinner::new("Working", &theme, SpinnerStyle::Runes);
        let crush = AnySpinner::new("Working", &theme, SpinnerStyle::Crush);
        // The crush port scrambles 15 columns; the runic scramble 8 —
        // plus the 3-cell ellipsis slot after the label. ("Working" labels:
        // runes 8+1+7+1 = 17, crush 15+1+7+3 = 26.)
        assert_eq!(runes.width(), 17);
        assert_eq!(crush.width(), 26);
    }

    #[test]
    fn classic_cycles_the_shared_braille_frames() {
        let registry = crate::theme::ThemeRegistry::new();
        let theme = registry.get("opencode").cloned().unwrap();

        let mut classic = AnySpinner::new("W", &theme, SpinnerStyle::Classic);
        let before = classic.width();
        for _ in 0..10 {
            classic.advance();
        }
        // Still alive and laid out identically after advancing.
        assert_eq!(classic.width(), before);
    }

    #[test]
    fn style_round_trips_through_the_persisted_id() {
        for style in SpinnerStyle::ALL {
            assert_eq!(
                SpinnerStyle::from_id(style.id()),
                Some(style),
                "{style:?} id must round-trip"
            );
        }
        assert!(SpinnerStyle::from_id("nope").is_none());
    }
}
