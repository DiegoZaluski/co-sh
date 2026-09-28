use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    ScrollUp,
    ScrollDown,
    ScrollUpPage,
    ScrollDownPage,
    ScrollToTop,
    ScrollToBottom,
    SendMessage,
    FocusInput,
    ToggleSidebar,
    ToggleRightPanel,
    ToggleUsage,
    ShowSessionHistory,
    ShowFileExplorer,
    ToggleHelp,
    ToggleConceal,
    ToggleThinking,
    ToggleDiagnostics,
    ToggleToolDetails,
    ToggleGenericToolOutput,
    Confirm,
    Cancel,
    Interrupt,
    NextAgent,
    PrevAgent,
    HistoryUp,
    HistoryDown,
    NextSession,
    PrevSession,
    ToggleMode,
    ClearQueue,
    /// Maximize the Subagent section over the right panel (header hint F1).
    MaximizeSubagent,
    /// Maximize the Bash section over the right panel (header hint F2).
    MaximizeBash,
    /// Maximize the TODO section over the right panel (header hint F3).
    MaximizeTodo,
    /// Bring the right panel's mixed view back (header hint F4).
    MaximizeMixedView,
}

#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone)]
pub struct KeyMap {
    pub bindings: Vec<(Action, KeyBinding)>,
}

impl KeyMap {
    pub fn default_vim() -> Self {
        use KeyCode::{Char, Down, Enter, Esc, Left, PageDown, PageUp, Right, Tab, Up};

        Self {
            bindings: vec![
                (
                    Action::ScrollUpPage,
                    KeyBinding {
                        key: PageUp,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::ScrollDownPage,
                    KeyBinding {
                        key: PageDown,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::SendMessage,
                    KeyBinding {
                        key: Enter,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::ToggleSidebar,
                    KeyBinding {
                        key: Char('b'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleRightPanel,
                    KeyBinding {
                        key: Char('p'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleUsage,
                    KeyBinding {
                        key: Char('u'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ShowSessionHistory,
                    KeyBinding {
                        key: Char('s'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ShowFileExplorer,
                    KeyBinding {
                        key: Char('f'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleHelp,
                    KeyBinding {
                        key: Char('k'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleConceal,
                    KeyBinding {
                        key: Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleThinking,
                    KeyBinding {
                        key: Char('t'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleToolDetails,
                    KeyBinding {
                        key: Char('d'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleDiagnostics,
                    KeyBinding {
                        key: Char('e'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleGenericToolOutput,
                    KeyBinding {
                        key: Char('g'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::Confirm,
                    KeyBinding {
                        key: Enter,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::Cancel,
                    KeyBinding {
                        key: Esc,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::Interrupt,
                    KeyBinding {
                        key: Esc,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::ToggleMode,
                    KeyBinding {
                        key: Tab,
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::PrevAgent,
                    KeyBinding {
                        key: Left,
                        modifiers: KeyModifiers::ALT,
                    },
                ),
                (
                    Action::NextAgent,
                    KeyBinding {
                        key: Right,
                        modifiers: KeyModifiers::ALT,
                    },
                ),
                (
                    Action::HistoryUp,
                    KeyBinding {
                        key: Up,
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::HistoryDown,
                    KeyBinding {
                        key: Down,
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::MaximizeSubagent,
                    KeyBinding {
                        key: KeyCode::F(1),
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::MaximizeBash,
                    KeyBinding {
                        key: KeyCode::F(2),
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::MaximizeTodo,
                    KeyBinding {
                        key: KeyCode::F(3),
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                (
                    Action::MaximizeMixedView,
                    KeyBinding {
                        key: KeyCode::F(4),
                        modifiers: KeyModifiers::NONE,
                    },
                ),
                // 'n'/'p' for NextSession/PrevSession intentionally omitted
                // to avoid blocking normal typing of these characters.
            ],
        }
    }

    pub fn lookup(&self, key: KeyCode, modifiers: KeyModifiers) -> Option<&Action> {
        self.bindings
            .iter()
            .find(|(_, binding)| binding.key == key && binding.modifiers == modifiers)
            .map(|(action, _)| action)
    }
}
