// button label overlay at screen edges
//
// renders action labels ("Back", "OK", "<<", ">>") near the
// physical button positions so users know what each button does.
// uses the shared ButtonMapper so labels update when buttons are
// swapped via settings.

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle};

use crate::board::action::{Action, ButtonMapper};
use crate::board::button::Button;
use crate::board::layout::{CX_BACK, CX_CONFIRM, CX_LEFT, CX_RIGHT, CY_VOL_DOWN, CY_VOL_UP};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;
use crate::ui::{Alignment, Region};

// the physical keys the overlay labels, by position: X4 Back / Confirm / Left /
// Right on the bottom edge and Vol Up / Vol Down on the right edge; on the
// OnePage C61 the front ladder Back / Enter / Left / Right and the side keys
// Prev / Next
mod keys {
    use crate::board::button::Button;

    #[cfg(feature = "board-x4")]
    pub const BACK: Button = Button::Back;
    #[cfg(feature = "board-x4")]
    pub const CONFIRM: Button = Button::Confirm;
    #[cfg(feature = "board-x4")]
    pub const LEFT: Button = Button::Left;
    #[cfg(feature = "board-x4")]
    pub const RIGHT: Button = Button::Right;
    #[cfg(feature = "board-x4")]
    pub const SIDE_UP: Button = Button::VolUp;
    #[cfg(feature = "board-x4")]
    pub const SIDE_DOWN: Button = Button::VolDown;

    #[cfg(feature = "board-onepage-c61")]
    pub const BACK: Button = Button::Back;
    #[cfg(feature = "board-onepage-c61")]
    pub const CONFIRM: Button = Button::Enter;
    #[cfg(feature = "board-onepage-c61")]
    pub const LEFT: Button = Button::Left;
    #[cfg(feature = "board-onepage-c61")]
    pub const RIGHT: Button = Button::Right;
    #[cfg(feature = "board-onepage-c61")]
    pub const SIDE_UP: Button = Button::Prev;
    #[cfg(feature = "board-onepage-c61")]
    pub const SIDE_DOWN: Button = Button::Next;
}

const TAB_W: u16 = 60;
const TAB_H: u16 = 22;

pub const BUTTON_BAR_H: u16 = TAB_H + BOTTOM_INSET;

const RIDGE_W: u16 = 22;
const RIDGE_H: u16 = 36;

const NUM_BUMPS: usize = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Bottom,
    Right,
}

#[derive(Clone, Copy)]
struct BumpDef {
    button: Button,
    edge: Edge,
    center: u16, // x for bottom edge; y for right edge
}

const BUMPS: [BumpDef; NUM_BUMPS] = [
    BumpDef {
        button: keys::BACK,
        edge: Edge::Bottom,
        center: CX_BACK,
    },
    BumpDef {
        button: keys::CONFIRM,
        edge: Edge::Bottom,
        center: CX_CONFIRM,
    },
    BumpDef {
        button: keys::LEFT,
        edge: Edge::Bottom,
        center: CX_LEFT,
    },
    BumpDef {
        button: keys::RIGHT,
        edge: Edge::Bottom,
        center: CX_RIGHT,
    },
    BumpDef {
        button: keys::SIDE_UP,
        edge: Edge::Right,
        center: CY_VOL_UP,
    },
    BumpDef {
        button: keys::SIDE_DOWN,
        edge: Edge::Right,
        center: CY_VOL_DOWN,
    },
];

const BOTTOM_INSET: u16 = 4;

fn bump_region(def: &BumpDef) -> Region {
    match def.edge {
        Edge::Bottom => Region::new(
            def.center.saturating_sub(TAB_W / 2),
            SCREEN_H - TAB_H - BOTTOM_INSET,
            TAB_W,
            TAB_H,
        ),
        Edge::Right => Region::new(
            SCREEN_W - RIDGE_W,
            def.center.saturating_sub(RIDGE_H / 2),
            RIDGE_W,
            RIDGE_H,
        ),
    }
}

fn action_label(action: Action) -> &'static str {
    match action {
        Action::Next => "Next",
        Action::Prev => "Prev",
        Action::NextJump => ">>",
        Action::PrevJump => "<<",
        Action::Select => "OK",
        Action::Back => "Back",
        Action::Menu => "",
    }
}

pub struct ButtonFeedback {
    swap: bool,
    font: Option<&'static BitmapFont>,
}

impl Default for ButtonFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonFeedback {
    pub const fn new() -> Self {
        Self {
            swap: false,
            font: None,
        }
    }

    pub fn set_chrome_font(&mut self, font: &'static BitmapFont) {
        self.font = Some(font);
    }

    pub fn set_swap(&mut self, swap: bool) -> bool {
        if self.swap != swap {
            self.swap = swap;
            true
        } else {
            false
        }
    }

    pub fn draw(&self, strip: &mut StripBuffer) {
        let font = self.font.unwrap_or(&font_data::REGULAR_BODY_SMALL);
        let mapper = if self.swap {
            let mut m = ButtonMapper::new();
            m.set_swap(true);
            m
        } else {
            ButtonMapper::new()
        };

        for def in BUMPS.iter() {
            if def.edge != Edge::Bottom {
                continue;
            }

            let r = bump_region(def);

            if !r.intersects(strip.logical_window()) {
                continue;
            }

            r.to_rect()
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(strip)
                .unwrap();

            let action = mapper.map_button(def.button);
            let label = action_label(action);
            if label.is_empty() {
                continue;
            }

            font.draw_aligned(strip, r, label, Alignment::Center, BinaryColor::On);
        }
    }
}
