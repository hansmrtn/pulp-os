mod epubs;
mod images;
mod paging;

pub use pulp_kernel::util::decode_utf8_char;

use crate::apps::PendingSetting;
use crate::fonts::bitmap::{self, BitmapFont};

use alloc::vec::Vec;
use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_9X18;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;

use crate::apps::{App, AppContext, AppId, RECENT_FILE, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::error::{Error, ErrorKind};
use crate::fonts;
use crate::kernel::KernelHandle;
use crate::kernel::QuickAction;
use crate::kernel::bookmarks;
use crate::kernel::work_queue;
use crate::kernel::work_queue::DecodedImage;
use crate::ui::{Alignment, CONTENT_TOP, HEADER_W, Region, StackFmt, TITLE_Y_OFFSET};
use smol_epub::cache;
use smol_epub::epub::{self, EpubMeta, EpubSpine, EpubToc, TocSource};
use smol_epub::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, ITALIC_OFF, ITALIC_ON, MARKER,
};
use smol_epub::zip::{self, ZipIndex};

// chrome margin: used for header, status, progress bar, loading indicator.
// this never changes; only the text content area responds to the reading theme.
pub(super) const MARGIN: u16 = 8;

pub(super) const HEADER_Y: u16 = CONTENT_TOP + TITLE_Y_OFFSET - 2; // slightly tighter
pub(super) const HEADER_H: u16 = 16;

pub(super) const TEXT_Y: u16 = HEADER_Y + HEADER_H + 2;

pub(super) const LINE_H: u16 = 20;

pub(super) const CHARS_PER_LINE: usize = 51;

pub(super) const LINES_PER_PAGE: usize = 37;

pub(super) const PAGE_BUF: usize = 8192;

pub(super) const MAX_PAGES: usize = 1024;

pub(super) const HEADER_REGION: Region = Region::new(MARGIN, HEADER_Y, HEADER_W, HEADER_H);

const STATUS_X: u16 = MARGIN + HEADER_W + 8;
const STATUS_W: u16 = SCREEN_W - STATUS_X - MARGIN;
pub(super) const STATUS_REGION: Region = Region::new(STATUS_X, HEADER_Y, STATUS_W, HEADER_H);

pub(super) const PAGE_REGION: Region = Region::new(0, HEADER_Y, SCREEN_W, SCREEN_H - HEADER_Y);

pub(super) const NO_PREFETCH: usize = usize::MAX;

pub(super) const TEXT_W: u32 = (SCREEN_W - 2 * MARGIN) as u32;

pub(super) const TEXT_AREA_H: u16 = SCREEN_H - TEXT_Y - 4;

pub(super) const EOCD_TAIL: usize = 512;

pub(super) const INDENT_PX: u32 = 24;

// max inline images tracked per page buffer for dimension pre-scan
pub(super) const MAX_IMAGES_PER_PAGE: usize = 8;

// default image height budget (half text area) used when actual
// dimensions are unavailable (e.g. uncached deflated images, or
// during preindex_all_pages where no pre-scan runs)
pub(super) const DEFAULT_IMG_H: u16 = 350;

// inline images are capped at this fraction of the text area height.
// keeps illustrations proportional to surrounding text, similar to
// Kindle / Apple Books.  fullscreen images (sole content on a page)
// are not affected — they use the full text_area_h budget.
pub(super) const INLINE_IMG_MAX_PCT: u16 = 40;

#[inline]
pub(super) fn inline_img_max_h(text_area_h: u16) -> u16 {
    ((text_area_h as u32 * INLINE_IMG_MAX_PCT as u32) / 100) as u16
}

pub(super) const CHAPTER_CACHE_MAX: usize = 98304;

// images <= this size are dispatched to async worker for decoding;
// images > this size are decoded on main loop via streaming SD reads
pub(super) const PRECACHE_IMG_MAX: u32 = 30 * 1024;

const POSITION_OVERLAY_W: u16 = 280;
const POSITION_OVERLAY_H: u16 = 40;
pub(super) const POSITION_OVERLAY: Region = Region::new(
    (SCREEN_W - POSITION_OVERLAY_W) / 2,
    (SCREEN_H - POSITION_OVERLAY_H) / 2,
    POSITION_OVERLAY_W,
    POSITION_OVERLAY_H,
);

const LOADING_W: u16 = SCREEN_W - 2 * MARGIN - 16;
const LOADING_H: u16 = 24;
pub(super) const LOADING_REGION: Region = Region::new(MARGIN, TEXT_Y, LOADING_W, LOADING_H);

pub const QA_FONT_SIZE: u8 = 1;
pub(super) const QA_PREV_CHAPTER: u8 = 3;
pub(super) const QA_NEXT_CHAPTER: u8 = 4;
pub(super) const QA_TOC: u8 = 5;

pub(super) const QA_MAX: usize = 4;

// reader state machine:
// NeedBookmark -> NeedInit -> NeedOpf -> NeedToc -> NeedCache -> NeedIndex -> NeedPage -> Ready
// Ready <-> ShowToc (toc overlay); any state -> Error on failure
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum State {
    NeedBookmark,
    NeedInit,
    NeedOpf,
    NeedToc,
    NeedCache,
    NeedIndex,
    NeedPage,
    Ready,
    ShowToc,
    Error,
}

// background caching progress, runs independently of the reading
// state so the user can read while chapters/images are cached
#[derive(Clone, Copy, PartialEq)]
pub(super) enum BgCacheState {
    // nothing to do
    Idle,
    CacheChapter,
    WaitNearbyImage,
    CacheImage,
    WaitImage,
}

#[derive(Clone, Copy)]
pub(super) struct LineSpan {
    pub(super) start: u16,
    pub(super) len: u16,
    pub(super) flags: u8,
    pub(super) indent: u8,
}

impl LineSpan {
    pub(super) const EMPTY: Self = Self {
        start: 0,
        len: 0,
        flags: 0,
        indent: 0,
    };

    pub(super) const FLAG_BOLD: u8 = 1 << 0;
    pub(super) const FLAG_ITALIC: u8 = 1 << 1;
    pub(super) const FLAG_HEADING: u8 = 1 << 2;
    pub(super) const FLAG_IMAGE: u8 = 1 << 3;

    #[inline]
    pub(super) fn is_image(&self) -> bool {
        self.flags & Self::FLAG_IMAGE != 0
    }

    #[inline]
    pub(super) fn is_image_origin(&self) -> bool {
        self.is_image() && self.len > 0
    }

