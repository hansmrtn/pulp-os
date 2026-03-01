// Semantic actions decoupled from physical buttons.
// Apps match on Action, never on HwButton. ButtonMapper translates
// physical events using the fixed portrait one-handed layout.

use crate::board::button::Button;
use crate::drivers::input::Event;

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

pub struct ButtonMapper {
    _private: (),
}

impl Default for ButtonMapper {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonMapper {
    pub const fn new() -> Self {
        Self { _private: () }
    }

    pub fn map_button(&self, button: Button) -> Action {
        match button {
            Button::VolDown => Action::Next,
            Button::VolUp => Action::Prev,
            Button::Right => Action::NextJump,
            Button::Left => Action::PrevJump,
            Button::Confirm => Action::Select,
            Button::Back => Action::Back,
            Button::Power => Action::Menu,
        }
    }

    pub fn map_event(&self, event: Event) -> ActionEvent {
        match event {
            Event::Press(b) => ActionEvent::Press(self.map_button(b)),
            Event::Release(b) => ActionEvent::Release(self.map_button(b)),
            Event::LongPress(b) => ActionEvent::LongPress(self.map_button(b)),
            Event::Repeat(b) => ActionEvent::Repeat(self.map_button(b)),
        }
    }
}
