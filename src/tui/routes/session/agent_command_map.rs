//! Client-side mapping between an agent's OWN slash-command names and the
//! cosh-standardized UI they open.
//!
//! A harness publishes its commands through ACP
//! (`available_commands_update`) with ITS OWN spellings — `/models` on one,
//! `/model` on another, `/llm` on a third — and the slash menu shows those
//! names verbatim. What gets standardized is what happens AFTER the user
//! picks one: equivalent commands across harnesses open the SAME kind of
//! cosh box (the model list, the provider list …),
//! filled with that agent's own ACP config options.
//!
//! Three buckets, resolved by [`resolve`]:
//!
//! 1. **Config-box groups** ([`CommandGroup::target`]) — the command opens
//!    the standardized select box bound to one of the session's ACP config
//!    options (matched by id or by category, since harnesses predate
//!    categories: the model option may be `model` or only carry the
//!    `Model` category).
//! 2. **Blocked** ([`is_blocked`]) — commands that make no sense inside
//!    `/agents` (yolo / bypass / permission modes: the split already runs
//!    auto-approve, so exposing the toggle would only fight the harness)
//!    are filtered OUT of the menu entirely.
//! 3. **Direct** — everything else falls through to the group-less base:
//!    the command is sent to the pane AS the prompt (`/name args…`) and the
//!    harness interprets it. No per-agent maintenance: an unmapped command
//!    still appears and still works.

/// Which ACP session config option a standardized box binds to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTarget {
    /// Match an option by its exact id (e.g. `provider` — no category
    /// exists for it in the spec).
    ById(&'static str),
    /// Match an option by its semantic category (e.g. `model` — harnesses
    /// predate the category system, so the id alone is not stable).
    ByCategory(&'static str),
}

/// One mapped command group: every harness spelling that means the same
/// thing, plus the standardized box it opens.
#[derive(Debug, Clone, Copy)]
pub struct CommandGroup {
    /// Stable group id (logs/tests).
    pub id: &'static str,
    /// Human title of the standardized box.
    pub title: &'static str,
    /// Human one-liner for the synthesized menu entry (see
    /// [`synthesized_group_commands`]).
    pub desc: &'static str,
    /// Equivalent command spellings across harnesses, normalized
    /// (lowercase, `-`/space folded to `_`, no leading slash).
    pub names: &'static [&'static str],
    /// The config option the standardized box binds to. `None` for
    /// direct-passthrough groups (none yet — every mapped group has a box).
    pub target: Option<ConfigTarget>,
}

/// The mapped groups. A group is found by normalizing the agent's command
/// name and looking it up in every group's `names`.
pub const COMMAND_GROUPS: &[CommandGroup] = &[
    CommandGroup {
        id: "models",
        title: "Model",
        desc: "Switch the agent's model",
        names: &["models", "model", "llm"],
        target: Some(ConfigTarget::ByCategory("model")),
    },
    CommandGroup {
        id: "provider",
        title: "Provider",
        desc: "Switch the agent's provider",
        names: &["provider", "providers"],
        target: Some(ConfigTarget::ById("provider")),
    },
    CommandGroup {
        id: "mode",
        title: "Mode",
        desc: "Switch the session mode",
        names: &["mode", "modes"],
        target: Some(ConfigTarget::ByCategory("mode")),
    },
];

/// Commands NEVER exposed in the `/agents` slash menu, per group —
/// permission-control toggles the split's YOLO contract already decides
/// (the harnesses run auto-approved, so exposing the toggle would only let
/// the user fight the mode the panes exist in). Applied on top of every
/// agent's list; a per-agent override table follows for the agents whose
/// spellings need adjusting over time.
pub const BLOCKED_COMMANDS: &[&str] = &[
    "yolo",
    "bypass",
    "bypass_permissions",
    "bypasspermissions",
    "dangerously_skip_permissions",
    "dangerously_skip",
    "auto_approve",
    "autoapprove",
    "auto_accept",
    "autoaccept",
    "permissions",
    "permission",
    "permission_mode",
    "permissionmode",
    // Claude's permission-mode VALUE that switches edits to auto-accept —
    // a permission toggle by any other name (no permission stem in it, so
    // the substring fallback below can never catch it).
    "accept_edits",
    "acceptedits",
    "acceptedit",
    "accept_edits_mode",
];

/// Per-agent blocklist overrides: agent name → additional blocked spellings.
/// Entries land here as we tune per agent from each harness's own docs:
pub const AGENT_BLOCKED_OVERRIDES: &[(&str, &[&str])] = &[
    // claude-code publishes its own `/agents` through ACP stamped
    // "(removed)" — the harness retired it, and offering it would both run
    // a dead command and collide with cosh's own `/agents` spelling.
    ("claude", &["agents"]),
];