    pub(super) fn style(&self) -> fonts::Style {
        if self.flags & Self::FLAG_HEADING != 0 {
            fonts::Style::Heading
        } else if self.flags & Self::FLAG_BOLD != 0 {
            fonts::Style::Bold
        } else if self.flags & Self::FLAG_ITALIC != 0 {
            fonts::Style::Italic
        } else {
            fonts::Style::Regular
        }
    }

    pub(super) fn pack_flags(bold: bool, italic: bool, heading: bool) -> u8 {
        (bold as u8) | ((italic as u8) << 1) | ((heading as u8) << 2)
    }
}

// page index, content buffer, and read-ahead state
pub(super) struct PageState {
    pub(super) offsets: [u32; MAX_PAGES],
    pub(super) total_pages: usize,
    pub(super) fully_indexed: bool,

    pub(super) page: usize,
    pub(super) buf: [u8; PAGE_BUF],
    pub(super) buf_len: usize,
    pub(super) lines: [LineSpan; LINES_PER_PAGE],
    pub(super) line_count: usize,

    pub(super) prefetch: [u8; PAGE_BUF],
    pub(super) prefetch_len: usize,
    pub(super) prefetch_page: usize,
}

impl PageState {
    pub(super) const fn new() -> Self {
        Self {
            offsets: [0u32; MAX_PAGES],
            total_pages: 0,
            fully_indexed: false,
            page: 0,
            buf: [0u8; PAGE_BUF],
            buf_len: 0,
            lines: [LineSpan::EMPTY; LINES_PER_PAGE],
            line_count: 0,
            prefetch: [0u8; PAGE_BUF],
            prefetch_len: 0,
            prefetch_page: NO_PREFETCH,
        }
    }
}

// epub-specific state: zip index, metadata, spine, toc, chapter
// cache, background cache progress, image cache scan position
pub(super) struct EpubState {
    // --- publicly accessible from sibling modules ---
    pub(super) zip: ZipIndex,
    pub(super) meta: EpubMeta,
    pub(super) spine: EpubSpine,
    pub(super) chapter: u16,

    pub(super) cache_dir: [u8; 8],
    chapter_sizes: [u32; cache::MAX_CACHE_CHAPTERS],
    pub(super) chapters_cached: bool,
    pub(super) cache_chapter: u16,
    pub(super) ch_cached: [bool; cache::MAX_CACHE_CHAPTERS],
    pub(super) ch_cache: Vec<u8>,

    pub(super) bg_cache: BgCacheState,
    pub(super) work_gen: u16,

    pub(super) img_cache_ch: u16,
    pub(super) img_cache_offset: u32,
    pub(super) img_scan_wrapped: bool,

    pub(super) toc: EpubToc,
    pub(super) toc_source: Option<TocSource>,
    pub(super) toc_selected: usize,
    pub(super) toc_scroll: usize,

    // --- private: only accessed by impl EpubState methods ---
    name_hash: u32,
    archive_size: u32,
}

impl EpubState {
    pub(super) const fn new() -> Self {
        Self {
            zip: ZipIndex::new(),
            meta: EpubMeta::new(),
            spine: EpubSpine::new(),
            chapter: 0,
            cache_dir: [0u8; 8],
            name_hash: 0,
            archive_size: 0,
            chapter_sizes: [0u32; cache::MAX_CACHE_CHAPTERS],
            chapters_cached: false,
            cache_chapter: 0,
            ch_cached: [false; cache::MAX_CACHE_CHAPTERS],
            ch_cache: Vec::new(),
            bg_cache: BgCacheState::Idle,
            work_gen: 0,
            img_cache_ch: 0,
            img_cache_offset: 0,
            img_scan_wrapped: false,
            toc: EpubToc::new(),
            toc_source: None,
            toc_selected: 0,
            toc_scroll: 0,
        }
    }

    #[inline]
    pub(super) fn cache_dir_str(&self) -> &str {
        cache::dir_name_str(&self.cache_dir)
    }

    #[inline]
    pub(super) fn chapter_size(&self, ch: usize) -> u32 {
        if ch < cache::MAX_CACHE_CHAPTERS {
            self.chapter_sizes[ch]
        } else {
            0
        }
    }
}

impl Default for ReaderApp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ReaderApp {
    pub(super) filename: [u8; 32],
    pub(super) filename_len: usize,
    pub(super) title: [u8; 96],
    pub(super) title_len: usize,
    pub(super) file_size: u32,

    pub(super) pg: PageState,
    pub(super) epub: EpubState,

    pub(super) state: State,
    pub(super) error: Option<Error>,
    pub(super) show_position: bool,

    pub(super) is_epub: bool,
    pub(super) goto_last_page: bool,
    pub(super) restore_offset: Option<u32>,

    pub(super) page_img: Option<DecodedImage>,
    pub(super) fullscreen_img: bool,
    pub(super) defer_image_decode: bool,

    pub(super) fonts: Option<fonts::FontSet>,
    pub(super) font_line_h: u16,
    pub(super) font_ascent: u16,
    pub(super) max_lines: usize,

    // reading theme: runtime layout derived from READING_THEMES
    pub(super) text_margin: u16, // horizontal margin for text content (from theme)
    pub(super) text_y: u16,      // top of text area (TEXT_Y + theme vertical margin)
    pub(super) text_w: u32,      // text content width (SCREEN_W - 2 * text_margin)
    pub(super) text_area_h: u16, // height of text area (SCREEN_H - text_y - bottom_pad)
    pub(super) reading_theme_idx: u8,

    // pre-scanned image heights for the current page buffer;
    // populated before wrapping so the pager can reserve the exact
    // number of lines each image needs at its natural aspect ratio
    pub(super) img_heights: [u16; MAX_IMAGES_PER_PAGE],
    pub(super) img_height_count: u8,

    pub(super) book_font_size_idx: u8,
    pub(super) applied_font_idx: u8,

    pub(super) chrome_font: Option<&'static BitmapFont>,
    pub(super) qa_buf: [QuickAction; QA_MAX],
    pub(super) qa_count: usize,
}

impl ReaderApp {
    pub const fn new() -> Self {
        Self {
            filename: [0u8; 32],
            filename_len: 0,
            title: [0u8; 96],
            title_len: 0,
            file_size: 0,

            pg: PageState::new(),
            epub: EpubState::new(),

            state: State::NeedPage,
            error: None,
            show_position: false,

            is_epub: false,
            goto_last_page: false,
            restore_offset: None,

            page_img: None,
            fullscreen_img: false,
            defer_image_decode: false,

            fonts: None,
            font_line_h: LINE_H,
            font_ascent: LINE_H,
            max_lines: LINES_PER_PAGE,

            text_margin: MARGIN,
            text_y: TEXT_Y,
            text_w: TEXT_W,
            text_area_h: TEXT_AREA_H,
            reading_theme_idx: 0,

            img_heights: [0u16; MAX_IMAGES_PER_PAGE],
            img_height_count: 0,

            book_font_size_idx: 0,
            applied_font_idx: 0,

            chrome_font: None,

            qa_buf: [QuickAction::trigger(0, "", ""); QA_MAX],
            qa_count: 0,
        }
    }

