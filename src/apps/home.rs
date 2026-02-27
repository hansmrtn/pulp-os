// Launcher screen, entry point after boot

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;
use crate::ui::{Alignment, BitmapButton, BitmapButtonStyle, BitmapLabel, CONTENT_TOP, Region};

// Screen is 480 px wide. Center a 280 px column.
const ITEM_W: u16 = 280;
const ITEM_H: u16 = 52;
const ITEM_GAP: u16 = 14;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;
const ITEM_X: u16 = (480 - ITEM_W) / 2; // 100

// Gap between the bottom of the title and the first menu item.
const TITLE_ITEM_GAP: u16 = 24;

struct MenuItem {
    name: &'static str,
    app: AppId,
}

const ITEMS: &[MenuItem] = &[
    MenuItem {
        name: "Files",
        app: AppId::Files,
    },
    MenuItem {
        name: "Reader",
        app: AppId::Reader,
    },
    MenuItem {
        name: "Settings",
        app: AppId::Settings,
    },
];

fn body_font(idx: u8) -> &'static BitmapFont {
    match idx {
        1 => &font_data::REGULAR_BODY_MEDIUM,
        2 => &font_data::REGULAR_BODY_LARGE,
        _ => &font_data::REGULAR_BODY_SMALL,
    }
}

fn heading_font(idx: u8) -> &'static BitmapFont {
    match idx {
        1 => &font_data::REGULAR_HEADING_MEDIUM,
        2 => &font_data::REGULAR_HEADING_LARGE,
        _ => &font_data::REGULAR_HEADING_SMALL,
    }
}

fn compute_item_regions(heading_line_h: u16) -> [Region; 3] {
    let item_y = CONTENT_TOP + 8 + heading_line_h + TITLE_ITEM_GAP;
    [
        Region::new(ITEM_X, item_y, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE * 2, ITEM_W, ITEM_H),
    ]
}

pub struct HomeApp {
    selected: usize,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
    item_regions: [Region; 3],
}

impl Default for HomeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl HomeApp {
    pub fn new() -> Self {
        let hf = heading_font(0);
        Self {
            selected: 0,
            body_font: body_font(0),
            heading_font: hf,
            item_regions: compute_item_regions(hf.line_height),
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = body_font(idx);
        self.heading_font = heading_font(idx);
        self.item_regions = compute_item_regions(self.heading_font.line_height);
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut AppContext) {
        let count = ITEMS.len();
        let new = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        if new != self.selected {
            ctx.mark_dirty(self.item_regions[self.selected]);
            self.selected = new;
            ctx.mark_dirty(self.item_regions[self.selected]);
        }
    }
}

impl App for HomeApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        ctx.clear_message();
        ctx.request_screen_redraw();
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Next) => {
                self.move_selection(1, ctx);
                Transition::None
            }
            ActionEvent::Press(Action::Prev) => {
                self.move_selection(-1, ctx);
                Transition::None
            }
            ActionEvent::Press(Action::Select) => Transition::Push(ITEMS[self.selected].app),
            _ => Transition::None,
        }
    }

    fn help_text(&self) -> &'static str {
        "Prev/Next: select    Confirm: open"
    }

    fn draw(&self, strip: &mut StripBuffer) {
        // Title centred across the full content width.
        let title_region = Region::new(
            ITEM_X,
            CONTENT_TOP + 8,
            ITEM_W,
            self.heading_font.line_height,
        );
        BitmapLabel::new(title_region, "pulp-os", self.heading_font)
            .alignment(Alignment::Center)
            .draw(strip)
            .unwrap();

        for (i, item) in ITEMS.iter().enumerate() {
            let mut btn = BitmapButton::new(self.item_regions[i], item.name, self.body_font)
                .style(BitmapButtonStyle::Rounded(10));
            if i == self.selected {
                btn.set_pressed(true);
            }
            btn.draw(strip).unwrap();
        }
    }
}
