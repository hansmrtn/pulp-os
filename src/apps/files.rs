// Paginated file browser for SD card root directory
//
// In-page scroll marks two rows dirty; cross-page sets needs_load
// and defers to AppWork for SD read + render decision.

use core::fmt::Write as _;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::apps::{App, AppContext, AppId, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::storage::DirEntry;
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::ui::{Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, Region};

const PAGE_SIZE: usize = 7;

// Centered list column: 448 px wide with 16 px margins each side.
const LIST_X: u16 = 16;
const LIST_W: u16 = 448;

// STATUS_REGION is fixed next to the header; its y matches CONTENT_TOP + 8.
const STATUS_REGION: Region = Region::new(320, CONTENT_TOP + 8, 144, 28);

const ROW_H: u16 = 52;
// Vertical gap between rows (border-to-border).
const ROW_GAP: u16 = 4;
// Vertical gap between the bottom of the heading and the first list row.
const HEADER_LIST_GAP: u16 = 8;

impl Default for FilesApp {
    fn default() -> Self {
        Self::new()
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
        let hf = fonts::heading_font(0);
        Self {
            entries: [DirEntry::EMPTY; PAGE_SIZE],
            count: 0,
            total: 0,
            scroll: 0,
            selected: 0,
            needs_load: false,
            stale_cache: false,
            error: None,
            body_font: fonts::body_font(0),
            heading_font: hf,
            list_y: CONTENT_TOP + 8 + hf.line_height + HEADER_LIST_GAP,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = fonts::body_font(idx);
        self.heading_font = fonts::heading_font(idx);
        self.list_y = CONTENT_TOP + 8 + self.heading_font.line_height + HEADER_LIST_GAP;
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
        Region::new(
            LIST_X,
            self.list_y + index as u16 * (ROW_H + ROW_GAP),
            LIST_W,
            ROW_H,
        )
    }

    fn list_region(&self) -> Region {
        Region::new(
            LIST_X,
            self.list_y,
            LIST_W,
            (ROW_H + ROW_GAP) * PAGE_SIZE as u16,
        )
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

    // jump backward by a full page
    fn jump_up(&mut self) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(PAGE_SIZE);
            self.selected = 0;
            self.needs_load = true;
        } else {
            // Already at top — snap selection to first item
            self.selected = 0;
        }
    }

    // jump forward by a full page
    fn jump_down(&mut self) {
        let remaining = self.total.saturating_sub(self.scroll + self.count);
        if remaining > 0 {
            self.scroll += PAGE_SIZE.min(remaining + self.count - 1);
            self.selected = 0;
            self.needs_load = true;
        } else {
            // Already at end — snap selection to last item
            if self.count > 0 {
                self.selected = self.count - 1;
            }
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

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                self.move_up(ctx);
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                self.move_down(ctx);
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) => {
                self.jump_up();
                if self.needs_load {
                    // on_work will mark dirty
                } else {
                    ctx.mark_dirty(self.list_region());
                    ctx.mark_dirty(STATUS_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) => {
                self.jump_down();
                if self.needs_load {
                    // on_work will mark dirty
                } else {
                    ctx.mark_dirty(self.list_region());
                    ctx.mark_dirty(STATUS_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Select) => {
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

            _ => Transition::None,
        }
    }

    fn help_text(&self) -> &'static str {
        "Prev/Next: scroll  Jump: page  Sel: open  Back: exit"
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let header_region =
            Region::new(LIST_X, CONTENT_TOP + 8, 300, self.heading_font.line_height);
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

                BitmapLabel::new(region, name, self.body_font)
                    .alignment(Alignment::CenterLeft)
                    .inverted(i == self.selected)
                    .draw(strip)
                    .unwrap();
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
