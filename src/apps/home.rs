// Launcher screen, entry point after boot

use embedded_graphics::mono_font::ascii::FONT_10X20;

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::button::Button as HwButton;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::ui::{Alignment, CONTENT_TOP, Label, Region, Widget};

const TITLE_REGION: Region = Region::new(16, CONTENT_TOP, 200, 32);

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

pub struct HomeApp {
    selected: usize,
}

impl HomeApp {
    pub const fn new() -> Self {
        Self { selected: 0 }
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
        let title =
            Label::new(TITLE_REGION, "pulp-os", &FONT_10X20).alignment(Alignment::CenterLeft);
        title.draw(strip).unwrap();

        for (i, item) in ITEMS.iter().enumerate() {
            let mut btn = crate::ui::Button::new(item.region, item.name, &FONT_10X20);
            if i == self.selected {
                btn.set_pressed(true);
            }
            btn.draw(strip).unwrap();
        }
    }
}