    // 0 = XSmall, 1 = Small, 2 = Medium, 3 = Large, 4 = XLarge
    pub fn set_book_font_size(&mut self, idx: u8) {
        self.book_font_size_idx = idx;
        self.apply_font_metrics();
        self.rebuild_quick_actions();
    }

    pub fn set_reading_theme(&mut self, idx: u8) {
        self.reading_theme_idx = idx;
        self.apply_theme_layout();
        self.apply_font_metrics();
    }

    fn apply_theme_layout(&mut self) {
        let theme = crate::kernel::config::reading_theme(self.reading_theme_idx);
        self.text_margin = theme.margin_h;
        self.text_y = TEXT_Y + theme.margin_v;
        self.text_w = (SCREEN_W - 2 * self.text_margin) as u32;
        self.text_area_h = SCREEN_H.saturating_sub(self.text_y + 4);
    }

    pub fn set_chrome_font(&mut self, font: &'static BitmapFont) {
        self.chrome_font = Some(font);
    }

    pub fn has_bg_work(&self) -> bool {
        self.is_epub && self.epub.bg_cache != BgCacheState::Idle
    }

    pub(super) fn cached_chapter_count(&self) -> usize {
        let n = self.epub.spine.len().min(cache::MAX_CACHE_CHAPTERS);
        self.epub.ch_cached[..n].iter().filter(|&&c| c).count()
    }

    // update the kernel loading indicator with current caching progress
    fn set_cache_loading(&self, ctx: &mut AppContext) {
        let cached = self.cached_chapter_count();
        let total = self.epub.spine.len();
        let mut lbuf = StackFmt::<28>::new();
        if matches!(
            self.epub.bg_cache,
            BgCacheState::CacheChapter | BgCacheState::WaitNearbyImage
        ) && cached < total
        {
            let _ = write!(lbuf, "Caching {}/{}", cached, total);
        } else {
            let _ = write!(lbuf, "Caching image(s)");
        }
        let pct = if total > 0 {
            ((cached * 100) / total).min(100) as u8
        } else {
            100
        };
        ctx.set_loading(LOADING_REGION, lbuf.as_str(), pct);
    }

    // transition to error state with consistent handling
    fn enter_error(&mut self, ctx: &mut AppContext, e: Error) {
        self.error = Some(e);
        self.state = State::Error;
        ctx.clear_loading();
        ctx.mark_dirty(PAGE_REGION);
    }

    // run one step of image work queue polling while suspended;
    // chapter caching is async and only runs during active background,
    // so this only handles the sync image recv states
    pub fn bg_work_tick(&mut self, k: &mut KernelHandle<'_>) {
        match self.epub.bg_cache {
            BgCacheState::WaitNearbyImage => match self.epub_recv_image_result(k) {
                Ok(Some(_)) => {
                    if !self.try_dispatch_nearby_image(k) {
                        self.epub.bg_cache = BgCacheState::CacheChapter;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("bg: nearby image error (suspended): {}", e);
                    self.epub.bg_cache = BgCacheState::CacheChapter;
                }
            },
            BgCacheState::WaitImage => match self.epub_recv_image_result(k) {
                Ok(Some(_)) => self.epub.bg_cache = BgCacheState::CacheImage,
                Ok(None) => {}
                Err(e) => {
                    log::warn!("bg: image recv error (suspended): {}", e);
                    self.epub.bg_cache = BgCacheState::CacheImage;
                }
            },
            _ => {}
        }
    }

    fn rebuild_quick_actions(&mut self) {
        let mut n = 0usize;

        self.qa_buf[n] = QuickAction::cycle(
            QA_FONT_SIZE,
            "Book Font",
            self.book_font_size_idx,
            fonts::FONT_SIZE_NAMES,
        );
        n += 1;

        if self.is_epub && self.epub.spine.len() > 1 {
            self.qa_buf[n] = QuickAction::trigger(QA_PREV_CHAPTER, "Prev Ch", "<<<");
            n += 1;
            self.qa_buf[n] = QuickAction::trigger(QA_NEXT_CHAPTER, "Next Ch", ">>>");
            n += 1;
        }

        if self.is_epub && !self.epub.toc.is_empty() {
            self.qa_buf[n] = QuickAction::trigger(QA_TOC, "Contents", "Open");
            n += 1;
        }

        self.qa_count = n;
    }

    fn apply_font_metrics(&mut self) {
        self.fonts = None;
        self.font_line_h = LINE_H;
        self.font_ascent = LINE_H;
        self.max_lines = LINES_PER_PAGE;

        let theme = crate::kernel::config::reading_theme(self.reading_theme_idx);
        let spacing_pct = theme.line_spacing_pct;

        if fonts::font_data::HAS_REGULAR {
            let fs = fonts::FontSet::for_size(self.book_font_size_idx);
            let native_h = fs.line_height(fonts::Style::Regular).max(1);
            // apply line spacing: scale native line height by theme percentage
            self.font_line_h = ((native_h as u32 * spacing_pct as u32) / 100).max(1) as u16;
            self.font_ascent = fs.ascent(fonts::Style::Regular);
            self.max_lines = ((self.text_area_h / self.font_line_h) as usize).min(LINES_PER_PAGE);
            log::info!(
                "font: size_idx={} line_h={} (native {} x {}%) ascent={} max_lines={} margin={}",
                self.book_font_size_idx,
                self.font_line_h,
                native_h,
                spacing_pct,
                self.font_ascent,
                self.max_lines,
                self.text_margin,
            );
            self.fonts = Some(fs);
        }
        self.applied_font_idx = self.book_font_size_idx;
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.filename[..self.filename_len]).unwrap_or("???")
    }

    fn name_copy(&self) -> ([u8; 32], usize) {
        let mut buf = [0u8; 32];
        buf[..self.filename_len].copy_from_slice(&self.filename[..self.filename_len]);
        (buf, self.filename_len)
    }

    // Session state accessors for RTC persistence
    #[inline]
    pub fn filename_len(&self) -> usize {
        self.filename_len
    }

    #[inline]
    pub fn filename_bytes(&self) -> &[u8] {
        &self.filename[..self.filename_len]
    }

    #[inline]
    pub fn is_epub(&self) -> bool {
        self.is_epub
    }

    #[inline]
    pub fn chapter(&self) -> u16 {
        self.epub.chapter
    }

