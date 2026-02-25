//! File browser app.
//!
//! Displays a paginated list of files from the SD card root directory.
//! Up/Down to navigate, Confirm to open files, Back to return home.
//!
//! Storage access pattern: FilesApp doesn't touch hardware directly.
//! It sets `needs_load = true`, and main.rs calls `load_page()` with
//! data from the storage driver.
//!
//! ## Render ownership
//!
//! Two kinds of visual updates:
//!
//! - **Within-page scroll**: selection highlight moves, data unchanged.
//!   `move_up`/`move_down` call `ctx.mark_dirty()` on the old and new
//!   rows. The framework coalesces them (union of ~100px vs full 364px
//!   list — 3.8× less SPI data, less visible flicker).
//!
//! - **Page-boundary scroll**: new data needed from cache.
//!   Sets `needs_load = true` only (no mark_dirty). The LoadDirectory
//!   job owns the render decision — it fires after data arrives and
//!   requests a partial redraw of LIST_REGION.

use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_6X13};
use embedded_graphics::prelude::Primitive;
use embedded_graphics::Drawable;

use crate::apps::{App, AppContext, AppId, Services, Transition};
use crate::board::button::Button as HwButton;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::drivers::storage::DirEntry;
use crate::ui::{Alignment, Button as UiButton, CONTENT_TOP, DynamicLabel, Label, Region, Widget};

/// How many entries fit on screen at once.
const PAGE_SIZE: usize = 7;

/// Layout — all Y values relative to CONTENT_TOP.
const HEADER_REGION: Region = Region::new(16, CONTENT_TOP + 4, 300, 28);
const STATUS_REGION: Region = Region::new(320, CONTENT_TOP + 4, 140, 28);

const LIST_Y: u16 = CONTENT_TOP + 40;
const ROW_H: u16 = 52;

/// The scrollable file list region.
const LIST_REGION: Region = Region::new(8, LIST_Y, 464, ROW_H * PAGE_SIZE as u16);

fn row_region(index: usize) -> Region {
    Region::new(16, LIST_Y + index as u16 * ROW_H, 448, ROW_H - 4)
}

pub struct FilesApp {
    entries: [DirEntry; PAGE_SIZE],
    count: usize,
    total: usize,
    scroll: usize,
    selected: usize,
    needs_load: bool,
    /// Set on fresh entry — tells on_work to invalidate the dir cache
    /// before loading, since SD contents may have changed.
    stale_cache: bool,
    error: Option<&'static str>,
}

impl FilesApp {
    pub const fn new() -> Self {
        Self {
            entries: [DirEntry::EMPTY; PAGE_SIZE],
            count: 0,
            total: 0,
            scroll: 0,
            selected: 0,
            needs_load: false,
            stale_cache: false,
            error: None,
        }
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

    fn move_up(&mut self, ctx: &mut AppContext) {
        if self.selected > 0 {
            // Within-page: mark old and new rows dirty.
            ctx.mark_dirty(row_region(self.selected));
            self.selected -= 1;
            ctx.mark_dirty(row_region(self.selected));
        } else if self.scroll > 0 {
            // Page boundary: need fresh data from cache.
            // LoadDirectory owns the render — don't mark dirty here.
            self.scroll = self.scroll.saturating_sub(1);
            self.needs_load = true;
        }
    }

    fn move_down(&mut self, ctx: &mut AppContext) {
        if self.selected + 1 < self.count {
            // Within-page: mark old and new rows dirty.
            ctx.mark_dirty(row_region(self.selected));
            self.selected += 1;
            ctx.mark_dirty(row_region(self.selected));
        } else if self.scroll + self.count < self.total {
            // Page boundary: need fresh data from cache.
            // LoadDirectory owns the render — don't mark dirty here.
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
        self.stale_cache = true; // SD may have changed since last visit
        self.error = None;
        // Full redraw for header/chrome. AppWork will populate the
        // list and inherit this Full request (partial won't downgrade
        // it — see AppContext::request_partial_redraw).
        ctx.request_full_redraw();
    }

    fn on_exit(&mut self) {
        self.count = 0;
    }

    /// Pushed behind a child app (e.g. Reader). Preserve scroll
    /// position and cached entries — don't clear count.
    fn on_suspend(&mut self) {
        // no-op: entries, scroll, selected all stay valid
    }

    /// Returning from a child app. Entries are still cached,
    /// just repaint. No SD reload needed.
    fn on_resume(&mut self, ctx: &mut AppContext) {
        ctx.request_full_redraw();
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

        ctx.mark_dirty(LIST_REGION);
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
        // Header
        Label::new(HEADER_REGION, "Files", &FONT_10X20)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        // Status (page indicator)
        if self.total > 0 {
            let mut status = DynamicLabel::<20>::new(STATUS_REGION, &FONT_6X13)
                .alignment(Alignment::CenterRight);
            use core::fmt::Write;
            let _ = write!(
                status,
                "{}/{}",
                self.scroll + self.selected + 1,
                self.total
            );
            status.draw(strip).unwrap();
        }

        // Error state
        if let Some(msg) = self.error {
            Label::new(row_region(0), msg, &FONT_10X20)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        // Empty state
        if self.count == 0 && !self.needs_load {
            Label::new(row_region(0), "No files found", &FONT_10X20)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        // File list
        for i in 0..PAGE_SIZE {
            let region = row_region(i);

            if i < self.count {
                let entry = &self.entries[i];
                let name = entry.name_str();

                let mut btn = UiButton::new(region, name, &FONT_10X20);
                if i == self.selected {
                    btn.set_pressed(true);
                }
                btn.draw(strip).unwrap();
            } else {
                // Clear empty rows
                region
                    .to_rect()
                    .into_styled(
                        embedded_graphics::primitives::PrimitiveStyle::with_fill(
                            embedded_graphics::pixelcolor::BinaryColor::Off,
                        ),
                    )
                    .draw(strip)
                    .unwrap();
            }
        }
    }

}
