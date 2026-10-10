//! Brand palettes of the supported ACP sub-agents.
//!
//! The TUI paints each `/agents` pane header with the agent's own logo
//! colors. The palettes here were sampled from each agent's official brand
//! material (brand pages, published logo SVGs, theme files of the tools
//! themselves — see the per-agent doc comments for the sources) and are
//! stored as plain RGB triples so this crate stays independent of the TUI
//! crate: the caller converts them into its own color type.
//!
//! Real logos carry one to four colors and some roles do not exist for
//! every agent, so the non-essential fields are `Option`s. A name the
//! catalog does not know (a user-configured agent, a future CLI) falls
//! back through [`palette_for`] to `None` — the caller then paints with
//! its own theme instead of inventing a brand.

/// One agent's logo palette, in plain RGB.
///
/// `primary` is the color that dominates the logo (the one the wordmark or
/// glyph is read in), `secondary` the next-largest surface. The remaining
/// roles exist only for the marks that actually use them, which is why they
/// are optional: a monochrome mark (OpenAI's Codex) fills none of them,
/// Google's gradient mark fills all of them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgentPalette {
    /// The dominant logo color — what the agent's name is painted in.
    pub primary: Rgb,
    /// The next-largest logo surface, when the mark has one.
    pub secondary: Option<Rgb>,
    /// A third brand color (gradient stop, tertiary accent), when any.
    pub tertiary: Option<Rgb>,
    /// The fourth brand color, when the mark is a full gradient.
    pub quaternary: Option<Rgb>,
    /// The color the brand itself writes its name in on a dark surface —
    /// for the monochrome marks this IS the brand color.
    pub accent: Option<Rgb>,
    /// For TWO-TONE wordmarks: the fraction of the name (0.0..=1.0) painted
    /// with the primary before the glyphs switch to the secondary — a hard
    /// per-word split, NOT a gradient. OpenCode's footer wordmark reads
    /// "Open" in the bright tone and "Code" in the gray one, with no blend
    /// between the words. `None` for gradient marks (Gemini's sparkle) and
    /// single-color marks, which sweep/hold continuously instead.
    pub hard_stop: Option<f32>,
}

/// A plain 8-bit RGB triple. Deliberately NOT the TUI's `RGBA`: this crate
/// must not depend on the TUI (the dependency points the other way), and a
/// bare triple converts into any color type with one constructor call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel, 0–255.
    pub r: u8,
    /// Green channel, 0–255.
    pub g: u8,
    /// Blue channel, 0–255.
    pub b: u8,
}