    #[inline]
    pub fn page(&self) -> usize {
        self.pg.page
    }

    #[inline]
    pub fn byte_offset(&self) -> u32 {
        if self.pg.page < self.pg.total_pages {
            self.pg.offsets[self.pg.page]
        } else {
            0
        }
    }

    #[inline]
    pub fn font_size_idx(&self) -> u8 {
        self.book_font_size_idx
    }

    pub fn restore_state(
        &mut self,
        filename: &[u8],
        is_epub: bool,
        chapter: u16,
        _page: usize,
        byte_offset: u32,
        font_size: u8,
    ) {
        let len = filename.len().min(32);
        self.filename[..len].copy_from_slice(&filename[..len]);
        self.filename_len = len;
        self.is_epub = is_epub;
        self.epub.chapter = chapter;
        self.restore_offset = if byte_offset > 0 {
            Some(byte_offset)
        } else {
            None
        };
        self.book_font_size_idx = font_size;

        log::info!(
            "reader: restore_state file={} ch={} off={}",
            self.name(),
            chapter,
            byte_offset
        );
    }

    pub fn save_position(&self, bm: &mut bookmarks::BookmarkCache) {
        if self.state == State::Ready {
            bm.save(
                &self.filename[..self.filename_len],
                self.pg.offsets[self.pg.page],
                self.epub.chapter,
            );
        }
    }

    fn bookmark_load(&mut self, bm: &bookmarks::BookmarkCache) -> bool {
        if let Some(slot) = bm.find(&self.filename[..self.filename_len]) {
            log::info!(
                "bookmark: restoring off={} ch={} for {}",
                slot.byte_offset,
                slot.chapter,
                slot.filename_str(),
            );
            self.epub.chapter = slot.chapter;
            self.restore_offset = if slot.byte_offset > 0 {
                Some(slot.byte_offset)
            } else {
                None
            };
            true
        } else {
            false
        }
    }

    fn display_name(&self) -> &str {
        if self.title_len > 0 {
            core::str::from_utf8(&self.title[..self.title_len]).unwrap_or(self.name())
        } else {
            self.name()
        }
    }

    fn progress_pct(&self) -> u8 {
        if self.is_epub && !self.epub.spine.is_empty() {
            let spine_len = self.epub.spine.len() as u64;
            let ch = self.epub.chapter as u64;

            if ch + 1 >= spine_len
                && self.pg.fully_indexed
                && self.pg.page + 1 >= self.pg.total_pages
            {
                return 100;
            }

            let in_ch = if self.file_size == 0 {
                0u64
            } else {
                let pos = self.pg.offsets[self.pg.page] as u64;
                let size = self.file_size as u64;
                ((pos * 100) / size).min(100)
            };

            let overall = (ch * 100 + in_ch) / spine_len;
            return overall.min(100) as u8;
        }

        if self.file_size == 0 {
            return 100;
        }
        if self.pg.fully_indexed && self.pg.page + 1 >= self.pg.total_pages {
            return 100;
        }
        let pos = self.pg.offsets[self.pg.page] as u64;
        let size = self.file_size as u64;
        ((pos * 100) / size).min(100) as u8
    }
}

// read_full: read exactly buf.len() bytes from name at offset
pub(super) fn read_full(
    k: &mut KernelHandle<'_>,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> crate::error::Result<()> {
    let mut total = 0usize;
    while total < buf.len() {
        let n = k.read_chunk(name, offset + total as u32, &mut buf[total..])?;
        if n == 0 {
            return Err(Error::new(
                ErrorKind::ReadFailed,
                "read_full: unexpected EOF",
            ));
        }
        total += n;
    }
    Ok(())
}

// extract_zip_entry: decompress or copy one ZIP entry to a Vec
pub(super) fn extract_zip_entry(
    k: &mut KernelHandle<'_>,
    name: &str,
    zip_index: &ZipIndex,
    entry_idx: usize,
) -> Result<alloc::vec::Vec<u8>, &'static str> {
    use core::cell::RefCell;
    let entry = zip_index.entry(entry_idx);
    let k = RefCell::new(k);
    zip::extract_entry(entry, entry.local_offset, |offset, buf| {
        k.borrow_mut()
            .read_chunk(name, offset, buf)
            .map_err(|e: Error| -> &'static str { e.into() })
    })
}

fn draw_chrome_text(
    strip: &mut StripBuffer,
    region: Region,
    text: &str,
    align: Alignment,
    font: Option<&'static BitmapFont>,
) {
    region
        .to_rect()
        .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
        .draw(strip)
        .unwrap();
    if text.is_empty() {
        return;
    }
    if let Some(f) = font {
        f.draw_aligned(strip, region, text, align, BinaryColor::On);
    } else {
        let tw = text.len() as u32 * 9;
        let pos = align.position(region, embedded_graphics::geometry::Size::new(tw, 18));
        let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::On);
        Text::new(text, Point::new(pos.x, pos.y + 18), style)
            .draw(strip)
            .unwrap();
    }
}

impl App<AppId> for ReaderApp {
    fn on_enter(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        let msg = ctx.message();
        let len = msg.len().min(32);
        self.filename[..len].copy_from_slice(&msg[..len]);
        self.filename_len = len;

        let n = self.filename_len.min(self.title.len());
        self.title[..n].copy_from_slice(&self.filename[..n]);
        self.title_len = n;

        // Bump to a new work-queue generation and drain stale work
        // from any previous book (covers the case where on_enter is
        // called without a preceding on_exit, e.g. Replace transition).
        self.epub.work_gen = work_queue::reset();
        self.epub.bg_cache = BgCacheState::Idle;
        self.epub.ch_cached = [false; cache::MAX_CACHE_CHAPTERS];
        self.epub.img_scan_wrapped = false;

        self.is_epub = epub::is_epub_filename(self.name());
        self.rebuild_quick_actions();
        self.apply_theme_layout();
        self.reset_paging();
        self.epub.ch_cache = Vec::new();
        self.file_size = 0;
        self.epub.chapter = 0;
        self.error = None;
        self.show_position = false;
        self.defer_image_decode = true;
        self.goto_last_page = false;
        self.restore_offset = None;

        self.apply_font_metrics();

        self.state = State::NeedBookmark;

        log::info!("reader: opening {}", self.name());

        ctx.set_loading(LOADING_REGION, "Opening", 0);
        ctx.mark_dirty(PAGE_REGION);
    }

