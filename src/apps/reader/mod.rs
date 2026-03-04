mod epub_pipeline;
mod images;
mod paging;

extern crate alloc;

use paging::decode_utf8_char;

use crate::apps::PendingSetting;
use crate::fonts::bitmap::{self, BitmapFont};

use alloc::vec::Vec;
use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
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
use crate::ui::{Alignment, BUTTON_BAR_H, CONTENT_TOP, Region, StackFmt, draw_progress_bar};
use smol_epub::DecodedImage;
use smol_epub::cache;
use smol_epub::epub::{self, EpubMeta, EpubSpine, EpubToc, TocSource};
use smol_epub::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, ITALIC_OFF, ITALIC_ON, MARKER,
};
use smol_epub::zip::{self, ZipIndex};

pub(super) const MARGIN: u16 = 8;
pub(super) const HEADER_Y: u16 = CONTENT_TOP + 2;
pub(super) const HEADER_H: u16 = 16;
pub(super) const TEXT_Y: u16 = HEADER_Y + HEADER_H + 2;
pub(super) const LINE_H: u16 = 13;
pub(super) const CHARS_PER_LINE: usize = 66;
pub(super) const LINES_PER_PAGE: usize = 58;
pub(super) const PAGE_BUF: usize = 8192;
pub(super) const MAX_PAGES: usize = 1024;

pub(super) const HEADER_REGION: Region = Region::new(MARGIN, HEADER_Y, 300, HEADER_H);
pub(super) const STATUS_REGION: Region = Region::new(308, HEADER_Y, 164, HEADER_H);

pub(super) const PAGE_REGION: Region = Region::new(0, HEADER_Y, SCREEN_W, SCREEN_H - HEADER_Y);

pub(super) const NO_PREFETCH: usize = usize::MAX;

pub(super) const TEXT_W: u32 = (SCREEN_W - 2 * MARGIN) as u32;
pub(super) const TEXT_AREA_H: u16 = SCREEN_H - TEXT_Y - BUTTON_BAR_H;
pub(super) const EOCD_TAIL: usize = 512;
pub(super) const INDENT_PX: u32 = 24;
pub(super) const IMAGE_DISPLAY_H: u16 = 200;
pub(super) const CHAPTER_CACHE_MAX: usize = 98304;

// images <= this size are dispatched to the async worker for decoding;
// images > this size are decoded on the main loop via streaming SD reads
pub(super) const PRECACHE_IMG_MAX: u32 = 30 * 1024;

pub(super) const PROGRESS_H: u16 = 2;
pub(super) const PROGRESS_Y: u16 = SCREEN_H - PROGRESS_H - 1;
pub(super) const PROGRESS_W: u16 = SCREEN_W - 2 * MARGIN;

pub(super) const POSITION_OVERLAY_W: u16 = 280;
pub(super) const POSITION_OVERLAY_H: u16 = 40;
pub(super) const POSITION_OVERLAY: Region = Region::new(
    (SCREEN_W - POSITION_OVERLAY_W) / 2,
    (SCREEN_H - POSITION_OVERLAY_H) / 2,
    POSITION_OVERLAY_W,
    POSITION_OVERLAY_H,
);

pub(super) const LOADING_REGION: Region = Region::new(MARGIN, TEXT_Y, 464, 20);
pub(super) const LOADING_BAR_REGION: Region = Region::new(MARGIN, TEXT_Y + 26, 200, 8);

pub const QA_FONT_SIZE: u8 = 1;
pub(super) const QA_PREV_CHAPTER: u8 = 3;
pub(super) const QA_NEXT_CHAPTER: u8 = 4;
pub(super) const QA_TOC: u8 = 5;

pub(super) const QA_MAX: usize = 4;

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

impl State {
    pub(super) fn loading_pct(self) -> u8 {
        match self {
            Self::NeedBookmark => 0,
            Self::NeedInit => 5,
            Self::NeedOpf => 15,
            Self::NeedToc => 30,
            Self::NeedCache => 45,
            Self::NeedIndex => 70,
            Self::NeedPage => 90,
            Self::Ready | Self::ShowToc | Self::Error => 100,
        }
    }
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

    pub(super) state: State,
    pub(super) error: Option<Error>,
    pub(super) show_position: bool,

