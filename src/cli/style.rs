//! Terminal styling for the CLI screens (`--help`, `paths`, `update`).
//!
//! The palette is the user's **active theme** — the same [`Theme`] the TUI
//! paints with — converted to 24-bit ANSI escapes. Loading goes through
//! [`cosh::setup::Setup`], so `cosh --help` looks like the app the user
//! configured, without a second colour configuration to keep in sync.
//!
//! Colour is emitted only when it cannot hurt: stdout must be a terminal,
//! `NO_COLOR` must be unset and `TERM` must not be `dumb`. On Windows the
//! terminal must also advertise VT support (`WT_SESSION` or `TERM`) — plain
//! `cmd.exe` without those would print raw escape bytes instead of colours.

use cosh_tui::core::lib::rgba::RGBA;

use crate::theme::{Theme, ThemeRegistry};

/// ANSI reset, appended by every painted fragment.
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

/// The CLI's view of the theme: one [`Theme`], a colour on/off switch and the
/// layout width.
#[derive(Debug, Clone)]
pub struct Palette {
    theme: Theme,
    colors: bool,
    width: usize,
}

impl Palette {
    /// The palette for the current process: the theme selected in `setup.json`
    /// (registry default when unset or unknown) plus the terminal's colour and
    /// width capabilities.
    pub fn load() -> Self {
        let setup = cosh::setup::Setup::load();
        let registry = ThemeRegistry::new();
        let theme = registry
            .get(&setup.appearance.theme)
            .unwrap_or_else(|| registry.default_theme())
            .clone();
        Self::new(theme, colors_enabled())
    }

    /// Explicit constructor — the seam every rendering test uses so output
    /// never depends on the machine's `setup.json`.
    pub fn new(theme: Theme, colors: bool) -> Self {
        Self {
            theme,
            colors,
            width: layout_width(),
        }
    }

    /// The active theme, for the rare place that needs a colour by role
    /// rather than through one of the helpers below.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Whether ANSI colour is being emitted.
    pub fn colors(&self) -> bool {
        self.colors
    }

    /// Total width (in columns) the layout may occupy.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Foreground-paint `text` in `color`.
    pub fn paint(&self, color: RGBA, text: &str) -> String {
        if !self.colors || text.is_empty() {
            return text.to_string();
        }
        let (r, g, b, _) = color.to_ints();
        format!("\x1b[38;2;{r};{g};{b}m{text}{RESET}")
    }

    /// Foreground-paint `text` in `color`, bold.
    pub fn paint_bold(&self, color: RGBA, text: &str) -> String {
        if !self.colors || text.is_empty() {
            return text.to_string();
        }
        let (r, g, b, _) = color.to_ints();
        format!("\x1b[38;2;{r};{g};{b}m{BOLD}{text}{RESET}")
    }

    /// The product/heading voice: bold primary.
    pub fn title(&self, text: &str) -> String {
        self.paint_bold(self.theme.primary, text)
    }

    /// Section headers: bold secondary.
    pub fn heading(&self, text: &str) -> String {
        self.paint_bold(self.theme.secondary, text)
    }

    /// Command/flag names and other emphasised body text.
    pub fn item(&self, text: &str) -> String {
        self.paint(self.theme.primary, text)
    }

    /// Regular prose.
    pub fn body(&self, text: &str) -> String {
        self.paint(self.theme.text, text)
    }

    /// Secondary prose: notes, hints, the repository link.
    pub fn muted(&self, text: &str) -> String {
        self.paint(self.theme.text_muted, text)
    }

    /// Frame and rule characters.
    pub fn border(&self, text: &str) -> String {
        self.paint(self.theme.border, text)
    }

    /// Success lines (`cosh vX is up to date`).
    pub fn success(&self, text: &str) -> String {
        self.paint(self.theme.success, text)
    }

    /// Failure lines.
    pub fn error(&self, text: &str) -> String {
        self.paint(self.theme.error, text)
    }

    /// Warnings and "nothing to do" lines.
    pub fn warning(&self, text: &str) -> String {
        self.paint(self.theme.warning, text)
    }
}

/// Whether ANSI colour is safe to emit on stdout.
pub fn colors_enabled() -> bool {
    use std::io::IsTerminal;

    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return false;
    }
    #[cfg(windows)]
    {
        // A VT-capable terminal on Windows always sets one of these (Windows
        // Terminal sets WT_SESSION, Git Bash/mintty sets TERM). Legacy
        // conhost sets neither and would show the escapes literally.
        return std::env::var_os("WT_SESSION").is_some() || std::env::var_os("TERM").is_some();
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Layout width: the terminal's, clamped so the help page keeps readable line
/// lengths (a 200-column terminal must not stretch a description across it).
pub fn layout_width() -> usize {
    use std::io::IsTerminal;

    const MIN: usize = 56;
    const MAX: usize = 84;
    if !std::io::stdout().is_terminal() {
        return MAX;
    }
    let columns = crossterm::terminal::size()
        .map(|(columns, _)| usize::from(columns))
        .unwrap_or(MAX);
    columns.clamp(MIN, MAX)
}

/// Display width of `text`, ignoring ANSI escapes (which occupy no columns).
pub fn display_width(text: &str) -> usize {
    use unicode_width::UnicodeWidthStr;

    let mut width = 0;
    let mut rest = text;
    while let Some(start) = rest.find('\x1b') {
        width += UnicodeWidthStr::width(&rest[..start]);
        // Skip the escape sequence itself: `ESC [ … m`.
        let Some(end) = rest[start..].find('m') else {
            break;
        };
        rest = &rest[start + end + 1..];
    }
    width + UnicodeWidthStr::width(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_is_a_no_op_without_colour() {
        let palette = Palette::new(ThemeRegistry::new().default_theme().clone(), false);
        assert_eq!(palette.title("cosh"), "cosh");
        assert_eq!(palette.error("boom"), "boom");
    }

    #[test]
    fn paint_emits_truecolor_and_resets() {
        let palette = Palette::new(ThemeRegistry::new().default_theme().clone(), true);
        let painted = palette.body("x");
        assert!(painted.starts_with("\x1b[38;2;"), "{painted:?}");
        assert!(painted.ends_with(RESET), "{painted:?}");
    }

    #[test]
    fn display_width_ignores_escapes() {
        let palette = Palette::new(ThemeRegistry::new().default_theme().clone(), true);
        let painted = palette.title("update");
        assert_eq!(display_width(&painted), 6);
        assert_eq!(display_width("plain"), 5);
    }
}