    fn on_exit(&mut self) {
        // Cancel any in-flight background cache work so the worker
        // doesn't write stale results after we switch books.
        if self.is_epub {
            work_queue::reset();
            self.epub.bg_cache = BgCacheState::Idle;
        }

        self.pg.line_count = 0;
        self.pg.buf_len = 0;
        self.pg.prefetch_page = NO_PREFETCH;
        self.pg.prefetch_len = 0;
        self.restore_offset = None;
        self.show_position = false;
        self.epub.ch_cache = Vec::new();
        self.page_img = None;

        if self.is_epub {
            self.epub.toc.clear();
            self.epub.toc_source = None;
        }
    }

    fn on_suspend(&mut self) {
        // background caching continues while suspended -- the worker
        // task runs independently and our work_gen stays valid
    }

    fn on_resume(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        // Restore our generation so the worker considers in-flight
        // results current again (another app may have submitted work
        // under a different generation while we were suspended).
        if self.epub.work_gen != 0 {
            work_queue::set_active_generation(self.epub.work_gen);
        }

        // re-derive text area geometry from the (possibly changed) theme
        self.apply_theme_layout();

        let font_changed = self.book_font_size_idx != self.applied_font_idx;
        self.apply_font_metrics();
        if font_changed {
            self.reset_paging();
            if self.is_epub && self.epub.chapters_cached {
                self.state = State::NeedIndex;
            } else {
                self.state = State::NeedPage;
            }
        }
        ctx.mark_dirty(PAGE_REGION);
    }

