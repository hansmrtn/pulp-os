// Semantic UI actions, shared by every board (R12).
//
// These two enums used to live in kernel/src/board/action.rs (X4 only). They
// are plain data with no HAL dependency, so they live here and the X4 module
// re-exports them: there is exactly one definition, and the OnePage C61 key
// mapping (`keys::Key::action`) is tested on the host against the very same
// type the apps match on. No action was added or removed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Next,
    Prev,
    NextJump,
    PrevJump,
    Select,
    Back,
    Menu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionEvent {
    Press(Action),
    Release(Action),
    LongPress(Action),
    Repeat(Action),
}

impl ActionEvent {
    pub fn action(self) -> Action {
        match self {
            Self::Press(a) | Self::Release(a) | Self::LongPress(a) | Self::Repeat(a) => a,
        }
    }

    pub fn is_press(self) -> bool {
        matches!(self, Self::Press(_))
    }

    pub fn is_repeat(self) -> bool {
        matches!(self, Self::Repeat(_))
    }

    pub fn is_press_or_repeat(self) -> bool {
        matches!(self, Self::Press(_) | Self::Repeat(_))
    }
}
