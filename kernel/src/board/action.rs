// semantic actions decoupled from physical buttons
// apps match on Action, never on HwButton

use crate::board::button::Button;
use crate::drivers::input::Event;

// Action / ActionEvent are HAL-free data and shared with the OnePage C61 key
// map; the single definition is in pulp-board-logic.
pub use pulp_board_logic::action::{Action, ActionEvent};

// portrait one-handed layout with optional button swap
//
// default layout (right-handed):
//   bottom row: Back  Confirm(=Select)  Left(=PrevJump)  Right(=NextJump)
//   side:       VolUp(=Prev)  VolDown(=Next)
//
// swapped layout (left-handed):
//   bottom row: Left(=PrevJump)  Right(=NextJump)  Back  Confirm(=Select)
//   this swaps the *physical roles* of Back<->Left and Confirm<->Right
//   so the spatial position of Back/OK moves to the right side of the
//   device where the left hand naturally rests.
//   volume buttons are NOT swapped (up=prev, down=next always).
#[derive(Default)]
pub struct ButtonMapper {
    swap_buttons: bool,
}

impl ButtonMapper {
    pub const fn new() -> Self {
        Self {
            swap_buttons: false,
        }
    }

    pub fn set_swap(&mut self, swap: bool) {
        self.swap_buttons = swap;
    }

    pub fn is_swapped(&self) -> bool {
        self.swap_buttons
    }

    pub fn map_button(&self, button: Button) -> Action {
        if self.swap_buttons {
            // swapped: Back<->Left, Confirm<->Right
            match button {
                Button::VolDown => Action::Next,
                Button::VolUp => Action::Prev,
                Button::Right => Action::Select,     // was NextJump
                Button::Left => Action::Back,        // was PrevJump
                Button::Confirm => Action::NextJump, // was Select
                Button::Back => Action::PrevJump,    // was Back
                Button::Power => Action::Menu,
            }
        } else {
            // default right-handed layout
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