impl Rgb {
    /// Build an RGB triple from hex like `"4285F4"` (with or without `#`).
    #[must_use]
    pub const fn from_parts(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl AgentPalette {
    /// Sample the palette at `t` in `0.0..=1.0` across the mark's colors —
    /// the walk from the first stop to the last, interpolated per channel.
    /// This is what paints a "mini logo" from a name: one call per glyph
    /// column with `t` walking left to right gives the gradient sweep of
    /// multi-color marks (Gemini's sparkle, Devin's tri-color), while
    /// single-color marks (Claude, Kilo, Cline) return the same color for
    /// every `t`.
    ///
    /// A two-tone wordmark (`hard_stop`) never interpolates at all: glyphs
    /// left of the stop take the primary, glyphs at/after it take the
    /// secondary — the hard per-word split the brand actually renders
    /// (OpenCode's "Open|Code"), not a blend through it.
    ///
    /// With a single color the result is that color; with several, `t`
    /// walks primary → secondary → tertiary → quaternary across the slots
    /// the mark actually defines, so an agent with two colors interpolates
    /// between just those two.
    #[must_use]
    pub fn sample(&self, t: f32) -> Rgb {
        // Hard two-tone split: NO blending at the boundary.
        if let Some(stop) = self.hard_stop {
            let stop = stop.clamp(0.0, 1.0);
            let secondary = self.secondary.unwrap_or(self.primary);
            return if t.clamp(0.0, 1.0) < stop {
                self.primary
            } else {
                secondary
            };
        }
        let stops = [
            Some(self.primary),
            self.secondary,
            self.tertiary,
            self.quaternary,
        ];
        let defined: Vec<Rgb> = stops.into_iter().flatten().collect();
        let last = defined.len().saturating_sub(1) as f32;
        let position = t.clamp(0.0, 1.0) * last;
        let index = (position as usize).min(defined.len().saturating_sub(1));
        let fraction = position - index as f32;
        let start = defined[index];
        let end = defined[(index + 1).min(defined.len().saturating_sub(1))];
        let mix = |a: u8, b: u8| -> u8 {
            (f32::from(a) * (1.0 - fraction) + f32::from(b) * fraction).round() as u8
        };
        Rgb::from_parts(
            mix(start.r, end.r),
            mix(start.g, end.g),
            mix(start.b, end.b),
        )
    }
}

/// The catalog of supported agents' palettes, keyed by the same names the
/// ACP registry uses (`acp::ACP_AGENTS`). Only the agents with a real,
/// distinctive logo identity carry an entry.
const PALETTES: &[(&str, AgentPalette)] = &[
    // Google Gemini: the sparkle gradient (blue → violet → coral) is the
    // brand fingerprint; the gradient is identical on light and dark.
    // The final "i" shares the same color as the "n" for harmony.
    // Sources: Gemini product brand gradient, Google blue 500 #4285F4,
    // Gemini violet #9B72CB, coral #D96570.
    (
        "gemini",
        AgentPalette {
            primary: Rgb::from_parts(0x42, 0x85, 0xF4),
            secondary: Some(Rgb::from_parts(0x9B, 0x72, 0xCB)),
            tertiary: Some(Rgb::from_parts(0xD9, 0x65, 0x70)),
            quaternary: Some(Rgb::from_parts(0xD9, 0x65, 0x70)),
            accent: None,
            hard_stop: None,
        },
    ),
    // Block's goose: the mark is LITERALLY a black goose — pure #000000.
    // On the pane's dark background the TUI's contrast floor lifts it
    // toward the readable foreground instead of letting it dissolve.
    (
        "goose",
        AgentPalette {
            primary: Rgb::from_parts(0x00, 0x00, 0x00),
            secondary: None,
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
    // OpenCode: the logo is gray and white, nothing else — sampled from
    // the official repo's own assets (`packages/console/app/src/asset/
    // brand/opencode-logo-dark.svg`). The footer wordmark is a HARD
    // two-tone and the DARK part comes first: "Open" in the muted gray,
    // "Code" in the bright warm white — no gradient between the words,
    // hence the 0.5 hard stop with primary = gray.
    (
        "opencode",
        AgentPalette {
            primary: Rgb::from_parts(0x4B, 0x46, 0x46),
            secondary: Some(Rgb::from_parts(0xF1, 0xEC, 0xEC)),
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: Some(0.5),
        },
    ),
    // Kilo Code: the acid yellow-green carries the entire identity (logo
    // tile, headlines, CTAs) on near-black — one expressive color, no
    // supporting cast.
    (
        "kilo",
        AgentPalette {
            primary: Rgb::from_parts(0xF8, 0xF6, 0x76),
            secondary: None,
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
    // Cline: the mark is black, same call as Block's goose — the violet
    // seen in marketing materials is the product UI's accent, not the
    // logo. On the pane's dark background the TUI's contrast floor lifts
    // it toward the readable foreground instead of letting it dissolve.
    (
        "cline",
        AgentPalette {
            primary: Rgb::from_parts(0x00, 0x00, 0x00),
            secondary: None,
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
    // Devin (Cognition): the three fixed fills of the official color
    // logomark — blue, green, cyan-blue. Source: devin.ai/brand assets.
    (
        "devin",
        AgentPalette {
            primary: Rgb::from_parts(0x39, 0x69, 0xCA),
            secondary: Some(Rgb::from_parts(0x21, 0xC1, 0x9A)),
            tertiary: Some(Rgb::from_parts(0x02, 0x94, 0xDE)),
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
    // Claude (Anthropic): the terracotta starburst, never recolored — the
    // mark is exactly brand orange #D97757. The muted blue and green of
    // the published palette are product accents, NOT mark colors, so they
    // stay out: the name paints solid terracotta.
    (
        "claude",
        AgentPalette {
            primary: Rgb::from_parts(0xD9, 0x77, 0x57),
            secondary: None,
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
    // OpenAI Codex: the logo uses a medium white (not stark white).
    (
        "codex",
        AgentPalette {
            primary: Rgb::from_parts(0xE8, 0xE8, 0xE8),
            secondary: None,
            tertiary: None,
            quaternary: None,
            accent: None,
            hard_stop: None,
        },
    ),
];

/// The palette of agent `name`, or `None` when the catalog does not know
/// the agent (the caller paints with its own theme instead of inventing a
/// brand). Matching is case-insensitive so `Claude`, `CLAUDE` and
/// `claude` all resolve.
#[must_use]
pub fn palette_for(name: &str) -> Option<AgentPalette> {
    let lowered = name.to_ascii_lowercase();
    PALETTES
        .iter()
        .find(|(agent, _)| *agent == lowered)
        .map(|(_, palette)| *palette)
}

/// The brand's own casing of an agent's name, for display. The ACP ids are
/// lowercase protocol identifiers (`opencode`), but the wordmarks are not —
/// OpenCode brands itself "OpenCode". The lowercase id stays the dispatch
/// key; this is only what the header paints. Unknown agents display as
/// their id unchanged.
#[must_use]
pub fn display_name(agent: &str) -> &str {
    if agent.eq_ignore_ascii_case("opencode") {
        "OpenCode"
    } else {
        agent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_acp_agent_has_a_palette() {
        for agent in crate::subagent::acp::ACP_AGENTS {
            let palette =
                palette_for(agent.name).unwrap_or_else(|| panic!("{} has no palette", agent.name));
            // Pure black BY ACCIDENT is the failure mode (an unsampled
            // placeholder). goose and cline are INTENTIONALLY black — the
            // marks are literally black — and the TUI's contrast floor
            // lifts them on dark surfaces, so each is pinned by its own
            // test below.
            let is_black = palette.primary == Rgb::from_parts(0, 0, 0);
            if !matches!(agent.name, "goose" | "cline") {
                assert!(!is_black, "{} must not resolve to pure black by accident", agent.name);
            }
        }
    }

    #[test]
    fn unknown_agents_resolve_to_none() {
        assert!(palette_for("cosh").is_none());
        assert!(palette_for("definitely-not-an-agent").is_none());
        assert!(palette_for("").is_none());
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(palette_for("Claude"), palette_for("claude"));
        assert_eq!(palette_for("CLAUDE"), palette_for("claude"));
        assert_eq!(palette_for("OpenCode"), palette_for("opencode"));
    }

    #[test]
    fn claude_carries_the_brand_terracotta() {
        let palette = palette_for("claude").expect("claude palette");
        assert_eq!(palette.primary, Rgb::from_parts(0xD9, 0x77, 0x57));
    }

    #[test]
    fn codex_carries_a_medium_white() {
        let palette = palette_for("codex").expect("codex palette");
        // The logo uses a medium white, not stark white.
        assert_eq!(palette.primary, Rgb::from_parts(0xE8, 0xE8, 0xE8));
        assert!(palette.secondary.is_none());
        assert!(palette.tertiary.is_none());
        assert!(palette.quaternary.is_none());
        assert!(palette.accent.is_none());
    }

    #[test]
    fn goose_is_the_black_mark_by_decision() {
        let palette = palette_for("goose").expect("goose palette");
        assert_eq!(palette.primary, Rgb::from_parts(0x00, 0x00, 0x00));
    }

    #[test]
    fn opencode_is_gray_and_white_nothing_else() {
        let palette = palette_for("opencode").expect("opencode palette");
        // Sampled from the official repo's logo-dark.svg, in WORDMARK
        // order: "Open" in the muted gray #4B4646 FIRST, "Code" in the
        // warm white #F1ECEC — the dark part leads, no other colors.
        assert_eq!(palette.primary, Rgb::from_parts(0x4B, 0x46, 0x46));
        assert_eq!(palette.secondary, Some(Rgb::from_parts(0xF1, 0xEC, 0xEC)));
        assert!(palette.tertiary.is_none());
        assert!(palette.quaternary.is_none());
    }

    #[test]
    fn gemini_carries_the_full_sparkle_gradient() {
        let palette = palette_for("gemini").expect("gemini palette");
        assert!(palette.secondary.is_some());
        assert!(palette.tertiary.is_some());
        assert!(palette.quaternary.is_some());
    }

    #[test]
    fn sample_walks_single_color_marks_constant() {
        let palette = palette_for("claude").expect("claude palette");
        // Claude's mark is one color: every t samples the terracotta.
        for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(palette.sample(t), palette.primary, "t={t}");
        }
    }

    #[test]
    fn sample_walks_the_two_color_marks_end_to_end() {
        let palette = palette_for("cline").expect("cline palette");
        assert_eq!(palette.sample(0.0), palette.primary);
        assert_eq!(palette.sample(1.0), palette.primary);
    }

    #[test]
    fn opencode_two_tone_splits_hard_not_a_gradient() {
        let palette = palette_for("opencode").expect("opencode palette");
        let stop = palette.hard_stop.expect("opencode is two-tone");
        // Left of the stop: the muted gray. At/after it: the bright warm
        // white. NO intermediate blend anywhere — every sample is one of
        // the two exact brand colors.
        assert_eq!(palette.sample(0.0), palette.primary);
        assert_eq!(palette.sample(stop - 0.01), palette.primary);
        assert_eq!(palette.sample(stop), palette.secondary.unwrap());
        assert_eq!(palette.sample(0.75), palette.secondary.unwrap());
        assert_eq!(palette.sample(1.0), palette.secondary.unwrap());
        // For "opencode" at stop 0.5 the glyph colors walk the wordmark:
        // "Open" (t < 0.5) gray, "Code" (t >= 0.5) bright.
        let name = "opencode";
        let last = name.len() as f32 - 1.0;
        let tones: Vec<Rgb> = (0..name.len())
            .map(|i| palette.sample(f32::from(i as u16) / last))
            .collect();
        assert_eq!(tones[0], palette.primary, "'O' is gray");
        assert_eq!(tones[3], palette.primary, "'n' of Open is gray");
        assert_eq!(
            tones[4],
            palette.secondary.unwrap(),
            "'c' of Code is bright"
        );
        assert_eq!(
            tones[name.len() - 1],
            palette.secondary.unwrap(),
            "final 'e' is bright"
        );
    }

    #[test]
    fn display_name_keeps_the_brand_casing() {
        // The ACP id is lowercase; the wordmark is CamelCase.
        assert_eq!(display_name("opencode"), "OpenCode");
        assert_eq!(display_name("OpenCode"), "OpenCode");
        // Everything else passes through unchanged (ids ARE the brands:
        // claude, kilo, codex, goose, devin, gemini, cline).
        assert_eq!(display_name("claude"), "claude");
        assert_eq!(display_name("cosh"), "cosh");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn sample_interpolates_between_gradient_stops() {
        let palette = palette_for("gemini").expect("gemini palette");
        // The sweep starts at primary (Google blue) and ends at the last
        // defined stop (coral, same as tertiary so the final "i" matches "n").
        let start = palette.sample(0.0);
        assert_eq!(start, palette.primary);
        let end = palette.sample(1.0);
        assert_eq!(end, palette.quaternary.expect("gemini quaternary"));
        assert_eq!(end, palette.tertiary.expect("gemini tertiary"), "final i matches n");
        // A midpoint between the first two stops is BETWEEN their channels.
        let blue = palette.primary;
        let violet = palette.secondary.expect("gemini secondary");
        let mid = palette.sample(0.25);
        let between = |a: u8, b: u8, v: u8| v >= a.min(b) && v <= a.max(b);
        assert!(between(blue.r, violet.r, mid.r));
        assert!(between(blue.g, violet.g, mid.g));
        assert!(between(blue.b, violet.b, mid.b));
    }

    #[test]
    fn sample_clamps_out_of_range_t() {
        let palette = palette_for("gemini").expect("gemini palette");
        assert_eq!(palette.sample(-1.0), palette.sample(0.0));
        assert_eq!(palette.sample(2.0), palette.sample(1.0));
    }
}
