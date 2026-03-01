// Launcher screen, entry point after boot.
// Menu: Continue (if recent) / Files / Bookmarks / Settings / Upload.
// Bookmarks shows a scrollable list of saved positions, most-recent-first.

use core::fmt::Write as _;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::apps::bookmarks::{self, BmListEntry};
use crate::apps::reader::RECENT_FILE;
use crate::apps::{App, AppContext, AppId, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::ui::{Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, Region};

// menu layout constants
const ITEM_W: u16 = 280;
const ITEM_H: u16 = 52;
const ITEM_GAP: u16 = 14;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;
const ITEM_X: u16 = (480 - ITEM_W) / 2;
const TITLE_ITEM_GAP: u16 = 24;
const MAX_ITEMS: usize = 5;

// bookmark list layout
const BM_MARGIN: u16 = 8;
const BM_HEADER_GAP: u16 = 4;
const BM_BOTTOM: u16 = 790;
const SCREEN_W: u16 = 480;

fn compute_item_regions(heading_line_h: u16) -> [Region; MAX_ITEMS] {
    let item_y = CONTENT_TOP + 8 + heading_line_h + TITLE_ITEM_GAP;
    [
        Region::new(ITEM_X, item_y, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE * 2, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE * 3, ITEM_W, ITEM_H),
        Region::new(ITEM_X, item_y + ITEM_STRIDE * 4, ITEM_W, ITEM_H),
    ]
}

#[derive(Clone, Copy, PartialEq)]
enum HomeState {
    Menu,
    ShowBookmarks,
}

enum MenuAction {
    Continue,
    Push(AppId),
    OpenBookmarks,
}

pub struct HomeApp {
    state: HomeState,
    selected: usize,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
    item_regions: [Region; MAX_ITEMS],
    item_count: usize,

    // recent book for "Continue" button
    recent_book: [u8; 32],
    recent_book_len: usize,
    needs_load_recent: bool,

    // bookmark browser state
    bm_entries: [BmListEntry; bookmarks::SLOTS],
    bm_count: usize,
    bm_selected: usize,
    bm_scroll: usize,
    needs_load_bookmarks: bool,
}

impl Default for HomeApp {
    fn default() -> Self {
        Self::new()
    }
}

impl HomeApp {
    pub fn new() -> Self {
        let hf = fonts::heading_font(0);
        Self {
            state: HomeState::Menu,
            selected: 0,
            body_font: fonts::body_font(0),
            heading_font: hf,
            item_regions: compute_item_regions(hf.line_height),
            item_count: 4, // updated after load; may include Continue
            recent_book: [0u8; 32],
            recent_book_len: 0,
            needs_load_recent: false,
            bm_entries: [BmListEntry::EMPTY; bookmarks::SLOTS],
            bm_count: 0,
            bm_selected: 0,
            bm_scroll: 0,
            needs_load_bookmarks: false,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = fonts::body_font(idx);
        self.heading_font = fonts::heading_font(idx);
        self.item_regions = compute_item_regions(self.heading_font.line_height);
    }

    // called once at boot before first render
    pub fn load_recent<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        services: &mut Services<'_, SPI>,
    ) {
        let mut buf = [0u8; 32];
        match services.read_pulp_start(RECENT_FILE, &mut buf) {
            Ok((_, n)) if n > 0 => {
                let n = n.min(32);
                self.recent_book[..n].copy_from_slice(&buf[..n]);
                self.recent_book_len = n;
            }
            _ => {
                self.recent_book_len = 0;
            }
        }
        self.rebuild_item_count();
        self.needs_load_recent = false;
    }

    fn rebuild_item_count(&mut self) {
        self.item_count = if self.recent_book_len > 0 { 5 } else { 4 };
        if self.selected >= self.item_count {
            self.selected = 0;
        }
    }

    fn has_recent(&self) -> bool {
        self.recent_book_len > 0
    }

    fn item_label(&self, idx: usize) -> &str {
        if self.has_recent() {
            match idx {
                0 => "Continue",
                1 => "Files",
                2 => "Bookmarks",
                3 => "Settings",
                _ => "Upload",
            }
        } else {
            match idx {
                0 => "Files",
                1 => "Bookmarks",
                2 => "Settings",
                _ => "Upload",
            }
        }
    }

    fn item_action(&self, idx: usize) -> MenuAction {
        if self.has_recent() {
            match idx {
                0 => MenuAction::Continue,
                1 => MenuAction::Push(AppId::Files),
                2 => MenuAction::OpenBookmarks,
                3 => MenuAction::Push(AppId::Settings),
                _ => MenuAction::Push(AppId::Upload),
            }
        } else {
            match idx {
                0 => MenuAction::Push(AppId::Files),
                1 => MenuAction::OpenBookmarks,
                2 => MenuAction::Push(AppId::Settings),
                _ => MenuAction::Push(AppId::Upload),
            }
        }
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut AppContext) {
        let count = self.item_count;
        if count == 0 {
            return;
        }
        let new = (self.selected as isize + delta).rem_euclid(count as isize) as usize;
        if new != self.selected {
            ctx.mark_dirty(self.item_regions[self.selected]);
            self.selected = new;
            ctx.mark_dirty(self.item_regions[self.selected]);
        }
    }

    fn bm_text_y(&self) -> u16 {
        CONTENT_TOP + 4 + self.heading_font.line_height + BM_HEADER_GAP
    }

    fn bm_visible_lines(&self) -> usize {
        let area_h = BM_BOTTOM.saturating_sub(self.bm_text_y());
        (area_h / self.body_font.line_height).max(1) as usize
    }

    fn bm_page_region(&self) -> Region {
        Region::new(0, self.bm_text_y(), SCREEN_W, BM_BOTTOM - self.bm_text_y())
    }
}

impl App for HomeApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        ctx.clear_message();
        self.state = HomeState::Menu;
        self.selected = 0;
        ctx.mark_dirty(Region::new(0, CONTENT_TOP, 480, 800 - CONTENT_TOP));
    }

    fn on_resume(&mut self, ctx: &mut AppContext) {
        self.state = HomeState::Menu;
        self.selected = 0;
        self.needs_load_recent = true;
        ctx.mark_dirty(Region::new(0, CONTENT_TOP, 480, 800 - CONTENT_TOP));
    }

    fn needs_work(&self) -> bool {
        self.needs_load_recent || self.needs_load_bookmarks
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        if self.needs_load_recent {
            let old_count = self.item_count;
            let mut buf = [0u8; 32];
            match svc.read_pulp_start(RECENT_FILE, &mut buf) {
                Ok((_, n)) if n > 0 => {
                    let n = n.min(32);
                    self.recent_book[..n].copy_from_slice(&buf[..n]);
                    self.recent_book_len = n;
                }
                _ => {
                    self.recent_book_len = 0;
                }
            }
            self.rebuild_item_count();
            self.needs_load_recent = false;
            if self.item_count != old_count {
                ctx.request_full_redraw();
            }
        }

        if self.needs_load_bookmarks {
            self.bm_count = svc.bookmarks().load_all(&mut self.bm_entries);
            self.needs_load_bookmarks = false;
            if self.state == HomeState::ShowBookmarks {
                ctx.mark_dirty(self.bm_page_region());
            }
        }
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match self.state {
            HomeState::Menu => self.on_event_menu(event, ctx),
            HomeState::ShowBookmarks => self.on_event_bookmarks(event, ctx),
        }
    }

    fn help_text(&self) -> &'static str {
        match self.state {
            HomeState::Menu => "Prev/Next: select    Confirm: open",
            HomeState::ShowBookmarks => "Prev/Next: scroll  Sel: open  Back: menu",
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        match self.state {
            HomeState::Menu => self.draw_menu(strip),
            HomeState::ShowBookmarks => self.draw_bookmarks(strip),
        }
    }
}