    pub(super) is_epub: bool,
    pub(super) zip: ZipIndex,
    pub(super) meta: EpubMeta,
    pub(super) spine: EpubSpine,
    pub(super) chapter: u16,
    pub(super) goto_last_page: bool,
    pub(super) restore_offset: Option<u32>,

    pub(super) cache_dir: [u8; 8],
    pub(super) epub_name_hash: u32,
    pub(super) epub_file_size: u32,
    pub(super) chapter_sizes: [u32; cache::MAX_CACHE_CHAPTERS],
    pub(super) chapters_cached: bool,
    pub(super) cache_chapter: u16,
    pub(super) img_cache_ch: u16,
    pub(super) img_cache_offset: u32,
    pub(super) img_scan_wrapped: bool,

    pub(super) bg_cache: BgCacheState,
    pub(super) ch_cached: [bool; cache::MAX_CACHE_CHAPTERS],
    pub(super) work_gen: u16,

    pub(super) ch_cache: Vec<u8>,
    pub(super) page_img: Option<DecodedImage>,
    pub(super) fullscreen_img: bool,
    pub(super) defer_image_decode: bool,
    pub(super) toc: EpubToc,
    pub(super) toc_source: Option<TocSource>,
    pub(super) toc_selected: usize,
    pub(super) toc_scroll: usize,

    pub(super) fonts: Option<fonts::FontSet>,
    pub(super) font_line_h: u16,
    pub(super) font_ascent: u16,
    pub(super) max_lines: usize,

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

            state: State::NeedPage,
            error: None,
            show_position: false,

            is_epub: false,
            zip: ZipIndex::new(),
            meta: EpubMeta::new(),
            spine: EpubSpine::new(),
            chapter: 0,
            goto_last_page: false,
            restore_offset: None,

            cache_dir: [0u8; 8],
            epub_name_hash: 0,
            epub_file_size: 0,
            chapter_sizes: [0u32; cache::MAX_CACHE_CHAPTERS],
            chapters_cached: false,
            cache_chapter: 0,
            img_cache_ch: 0,
            img_cache_offset: 0,
            img_scan_wrapped: false,

            bg_cache: BgCacheState::Idle,
            ch_cached: [false; cache::MAX_CACHE_CHAPTERS],
            work_gen: 0,

            ch_cache: Vec::new(),

            page_img: None,
            fullscreen_img: false,
            defer_image_decode: false,

            toc: EpubToc::new(),
            toc_source: None,
            toc_selected: 0,
            toc_scroll: 0,

            fonts: None,
            font_line_h: LINE_H,
            font_ascent: LINE_H,
            max_lines: LINES_PER_PAGE,

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

    pub fn set_chrome_font(&mut self, font: &'static BitmapFont) {
        self.chrome_font = Some(font);
    }

    pub fn has_bg_work(&self) -> bool {
        self.is_epub && self.bg_cache != BgCacheState::Idle
    }

