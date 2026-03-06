// launcher screen: menu, bookmarks browser

use core::fmt::Write as _;

use crate::apps::{App, AppContext, AppId, RECENT_FILE, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::kernel::KernelHandle;
use crate::kernel::bookmarks::{self, BmListEntry};
use crate::ui::{
    Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, FULL_CONTENT_W, HEADER_W, LARGE_MARGIN,
    Region, SECTION_GAP, TITLE_Y_OFFSET,
};

const ITEM_W: u16 = 280;
const ITEM_H: u16 = 52;
const ITEM_GAP: u16 = 14;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;
const ITEM_X: u16 = (SCREEN_W - ITEM_W) / 2;
const TITLE_ITEM_GAP: u16 = 24;
const MAX_ITEMS: usize = 5;

// bookmark list layout (matches Files app)
const BM_ROW_H: u16 = 52;
const BM_ROW_GAP: u16 = 4;
const BM_ROW_STRIDE: u16 = BM_ROW_H + BM_ROW_GAP;
const BM_TITLE_Y: u16 = CONTENT_TOP + TITLE_Y_OFFSET;
const BM_HEADER_LIST_GAP: u16 = SECTION_GAP;
const BM_STATUS_W: u16 = 144;
const BM_STATUS_X: u16 = SCREEN_W - LARGE_MARGIN - BM_STATUS_W;

const CONTENT_REGION: Region = Region::new(0, CONTENT_TOP, SCREEN_W, SCREEN_H - CONTENT_TOP);

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

#[derive(Clone, Copy, PartialEq, Debug)]
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
    ui_fonts: fonts::UiFonts,
    item_regions: [Region; MAX_ITEMS],
    item_count: usize,

    recent_book: [u8; 32],
    recent_book_len: usize,
    needs_load_recent: bool,

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
        let uf = fonts::UiFonts::for_size(0);
        Self {
            state: HomeState::Menu,
            selected: 0,
            ui_fonts: uf,
            item_regions: compute_item_regions(uf.heading.line_height),
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
        self.ui_fonts = fonts::UiFonts::for_size(idx);
        self.item_regions = compute_item_regions(self.ui_fonts.heading.line_height);
    }

    // Session state accessors for RTC persistence
    #[inline]
    pub fn state_id(&self) -> u8 {
        match self.state {
            HomeState::Menu => 0,
            HomeState::ShowBookmarks => 1,
        }
    }

    #[inline]
    pub fn selected(&self) -> usize {
        self.selected
    }

    #[inline]
    pub fn bm_selected(&self) -> usize {
        self.bm_selected
    }

    #[inline]
    pub fn bm_scroll(&self) -> usize {
        self.bm_scroll
    }

    // restore home state from RTC session data
    pub fn restore_state(
        &mut self,
        state_id: u8,
        selected: usize,
        bm_selected: usize,
        bm_scroll: usize,
    ) {
        self.state = match state_id {
            1 => HomeState::ShowBookmarks,
            _ => HomeState::Menu,
        };
        self.selected = selected;
        self.bm_selected = bm_selected;
        self.bm_scroll = bm_scroll;
        if self.state == HomeState::ShowBookmarks {
            self.needs_load_bookmarks = true;
        }
        log::info!(
            "home: restore_state state={:?} selected={}",
            self.state,
            selected
        );
    }

    pub fn load_recent(&mut self, k: &mut KernelHandle<'_>) {
        let mut buf = [0u8; 32];
        match k.read_app_data_start(RECENT_FILE, &mut buf) {
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

    fn bm_list_y(&self) -> u16 {
        BM_TITLE_Y + self.ui_fonts.heading.line_height + BM_HEADER_LIST_GAP
    }

    fn bm_visible_lines(&self) -> usize {
        let available = SCREEN_H.saturating_sub(self.bm_list_y());
        let rows = (available / BM_ROW_STRIDE) as usize;
        rows.max(1).min(bookmarks::SLOTS)
    }

    fn bm_row_region(&self, i: usize) -> Region {
        Region::new(
            LARGE_MARGIN,
            self.bm_list_y() + i as u16 * BM_ROW_STRIDE,
            FULL_CONTENT_W,
            BM_ROW_H,
        )
    }

    fn bm_list_region(&self) -> Region {
        let vis = self.bm_visible_lines();
        Region::new(
            LARGE_MARGIN,
            self.bm_list_y(),
            FULL_CONTENT_W,
            BM_ROW_STRIDE * vis as u16,
        )
    }

    fn bm_status_region(&self) -> Region {
        Region::new(
            BM_STATUS_X,
            BM_TITLE_Y,
            BM_STATUS_W,
            self.ui_fonts.heading.line_height,
        )
    }
}

impl App<AppId> for HomeApp {
    fn on_enter(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        ctx.clear_message();
        self.state = HomeState::Menu;
        self.selected = 0;
        ctx.mark_dirty(CONTENT_REGION);
    }

    fn on_resume(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        self.state = HomeState::Menu;
        self.selected = 0;
        self.needs_load_recent = true;
        ctx.mark_dirty(CONTENT_REGION);
    }

    async fn background(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        if self.needs_load_recent {
            let old_count = self.item_count;
            let mut buf = [0u8; 32];
            match k.read_app_data_start(RECENT_FILE, &mut buf) {
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
            self.bm_count = k.bookmark_cache().load_all(&mut self.bm_entries);
            // resolve titles from dir cache
            let _ = k.ensure_dir_cache_loaded();
            for i in 0..self.bm_count {
                let entry = &self.bm_entries[i];
                let fname = &entry.filename[..entry.name_len as usize];
                if let Some((title, len)) = k.dir_cache_mut().find_title(fname) {
                    let mut tbuf = [0u8; 96];
                    let n = (len as usize).min(96);
                    tbuf[..n].copy_from_slice(&title[..n]);
                    self.bm_entries[i].set_title(&tbuf[..n]);
                } else {
                    // inline humanize: lowercase all-upper SFN filenames
                    humanize_bm_entry(&mut self.bm_entries[i]);
                }
            }
            self.needs_load_bookmarks = false;
            if self.state == HomeState::ShowBookmarks {
                ctx.mark_dirty(self.bm_list_region());
            }
        }
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match self.state {
            HomeState::Menu => self.on_event_menu(event, ctx),
            HomeState::ShowBookmarks => self.on_event_bookmarks(event, ctx),
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
                    let old = self.bm_selected;
                    let vis = self.bm_visible_lines();
                    if self.bm_selected + 1 < self.bm_count {
                        self.bm_selected += 1;
                        if self.bm_selected >= self.bm_scroll + vis {
                            self.bm_scroll = self.bm_selected + 1 - vis;
                            ctx.mark_dirty(self.bm_list_region());
                        } else {
                            ctx.mark_dirty(self.bm_row_region(old - self.bm_scroll));
                            ctx.mark_dirty(self.bm_row_region(self.bm_selected - self.bm_scroll));
                        }
                    } else {
                        self.bm_selected = 0;
                        self.bm_scroll = 0;
                        ctx.mark_dirty(self.bm_list_region());
                    }
                    ctx.mark_dirty(self.bm_status_region());
                }
                Transition::None
            }

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                if self.bm_count > 0 {
                    let old = self.bm_selected;
                    let vis = self.bm_visible_lines();
                    if self.bm_selected > 0 {
                        self.bm_selected -= 1;
                        if self.bm_selected < self.bm_scroll {
                            self.bm_scroll = self.bm_selected;
                            ctx.mark_dirty(self.bm_list_region());
                        } else {
                            ctx.mark_dirty(self.bm_row_region(old - self.bm_scroll));
                            ctx.mark_dirty(self.bm_row_region(self.bm_selected - self.bm_scroll));
                        }
                    } else {
                        self.bm_selected = self.bm_count - 1;
                        if self.bm_selected >= vis {
                            self.bm_scroll = self.bm_selected + 1 - vis;
                        }
                        ctx.mark_dirty(self.bm_list_region());
                    }
                    ctx.mark_dirty(self.bm_status_region());
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
                    ctx.mark_dirty(self.bm_list_region());
                    ctx.mark_dirty(self.bm_status_region());
                }
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) => {
                let vis = self.bm_visible_lines();
                self.bm_selected = self.bm_selected.saturating_sub(vis);
                if self.bm_selected < self.bm_scroll {
                    self.bm_scroll = self.bm_selected;
                }
                ctx.mark_dirty(self.bm_list_region());
                ctx.mark_dirty(self.bm_status_region());
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
            self.ui_fonts.heading.line_height,
        );
        BitmapLabel::new(title_region, "pulp-os", self.ui_fonts.heading)
            .alignment(Alignment::Center)
            .draw(strip)
            .unwrap();

        for i in 0..self.item_count {
            let label = self.item_label(i);
            BitmapLabel::new(self.item_regions[i], label, self.ui_fonts.body)
                .alignment(Alignment::Center)
                .inverted(i == self.selected)
                .draw(strip)
                .unwrap();
        }
    }

    fn draw_bookmarks(&self, strip: &mut StripBuffer) {
        let header_region = Region::new(
            LARGE_MARGIN,
            BM_TITLE_Y,
            HEADER_W,
            self.ui_fonts.heading.line_height,
        );
        BitmapLabel::new(header_region, "Bookmarks", self.ui_fonts.heading)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.bm_count > 0 {
            let mut status = BitmapDynLabel::<20>::new(self.bm_status_region(), self.ui_fonts.body)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "{}/{}", self.bm_selected + 1, self.bm_count);
            status.draw(strip).unwrap();
        }

        if self.bm_count == 0 {
            BitmapLabel::new(self.bm_row_region(0), "No bookmarks", self.ui_fonts.body)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        let vis = self.bm_visible_lines();
        let visible = vis.min(self.bm_count.saturating_sub(self.bm_scroll));

        for i in 0..vis {
            let region = self.bm_row_region(i);
            if i < visible {
                let idx = self.bm_scroll + i;
                let entry = &self.bm_entries[idx];
                let name = entry.display_name();

                BitmapLabel::new(region, name, self.ui_fonts.body)
                    .alignment(Alignment::CenterLeft)
                    .inverted(idx == self.bm_selected)
                    .draw(strip)
                    .unwrap();
            }
        }
    }
}

// humanize an all-uppercase SFN bookmark filename into the title field
fn humanize_bm_entry(entry: &mut BmListEntry) {
    if entry.name_len == 0 || entry.title_len > 0 {
        return;
    }
    if let Some(n) = crate::drivers::storage::humanize_upper(
        &entry.filename[..entry.name_len as usize],
        &mut entry.title,
    ) {
        entry.title_len = n;
    }
}
