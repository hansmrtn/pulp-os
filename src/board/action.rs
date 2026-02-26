// Semantic actions decoupled from physical buttons.
//
// Apps match on Action, never on HwButton.  The ButtonMapper
// translates physical input events into semantic actions based
// on the active profile stored in SystemSettings::button_map.
//
// This allows the same physical buttons to have different
// meanings depending on user preference or hand orientation.

use crate::board::button::Button;
use crate::drivers::input::Event;

/// Semantic input actions consumed by apps and the quick menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Primary axis forward (page turn, list scroll down)
    Next,
    /// Primary axis backward (page back, list scroll up)
    Prev,
    /// Secondary axis forward (chapter jump, page-of-files jump)
    NextJump,
    /// Secondary axis backward
    PrevJump,
    /// Confirm / select / enter
    Select,
    /// Cancel / go back
    Back,
    /// Toggle the quick-action overlay
    Menu,
}

/// Semantic input event — mirrors `drivers::input::Event` but carries `Action`.
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

/// Named button layout profiles.
///
/// Persisted as `SystemSettings::button_map: u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonProfile {
    /// Bottom pair (Right/Left) = primary, side pair (VolUp/VolDown) = secondary
    Default = 0,
    /// Side pair = primary, bottom pair = secondary (left-hand page turn)
    SidePrimary = 1,
}

impl ButtonProfile {
    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::SidePrimary,
            _ => Self::Default,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::SidePrimary => "Side Primary",
        }
    }
}

/// Translates physical `Event` into semantic `ActionEvent`.
pub struct ButtonMapper {
    profile: ButtonProfile,
}

impl ButtonMapper {
    pub const fn new() -> Self {
        Self {
            profile: ButtonProfile::Default,
        }
    }

    pub fn set_profile(&mut self, profile: ButtonProfile) {
        self.profile = profile;
    }

    pub fn profile(&self) -> ButtonProfile {
        self.profile
    }

    /// Map a physical button to a semantic action.
    fn map_button(&self, button: Button) -> Action {
        match self.profile {
            ButtonProfile::Default => match button {
                Button::Right => Action::Next,
                Button::Left => Action::Prev,
                Button::VolDown => Action::NextJump,
                Button::VolUp => Action::PrevJump,
                Button::Confirm => Action::Select,
                Button::Back => Action::Back,
                Button::Power => Action::Menu,
            },
            ButtonProfile::SidePrimary => match button {
                Button::VolDown => Action::Next,
                Button::VolUp => Action::Prev,
                Button::Right => Action::NextJump,
                Button::Left => Action::PrevJump,
                Button::Confirm => Action::Select,
                Button::Back => Action::Back,
                Button::Power => Action::Menu,
            },
        }
    }

    /// Translate a full hardware event into an action event.
    pub fn map_event(&self, event: Event) -> ActionEvent {
        match event {
            Event::Press(b) => ActionEvent::Press(self.map_button(b)),
            Event::Release(b) => ActionEvent::Release(self.map_button(b)),
            Event::LongPress(b) => ActionEvent::LongPress(self.map_button(b)),
            Event::Repeat(b) => ActionEvent::Repeat(self.map_button(b)),
        }
    }
}