    // run one step of image work queue polling while suspended;
    // chapter caching is async and only runs during active background,
    // so this only handles the sync image recv states
    pub fn bg_work_tick(&mut self, k: &mut KernelHandle<'_>) {
        match self.bg_cache {
            BgCacheState::WaitNearbyImage => match self.epub_recv_image_result(k) {
                Ok(Some(_)) => {
                    if !self.try_dispatch_nearby_image(k) {
                        self.bg_cache = BgCacheState::CacheChapter;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("bg: nearby image error (suspended): {}", e);
                    self.bg_cache = BgCacheState::CacheChapter;
                }
            },
            BgCacheState::WaitImage => match self.epub_recv_image_result(k) {
                Ok(Some(_)) => self.bg_cache = BgCacheState::CacheImage,
                Ok(None) => {}
                Err(e) => {
                    log::warn!("bg: image recv error (suspended): {}", e);
                    self.bg_cache = BgCacheState::CacheImage;
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

        if self.is_epub && self.spine.len() > 1 {
            self.qa_buf[n] = QuickAction::trigger(QA_PREV_CHAPTER, "Prev Ch", "<<<");
            n += 1;
            self.qa_buf[n] = QuickAction::trigger(QA_NEXT_CHAPTER, "Next Ch", ">>>");
            n += 1;
        }

        if self.is_epub && !self.toc.is_empty() {
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

        if fonts::font_data::HAS_REGULAR {
            let fs = fonts::FontSet::for_size(self.book_font_size_idx);
            self.font_line_h = fs.line_height(fonts::Style::Regular).max(1);
            self.font_ascent = fs.ascent(fonts::Style::Regular);
            self.max_lines = ((TEXT_AREA_H / self.font_line_h) as usize).min(LINES_PER_PAGE);
            log::info!(
                "font: size_idx={} line_h={} ascent={} max_lines={}",
                self.book_font_size_idx,
                self.font_line_h,
                self.font_ascent,
                self.max_lines
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

    pub fn save_position(&self, bm: &mut bookmarks::BookmarkCache) {
        if self.state == State::Ready {
            bm.save(
                &self.filename[..self.filename_len],
                self.offsets[self.page],
                self.chapter,
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
            self.chapter = slot.chapter;
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
        if self.is_epub && !self.spine.is_empty() {
            let spine_len = self.spine.len() as u64;
            let ch = self.chapter as u64;

            if ch + 1 >= spine_len && self.fully_indexed && self.page + 1 >= self.total_pages {
                return 100;
            }

            let in_ch = if self.file_size == 0 {
                0u64
            } else {
                let pos = self.offsets[self.page] as u64;
                let size = self.file_size as u64;
                ((pos * 100) / size).min(100)
            };

            let overall = (ch * 100 + in_ch) / spine_len;
            return overall.min(100) as u8;
        }

        if self.file_size == 0 {
            return 100;
        }
        if self.fully_indexed && self.page + 1 >= self.total_pages {
            return 100;
        }
        let pos = self.offsets[self.page] as u64;
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
        let tw = text.len() as u32 * 6;
        let pos = align.position(region, embedded_graphics::geometry::Size::new(tw, 13));
        let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
        Text::new(text, Point::new(pos.x, pos.y + 13), style)
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
        self.work_gen = work_queue::reset();
        self.bg_cache = BgCacheState::Idle;
        self.ch_cached = [false; cache::MAX_CACHE_CHAPTERS];
        self.img_scan_wrapped = false;

        self.is_epub = epub::is_epub_filename(self.name());
        self.rebuild_quick_actions();
        self.reset_paging();
        self.ch_cache = Vec::new();
        self.file_size = 0;
        self.chapter = 0;
        self.error = None;
        self.show_position = false;
        self.defer_image_decode = true;
        self.goto_last_page = false;
        self.restore_offset = None;

        self.apply_font_metrics();

        self.state = State::NeedBookmark;

        log::info!("reader: opening {}", self.name());

        ctx.mark_dirty(PAGE_REGION);
    }

    fn on_exit(&mut self) {
        // Cancel any in-flight background cache work so the worker
        // doesn't write stale results after we switch books.
        if self.is_epub {
            work_queue::reset();
            self.bg_cache = BgCacheState::Idle;
        }

        self.line_count = 0;
        self.buf_len = 0;
        self.prefetch_page = NO_PREFETCH;
        self.prefetch_len = 0;
        self.restore_offset = None;
        self.show_position = false;
        self.ch_cache = Vec::new();
        self.page_img = None;

        if self.is_epub {
            self.toc.clear();
            self.toc_source = None;
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
        if self.work_gen != 0 {
            work_queue::set_active_generation(self.work_gen);
        }

        let font_changed = self.book_font_size_idx != self.applied_font_idx;
        self.apply_font_metrics();
        if font_changed {
            self.reset_paging();
            if self.is_epub && self.chapters_cached {
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
                        self.zip.clear();
                        self.meta = EpubMeta::new();
                        self.spine = EpubSpine::new();
                        self.chapters_cached = false;
                        self.goto_last_page = false;
                        self.state = State::NeedInit;
                    } else {
                        self.state = State::NeedPage;
                    }
                    continue;
                }

                State::NeedInit => match self.epub_init_zip(k) {
                    Ok(()) => {
                        self.state = State::NeedOpf;
                        ctx.mark_dirty(LOADING_BAR_REGION);
                    }
                    Err(e) => {
                        log::info!("reader: epub init (zip) failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedOpf => match self.epub_init_opf(k) {
                    Ok(()) => {
                        self.state = State::NeedToc;
                        ctx.mark_dirty(LOADING_BAR_REGION);
                    }
                    Err(e) => {
                        log::info!("reader: epub init (opf) failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedToc => {
                    if let Some(source) = self.toc_source.take() {
                        let (nb, nl) = self.name_copy();
                        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
                        let toc_idx = source.zip_index();

                        let mut toc_dir_buf = [0u8; 256];
                        let toc_dir_len = {
                            let toc_path = self.zip.entry_name(toc_idx);
                            let dir = toc_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                            let n = dir.len().min(toc_dir_buf.len());
                            toc_dir_buf[..n].copy_from_slice(dir.as_bytes());
                            n
                        };
                        let toc_dir =
                            core::str::from_utf8(&toc_dir_buf[..toc_dir_len]).unwrap_or("");

                        match extract_zip_entry(k, name, &self.zip, toc_idx) {
                            Ok(toc_data) => {
                                epub::parse_toc(
                                    source,
                                    &toc_data,
                                    toc_dir,
                                    &self.spine,
                                    &self.zip,
                                    &mut self.toc,
                                );
                                log::info!("epub: TOC has {} entries", self.toc.len());
                            }
                            Err(_e) => {
                                log::warn!("epub: failed to read TOC");
                            }
                        }
                    }
                    self.rebuild_quick_actions();
                    self.state = State::NeedCache;
                    // break instead of continue so the progress bar
                    // renders before potentially heavy chapter caching;
                    // for cached books this mark coalesces with Ready
                    // because both complete during the initial waveform
                    ctx.mark_dirty(LOADING_BAR_REGION);
                }

                State::NeedCache => match self.epub_check_cache(k) {
                    Ok(true) => {
                        self.state = State::NeedIndex;
                        continue;
                    }
                    Ok(false) => {
                        // cache the current chapter; async version yields
                        // during deflate so the scheduler's select can
                        // interrupt if the user presses back
                        let ch = self.chapter as usize;
                        match self.epub_cache_chapter_async(k, ch).await {
                            Ok(()) => {
                                self.chapters_cached = true;
                                self.cache_chapter = 0;

                                // eagerly dispatch nearby images to
                                // the worker so they decode while the
                                // user reads the first page
                                if self.try_dispatch_nearby_image(k) {
                                    self.bg_cache = BgCacheState::WaitNearbyImage;
                                } else {
                                    self.bg_cache = BgCacheState::CacheChapter;
                                }

                                self.state = State::NeedIndex;
                                continue;
                            }
                            Err(e) => {
                                log::info!("reader: cache ch{} failed: {}", ch, e);
                                self.error = Some(e);
                                self.state = State::Error;
                                ctx.mark_dirty(PAGE_REGION);
                            }
                        }
                    }
                    Err(e) => {
                        log::info!("reader: cache check failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedIndex => {
                    // ensure the target chapter is cached before
                    // indexing (it may not be if background caching
                    // hasn't reached it yet)
                    if self.is_epub
                        && self.chapters_cached
                        && !self.ch_cached[self.chapter as usize]
                    {
                        // async version yields during deflate so the
                        // scheduler's select can interrupt on input
                        if let Err(e) = self
                            .epub_cache_chapter_async(k, self.chapter as usize)
                            .await
                        {
                            self.error = Some(e);
                            self.state = State::Error;
                            ctx.mark_dirty(PAGE_REGION);
                            break;
                        }
                    }

                    let want_last = self.goto_last_page;
                    self.goto_last_page = false;

                    self.epub_index_chapter();

                    if self.try_cache_chapter(k) {
                        self.preindex_all_pages();
                    }

                    if want_last {
                        match self.scan_to_last_page(k) {
                            Ok(()) => {
                                self.defer_image_decode = false;
                                self.state = State::Ready;
                                ctx.mark_dirty(PAGE_REGION);
                            }
                            Err(e) => {
                                self.error = Some(e);
                                self.state = State::Error;
                                ctx.mark_dirty(PAGE_REGION);
                            }
                        }
                    } else {
                        self.state = State::NeedPage;
                        continue;
                    }
                }

                State::NeedPage => {
                    if let Some(target_off) = self.restore_offset.take() {
                        self.page = 0;
                        loop {
                            match self.load_and_prefetch(k) {
                                Ok(()) => {}
                                Err(e) => {
                                    self.error = Some(e);
                                    self.state = State::Error;
                                    ctx.mark_dirty(PAGE_REGION);
                                    break;
                                }
                            }
                            if self.page + 1 >= self.total_pages {
                                break;
                            }
                            if self.offsets[self.page + 1] > target_off {
                                break;
                            }
                            self.page += 1;
                        }
                        if self.state != State::Error {
                            self.defer_image_decode = false;
                            self.state = State::Ready;
                            ctx.mark_dirty(PAGE_REGION);
                        }
                    } else {
                        match self.load_and_prefetch(k) {
                            Ok(()) => {
                                self.defer_image_decode = false;
                                self.state = State::Ready;
                                ctx.mark_dirty(PAGE_REGION);
                            }
                            Err(e) => {
                                log::info!("reader: load failed: {}", e);
                                self.error = Some(e);
                                self.state = State::Error;
                                ctx.mark_dirty(PAGE_REGION);
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
        ) && self.bg_cache != BgCacheState::Idle
        {
            self.bg_cache_step(k).await;
        }
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        if self.state == State::ShowToc {
            match event {
                ActionEvent::Press(Action::Back) => {
                    self.state = State::Ready;
                    ctx.mark_dirty_immediate(PAGE_REGION);
                    return Transition::None;
                }
                ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                    let len = self.toc.len();
                    if len > 0 {
                        if self.toc_selected + 1 < len {
                            self.toc_selected += 1;
                        } else {
                            self.toc_selected = 0;
                            self.toc_scroll = 0;
                        }
                        let vis = (TEXT_AREA_H / self.font_line_h) as usize;
                        if self.toc_selected >= self.toc_scroll + vis {
                            self.toc_scroll = self.toc_selected + 1 - vis;
                        }
                        ctx.mark_dirty_immediate(PAGE_REGION);
                    }
                    return Transition::None;
                }
                ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                    let len = self.toc.len();
                    if len > 0 {
                        if self.toc_selected > 0 {
                            self.toc_selected -= 1;
                        } else {
                            self.toc_selected = len - 1;
                            let vis = (TEXT_AREA_H / self.font_line_h) as usize;
                            if self.toc_selected >= vis {
                                self.toc_scroll = self.toc_selected + 1 - vis;
                            }
                        }
                        if self.toc_selected < self.toc_scroll {
                            self.toc_scroll = self.toc_selected;
                        }
                        ctx.mark_dirty_immediate(PAGE_REGION);
                    }
                    return Transition::None;
                }
                ActionEvent::Press(Action::Select) | ActionEvent::Press(Action::NextJump) => {
                    let entry = &self.toc.entries[self.toc_selected];
                    if entry.spine_idx != 0xFFFF {
                        log::info!(
                            "toc: jumping to \"{}\" -> spine {}",
                            entry.title_str(),
                            entry.spine_idx
                        );
                        self.chapter = entry.spine_idx;
                        self.page = 0;
                        self.goto_last_page = false;
                        self.state = State::NeedIndex;
                        ctx.mark_dirty_immediate(PAGE_REGION);
                    } else {
                        log::warn!(
                            "toc: entry \"{}\" unresolved (spine_idx=0xFFFF), ignoring",
                            entry.title_str()
                        );
                        self.state = State::Ready;
                        ctx.mark_dirty_immediate(PAGE_REGION);
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
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }
            ActionEvent::LongPress(Action::Prev) => {
                if self.state == State::Ready {
                    self.show_position = true;
                }
                if self.page_backward() {
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Release(Action::Next) | ActionEvent::Release(Action::Prev) => {
                if self.show_position {
                    self.show_position = false;
                    ctx.mark_dirty_immediate(POSITION_OVERLAY);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                if self.page_forward() {
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                if self.page_backward() {
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) | ActionEvent::Repeat(Action::NextJump) => {
                if self.jump_forward() {
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) | ActionEvent::Repeat(Action::PrevJump) => {
                if self.jump_backward() {
                    ctx.mark_dirty_immediate(PAGE_REGION);
                }
                Transition::None
            }

            _ => Transition::None,
        }
    }

    fn quick_actions(&self) -> &[QuickAction] {
        &self.qa_buf[..self.qa_count]
    }

    fn on_quick_trigger(&mut self, id: u8, ctx: &mut AppContext) {
        match id {
            QA_PREV_CHAPTER => {
                if self.is_epub && self.chapter > 0 {
                    self.chapter -= 1;
                    self.goto_last_page = false;
                    self.state = State::NeedIndex;
                }
            }
            QA_NEXT_CHAPTER => {
                if self.is_epub && (self.chapter as usize + 1) < self.spine.len() {
                    self.chapter += 1;
                    self.goto_last_page = false;
                    self.state = State::NeedIndex;
                }
            }
            QA_TOC => {
                if self.is_epub && !self.toc.is_empty() {
                    log::info!("toc: opening ({} entries)", self.toc.len());
                    self.toc_selected = 0;
                    self.toc_scroll = 0;
                    for i in 0..self.toc.len() {
                        if self.toc.entries[i].spine_idx == self.chapter {
                            self.toc_selected = i;
                            let vis = (TEXT_AREA_H / self.font_line_h) as usize;
                            if self.toc_selected >= vis {
                                self.toc_scroll = self.toc_selected + 1 - vis;
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
                if self.is_epub && self.chapters_cached {
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
        } else if self.is_epub && !self.spine.is_empty() {
            let mut sbuf = StackFmt::<32>::new();
            if self.spine.len() > 1 {
                if self.fully_indexed {
                    let _ = write!(
                        sbuf,
                        "Ch{}/{} {}/{}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1,
                        self.total_pages
                    );
                } else {
                    let _ = write!(
                        sbuf,
                        "Ch{}/{} p{}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1
                    );
                }
            } else if self.fully_indexed {
                let _ = write!(sbuf, "{}/{}", self.page + 1, self.total_pages);
            } else {
                let _ = write!(sbuf, "p{}", self.page + 1);
            }
            if self.bg_cache != BgCacheState::Idle {
                let _ = write!(sbuf, " *");
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
            if self.fully_indexed {
                let _ = write!(sbuf, "{}/{}", self.page + 1, self.total_pages);
            } else {
                let _ = write!(sbuf, "{} | {}%", self.page + 1, self.progress_pct());
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

        if self.state != State::Ready && self.state != State::Error && self.state != State::ShowToc
        {
            let mut lbuf = StackFmt::<48>::new();
            match self.state {
                State::NeedCache => {
                    let _ = write!(lbuf, "Preparing...");
                }
                State::NeedIndex => {
                    let _ = write!(lbuf, "Indexing...");
                }
                State::NeedPage => {
                    let _ = write!(lbuf, "Loading...");
                }
                _ => {
                    let _ = write!(lbuf, "Loading...");
                }
            }
            draw_chrome_text(
                strip,
                LOADING_REGION,
                lbuf.as_str(),
                Alignment::CenterLeft,
                cf,
            );
            draw_progress_bar(strip, LOADING_BAR_REGION, self.state.loading_pct());
            return;
        }

        if self.state == State::ShowToc {
            let toc_len = self.toc.len();
            if self.fonts.is_some() {
                let font = fonts::body_font(self.book_font_size_idx);
                let line_h = font.line_height as i32;
                let ascent = font.ascent as i32;
                let vis_max = (TEXT_AREA_H / font.line_height) as usize;
                let visible = vis_max.min(toc_len.saturating_sub(self.toc_scroll));
                for i in 0..visible {
                    let idx = self.toc_scroll + i;
                    let entry = &self.toc.entries[idx];
                    let y_top = TEXT_Y as i32 + i as i32 * line_h;
                    let baseline = y_top + ascent;
                    let selected = idx == self.toc_selected;

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
                    let mut cx = MARGIN as i32;
                    if entry.spine_idx != 0xFFFF && entry.spine_idx == self.chapter {
                        cx += font.draw_char_fg(strip, '>', fg, cx, baseline) as i32;
                        cx += font.draw_char_fg(strip, ' ', fg, cx, baseline) as i32;
                    }
                    font.draw_str_fg(strip, entry.title_str(), fg, cx, baseline);
                }
            } else {
                let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
                let vis_max = (TEXT_AREA_H / LINE_H) as usize;
                let visible = vis_max.min(toc_len.saturating_sub(self.toc_scroll));
                for i in 0..visible {
                    let idx = self.toc_scroll + i;
                    let entry = &self.toc.entries[idx];
                    let y = TEXT_Y as i32 + i as i32 * LINE_H as i32 + LINE_H as i32;
                    let marker = if idx == self.toc_selected { "> " } else { "  " };
                    Text::new(marker, Point::new(0, y), style)
                        .draw(strip)
                        .unwrap();
                    Text::new(entry.title_str(), Point::new(MARGIN as i32, y), style)
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
                    let img_x = MARGIN as i32 + ((TEXT_W as i32 - img.width as i32) / 2).max(0);
                    let img_y =
                        TEXT_Y as i32 + ((TEXT_AREA_H as i32 - img.height as i32) / 2).max(0);
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
                for i in 0..self.line_count {
                    let span = self.lines[i];

                    if span.is_image() {
                        if span.is_image_origin() && !img_rendered {
                            let y_top = TEXT_Y as i32 + i as i32 * line_h;
                            if let Some(ref img) = self.page_img {
                                let img_x =
                                    MARGIN as i32 + ((TEXT_W as i32 - img.width as i32) / 2).max(0);
                                let blit_h = (img.height as usize).min(IMAGE_DISPLAY_H as usize);
                                strip.blit_1bpp(
                                    &img.data,
                                    0,
                                    img.width as usize,
                                    blit_h,
                                    img.stride,
                                    img_x,
                                    y_top,
                                    true,
                                );
                                img_rendered = true;
                            } else {
                                let baseline = y_top + ascent;
                                fs.draw_str(
                                    strip,
                                    "[image]",
                                    fonts::Style::Italic,
                                    MARGIN as i32,
                                    baseline,
                                );
                            }
                        }
                        continue;
                    }

                    let start = span.start as usize;
                    let end = start + span.len as usize;
                    let baseline = TEXT_Y as i32 + i as i32 * line_h + ascent;
                    let x_indent = INDENT_PX as i32 * span.indent as i32;

                    let line = &self.buf[start..end];
                    let mut cx = MARGIN as i32 + x_indent;
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
            let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
            for i in 0..self.line_count {
                let span = self.lines[i];
                let start = span.start as usize;
                let end = start + span.len as usize;
                let text = core::str::from_utf8(&self.buf[start..end]).unwrap_or("");
                let y = TEXT_Y as i32 + i as i32 * LINE_H as i32 + LINE_H as i32;
                Text::new(text, Point::new(MARGIN as i32, y), style)
                    .draw(strip)
                    .unwrap();
            }
        }

        if self.state == State::Ready && (self.file_size > 0 || self.is_epub) {
            let pct = self.progress_pct() as u32;
            let filled_w = (PROGRESS_W as u32 * pct / 100).min(PROGRESS_W as u32);
            if filled_w > 0 {
                Rectangle::new(
                    Point::new(MARGIN as i32, PROGRESS_Y as i32),
                    Size::new(filled_w, PROGRESS_H as u32),
                )
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(strip)
                .unwrap();
            }
        }

        if self.show_position
            && self.state == State::Ready
            && POSITION_OVERLAY.intersects(strip.logical_window())
        {
            let mut pbuf = StackFmt::<48>::new();
            if self.is_epub && self.spine.len() > 1 {
                if self.fully_indexed {
                    let _ = write!(
                        pbuf,
                        "Ch {}/{}  Page {}/{}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1,
                        self.total_pages
                    );
                } else {
                    let _ = write!(
                        pbuf,
                        "Ch {}/{}  Page {}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1
                    );
                }
            } else if self.fully_indexed {
                let _ = write!(pbuf, "Page {}/{}", self.page + 1, self.total_pages);
            } else {
                let _ = write!(pbuf, "Page {}  ({}%)", self.page + 1, self.progress_pct());
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
                let tw = text.len() as u32 * 6;
                let pos = Alignment::Center.position(POSITION_OVERLAY, Size::new(tw, 13));
                let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
                Text::new(text, Point::new(pos.x, pos.y + 13), style)
                    .draw(strip)
                    .unwrap();
            }
        }
    }
}