/// The ONE-LINE description a slash-menu row shows for an agent command.
///
/// Harnesses publish descriptions of wildly different shapes: opencode's
/// `customize-opencode` carries the ENTIRE skill brief (a wrapped paragraph
/// that floods the menu row — the built-in skill's full trigger document,
/// per opencode's own skill registry), and claude's skill commands carry
/// multi-page trigger documentation. The menu shows the agent's exact
/// command NAME verbatim; the description is UNWRAPPED to its first
/// sentence (up to the first sentence terminator followed by a space, or
/// the first line break) and hard-capped, so every row reads on one line
/// without inventing text the harness never wrote. Abbreviations like
/// `.opencode/` never precede a space, so they don't cut the sentence.
#[must_use]
pub fn menu_description(description: &str) -> String {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut end = trimmed.len();
    for (i, ch) in trimmed.char_indices() {
        if ch == '\n' {
            end = i;
            break;
        }
        if matches!(ch, '.' | '!' | '?') {
            let rest = &trimmed[i + ch.len_utf8()..];
            if rest.is_empty() || rest.starts_with(' ') {
                end = i + ch.len_utf8();
                break;
            }
        }
    }
    let mut line = trimmed[..end].trim().to_string();
    // Hard cap keeps the harnesses whose first "sentence" is itself a
    // paragraph (claude's `/doctor`) readable.
    const MAX_CHARS: usize = 120;
    if line.chars().count() > MAX_CHARS {
        line = line.chars().take(MAX_CHARS).collect();
        line.push('…');
    }
    line
}

/// Normalize one command spelling the way the tables store them.
#[must_use]
pub fn normalize(name: &str) -> String {
    name.trim()
        .trim_start_matches('/')
        .to_lowercase()
        .replace(['-', ' '], "_")
}

/// Whether an agent's command must be hidden from the `/agents` slash menu.
/// `agent` selects the per-agent override blocklist
/// ([`AGENT_BLOCKED_OVERRIDES`]).
///
/// On top of the exact-match table a SUBSTRING fallback guards the
/// permission/privacy family: harness spellings drift (`permissions_mode`
/// vs `permission_mode`, `accept_edits`, `--yolo`), and a missed spelling
/// here would expose a toggle the YOLO mode must keep decided server-side.
/// Only these three stems qualify — no generic substring pass (an innocent
/// `compact` shares no stem with them).
#[must_use]
pub fn is_blocked(agent: &str, name: &str) -> bool {
    let normalized = normalize(name);
    if BLOCKED_COMMANDS.contains(&normalized.as_str()) {
        return true;
    }
    if ["permission", "yolo", "bypass"]
        .iter()
        .any(|stem| normalized.contains(stem))
    {
        return true;
    }
    AGENT_BLOCKED_OVERRIDES
        .iter()
        .any(|(owner, blocked)| *owner == agent && blocked.contains(&normalized.as_str()))
}

/// The group a command name belongs to, `None` for the group-less base
/// (direct passthrough to the pane).
#[must_use]
pub fn resolve(name: &str) -> Option<&'static CommandGroup> {
    let normalized = normalize(name);
    COMMAND_GROUPS
        .iter()
        .find(|group| group.names.contains(&normalized.as_str()))
}

/// Match one session config option against a [`ConfigTarget`].
///
/// `ByCategory` is a TWO-PASS match: an option whose `id` equals the
/// category name wins first — cline categorizes BOTH its `provider` and
/// `model` options as `Model`, so a single-pass category scan in list
/// order would hand the model box the provider option. Only when no such
/// id exists does the category field decide (harnesses that predate ids —
/// the reason the category fallback exists at all).
#[must_use]
pub fn match_option(
    options: &[cosh_tools::subagent::events::AgentConfigOption],
    target: ConfigTarget,
) -> Option<&cosh_tools::subagent::events::AgentConfigOption> {
    match target {
        ConfigTarget::ById(id) => options.iter().find(|o| o.id == id),
        ConfigTarget::ByCategory(category) => options
            .iter()
            .find(|o| o.id == category)
            .or_else(|| options.iter().find(|o| o.category.as_deref() == Some(category))),
    }
}

