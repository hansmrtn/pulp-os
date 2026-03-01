// Button feedback: plain text edge labels (no inversion).
// Bottom tabs show the mapped action text; side labels hidden.
// No visual change on press/release; purely informational.

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle};

use super::widget::{Alignment, Region};
use crate::board::action::{Action, ButtonMapper};
use crate::board::button::Button;
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;

const TAB_W: u16 = 60;
const TAB_H: u16 = 22;

// total height reserved at bottom of screen for button labels
pub const BUTTON_BAR_H: u16 = TAB_H + BOTTOM_INSET;

const RIDGE_W: u16 = 22;
const RIDGE_H: u16 = 36;

// center positions of each button on the screen edge (px)
const CX_BACK: u16 = 84;
const CX_CONFIRM: u16 = 194;
const CX_LEFT: u16 = 286;
const CX_RIGHT: u16 = 396;

const CY_VOL_UP: u16 = 364;
const CY_VOL_DOWN: u16 = 484;

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
        button: Button::Back,
        edge: Edge::Bottom,
        center: CX_BACK,
    },
    BumpDef {
        button: Button::Confirm,
        edge: Edge::Bottom,
        center: CX_CONFIRM,
    },
    BumpDef {
        button: Button::Left,
        edge: Edge::Bottom,
        center: CX_LEFT,
    },
    BumpDef {
        button: Button::Right,
        edge: Edge::Bottom,
        center: CX_RIGHT,
    },
    BumpDef {
        button: Button::VolUp,
        edge: Edge::Right,
        center: CY_VOL_UP,
    },
    BumpDef {
        button: Button::VolDown,
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
    mapper: ButtonMapper,
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
            mapper: ButtonMapper::new(),
            font: None,
        }
    }

    // set chrome font for button label text; call on UI font size change
    pub fn set_chrome_font(&mut self, font: &'static BitmapFont) {
        self.font = Some(font);
    }

    // draw bottom-edge labels only; no side indicators or inversion
    pub fn draw(&self, strip: &mut StripBuffer) {
        let font = self.font.unwrap_or(&font_data::REGULAR_BODY_SMALL);

        for def in BUMPS.iter() {
            // skip side-edge indicators (VolUp/VolDown)
            if def.edge != Edge::Bottom {
                continue;
            }

            let r = bump_region(def);

            if !r.intersects(strip.logical_window()) {
                continue;
            }

            // plain: white background, black text
            r.to_rect()
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(strip)
                .unwrap();

            let action = self.mapper.map_button(def.button);
            let label = action_label(action);
            if label.is_empty() {
                continue;
            }

            font.draw_aligned(strip, r, label, Alignment::Center, BinaryColor::On);
        }
    }
}