    async fn background(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        loop {
            match self.state {
                State::NeedBookmark => {
                    self.bookmark_load(k.bookmark_cache());

                    let _ = k.write_app_data(RECENT_FILE, &self.filename[..self.filename_len]);

                    if self.is_epub {
                        self.epub.zip.clear();
                        self.epub.meta = EpubMeta::new();
                        self.epub.spine = EpubSpine::new();
                        self.epub.chapters_cached = false;
                        self.goto_last_page = false;
                        self.state = State::NeedInit;
                        ctx.set_loading(LOADING_REGION, "Loading", 10);
                    } else {
                        self.state = State::NeedPage;
                        ctx.set_loading(LOADING_REGION, "Loading", 50);
                    }
                    continue;
                }

                State::NeedInit => {
                    let (nb, nl) = self.name_copy();
                    let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
                    match self.epub.init_zip(k, name, &mut self.pg.buf) {
                        Ok(()) => {
                            self.state = State::NeedOpf;
                            ctx.set_loading(LOADING_REGION, "Loading", 25);
                        }
                        Err(e) => {
                            log::info!("reader: epub init (zip) failed: {}", e);
                            self.enter_error(ctx, e);
                        }
                    }
                }

                State::NeedOpf => match self.epub_init_opf(k) {
                    Ok(()) => {
                        // clamp restored chapter to valid spine range
                        let spine_len = self.epub.spine.len();
                        if spine_len > 0 && self.epub.chapter as usize >= spine_len {
                            self.epub.chapter = (spine_len - 1) as u16;
                        }
                        self.state = State::NeedToc;
                        ctx.set_loading(LOADING_REGION, "Loading", 40);
                    }
                    Err(e) => {
                        log::info!("reader: epub init (opf) failed: {}", e);
                        self.enter_error(ctx, e);
                    }
                },

                State::NeedToc => {
                    if let Some(source) = self.epub.toc_source.take() {
                        let (nb, nl) = self.name_copy();
                        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
                        let toc_idx = source.zip_index();

                        let mut toc_dir_buf = [0u8; 256];
                        let toc_dir_len = {
                            let toc_path = self.epub.zip.entry_name(toc_idx);
                            let dir = toc_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                            let n = dir.len().min(toc_dir_buf.len());
                            toc_dir_buf[..n].copy_from_slice(dir.as_bytes());
                            n
                        };
                        let toc_dir =
                            core::str::from_utf8(&toc_dir_buf[..toc_dir_len]).unwrap_or("");

                        match extract_zip_entry(k, name, &self.epub.zip, toc_idx) {
                            Ok(toc_data) => {
                                epub::parse_toc(
                                    source,
                                    &toc_data,
                                    toc_dir,
                                    &self.epub.spine,
                                    &self.epub.zip,
                                    &mut self.epub.toc,
                                );
                                log::info!("epub: TOC has {} entries", self.epub.toc.len());
                            }
                            Err(_e) => {
                                log::warn!("epub: failed to read TOC");
                            }
                        }
                    }
                    self.rebuild_quick_actions();
                    self.state = State::NeedCache;
                    ctx.set_loading(LOADING_REGION, "Caching", 55);
                }

                State::NeedCache => match self.epub.check_cache(k, &mut self.pg.buf) {
                    Ok(true) => {
                        self.state = State::NeedIndex;
                        ctx.set_loading(LOADING_REGION, "Indexing", 75);
                    }
                    Ok(false) => {
                        // cache the current chapter; async version yields
                        // during deflate so the scheduler's select can
                        // interrupt if the user presses back
                        let ch = self.epub.chapter as usize;
                        let (nb, nl) = self.name_copy();
                        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
                        match self.epub.cache_chapter_async(k, ch, epub_name).await {
                            Ok(()) => {
                                self.epub.chapters_cached = true;
                                self.epub.cache_chapter = 0;

                                // eagerly dispatch nearby images to
                                // the worker so they decode while the
                                // user reads the first page
                                if self.try_dispatch_nearby_image(k) {
                                    self.epub.bg_cache = BgCacheState::WaitNearbyImage;
                                } else {
                                    self.epub.bg_cache = BgCacheState::CacheChapter;
                                }

                                self.state = State::NeedIndex;
                                ctx.set_loading(LOADING_REGION, "Indexing", 75);
                            }
                            Err(e) => {
                                log::info!("reader: cache ch{} failed: {}", ch, e);
                                self.enter_error(ctx, e);
                            }
                        }
                    }
                    Err(e) => {
                        log::info!("reader: cache check failed: {}", e);
                        self.enter_error(ctx, e);
                    }
                },

                State::NeedIndex => {
                    // ensure the target chapter is cached before
                    // indexing (it may not be if background caching
                    // hasn't reached it yet)
                    if self.is_epub
                        && self.epub.chapters_cached
                        && !self.epub.ch_cached[self.epub.chapter as usize]
                    {
                        // async version yields during deflate so the
                        // scheduler's select can interrupt on input
                        let ch = self.epub.chapter as usize;
                        let (nb, nl) = self.name_copy();
                        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
                        if let Err(e) = self.epub.cache_chapter_async(k, ch, epub_name).await {
                            self.enter_error(ctx, e);
                            break;
                        }
                    }

                    let want_last = self.goto_last_page;
                    self.goto_last_page = false;

                    self.epub_index_chapter();

                    if self.is_epub && self.epub.try_cache_chapter(k) {
                        self.preindex_all_pages();
                    }

                    if want_last {
                        match self.scan_to_last_page(k) {
                            Ok(()) => {
                                self.defer_image_decode = false;
                                self.state = State::Ready;
                                ctx.clear_loading();
                                ctx.mark_dirty(PAGE_REGION);
                            }
                            Err(e) => self.enter_error(ctx, e),
                        }
                    } else {
                        self.state = State::NeedPage;
                        ctx.set_loading(LOADING_REGION, "Loading page", 90);
                    }
                }

                State::NeedPage => {
                    if let Some(target_off) = self.restore_offset.take() {
                        self.pg.page = 0;
                        loop {
                            match self.load_and_prefetch(k) {
                                Ok(()) => {}
                                Err(e) => {
                                    self.enter_error(ctx, e);
                                    break;
                                }
                            }
                            if self.pg.page + 1 >= self.pg.total_pages {
                                break;
                            }
                            if self.pg.offsets[self.pg.page + 1] > target_off {
                                break;
                            }
                            self.pg.page += 1;
                        }
                        if self.state != State::Error {
                            self.defer_image_decode = false;
                            self.state = State::Ready;
                            ctx.clear_loading();
                            ctx.mark_dirty(PAGE_REGION);
                        }
                    } else {
                        match self.load_and_prefetch(k) {
                            Ok(()) => {
                                self.defer_image_decode = false;
                                self.state = State::Ready;
                                ctx.clear_loading();
                                ctx.mark_dirty(PAGE_REGION);
                            }
                            Err(e) => {
                                log::info!("reader: load failed: {}", e);
                                self.enter_error(ctx, e);
                            }
                        }
                    }
                }

                _ => {}
            }
            break;
        }

        // background caching; runs whenever the page content is
        // settled and there is work to do. NeedIndex is included so
        // adjacent-chapter caching can overlap with page indexing
        // after a chapter jump. the scheduler wraps run_background
        // in select(run_background, input) so every .await inside
        // bg_cache_step is interruptible by user input.
        if matches!(
            self.state,
            State::Ready | State::ShowToc | State::NeedIndex | State::NeedPage
        ) && self.epub.bg_cache != BgCacheState::Idle
        {
            // ensure caching indicator is visible (covers resume
            // and the transition from initial load to bg caching)
            if !ctx.loading_active() {
                self.set_cache_loading(ctx);
            }
            let prev_count = self.cached_chapter_count();
            let prev_bg = self.epub.bg_cache;
            self.bg_cache_step(k).await;
            if self.epub.bg_cache == BgCacheState::Idle {
                ctx.clear_loading();
            } else if self.cached_chapter_count() != prev_count || self.epub.bg_cache != prev_bg {
                self.set_cache_loading(ctx);
            }
        }
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        if self.state == State::ShowToc {
            match event {
                ActionEvent::Press(Action::Back) => {
                    self.state = State::Ready;
                    ctx.mark_dirty(PAGE_REGION);
                    return Transition::None;
                }
                ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                    let len = self.epub.toc.len();
                    if len > 0 {
                        if self.epub.toc_selected + 1 < len {
                            self.epub.toc_selected += 1;
                        } else {
                            self.epub.toc_selected = 0;
                            self.epub.toc_scroll = 0;
                        }
                        let vis = (self.text_area_h / self.font_line_h) as usize;
                        if self.epub.toc_selected >= self.epub.toc_scroll + vis {
                            self.epub.toc_scroll = self.epub.toc_selected + 1 - vis;
                        }
                        ctx.mark_dirty(PAGE_REGION);
                    }
                    return Transition::None;
                }
                ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                    let len = self.epub.toc.len();
                    if len > 0 {
                        if self.epub.toc_selected > 0 {
                            self.epub.toc_selected -= 1;
                        } else {
                            self.epub.toc_selected = len - 1;
                            let vis = (self.text_area_h / self.font_line_h) as usize;
                            if self.epub.toc_selected >= vis {
                                self.epub.toc_scroll = self.epub.toc_selected + 1 - vis;
                            }
                        }
                        if self.epub.toc_selected < self.epub.toc_scroll {
                            self.epub.toc_scroll = self.epub.toc_selected;
                        }
                        ctx.mark_dirty(PAGE_REGION);
                    }
                    return Transition::None;
                }
                ActionEvent::Press(Action::Select) | ActionEvent::Press(Action::NextJump) => {
                    let entry = &self.epub.toc.entries[self.epub.toc_selected];
                    if entry.spine_idx != 0xFFFF {
                        log::info!(
                            "toc: jumping to \"{}\" -> spine {}",
                            entry.title_str(),
                            entry.spine_idx
                        );
                        self.epub.chapter = entry.spine_idx;
                        self.pg.page = 0;
                        self.goto_last_page = false;
                        self.state = State::NeedIndex;
                        ctx.mark_dirty(PAGE_REGION);
                    } else {
                        log::warn!(
                            "toc: entry \"{}\" unresolved (spine_idx=0xFFFF), ignoring",
                            entry.title_str()
                        );
                        self.state = State::Ready;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                    return Transition::None;
                }
                _ => return Transition::None,
            }
        }

        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            ActionEvent::LongPress(Action::Next) => {
                if self.state == State::Ready {
                    self.show_position = true;
                }
                if self.page_forward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }
            ActionEvent::LongPress(Action::Prev) => {
                if self.state == State::Ready {
                    self.show_position = true;
                }
                if self.page_backward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Release(Action::Next) | ActionEvent::Release(Action::Prev) => {
                if self.show_position {
                    self.show_position = false;
                    ctx.mark_dirty(POSITION_OVERLAY);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                if self.page_forward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                if self.page_backward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) | ActionEvent::Repeat(Action::NextJump) => {
                if self.jump_forward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) | ActionEvent::Repeat(Action::PrevJump) => {
                if self.jump_backward() {
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            // LongPress(NextJump): jump to end of current chapter
            ActionEvent::LongPress(Action::NextJump) => {
                if self.state == State::Ready && self.pg.total_pages > 0 {
                    self.pg.page = self.pg.total_pages - 1;
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            // LongPress(PrevJump): jump to start of current chapter
            ActionEvent::LongPress(Action::PrevJump) => {
                if self.state == State::Ready {
                    self.pg.page = 0;
                    ctx.mark_dirty(PAGE_REGION);
                }
                Transition::None
            }

            // LongPress(Select): reserved for bookmark toggle (Phase 6)
            // ActionEvent::LongPress(Action::Select) => { ... }
            _ => Transition::None,
        }
    }

    fn quick_actions(&self) -> &[QuickAction] {
        &self.qa_buf[..self.qa_count]
    }

    fn on_quick_trigger(&mut self, id: u8, ctx: &mut AppContext) {
        match id {
            QA_PREV_CHAPTER => {
                if self.is_epub && self.epub.chapter > 0 {
                    self.epub.chapter -= 1;
                    self.goto_last_page = false;
                    self.state = State::NeedIndex;
                }
            }
            QA_NEXT_CHAPTER => {
                if self.is_epub && (self.epub.chapter as usize + 1) < self.epub.spine.len() {
                    self.epub.chapter += 1;
                    self.goto_last_page = false;
                    self.state = State::NeedIndex;
                }
            }
            QA_TOC => {
                if self.is_epub && !self.epub.toc.is_empty() {
                    log::info!("toc: opening ({} entries)", self.epub.toc.len());
                    self.epub.toc_selected = 0;
                    self.epub.toc_scroll = 0;
                    for i in 0..self.epub.toc.len() {
                        if self.epub.toc.entries[i].spine_idx == self.epub.chapter {
                            self.epub.toc_selected = i;
                            let vis = (self.text_area_h / self.font_line_h) as usize;
                            if self.epub.toc_selected >= vis {
                                self.epub.toc_scroll = self.epub.toc_selected + 1 - vis;
                            }
                            break;
                        }
                    }
                    self.state = State::ShowToc;
                    ctx.mark_dirty(PAGE_REGION);
                }
            }
            _ => {}
        }
    }

    fn on_quick_cycle_update(&mut self, id: u8, value: u8, _ctx: &mut AppContext) {
        if id == QA_FONT_SIZE {
            self.book_font_size_idx = value;
            self.apply_font_metrics();
            if self.state == State::Ready {
                if self.is_epub && self.epub.chapters_cached {
                    self.state = State::NeedIndex;
                } else {
                    self.state = State::NeedPage;
                }
            }
            self.rebuild_quick_actions();
        }
    }

    fn pending_setting(&self) -> Option<PendingSetting> {
        Some(PendingSetting::BookFontSize(self.book_font_size_idx))
    }

    fn save_state(&self, bm: &mut bookmarks::BookmarkCache) {
        self.save_position(bm);
    }

    fn has_background_when_suspended(&self) -> bool {
        self.has_bg_work()
    }

    fn background_suspended(&mut self, k: &mut KernelHandle<'_>) {
        self.bg_work_tick(k);
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let cf = self.chrome_font;

        draw_chrome_text(
            strip,
            HEADER_REGION,
            self.display_name(),
            Alignment::CenterLeft,
            cf,
        );

        if self.state == State::ShowToc {
            draw_chrome_text(strip, STATUS_REGION, "Contents", Alignment::CenterRight, cf);
        } else if self.is_epub && !self.epub.spine.is_empty() {
            let mut sbuf = StackFmt::<40>::new();
            if self.epub.spine.len() > 1 {
                if self.pg.fully_indexed {
                    let _ = write!(
                        sbuf,
                        "Ch{}/{} {}/{}",
                        self.epub.chapter + 1,
                        self.epub.spine.len(),
                        self.pg.page + 1,
                        self.pg.total_pages
                    );
                } else {
                    let _ = write!(
                        sbuf,
                        "Ch{}/{} p{}",
                        self.epub.chapter + 1,
                        self.epub.spine.len(),
                        self.pg.page + 1
                    );
                }
            } else if self.pg.fully_indexed {
                let _ = write!(sbuf, "{}/{}", self.pg.page + 1, self.pg.total_pages);
            } else {
                let _ = write!(sbuf, "p{}", self.pg.page + 1);
            }
            if self.epub.bg_cache != BgCacheState::Idle {
                let cached = self.cached_chapter_count();
                let total = self.epub.spine.len();
                if cached < total {
                    let _ = write!(sbuf, " [{}/{}]", cached, total);
                } else {
                    let _ = write!(sbuf, " [img]");
                }
            }
            draw_chrome_text(
                strip,
                STATUS_REGION,
                sbuf.as_str(),
                Alignment::CenterRight,
                cf,
            );
        } else if self.file_size > 0 {
            let mut sbuf = StackFmt::<24>::new();
            if self.pg.fully_indexed {
                let _ = write!(sbuf, "{}/{}", self.pg.page + 1, self.pg.total_pages);
            } else {
                let _ = write!(sbuf, "p{}", self.pg.page + 1);
            }
            draw_chrome_text(
                strip,
                STATUS_REGION,
                sbuf.as_str(),
                Alignment::CenterRight,
                cf,
            );
        }

        if let Some(e) = self.error {
            let mut ebuf = StackFmt::<32>::new();
            let _ = write!(ebuf, "{}", e);
            draw_chrome_text(
                strip,
                LOADING_REGION,
                ebuf.as_str(),
                Alignment::CenterLeft,
                cf,
            );
            return;
        }

        // loading states: the kernel loading indicator (drawn by
        // AppManager) handles feedback text; nothing else to draw
        if self.state != State::Ready && self.state != State::Error && self.state != State::ShowToc
        {
            return;
        }

        if self.state == State::ShowToc {
            let toc_len = self.epub.toc.len();
            let tx = self.text_margin as i32;
            let ty = self.text_y as i32;
            if self.fonts.is_some() {
                let font = fonts::body_font(self.book_font_size_idx);
                let line_h = font.line_height as i32;
                let ascent = font.ascent as i32;
                let vis_max = (self.text_area_h / font.line_height) as usize;
                let visible = vis_max.min(toc_len.saturating_sub(self.epub.toc_scroll));
                for i in 0..visible {
                    let idx = self.epub.toc_scroll + i;
                    let entry = &self.epub.toc.entries[idx];
                    let y_top = ty + i as i32 * line_h;
                    let baseline = y_top + ascent;
                    let selected = idx == self.epub.toc_selected;

                    if selected {
                        Rectangle::new(
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
                    let mut cx = tx;
                    if entry.spine_idx != 0xFFFF && entry.spine_idx == self.epub.chapter {
                        cx += font.draw_char_fg(strip, '>', fg, cx, baseline) as i32;
                        cx += font.draw_char_fg(strip, ' ', fg, cx, baseline) as i32;
                    }
                    font.draw_str_fg(strip, entry.title_str(), fg, cx, baseline);
                }
            } else {
                let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::On);
                let vis_max = (self.text_area_h / LINE_H) as usize;
                let visible = vis_max.min(toc_len.saturating_sub(self.epub.toc_scroll));
                for i in 0..visible {
                    let idx = self.epub.toc_scroll + i;
                    let entry = &self.epub.toc.entries[idx];
                    let y = ty + i as i32 * LINE_H as i32 + LINE_H as i32;
                    let marker = if idx == self.epub.toc_selected {
                        "> "
                    } else {
                        "  "
                    };
                    Text::new(marker, Point::new(0, y), style)
                        .draw(strip)
                        .unwrap();
                    Text::new(entry.title_str(), Point::new(tx, y), style)
                        .draw(strip)
                        .unwrap();
                }
            }
            return;
        }

        if let Some(ref fs) = self.fonts {
            let line_h = self.font_line_h as i32;
            let ascent = self.font_ascent as i32;

            // fullscreen image: centre in text area, skip normal line layout
            if self.fullscreen_img {
                if let Some(ref img) = self.page_img {
                    let img_x = self.text_margin as i32
                        + ((self.text_w as i32 - img.width as i32) / 2).max(0);
                    let img_y = self.text_y as i32
                        + ((self.text_area_h as i32 - img.height as i32) / 2).max(0);
                    strip.blit_1bpp(
                        &img.data,
                        0,
                        img.width as usize,
                        img.height as usize,
                        img.stride,
                        img_x,
                        img_y,
                        true,
                    );
                }
            } else {
                let mut img_rendered = false;
                for i in 0..self.pg.line_count {
                    let span = &self.pg.lines[i];

                    if span.is_image() {
                        if span.is_image_origin() && !img_rendered {
                            let y_top = self.text_y as i32 + i as i32 * line_h;
                            if let Some(ref img) = self.page_img {
                                let img_x = self.text_margin as i32
                                    + ((self.text_w as i32 - img.width as i32) / 2).max(0);

                                // count reserved image lines for vertical centering
                                let mut img_line_count = 0i32;
                                for j in i..self.pg.line_count {
                                    if self.pg.lines[j].is_image() {
                                        img_line_count += 1;
                                    } else {
                                        break;
                                    }
                                }
                                let reserved_h = img_line_count * line_h;

                                // the image is already decoded at the correct
                                // budget (inline or fullscreen); just clamp to
                                // remaining vertical space as a safety net
                                let space_below =
                                    (self.text_area_h as i32 - i as i32 * line_h).max(0);
                                let blit_h = (img.height as i32).min(space_below).max(0) as usize;

                                // center vertically within reserved lines
                                let y_offset = ((reserved_h - blit_h as i32) / 2).max(0);

                                strip.blit_1bpp(
                                    &img.data,
                                    0,
                                    img.width as usize,
                                    blit_h,
                                    img.stride,
                                    img_x,
                                    y_top + y_offset,
                                    true,
                                );
                                img_rendered = true;
                            } else {
                                let baseline = y_top + ascent;
                                fs.draw_str(
                                    strip,
                                    "[image]",
                                    fonts::Style::Italic,
                                    self.text_margin as i32,
                                    baseline,
                                );
                            }
                        }
                        continue;
                    }

                    let start = span.start as usize;
                    let end = start + span.len as usize;
                    let baseline = self.text_y as i32 + i as i32 * line_h + ascent;
                    let x_indent = INDENT_PX as i32 * span.indent as i32;

                    let line = &self.pg.buf[start..end];
                    let mut cx = self.text_margin as i32 + x_indent;
                    let mut sty = span.style();
                    let mut j = 0usize;
                    while j < line.len() {
                        let b = line[j];
                        if b == MARKER && j + 1 < line.len() {
                            sty = match line[j + 1] {
                                BOLD_ON => fonts::Style::Bold,
                                ITALIC_ON => fonts::Style::Italic,
                                HEADING_ON => fonts::Style::Heading,
                                BOLD_OFF | ITALIC_OFF | HEADING_OFF => fonts::Style::Regular,
                                _ => sty,
                            };
                            j += 2;
                            continue;
                        }
                        if b >= 0xC0 {
                            let (ch, seq_len) = decode_utf8_char(line, j);
                            cx += fs.draw_char(strip, ch, sty, cx, baseline) as i32;
                            j += seq_len;
                            continue;
                        }
                        if b >= 0x80 {
                            // continuation byte mid-stream (already consumed
                            // by a lead byte above, or stray), skip
                            j += 1;
                            continue;
                        }
                        if b < bitmap::FIRST_CHAR {
                            j += 1;
                            continue; // control char
                        }
                        cx += fs.draw_char(strip, b as char, sty, cx, baseline) as i32;
                        j += 1;
                    }
                }
            }
        } else {
            let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::On);
            for i in 0..self.pg.line_count {
                let span = self.pg.lines[i];
                let start = span.start as usize;
                let end = start + span.len as usize;
                let text = core::str::from_utf8(&self.pg.buf[start..end]).unwrap_or("");
                let y = self.text_y as i32 + i as i32 * LINE_H as i32 + LINE_H as i32;
                Text::new(text, Point::new(self.text_margin as i32, y), style)
                    .draw(strip)
                    .unwrap();
            }
        }

        if self.show_position
            && self.state == State::Ready
            && POSITION_OVERLAY.intersects(strip.logical_window())
        {
            let mut pbuf = StackFmt::<48>::new();
            if self.is_epub && self.epub.spine.len() > 1 {
                if self.pg.fully_indexed {
                    let _ = write!(
                        pbuf,
                        "Ch {}/{}  Page {}/{}",
                        self.epub.chapter + 1,
                        self.epub.spine.len(),
                        self.pg.page + 1,
                        self.pg.total_pages
                    );
                } else {
                    let _ = write!(
                        pbuf,
                        "Ch {}/{}  Page {}",
                        self.epub.chapter + 1,
                        self.epub.spine.len(),
                        self.pg.page + 1
                    );
                }
            } else if self.pg.fully_indexed {
                let _ = write!(pbuf, "Page {}/{}", self.pg.page + 1, self.pg.total_pages);
            } else {
                let _ = write!(
                    pbuf,
                    "Page {}  ({}%)",
                    self.pg.page + 1,
                    self.progress_pct()
                );
            }

            POSITION_OVERLAY
                .to_rect()
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(strip)
                .unwrap();
            let text = pbuf.as_str();
            if let Some(f) = cf {
                f.draw_aligned(
                    strip,
                    POSITION_OVERLAY,
                    text,
                    Alignment::Center,
                    BinaryColor::Off,
                );
            } else {
                let tw = text.len() as u32 * 9;
                let pos = Alignment::Center.position(POSITION_OVERLAY, Size::new(tw, 18));
                let style = MonoTextStyle::new(&FONT_9X18, BinaryColor::Off);
                Text::new(text, Point::new(pos.x, pos.y + 18), style)
                    .draw(strip)
                    .unwrap();
            }
        }
    }
}
