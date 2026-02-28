// Button feedback — edge labels with press inversion
//
// Bottom tabs show the mapped action; side labels for vol.
// On press the text inverts (white-on-black); no borders or shapes.

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle};

use super::widget::Region;
use crate::board::action::{Action, ButtonMapper};
use crate::board::button::Button;
use crate::drivers::strip::StripBuffer;
use crate::fonts::font_data;

const TAB_W: u16 = 60;
const TAB_H: u16 = 22;

const RIDGE_W: u16 = 22;
const RIDGE_H: u16 = 36;

const SCREEN_W: u16 = 480;
const SCREEN_H: u16 = 800;

// Center positions of each button on the screen edge (px).
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
    center: u16, // x for bottom edge, y for right edge
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

fn bump_region(def: &BumpDef) -> Region {
    match def.edge {
        Edge::Bottom => Region::new(
            def.center.saturating_sub(TAB_W / 2),
            SCREEN_H - TAB_H,
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
    active: Option<usize>, // index into BUMPS, or None
    mapper: ButtonMapper,
}

impl Default for ButtonFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl ButtonFeedback {
    pub const fn new() -> Self {
        Self {
            active: None,
            mapper: ButtonMapper::new(),
        }
    }

    // returns dirty region for pressed button, or None for Power
    pub fn on_press(&mut self, button: Button) -> Option<Region> {
        if button == Button::Power {
            return None;
        }
        let idx = BUMPS.iter().position(|d| d.button == button)?;
        let mut region = bump_region(&BUMPS[idx]);
        if let Some(old) = self.active
            && old != idx
        {
            region = region.union(bump_region(&BUMPS[old]));
        }
        self.active = Some(idx);
        Some(region)
    }

    // returns dirty region so bump reverts to normal appearance
    pub fn on_release(&mut self) -> Option<Region> {
        let idx = self.active.take()?;
        Some(bump_region(&BUMPS[idx]))
    }

    // draw all bumps; call after app and overlay so bumps layer on top
    pub fn draw(&self, strip: &mut StripBuffer) {
        let font = &font_data::REGULAR_BODY_SMALL;

        for (i, def) in BUMPS.iter().enumerate() {
            let pressed = self.active == Some(i);
            let r = bump_region(def);

            if !r.intersects(strip.logical_window()) {
                continue;
            }

            let (bg, fg) = if pressed {
                (BinaryColor::On, BinaryColor::Off)
            } else {
                (BinaryColor::Off, BinaryColor::On)
            };

            // fill background
            r.to_rect()
                .into_styled(PrimitiveStyle::with_fill(bg))
                .draw(strip)
                .unwrap();

            // draw label text (bottom tabs only; side ridges are too narrow)
            if def.edge == Edge::Bottom {
                let action = self.mapper.map_button(def.button);
                let label = action_label(action);
                if label.is_empty() {
                    continue;
                }

                let text_w = font.measure_str(label) as i32;
                let lh = font.line_height as i32;
                let asc = font.ascent as i32;

                let text_x = r.x as i32 + (r.w as i32 - text_w) / 2;
                let text_top = r.y as i32 + (r.h as i32 - lh) / 2;
                let baseline = text_top + asc;

                font.draw_str_fg(strip, label, fg, text_x, baseline);
            }
        }
    }
}
