//! Settings app (stub).

use embedded_graphics::mono_font::ascii::FONT_10X20;

use crate::apps::{App, AppContext, Transition};
use crate::board::button::Button as HwButton;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::ui::{CONTENT_TOP, Label, Region, Widget};

const TITLE_REGION: Region = Region::new(16, CONTENT_TOP, 300, 32);

pub struct SettingsApp;

impl SettingsApp {
    pub const fn new() -> Self {
        Self
    }
}

impl App for SettingsApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        ctx.request_full_redraw();
    }

    fn on_event(&mut self, event: Event, _ctx: &mut AppContext) -> Transition {
        match event {
            Event::Press(HwButton::Back) => Transition::Pop,
            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        Label::new(TITLE_REGION, "Settings", &FONT_10X20)
            .draw(strip)
            .unwrap();
    }
}