impl HomeApp {
    fn on_event_menu(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Next) => {
                self.move_selection(1, ctx);
                Transition::None
            }
            ActionEvent::Press(Action::Prev) => {
                self.move_selection(-1, ctx);
                Transition::None
            }
            ActionEvent::Press(Action::Select) => match self.item_action(self.selected) {
                MenuAction::Continue => {
                    if self.has_recent() {
                        ctx.set_message(&self.recent_book[..self.recent_book_len]);
                    }
                    Transition::Push(AppId::Reader)
                }
                MenuAction::Push(app) => Transition::Push(app),
                MenuAction::OpenBookmarks => {
                    self.bm_selected = 0;
                    self.bm_scroll = 0;
                    self.needs_load_bookmarks = true;
                    self.state = HomeState::ShowBookmarks;
                    ctx.request_full_redraw();
                    Transition::None
                }
            },
            _ => Transition::None,
        }
    }

    fn on_event_bookmarks(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Back) | ActionEvent::LongPress(Action::Back) => {
                self.state = HomeState::Menu;
                ctx.request_full_redraw();
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                if self.bm_count > 0 {
                    if self.bm_selected + 1 < self.bm_count {
                        self.bm_selected += 1;
                    } else {
                        self.bm_selected = 0;
                        self.bm_scroll = 0;
                    }
                    let vis = self.bm_visible_lines();
                    if self.bm_selected >= self.bm_scroll + vis {
                        self.bm_scroll = self.bm_selected + 1 - vis;
                    }
                    ctx.mark_dirty(self.bm_page_region());
                }
                Transition::None
            }

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                if self.bm_count > 0 {
                    if self.bm_selected > 0 {
                        self.bm_selected -= 1;
                    } else {
                        self.bm_selected = self.bm_count - 1;
                        let vis = self.bm_visible_lines();
                        if self.bm_selected >= vis {
                            self.bm_scroll = self.bm_selected + 1 - vis;
                        }
                    }
                    if self.bm_selected < self.bm_scroll {
                        self.bm_scroll = self.bm_selected;
                    }
                    ctx.mark_dirty(self.bm_page_region());
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) => {
                if self.bm_count > 0 {
                    let vis = self.bm_visible_lines();
                    self.bm_selected = (self.bm_selected + vis).min(self.bm_count - 1);
                    if self.bm_selected >= self.bm_scroll + vis {
                        self.bm_scroll = self.bm_selected + 1 - vis;
                    }
                    ctx.mark_dirty(self.bm_page_region());
                }
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) => {
                let vis = self.bm_visible_lines();
                self.bm_selected = self.bm_selected.saturating_sub(vis);
                if self.bm_selected < self.bm_scroll {
                    self.bm_scroll = self.bm_selected;
                }
                ctx.mark_dirty(self.bm_page_region());
                Transition::None
            }

            ActionEvent::Press(Action::Select) => {
                if self.bm_count > 0 && self.bm_selected < self.bm_count {
                    let slot = &self.bm_entries[self.bm_selected];
                    ctx.set_message(&slot.filename[..slot.name_len as usize]);
                    self.state = HomeState::Menu;
                    Transition::Push(AppId::Reader)
                } else {
                    Transition::None
                }
            }

            _ => Transition::None,
        }
    }
}