/// The menu entries SYNTHESIZED from the session's own ACP config options.
///
/// The probe and each harness's own ACP surface (opencode issue #27942 —
/// `model`/`mode` are NOT in its `available_commands_update`; the kilo ACP
/// publishes only prompt-commands) proved the mapping above can never rely
/// on a harness PUBLISHING a command: the most useful controls — model,
/// provider, mode — reach the client as session config OPTIONS
/// (`session/new` / `session/resume` payloads and `config_option_update`s),
/// with no slash command advertising them at all. Reasoning effort is NOT
/// synthesized here: it follows the cosh model-picker flow — a SECOND box
/// opened after the model pick (see the app's config confirm), and it
/// simply never appears for agents without a controllable level.
/// So for every mapped group whose config option the pane actually holds,
/// the menu synthesizes the group's entry (the group id as the name — the
/// one spelling cosh standardizes on — and the group's one-liner). The box
/// that opens is the SAME standardized box a published command would have
/// opened, bound to the pane's own option.
///
/// Groups whose option is absent or valueless (gemini publishes ZERO config
/// options; a boolean-only option mirrors with an empty value list) are
/// omitted — a dead entry would only route into the "still loading" toast.
pub fn synthesized_group_commands(
    options: &[cosh_tools::subagent::events::AgentConfigOption],
) -> Vec<crate::ui::slash_menu::SlashCommand> {
    COMMAND_GROUPS
        .iter()
        .filter_map(|group| {
            let target = group.target?;
            let option = match_option(options, target)?;
            if option.values.is_empty() {
                return None;
            }
            Some(crate::ui::slash_menu::SlashCommand {
                name: group.id.to_string(),
                desc: group.desc.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_spellings_resolve_to_one_group() {
        for name in ["models", "model", "/models", "Model", "llm"] {
            let group = resolve(name).expect("models group");
            assert_eq!(group.id, "models");
            assert!(matches!(group.target, Some(ConfigTarget::ByCategory("model"))));
        }
        assert_eq!(resolve("provider").unwrap().id, "provider");
        assert_eq!(resolve("PROVIDERS").unwrap().id, "provider");
        assert_eq!(resolve("modes").unwrap().id, "mode");
        // Reasoning is NOT a mapped group anymore: the cosh flow chains the
        // level sub-box after the model pick instead of its own entry — the
        // spellings resolve to nothing (direct passthrough), like any
        // unknown command.
        for name in ["reasoning", "thinking", "thought", "effort"] {
            assert!(resolve(name).is_none(), "{name} must not resolve");
        }
    }

    #[test]
    fn unknown_names_fall_through_to_the_base() {
        assert!(resolve("compact").is_none());
        assert!(resolve("review").is_none());
        assert!(resolve("export").is_none());
    }

    #[test]
    fn permission_toggles_are_blocked_regardless_of_spelling() {
        for name in [
            "yolo",
            "bypass",
            "BypassPermissions",
            "dangerously-skip-permissions",
            // Stem fallback: spellings that drift around the exact table.
            "permissions_mode",
            "permission-mode",
            "AcceptEdits",
            "--yolo",
        ] {
            assert!(is_blocked("claude", name), "{name} must be blocked");
        }
        assert!(!is_blocked("claude", "models"));
        assert!(!is_blocked("claude", "compact"));
        // Per-agent override: claude's own retired `/agents` (the harness
        // stamps it "(removed)") is blocked only for claude — other
        // harnesses may publish a live `/agents`.
        assert!(is_blocked("claude", "agents"));
        assert!(!is_blocked("opencode", "agents"));
        assert!(!is_blocked("gemini", "unknown-command"));
    }

    #[test]
    fn two_pass_category_match_wins_by_id_first() {
        use cosh_tools::subagent::events::AgentConfigOption;
        // cline: BOTH options carry category "model"; the ids decide.
        let options = vec![
            AgentConfigOption {
                id: "provider".to_string(),
                name: "Provider".to_string(),
                description: None,
                category: Some("model".to_string()),
                values: vec![],
                current: "cline".to_string(),
                boolean: None,
            },
            AgentConfigOption {
                id: "model".to_string(),
                name: "Model".to_string(),
                description: None,
                category: Some("model".to_string()),
                values: vec![],
                current: String::new(),
                boolean: None,
            },
        ];
        let picked = match_option(&options, ConfigTarget::ByCategory("model")).unwrap();
        assert_eq!(picked.id, "model");
        // A category no option id shares still falls through to the field.
        let category_only = vec![AgentConfigOption {
            id: "legacy_id".to_string(),
            name: "Reasoning".to_string(),
            description: None,
            category: Some("thought_level".to_string()),
            values: vec![],
            current: String::new(),
            boolean: None,
        }];
        let picked = match_option(&category_only, ConfigTarget::ByCategory("thought_level"));
        assert_eq!(picked.map(|o| o.id.as_str()), Some("legacy_id"));
    }

    #[test]
    fn menu_description_unwraps_to_one_line() {
        // opencode's `customize-opencode` publishes the ENTIRE skill brief.
        let opencode = "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .opencode/, or files under ~/.config/opencode/. Also use when creating or fixing opencode agents, subagents, skills, plugins, MCP servers, or permission rules. Do not use for the user's own application code, or for any project that is not configuring opencode itself.";
        let line = menu_description(opencode);
        // The first "sentence" is itself over the cap: the cap wins (and
        // the abbreviation `.opencode/` was NOT mistaken for a terminator
        // on the way).
        assert_eq!(line, "Use ONLY when the user is editing or creating opencode's own configuration: opencode.json, opencode.jsonc, files under .…");
        assert!(!line.contains('\n'));
        // Multi-line briefs cut at the first line break.
        assert_eq!(menu_description("Review changes.\nSecond paragraph."), "Review changes.");
        // Exclamation/question terminators end the sentence too.
        assert_eq!(menu_description("Set the model! Then more."), "Set the model!");
        // A first sentence that is itself a paragraph gets hard-capped.
        let long = "word ".repeat(60);
        let capped = menu_description(long.trim());
        assert!(capped.chars().count() <= 121);
        assert!(capped.ends_with('…'));
        // Empty stays empty (no invented text).
        assert_eq!(menu_description("   "), "");
        // Short one-liners pass through unchanged.
        assert_eq!(menu_description("guided AGENTS.md setup"), "guided AGENTS.md setup");
    }

    /// A select option shaped like the probe's mirror output.
    fn option(id: &str, category: Option<&str>, values: &[&str]) -> cosh_tools::subagent::events::AgentConfigOption {
        cosh_tools::subagent::events::AgentConfigOption {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            category: category.map(str::to_string),
            values: values
                .iter()
                .map(|v| cosh_tools::subagent::events::AgentConfigValue {
                    value: (*v).to_string(),
                    name: (*v).to_string(),
                })
                .collect(),
            current: values.first().copied().unwrap_or_default().to_string(),
            boolean: None,
        }
    }

    #[test]
    fn synthesis_covers_every_option_backed_group_per_agent() {
        // opencode: model + mode options only (its ACP publishes no
        // model/mode COMMANDS — issue #27942).
        let opencode = vec![
            option("model", Some("model"), &["opencode/big-pickle", "opencode/grok-code"]),
            option("mode", Some("mode"), &["build", "plan"]),
        ];
        let names: Vec<String> = synthesized_group_commands(&opencode)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["models", "mode"]);

        // kilo: model + mode (effort/thought_level exists but is NOT
        // synthesized — it chains after the model pick instead).
        let kilo = vec![
            option("model", Some("model"), &["kilo/google/gemini-3-pro-image"]),
            option("effort", Some("thought_level"), &["thinking"]),
            option("mode", Some("mode"), &["code", "ask", "debug"]),
        ];
        let names: Vec<String> = synthesized_group_commands(&kilo)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["models", "mode"]);

        // cline: provider and model BOTH category "model" — the two-pass
        // match must bind the models entry to id `model`, the provider
        // entry to id `provider`.
        let cline = vec![
            option("provider", Some("model"), &["cline", "openai-codex"]),
            option("model", Some("model"), &["anthropic/claude-sonnet-5"]),
            option("mode", Some("mode"), &["plan", "act"]),
        ];
        let names: Vec<String> = synthesized_group_commands(&cline)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["models", "provider", "mode"]);

        // codex: model + mode (collaboration_mode maps to no group).
        let codex = vec![
            option("mode", Some("mode"), &["read-only", "workspace-write", "agent"]),
            option("collaboration_mode", Some("collaboration_mode"), &["default", "plan"]),
            option("model", Some("model"), &["gpt-6-astra"]),
        ];
        let names: Vec<String> = synthesized_group_commands(&codex)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["models", "mode"]);

        // claude: model + mode (fast is a boolean — valueless; the effort
        // option chains after the model pick, no standalone entry).
        let claude = vec![
            option("mode", Some("mode"), &["default", "plan", "auto"]),
            option("model", Some("model"), &["default", "opus", "fable"]),
            option("effort", Some("thought_level"), &["default", "low", "high"]),
            option("fast", Some("model_config"), &[]),
        ];
        let names: Vec<String> = synthesized_group_commands(&claude)
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert_eq!(names, vec!["models", "mode"]);

        // gemini publishes ZERO config options: nothing synthesizes.
        assert!(synthesized_group_commands(&[]).is_empty());
    }

    #[test]
    fn synthesized_entry_names_resolve_back_to_their_group() {
        // Every synthesized name must route back to its own group — the
        // entry the menu shows and the box that opens can never disagree.
        let kilo = vec![
            option("effort", Some("thought_level"), &["thinking"]),
            option("mode", Some("mode"), &["code", "plan"]),
            option("model", Some("model"), &["kilo/google/gemini-3-pro-image"]),
        ];
        for entry in synthesized_group_commands(&kilo) {
            let group = resolve(&entry.name).expect("synthesized name resolves");
            assert_eq!(group.id, entry.name);
            assert!(group.target.is_some());
        }
    }
}
