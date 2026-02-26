// Launcher screen, entry point after boot

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::button::Button as HwButton;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;
use crate::ui::{Alignment, BitmapButton, BitmapLabel, CONTENT_TOP, Region};

const TITLE_REGION: Region = Region::new(16, CONTENT_TOP, 448, 32);

const ITEM_Y: u16 = CONTENT_TOP + 48;
const ITEM_H: u16 = 48;
const ITEM_GAP: u16 = 16;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;

struct MenuItem {
    region: Region,
    name: &'static str,
    app: AppId,
}

const ITEMS: &[MenuItem] = &[
    MenuItem {
        region: Region::new(16, ITEM_Y, 200, ITEM_H),
        name: "Files",
        app: AppId::Files,
    },
    MenuItem {
        region: Region::new(16, ITEM_Y + ITEM_STRIDE, 200, ITEM_H),
        name: "Reader",
        app: AppId::Reader,
    },
    MenuItem {
        region: Region::new(16, ITEM_Y + ITEM_STRIDE * 2, 200, ITEM_H),
        name: "Settings",
        app: AppId::Settings,
    },
];

// Select a body BitmapFont by size index (0 = Small, 1 = Medium, 2 = Large).
fn body_font(idx: u8) -> &'static BitmapFont {
    match idx {
        1 => &font_data::REGULAR_BODY_MEDIUM,
        2 => &font_data::REGULAR_BODY_LARGE,
        _ => &font_data::REGULAR_BODY_SMALL,
    }
}

pub struct HomeApp {
    selected: usize,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
}

impl HomeApp {
    pub fn new() -> Self {
        Self {
            selected: 0,
            body_font: body_font(0),
            heading_font: &font_data::REGULAR_HEADING,
        }
    }

    /// Called by main.rs whenever ui_font_size_idx changes.
    /// The heading font is always the fixed 24 px cut; only body text scales.
    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = body_font(idx);
    }

    fn item_count(&self) -> usize {
        ITEMS.len()
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut AppContext) {
        let count = self.item_count();
        let new = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        if new != self.selected {
            ctx.mark_dirty(ITEMS[self.selected].region);
            self.selected = new;
            ctx.mark_dirty(ITEMS[self.selected].region);
        }
    }
}

impl App for HomeApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        ctx.clear_message();
        ctx.request_screen_redraw();
    }

    fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition {
        match event {
            Event::Press(HwButton::Right | HwButton::VolDown) => {
                self.move_selection(1, ctx);
                Transition::None
            }
            Event::Press(HwButton::Left | HwButton::VolUp) => {
                self.move_selection(-1, ctx);
                Transition::None
            }
            Event::Press(HwButton::Confirm) => Transition::Push(ITEMS[self.selected].app),
            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        BitmapLabel::new(TITLE_REGION, "pulp-os", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        for (i, item) in ITEMS.iter().enumerate() {
            let mut btn = BitmapButton::new(item.region, item.name, self.body_font);
            if i == self.selected {
                btn.set_pressed(true);
            }
            btn.draw(strip).unwrap();
        }
    }
}