impl HomeApp {
    fn draw_menu(&self, strip: &mut StripBuffer) {
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

        for i in 0..self.item_count {
            let label = self.item_label(i);
            BitmapLabel::new(self.item_regions[i], label, self.body_font)
                .alignment(Alignment::Center)
                .inverted(i == self.selected)
                .draw(strip)
                .unwrap();
        }
    }

    fn draw_bookmarks(&self, strip: &mut StripBuffer) {
        let header_region = Region::new(
            BM_MARGIN,
            CONTENT_TOP + 4,
            SCREEN_W - BM_MARGIN * 2,
            self.heading_font.line_height,
        );
        BitmapLabel::new(header_region, "Bookmarks", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.bm_count > 0 {
            let status_region = Region::new(
                SCREEN_W / 2,
                CONTENT_TOP + 4,
                SCREEN_W / 2 - BM_MARGIN,
                self.heading_font.line_height,
            );
            let mut status = BitmapDynLabel::<20>::new(status_region, self.body_font)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "{}/{}", self.bm_selected + 1, self.bm_count);
            status.draw(strip).unwrap();
        }

        if self.bm_count == 0 {
            let r = Region::new(BM_MARGIN, self.bm_text_y(), 300, self.body_font.line_height);
            BitmapLabel::new(r, "No bookmarks saved", self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        let font = self.body_font;
        let line_h = font.line_height as i32;
        let ascent = font.ascent as i32;
        let text_y = self.bm_text_y() as i32;
        let vis = self.bm_visible_lines();
        let visible = vis.min(self.bm_count.saturating_sub(self.bm_scroll));

        for i in 0..visible {
            let idx = self.bm_scroll + i;
            let entry = &self.bm_entries[idx];
            let y_top = text_y + i as i32 * line_h;
            let baseline = y_top + ascent;
            let selected = idx == self.bm_selected;

            if selected {
                embedded_graphics::primitives::Rectangle::new(
                    Point::new(0, y_top),
                    Size::new(SCREEN_W as u32, line_h as u32),
                )
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(strip)
                .unwrap();
            }

            let fg = if selected {
                BinaryColor::Off
            } else {
                BinaryColor::On
            };

            let mut cx = BM_MARGIN as i32;

            if entry.chapter > 0 {
                let mut ch_buf = [0u8; 8];
                let ch_len = fmt_chapter_prefix(&mut ch_buf, entry.chapter);
                for &b in &ch_buf[..ch_len] {
                    let ch = if (0x20..=0x7E).contains(&b) {
                        b as char
                    } else {
                        '?'
                    };
                    cx += font.draw_char_fg(strip, ch, fg, cx, baseline) as i32;
                }
            }

            font.draw_str_fg(strip, entry.filename_str(), fg, cx, baseline);
        }
    }
}

// format "Ch{N} " into buf (1-based), return byte count
fn fmt_chapter_prefix(buf: &mut [u8; 8], chapter: u16) -> usize {
    let n = chapter + 1;
    buf[0] = b'C';
    buf[1] = b'h';
    let mut pos = 2;
    if n >= 100 {
        buf[pos] = b'0' + ((n / 100) % 10) as u8;
        pos += 1;
    }
    if n >= 10 {
        buf[pos] = b'0' + ((n / 10) % 10) as u8;
        pos += 1;
    }
    buf[pos] = b'0' + (n % 10) as u8;
    pos += 1;
    buf[pos] = b' ';
    pos + 1
}
