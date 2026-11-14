//! The `--help` pages.
//!
//! Every page renders inside one frame: the logo's "O" in the top-right
//! corner with the running version beside it, a title row on the left, one
//! section per concern, and a footer with the repository. Colours
//! come from [`Palette`], so the page carries the user's own theme.
//!
//! The layout is plain text first — padding is computed on the visible width,
//! then fragments are painted — so a monochrome terminal gets the exact same
//! alignment without the escape bytes.

use crate::cli::VERSION;
use crate::cli::style::{Palette, display_width};
use crate::logo::{O_GLYPH, O_GLYPH_W};

/// Width reserved for the left column of an item row.
const LABEL_WIDTH: usize = 23;
/// Indentation of rows inside a section.
const ROW_INDENT: usize = 2;

/// One command of [`crate::cli::COMMANDS`], with its one-line summary. The
/// overview and the `help` page both read this table, and a guard test keeps it
/// in lockstep with the parser's command list.
const COMMAND_HELP: &[(&str, &str)] = &[
    (
        "update [version]",
        "Install the latest release, or a pinned version",
    ),
    ("paths", "Print the data, config and log directories"),
    ("help [command]", "Show this help, or one command's page"),
];

/// A row of a section.
enum Row {
    /// Usage or explanation text, wrapped and painted as body.
    Prose(&'static str),
    /// A two-column row: the emphasised left cell and its description.
    Item(&'static str, &'static str),
    /// An example: the command (emphasised) and a comment (muted).
    Example(&'static str, &'static str),
    /// Secondary prose (notes, hints), painted muted.
    Note(&'static str),
}

struct Section {
    title: &'static str,
    rows: &'static [Row],
}

struct Page {
    title: &'static str,
    sections: &'static [Section],
}

// ── Pages ────────────────────────────────────────────────────────────────

const COMMAND_ROWS: &[Row] = &[
    Row::Item(COMMAND_HELP[0].0, COMMAND_HELP[0].1),
    Row::Item(COMMAND_HELP[1].0, COMMAND_HELP[1].1),
    Row::Item(COMMAND_HELP[2].0, COMMAND_HELP[2].1),
];

const OVERVIEW: Page = Page {
    title: "co-sh",
    sections: &[
        Section {
            title: "Usage",
            rows: &[
                Row::Prose("cosh [options]"),
                Row::Prose("cosh <command> [options]"),
            ],
        },
        Section {
            title: "Session",
            rows: &[
                Row::Item(
                    "-c, --continue",
                    "Start in the most recently active session of this directory",
                ),
                Row::Item(
                    "-s, --session <id>",
                    "Start in a specific session of this directory",
                ),
            ],
        },
        Section {
            title: "Commands",
            rows: COMMAND_ROWS,
        },
        Section {
            title: "Other",
            rows: &[
                Row::Item("-h, --help", "Show this help"),
                Row::Item("-v, --version", "Print the version"),
                Row::Item("--check-keyring", "Report API-key storage status"),
            ],
        },
        Section {
            title: "Examples",
            rows: &[
                Row::Example("cosh -c", "continue where you left off"),
                Row::Example("cosh -s 1788063765396", "open a specific session"),
                Row::Example("cosh update", "install the latest release"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[
                Row::Note(
                    "Sessions are stored per directory: -c and -s only see sessions started in the current one.",
                ),
                Row::Note("Run `cosh help <command>` for one command's page."),
            ],
        },
    ],
};

const UPDATE_PAGE: Page = Page {
    title: "cosh update",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("cosh update [version] [--yes]")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Installs a release through the official installer for this OS and variant, replacing the binaries next to the running cosh. With no version, targets the latest GitHub release.",
            )],
        },
        Section {
            title: "Arguments",
            rows: &[Row::Item(
                "version",
                "Release to install, `v0.2.3` or `0.2.3`; the latest when omitted",
            )],
        },
        Section {
            title: "Flags",
            rows: &[
                Row::Item(
                    "--yes, -y",
                    "Skip the confirmation prompt (required without a terminal)",
                ),
                Row::Item("-h, --help", "Show this page"),
            ],
        },
        Section {
            title: "Examples",
            rows: &[
                Row::Example("cosh update", "install the latest release"),
                Row::Example("cosh update v0.2.3", "pin a specific release"),
                Row::Example("cosh update --yes", "unattended: scripts and containers"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[
                Row::Note(
                    "The update replaces the running binary; start cosh again when it finishes.",
                ),
                Row::Note("Set COSH_BIN_DIR to install into a different directory."),
            ],
        },
    ],
};

const PATHS_PAGE: Page = Page {
    title: "cosh paths",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("cosh paths")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Prints the directories cosh reads and writes, one per line, so other tools can use them.",
            )],
        },
        Section {
            title: "Prints",
            rows: &[
                Row::Item("data", "sessions, usage cache, telemetry queue"),
                Row::Item("config", "setup.json and its lock"),
                Row::Item("logs", "stdout.log and laya.log"),
                Row::Item("sessions", "this directory's session histories"),
                Row::Item("binary", "the running executable"),
            ],
        },
        Section {
            title: "Notes",
            rows: &[Row::Note(
                "COSH_DATA_DIR and COSH_CONFIG_DIR override the first two.",
            )],
        },
    ],
};

