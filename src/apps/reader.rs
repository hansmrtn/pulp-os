//! Reader app (stub).
//!
//! Will eventually render e-book content. For now shows the filename
//! passed from the file browser.

use embedded_graphics::mono_font::ascii::FONT_10X20;

use crate::apps::{App, AppContext, Transition};
use crate::board::button::Button as HwButton;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::ui::{CONTENT_TOP, Label, Region, Widget};

const TITLE_REGION: Region = Region::new(16, CONTENT_TOP, 300, 32);
const INFO_REGION: Region = Region::new(16, CONTENT_TOP + 48, 440, 32);

pub struct ReaderApp {
    filename: [u8; 32],
    filename_len: usize,
}

impl ReaderApp {
    pub const fn new() -> Self {
        Self {
            filename: [0u8; 32],
            filename_len: 0,
        }
    }
}

impl App for ReaderApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        let msg = ctx.message();
        let len = msg.len().min(32);
        self.filename[..len].copy_from_slice(&msg[..len]);
        self.filename_len = len;
        ctx.request_full_redraw();
    }

    fn on_event(&mut self, event: Event, _ctx: &mut AppContext) -> Transition {
        match event {
            Event::Press(HwButton::Back) => Transition::Pop,
            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        Label::new(TITLE_REGION, "Reader", &FONT_10X20)
            .draw(strip)
            .unwrap();

        if self.filename_len > 0 {
            let name = core::str::from_utf8(&self.filename[..self.filename_len]).unwrap_or("???");
            Label::new(INFO_REGION, name, &FONT_10X20)
                .draw(strip)
                .unwrap();
        }
    }
}
