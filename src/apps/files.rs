// Paginated file browser for SD card root directory
//
// Scrolling within a page marks two rows dirty (old + new selection).
// Scrolling across a page boundary sets needs_load; the kernel runs
// AppWork which reads from DirCache and owns the render decision.

use core::fmt::Write as _;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::apps::{App, AppContext, AppId, Services, Transition};
use crate::board::button::Button as HwButton;
use crate::drivers::input::Event;
use crate::drivers::storage::DirEntry;
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;
use crate::ui::{Alignment, BitmapButton, BitmapDynLabel, BitmapLabel, CONTENT_TOP, Region};

const PAGE_SIZE: usize = 7;

// STATUS_REGION is fixed next to the header; its y matches CONTENT_TOP + 4.
const STATUS_REGION: Region = Region::new(320, CONTENT_TOP + 4, 140, 28);

const ROW_H: u16 = 52;
// Vertical gap between the bottom of the heading and the first list row.
const HEADER_LIST_GAP: u16 = 4;

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

pub struct FilesApp {
    entries: [DirEntry; PAGE_SIZE],
    count: usize,
    total: usize,
    scroll: usize,
    selected: usize,
    needs_load: bool,
    stale_cache: bool,
    error: Option<&'static str>,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
    list_y: u16,
}

impl FilesApp {
    pub fn new() -> Self {
        let hf = heading_font(0);
        Self {
            entries: [DirEntry::EMPTY; PAGE_SIZE],
            count: 0,
            total: 0,
            scroll: 0,
            selected: 0,
            needs_load: false,
            stale_cache: false,
            error: None,
            body_font: body_font(0),
            heading_font: hf,
            list_y: CONTENT_TOP + 4 + hf.line_height + HEADER_LIST_GAP,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = body_font(idx);
        self.heading_font = heading_font(idx);
        self.list_y = CONTENT_TOP + 4 + self.heading_font.line_height + HEADER_LIST_GAP;
    }

    pub fn selected_entry(&self) -> Option<&DirEntry> {
        if self.selected < self.count {
            Some(&self.entries[self.selected])
        } else {
            None
        }
    }

    fn load_page(&mut self, entries: &[DirEntry], total: usize) {
        let n = entries.len().min(PAGE_SIZE);
        self.entries[..n].clone_from_slice(&entries[..n]);
        self.count = n;
        self.total = total;
        self.needs_load = false;
        self.error = None;
        if self.selected >= self.count && self.count > 0 {
            self.selected = self.count - 1;
        }
    }

    fn load_failed(&mut self, msg: &'static str) {
        self.needs_load = false;
        self.error = Some(msg);
        self.count = 0;
    }

    fn row_region(&self, index: usize) -> Region {
        Region::new(16, self.list_y + index as u16 * ROW_H, 448, ROW_H - 4)
    }

    fn list_region(&self) -> Region {
        Region::new(8, self.list_y, 464, ROW_H * PAGE_SIZE as u16)
    }

    fn move_up(&mut self, ctx: &mut AppContext) {
        if self.selected > 0 {
            ctx.mark_dirty(self.row_region(self.selected));
            self.selected -= 1;
            ctx.mark_dirty(self.row_region(self.selected));
            ctx.mark_dirty(STATUS_REGION);
        } else if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(1);
            self.needs_load = true;
        }
    }

    fn move_down(&mut self, ctx: &mut AppContext) {
        if self.selected + 1 < self.count {
            ctx.mark_dirty(self.row_region(self.selected));
            self.selected += 1;
            ctx.mark_dirty(self.row_region(self.selected));
            ctx.mark_dirty(STATUS_REGION);
        } else if self.scroll + self.count < self.total {
            self.scroll += 1;
            self.needs_load = true;
        }
    }
}

impl App for FilesApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        self.scroll = 0;
        self.selected = 0;
        self.needs_load = true;
        self.stale_cache = true;
        self.error = None;
        ctx.request_screen_redraw();
    }

    fn on_exit(&mut self) {
        self.count = 0;
    }

    fn on_suspend(&mut self) {}

    fn on_resume(&mut self, ctx: &mut AppContext) {
        ctx.request_screen_redraw();
    }

    fn needs_work(&self) -> bool {
        self.needs_load
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        if self.stale_cache {
            svc.invalidate_dir_cache();
            self.stale_cache = false;
        }

        let mut buf = [DirEntry::EMPTY; PAGE_SIZE];
        match svc.dir_page(self.scroll, &mut buf) {
            Ok(page) => {
                self.load_page(&buf[..page.count], page.total);
            }
            Err(e) => {
                log::info!("SD load failed: {}", e);
                self.load_failed(e);
            }
        }

        ctx.mark_dirty(self.list_region());
        ctx.mark_dirty(STATUS_REGION);
    }

    fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition {
        match event {
            Event::Press(HwButton::Back) => Transition::Pop,

            Event::Press(HwButton::Left | HwButton::VolUp) => {
                self.move_up(ctx);
                Transition::None
            }

            Event::Press(HwButton::Right | HwButton::VolDown) => {
                self.move_down(ctx);
                Transition::None
            }

            Event::Press(HwButton::Confirm) => {
                if let Some(entry) = self.selected_entry() {
                    if entry.is_dir {
                        Transition::None
                    } else {
                        ctx.set_message(entry.name_str().as_bytes());
                        Transition::Push(AppId::Reader)
                    }
                } else {
                    Transition::None
                }
            }

            Event::Repeat(HwButton::Left | HwButton::VolUp) => {
                self.move_up(ctx);
                Transition::None
            }
            Event::Repeat(HwButton::Right | HwButton::VolDown) => {
                self.move_down(ctx);
                Transition::None
            }

            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let header_region = Region::new(16, CONTENT_TOP + 4, 300, self.heading_font.line_height);
        BitmapLabel::new(header_region, "Files", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.total > 0 {
            let mut status = BitmapDynLabel::<20>::new(STATUS_REGION, self.body_font)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "{}/{}", self.scroll + self.selected + 1, self.total);
            status.draw(strip).unwrap();
        }

        if let Some(msg) = self.error {
            BitmapLabel::new(self.row_region(0), msg, self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        if self.count == 0 && !self.needs_load {
            BitmapLabel::new(self.row_region(0), "No files found", self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        for i in 0..PAGE_SIZE {
            let region = self.row_region(i);

            if i < self.count {
                let entry = &self.entries[i];
                let name = entry.name_str();

                let mut btn = BitmapButton::new(region, name, self.body_font);
                if i == self.selected {
                    btn.set_pressed(true);
                }
                btn.draw(strip).unwrap();
            } else {
                // Clear phantom rows below the list end.
                region
                    .to_rect()
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                    .draw(strip)
                    .unwrap();
            }
        }
    }
}
