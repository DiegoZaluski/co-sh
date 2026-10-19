//! Agent pane colors: the rotating palette that distinguishes the /agents
//! panes visually. UI-only view of the theme; extracted from the former
//! tui/types.rs when the conversation domain model moved to the library.

use cosh_tui::core::lib::rgba::RGBA;

use crate::theme::Theme;

pub struct AgentColors {
    palette: Vec<RGBA>,
}

impl AgentColors {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            palette: vec![
                theme.secondary,
                theme.accent,
                theme.success,
                theme.warning,
                theme.primary,
                theme.error,
                theme.info,
            ],
        }
    }

    pub fn get(&self, name: &str, unique_agents: &[String]) -> RGBA {
        let index = unique_agents.iter().position(|a| a == name).unwrap_or(0);
        self.palette[index % self.palette.len()]
    }
}