const HELP_PAGE: Page = Page {
    title: "cosh help",
    sections: &[
        Section {
            title: "Usage",
            rows: &[Row::Prose("cosh help [command]")],
        },
        Section {
            title: "Description",
            rows: &[Row::Prose(
                "Prints the top-level help, or the page of the named command.",
            )],
        },
        Section {
            title: "Commands",
            rows: COMMAND_ROWS,
        },
        Section {
            title: "Examples",
            rows: &[Row::Example("cosh help update", "the update page")],
        },
    ],
};

/// Render the help page for `subject` (`None` = the top-level page).
pub fn render(subject: Option<&str>, palette: &Palette) -> String {
    page_for(subject).render(palette)
}

fn page_for(subject: Option<&str>) -> &'static Page {
    match subject {
        Some("update") => &UPDATE_PAGE,
        Some("paths") => &PATHS_PAGE,
        Some("help") => &HELP_PAGE,
        _ => &OVERVIEW,
    }
}

// ── Rendering ────────────────────────────────────────────────────────────

impl Page {
    fn render(&self, palette: &Palette) -> String {
        // `│ ` + content + ` │`
        let outer = palette.width();
        let inner = outer - 4;
        let rule = "─".repeat(inner + 2);
        let mut out = String::new();

        out.push_str(&format!("{}\n", palette.border(&format!("┌{rule}┐"))));

        // Brand mark: the logo's "O" in the top-right corner. The title carries
        // the running version where "terminal coding agent" used to be, and the
        // first section ("Usage") plus its rows fill the mark's remaining lines
        // so no empty row is left over.
        let version = format!("v{VERSION}");
        let mark_indent = inner.saturating_sub(O_GLYPH_W);
        let title_line = format!("{} · {}", self.title, version);
        let title_line_width = display_width(&title_line);
        let title_line_pad = " ".repeat(mark_indent.saturating_sub(title_line_width));
        let first_heading = self.sections.first().map(|s| s.title).unwrap_or("");
        let heading_width = display_width(first_heading);
        let heading_pad = " ".repeat(mark_indent.saturating_sub(heading_width));
        // Inline text for the first section's first two rows (Prose only).
        let first_inline: Vec<Option<String>> = self
            .sections
            .first()
            .map(|s| {
                s.rows
                    .iter()
                    .map(|r| match r {
                        Row::Prose(t) => Some(format!("{}{}", " ".repeat(ROW_INDENT), t)),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (index, row) in O_GLYPH.iter().enumerate() {
            let left = match index {
                0 => format!(
                    "{}{}",
                    palette.title(&title_line),
                    title_line_pad
                ),
                1 => format!("{}{}", palette.heading(first_heading), heading_pad),
                2 => first_inline
                    .first()
                    .and_then(|o| o.as_ref())
                    .map(|t| {
                        format!("{}{}", palette.body(t), " ".repeat(mark_indent.saturating_sub(display_width(t))))
                    })
                    .unwrap_or_else(|| " ".repeat(mark_indent)),
                3 => first_inline
                    .get(1)
                    .and_then(|o| o.as_ref())
                    .map(|t| {
                        format!("{}{}", palette.body(t), " ".repeat(mark_indent.saturating_sub(display_width(t))))
                    })
                    .unwrap_or_else(|| " ".repeat(mark_indent)),
                _ => " ".repeat(mark_indent),
            };
            out.push_str(&framed(
                palette,
                &format!("{left}{}", palette.title(row)),
                inner,
                inner,
            ));
        }

        for (index, section) in self.sections.iter().enumerate() {
            if index == 0 {
                // Heading and rows already drawn inside the mark block.
                continue;
            }
            out.push_str(&framed(palette, "", 0, inner));
            out.push_str(&framed(
                palette,
                &palette.heading(section.title),
                display_width(section.title),
                inner,
            ));
            for row in section.rows {
                out.push_str(&self.render_row(row, palette, inner));
            }
        }

        // Footer: where the project lives, for anyone who wants the source.
        let repo = format!("https://github.com/{}", cosh::update::REPO);
        out.push_str(&format!("{}\n", palette.border(&format!("├{rule}┤"))));
        out.push_str(&framed(
            palette,
            &palette.muted(&repo),
            display_width(&repo),
            inner,
        ));
        out.push_str(&format!("{}\n", palette.border(&format!("└{rule}┘"))));
        out
    }

    fn render_row(&self, row: &Row, palette: &Palette, inner: usize) -> String {
        let indent = " ".repeat(ROW_INDENT);
        match row {
            Row::Prose(text) => wrapped(text, |palette, line| palette.body(line), palette, inner, &indent),
            Row::Note(text) => wrapped(text, |palette, line| palette.muted(line), palette, inner, &indent),
            Row::Item(left, desc) => {
                let desc_width = inner.saturating_sub(ROW_INDENT + LABEL_WIDTH);
                wrap(desc, desc_width)
                    .into_iter()
                    .enumerate()
                    .map(|(line_index, line)| {
                        let cell = if line_index == 0 { *left } else { "" };
                        let padding = LABEL_WIDTH.saturating_sub(display_width(cell));
                        let plain =
                            format!("{indent}{cell}{}{line}", " ".repeat(padding));
                        framed(
                            palette,
                            &format!(
                                "{indent}{}{}{}",
                                palette.item(cell),
                                " ".repeat(padding),
                                palette.body(&line)
                            ),
                            display_width(&plain),
                            inner,
                        )
                    })
                    .collect()
            }
            Row::Example(command, comment) => {
                let command_width = 30.min(inner.saturating_sub(ROW_INDENT + 4));
                let left = format!("{indent}{command}");
                let padding = command_width.saturating_sub(display_width(command));
                let plain = format!("{left}{}{comment}", " ".repeat(padding));
                framed(
                    palette,
                    &format!("{left}{}{}", " ".repeat(padding), palette.muted(comment)),
                    display_width(&plain),
                    inner,
                )
            }
        }
    }
}

/// Wrap `text` and frame every line, painted with `paint`.
fn wrapped(
    text: &str,
    paint: fn(&Palette, &str) -> String,
    palette: &Palette,
    inner: usize,
    indent: &str,
) -> String {
    wrap(text, inner.saturating_sub(ROW_INDENT))
        .into_iter()
        .map(|line| {
            let plain = format!("{indent}{line}");
            framed(
                palette,
                &format!("{indent}{}", paint(palette, &line)),
                display_width(&plain),
                inner,
            )
        })
        .collect()
}

/// One framed line: `│ ` + `content` + padding + ` │`, `content` already
/// painted and `content_width` its visible width.
fn framed(palette: &Palette, content: &str, content_width: usize, inner: usize) -> String {
    let padding = " ".repeat(inner.saturating_sub(content_width));
    format!(
        "{}{content}{}{}\n",
        palette.border("│ "),
        padding,
        palette.border(" │")
    )
}

/// Word-wrap `text` at `width` columns. Words longer than the width are kept
/// whole — a URL or a version never gets split mid-token.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate =
            display_width(&current) + usize::from(!current.is_empty()) + display_width(word);
        if !current.is_empty() && candidate > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::COMMANDS;
    use crate::theme::ThemeRegistry;

    fn palette() -> Palette {
        Palette::new(ThemeRegistry::new().default_theme().clone(), false)
    }

    fn plain(subject: Option<&str>) -> String {
        render(subject, &palette())
    }

    #[test]
    fn every_command_is_documented_and_has_a_page() {
        for command in COMMANDS {
            assert!(
                COMMAND_HELP
                    .iter()
                    .any(|(label, _)| label.split(' ').next() == Some(*command)),
                "command `{command}` is missing from COMMAND_HELP"
            );
            let page = plain(Some(command));
            assert!(
                page.contains(&format!("cosh {command}")),
                "the `{command}` page does not name the command"
            );
        }
    }

    #[test]
    fn the_overview_lists_every_command_and_flag() {
        let page = plain(None);
        for needle in [
            "update [version]",
            "paths",
            "help [command]",
            "-c, --continue",
            "-s, --session <id>",
            "-v, --version",
            "-h, --help",
            "--check-keyring",
            "co-sh",
            VERSION,
        ] {
            assert!(page.contains(needle), "the overview is missing {needle:?}");
        }
    }

    #[test]
    fn the_logo_mark_sits_in_the_top_right_corner() {
        let page = plain(None);
        let lines: Vec<&str> = page.lines().collect();
        // The first four rows inside the frame are the O, flush against the
        // right border, with the version on its left.
        for (index, row) in O_GLYPH.iter().enumerate() {
            let line = lines[index + 1];
            assert!(
                line.contains(row.trim_end()) && line.trim_end().ends_with('│'),
                "row {index} does not carry the mark: {line:?}"
            );
        }
        // Title + version on the first row, Usage on the second, and the two
        // Usage rows on the third and fourth, so no empty row is left over.
        assert!(lines[1].contains("co-sh"));
        assert!(lines[1].contains(&format!("v{VERSION}")));
        assert!(lines[2].contains("Usage"));
        assert!(lines[3].contains("cosh [options]"));
        assert!(lines[4].contains("cosh <command> [options]"));
    }

    #[test]
    fn the_frame_stays_aligned_and_within_the_layout_width() {
        let page = plain(None);
        let widths: Vec<usize> = page.lines().map(display_width).collect();
        let expected = widths[0];
        assert!(
            widths.iter().all(|width| *width == expected),
            "ragged frame: {widths:?}"
        );
        assert!(expected <= palette().width());

        for line in page.lines() {
            assert!(
                line.starts_with(['│', '┌', '├', '└']),
                "row escapes the frame: {line:?}"
            );
            assert!(
                line.ends_with(['│', '┐', '┤', '┘']),
                "row escapes the frame: {line:?}"
            );
        }
    }

    #[test]
    fn coloured_output_keeps_the_same_visible_width() {
        let coloured = Palette::new(ThemeRegistry::new().default_theme().clone(), true);
        let widths: Vec<usize> = render(None, &coloured)
            .lines()
            .map(display_width)
            .collect();
        assert!(widths.iter().all(|width| *width == widths[0]), "{widths:?}");
    }

    #[test]
    fn wrap_keeps_long_tokens_whole() {
        let lines = wrap("see https://example.com/a/very/long/path", 12);
        assert_eq!(lines[0], "see");
        assert_eq!(lines[1], "https://example.com/a/very/long/path");
    }
}
