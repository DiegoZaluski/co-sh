use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Debug, Clone, PartialEq)]
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
    ToggleHelp,
    ToggleConceal,
    ToggleThinking,
    ToggleToolDetails,
    ToggleGenericToolOutput,
    ToggleTimestamps,
    Confirm,
    Cancel,
    Interrupt,
    NextAgent,
    PrevAgent,
    HistoryUp,
    HistoryDown,
    ToggleCommandPalette,
    NextSession,
    PrevSession,
    ToggleMode,
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
    #[allow(clippy::too_many_lines)]
    pub fn default_vim() -> Self {
        use KeyCode::{BackTab, Char, Down, Enter, Esc, PageDown, PageUp, Tab, Up};

        KeyMap {
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
                // '?' for ToggleHelp is intentionally omitted — it would block
                // typing the '?' character in the prompt (most terminals send
                // '?' with NONE modifier since shift is encoded in the char).
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
                    Action::ToggleGenericToolOutput,
                    KeyBinding {
                        key: Char('g'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleTimestamps,
                    KeyBinding {
                        key: Char('y'),
                        modifiers: KeyModifiers::CONTROL,
                    },
                ),
                (
                    Action::ToggleCommandPalette,
                    KeyBinding {
                        key: Char('p'),
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
                        key: BackTab,
                        modifiers: KeyModifiers::NONE,
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
