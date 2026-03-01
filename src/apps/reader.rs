// Plain text and EPUB reader.
// TXT: lazy page-indexed with prefetch. EPUB: ZIP/OPF parsed once,
// chapters stream-decompressed + HTML-stripped to SD cache; then both
// formats read identically. Cache keyed on file size + name hash.

extern crate alloc;

use crate::fonts::bitmap::BitmapFont;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;

use crate::apps::bookmarks;
use crate::apps::{App, AppContext, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::ui::quick_menu::QuickAction;
use crate::ui::{Alignment, BUTTON_BAR_H, CONTENT_TOP, Region};
use smol_epub::cache;
use smol_epub::epub::{self, EpubMeta, EpubSpine, EpubToc, TocSource};
use smol_epub::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, IMG_REF, ITALIC_OFF, ITALIC_ON, MARKER, QUOTE_OFF,
    QUOTE_ON,
};
use smol_epub::png::DecodedImage;
use smol_epub::zip::{self, ZipIndex};

const MARGIN: u16 = 8;
const HEADER_Y: u16 = CONTENT_TOP + 2;
const HEADER_H: u16 = 16;
const TEXT_Y: u16 = HEADER_Y + HEADER_H + 2;
const LINE_H: u16 = 13;
const CHARS_PER_LINE: usize = 66;
const LINES_PER_PAGE: usize = 58;
const PAGE_BUF: usize = 8192;
const MAX_PAGES: usize = 1024;

const HEADER_REGION: Region = Region::new(MARGIN, HEADER_Y, 300, HEADER_H);
const STATUS_REGION: Region = Region::new(308, HEADER_Y, 164, HEADER_H);

const PAGE_REGION: Region = Region::new(0, HEADER_Y, 480, 800 - HEADER_Y);

const NO_PREFETCH: usize = usize::MAX;

const TEXT_W: u32 = (480 - 2 * MARGIN) as u32;
const TEXT_AREA_H: u16 = 800 - TEXT_Y - BUTTON_BAR_H;
const EOCD_TAIL: usize = 512;
const INDENT_PX: u32 = 24; // px per blockquote indent level

// fixed display height for inline images; scaled to fit TEXT_W x IMAGE_DISPLAY_H
const IMAGE_DISPLAY_H: u16 = 200;

// when chapter stripped text fits in this limit, load into RAM once;
// page turns become zero-SD-I/O memcpy + word-wrap (~96KB covers most chapters)
const CHAPTER_CACHE_MAX: usize = 98304;

const PROGRESS_H: u16 = 2;
const PROGRESS_Y: u16 = 800 - PROGRESS_H - 1;
const PROGRESS_W: u16 = 480 - 2 * MARGIN;

// position overlay: centered banner shown while Next/Prev is held
const POSITION_OVERLAY_W: u16 = 280;
const POSITION_OVERLAY_H: u16 = 40;
const POSITION_OVERLAY: Region = Region::new(
    (480 - POSITION_OVERLAY_W) / 2,
    (800 - POSITION_OVERLAY_H) / 2,
    POSITION_OVERLAY_W,
    POSITION_OVERLAY_H,
);

const LOADING_REGION: Region = Region::new(MARGIN, TEXT_Y, 464, 20);

const QA_FONT_SIZE: u8 = 1;
const QA_SAVE_BOOKMARK: u8 = 2;
const QA_PREV_CHAPTER: u8 = 3;
const QA_NEXT_CHAPTER: u8 = 4;
const QA_TOC: u8 = 5;

const QA_FONT_OPTIONS: &[&str] = &["Small", "Medium", "Large"];
const QA_MAX: usize = 5;

pub const RECENT_FILE: &str = "RECENT";

#[derive(Clone, Copy, PartialEq)]
enum State {
    NeedBookmark,
    NeedInit,
    NeedOpf,
    NeedToc,
    NeedCache,
    NeedCacheChapter,
    NeedIndex,
    NeedPage,
    Ready,
    ShowToc,
    Error,
}

#[derive(Clone, Copy)]
struct LineSpan {
    start: u16,
    len: u16,
    flags: u8,  // bit 0 = bold, bit 1 = italic, bit 2 = heading
    indent: u8, // blockquote indent depth (0 = none)
}

impl LineSpan {
    const EMPTY: Self = Self {
        start: 0,
        len: 0,
        flags: 0,
        indent: 0,
    };

    const FLAG_BOLD: u8 = 1 << 0;
    const FLAG_ITALIC: u8 = 1 << 1;
    const FLAG_HEADING: u8 = 1 << 2;
    // first line of an inline image block; start/len point to src path in buf.
    // continuation lines have FLAG_IMAGE set with len == 0.
    const FLAG_IMAGE: u8 = 1 << 3;

    #[inline]
    fn is_image(&self) -> bool {
        self.flags & Self::FLAG_IMAGE != 0
    }

    // true for the first line of an image block (carries the path)
    #[inline]
    fn is_image_origin(&self) -> bool {
        self.is_image() && self.len > 0
    }

    fn style(&self) -> fonts::Style {
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

    fn pack_flags(bold: bool, italic: bool, heading: bool) -> u8 {
        (bold as u8) | ((italic as u8) << 1) | ((heading as u8) << 2)
    }
}

impl Default for ReaderApp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ReaderApp {
    filename: [u8; 32],
    filename_len: usize,
    title: [u8; 96],
    title_len: usize,
    file_size: u32,

    offsets: [u32; MAX_PAGES],
    total_pages: usize,
    fully_indexed: bool,

    page: usize,
    buf: [u8; PAGE_BUF],
    buf_len: usize,
    lines: [LineSpan; LINES_PER_PAGE],
    line_count: usize,

    prefetch: [u8; PAGE_BUF],
    prefetch_len: usize,
    prefetch_page: usize,

    state: State,
    error: Option<&'static str>,
    show_position: bool,

    // EPUB state
    is_epub: bool,
    zip: ZipIndex,
    meta: EpubMeta,
    spine: EpubSpine,
    chapter: u16,
    goto_last_page: bool,
    restore_offset: Option<u32>,

    // EPUB chapter cache (SD-backed)
    cache_dir: [u8; 8],
    epub_name_hash: u32,
    epub_file_size: u32,
    chapter_sizes: [u32; cache::MAX_CACHE_CHAPTERS],
    chapters_cached: bool,
    cache_chapter: u16,

    // RAM chapter cache: entire chapter held in heap; page turns are
    // zero-SD-I/O memcpy + word-wrap. cleared on chapter change/exit.
    ch_cache: Vec<u8>,

    // decoded image for current page; cleared on page turn/chapter change
    page_img: Option<DecodedImage>,

    // table of contents
    toc: EpubToc,
    toc_source: Option<TocSource>,
    toc_selected: usize,
    toc_scroll: usize,

    // fonts (None = FONT_6X13 fallback)
    fonts: Option<fonts::FontSet>,
    font_line_h: u16,
    font_ascent: u16,
    max_lines: usize,

    // persisted font preference; set by main before on_enter
    book_font_size_idx: u8,
    applied_font_idx: u8,

    // chrome font for header/status/loading text
    chrome_font: Option<&'static BitmapFont>,

    // quick-action buffer (rebuilt on state changes)
    qa_buf: [QuickAction; QA_MAX],
    qa_count: usize,
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

            ch_cache: Vec::new(),

            page_img: None,

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

    // 0 = Small, 1 = Medium, 2 = Large
    pub fn set_book_font_size(&mut self, idx: u8) {
        self.book_font_size_idx = idx;
        self.apply_font_metrics();
        self.rebuild_quick_actions();
    }

    // set chrome font; called from main on UI font size change
    pub fn set_chrome_font(&mut self, font: &'static BitmapFont) {
        self.chrome_font = Some(font);
    }

    fn rebuild_quick_actions(&mut self) {
        let mut n = 0usize;

        self.qa_buf[n] = QuickAction::cycle(
            QA_FONT_SIZE,
            "Book Font",
            self.book_font_size_idx,
            QA_FONT_OPTIONS,
        );
        n += 1;

        self.qa_buf[n] = QuickAction::trigger(QA_SAVE_BOOKMARK, "Bookmark", "Save pos");
        n += 1;

        // chapter nav only for multi-chapter EPUBs
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

    // reinit font metrics from book_font_size_idx
    fn apply_font_metrics(&mut self) {
        self.fonts = None;
        self.font_line_h = LINE_H;
        self.font_ascent = LINE_H;
        self.max_lines = LINES_PER_PAGE;

        if fonts::font_data::HAS_REGULAR {
            let fs = fonts::FontSet::for_size(self.book_font_size_idx);
            self.font_line_h = fs.line_height(fonts::Style::Regular);
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

            // last page of last chapter = 100%
            if ch + 1 >= spine_len && self.fully_indexed && self.page + 1 >= self.total_pages {
                return 100;
            }

            // within-chapter progress (0-100)
            let in_ch = if self.file_size == 0 {
                0u64
            } else {
                let pos = self.offsets[self.page] as u64;
                let size = self.file_size as u64;
                ((pos * 100) / size).min(100)
            };

            // overall: (chapter * 100 + in_chapter_pct) / spine_len
            let overall = (ch * 100 + in_ch) / spine_len;
            return overall.min(100) as u8;
        }

        // TXT fallback
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

    fn wrap_lines_counted(&mut self, n: usize) -> usize {
        let fonts_copy = self.fonts;

        if let Some(fs) = fonts_copy {
            let (c, count) =
                wrap_proportional(&self.buf, n, &fs, &mut self.lines, self.max_lines, TEXT_W);
            self.line_count = count;
            c
        } else {
            self.wrap_monospace(n)
        }
    }

    fn wrap_monospace(&mut self, n: usize) -> usize {
        let max = self.max_lines;
        self.line_count = 0;
        let mut col: usize = 0;
        let mut line_start: usize = 0;

        for i in 0..n {
            let b = self.buf[i];
            match b {
                b'\r' => {}
                b'\n' => {
                    let end = trim_trailing_cr(&self.buf, line_start, i);
                    self.push_line(line_start, end);
                    line_start = i + 1;
                    col = 0;
                    if self.line_count >= max {
                        return line_start;
                    }
                }
                _ => {
                    col += 1;
                    if col >= CHARS_PER_LINE {
                        self.push_line(line_start, i + 1);
                        line_start = i + 1;
                        col = 0;
                        if self.line_count >= max {
                            return line_start;
                        }
                    }
                }
            }
        }

        if line_start < n && self.line_count < max {
            let end = trim_trailing_cr(&self.buf, line_start, n);
            self.push_line(line_start, end);
        }

        n
    }

    fn push_line(&mut self, start: usize, end: usize) {
        if self.line_count < LINES_PER_PAGE {
            self.lines[self.line_count] = LineSpan {
                start: start as u16,
                len: (end - start) as u16,
                flags: 0,
                indent: 0,
            };
            self.line_count += 1;
        }
    }

    fn reset_paging(&mut self) {
        self.page = 0;
        self.offsets[0] = 0;
        self.total_pages = 1;
        self.fully_indexed = false;
        self.buf_len = 0;
        self.line_count = 0;
        self.prefetch_page = NO_PREFETCH;
        self.prefetch_len = 0;
        self.page_img = None;
    }

    fn load_and_prefetch<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        // fast path: chapter in RAM; memcpy + wrap, zero SD I/O
        if !self.ch_cache.is_empty() {
            let start = (self.offsets[self.page] as usize).min(self.ch_cache.len());
            let end = (start + PAGE_BUF).min(self.ch_cache.len());
            let n = end - start;
            if n > 0 {
                self.buf[..n].copy_from_slice(&self.ch_cache[start..end]);
            }
            self.buf_len = n;
            self.prefetch_page = NO_PREFETCH;
            self.prefetch_len = 0;
            // offsets already known from preindex_all_pages
            self.wrap_lines_counted(n);
            self.decode_page_images(svc);
            return Ok(());
        }

        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        if self.prefetch_page == self.page {
            // prefetch hit
            core::mem::swap(&mut self.buf, &mut self.prefetch);
            self.buf_len = self.prefetch_len;
            self.prefetch_page = NO_PREFETCH;
            self.prefetch_len = 0;
        } else if self.is_epub && self.chapters_cached {
            let dir_buf = self.cache_dir;
            let dir = cache::dir_name_str(&dir_buf);
            let ch_file = cache::chapter_file_name(self.chapter);
            let ch_str = cache::chapter_file_str(&ch_file);
            let n = svc.read_pulp_sub_chunk(dir, ch_str, self.offsets[self.page], &mut self.buf)?;
            self.buf_len = n;
        } else if self.file_size == 0 {
            let (size, n) = svc.read_file_start(name, &mut self.buf)?;
            self.file_size = size;
            self.buf_len = n;
            log::info!("reader: opened {} ({} bytes)", name, size);

            if size == 0 {
                self.fully_indexed = true;
                self.line_count = 0;
                return Ok(());
            }
        } else {
            let n = svc.read_file_chunk(name, self.offsets[self.page], &mut self.buf)?;
            self.buf_len = n;
        }

        let consumed = self.wrap_lines_counted(self.buf_len);
        let next_offset = self.offsets[self.page] + consumed as u32;

        if self.page + 1 >= self.total_pages && !self.fully_indexed {
            if self.line_count >= self.max_lines && next_offset < self.file_size {
                if self.total_pages < MAX_PAGES {
                    self.offsets[self.total_pages] = next_offset;
                    self.total_pages += 1;
                } else {
                    self.fully_indexed = true;
                }
            } else {
                self.fully_indexed = true;
            }
        }

        if self.page + 1 < self.total_pages {
            let pf_offset = self.offsets[self.page + 1];
            let pf_result = if self.is_epub && self.chapters_cached {
                let dir_buf = self.cache_dir;
                let dir = cache::dir_name_str(&dir_buf);
                let ch_file = cache::chapter_file_name(self.chapter);
                let ch_str = cache::chapter_file_str(&ch_file);
                svc.read_pulp_sub_chunk(dir, ch_str, pf_offset, &mut self.prefetch)
            } else {
                svc.read_file_chunk(name, pf_offset, &mut self.prefetch)
            };
            match pf_result {
                Ok(n) => {
                    self.prefetch_len = n;
                    self.prefetch_page = self.page + 1;
                }
                Err(_) => {
                    self.prefetch_page = NO_PREFETCH;
                    self.prefetch_len = 0;
                }
            }
        } else {
            self.prefetch_page = NO_PREFETCH;
            self.prefetch_len = 0;
        }

        self.decode_page_images(svc);
        Ok(())
    }

    // scan current page for image-origin lines, decode first image into page_img
    fn decode_page_images<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) {
        self.page_img = None;

        if !self.is_epub || self.spine.is_empty() {
            return;
        }

        // find first image-origin line; copy src path to local buf to
        // avoid borrowing self.buf across &mut self calls below
        let mut src_buf = [0u8; 128];
        let mut src_len = 0usize;
        for i in 0..self.line_count {
            if self.lines[i].is_image_origin() {
                let start = self.lines[i].start as usize;
                let len = self.lines[i].len as usize;
                if start + len <= self.buf_len {
                    let n = len.min(src_buf.len());
                    src_buf[..n].copy_from_slice(&self.buf[start..start + n]);
                    src_len = n;
                }
                break;
            }
        }

        if src_len == 0 {
            return;
        }

        let src_str = match core::str::from_utf8(&src_buf[..src_len]) {
            Ok(s) => s,
            Err(_) => return,
        };

        log::info!("reader: decoding image: {}", src_str);

        // resolve src path against chapter's directory
        let ch_zip_idx = self.spine.items[self.chapter as usize] as usize;
        let ch_path = self.zip.entry_name(ch_zip_idx);
        let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

        let mut path_buf = [0u8; 512];
        let path_len = epub::resolve_path(ch_dir, src_str, &mut path_buf);
        let full_path = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return,
        };

        // try SD image cache first
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let img_name = img_cache_name(cache::fnv1a(full_path.as_bytes()));
        let img_file = img_cache_str(&img_name);

        if let Ok(img) = load_cached_image(svc, dir, img_file) {
            log::info!(
                "reader: image cache hit {} ({}x{})",
                img_file,
                img.width,
                img.height
            );
            self.page_img = Some(img);
            return;
        }

        // cache miss; decode from ZIP
        let zip_idx = match self
            .zip
            .find(full_path)
            .or_else(|| self.zip.find_icase(full_path))
        {
            Some(idx) => idx,
            None => {
                log::warn!("reader: image not in ZIP: {}", full_path);
                return;
            }
        };

        let entry = *self.zip.entry(zip_idx);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        // absolute byte offset of the entry's raw data
        let data_offset = {
            let mut hdr = [0u8; 30];
            if svc
                .read_file_chunk(epub_name, entry.local_offset, &mut hdr)
                .is_err()
            {
                log::warn!("reader: failed to read ZIP local header");
                return;
            }
            match zip::ZipIndex::local_header_data_skip(&hdr) {
                Ok(skip) => entry.local_offset + skip,
                Err(e) => {
                    log::warn!("reader: {}", e);
                    return;
                }
            }
        };

        // detect format from extension; fall back to magic bytes for STORED entries
        let ext_jpeg = full_path.ends_with(".jpg")
            || full_path.ends_with(".jpeg")
            || full_path.ends_with(".JPG")
            || full_path.ends_with(".JPEG");
        let ext_png = full_path.ends_with(".png") || full_path.ends_with(".PNG");

        let (is_jpeg, is_png) = if ext_jpeg || ext_png {
            (ext_jpeg, ext_png)
        } else if entry.method == zip::METHOD_STORED {
            let mut magic = [0u8; 8];
            let n = svc
                .read_file_chunk(epub_name, data_offset, &mut magic)
                .unwrap_or(0);
            (
                n >= 2 && magic[0] == 0xFF && magic[1] == 0xD8,
                n >= 8 && magic[..8] == [137, 80, 78, 71, 13, 10, 26, 10],
            )
        } else {
            (false, false)
        };

        if !is_jpeg && !is_png {
            log::warn!("reader: unsupported image format: {}", full_path);
            return;
        }

        // free chapter RAM cache to maximise heap for image decode
        if !self.ch_cache.is_empty() {
            log::info!(
                "reader: releasing {} KB chapter cache for image decode",
                self.ch_cache.len() / 1024
            );
            self.ch_cache = Vec::new();
        }

        let result = if is_jpeg && entry.method == zip::METHOD_STORED {
            // stored JPEG: stream directly from SD
            let svc_ref = &*svc;
            smol_epub::jpeg::decode_jpeg_sd(
                |off, buf| svc_ref.read_file_chunk(epub_name, off, buf),
                data_offset,
                entry.uncomp_size,
                TEXT_W as u16,
                IMAGE_DISPLAY_H,
            )
        } else if is_jpeg {
            // deflate JPEG: stream-decompress + decode
            let svc_ref = &*svc;
            smol_epub::jpeg::decode_jpeg_deflate_sd(
                |off, buf| svc_ref.read_file_chunk(epub_name, off, buf),
                data_offset,
                entry.comp_size,
                entry.uncomp_size,
                TEXT_W as u16,
                IMAGE_DISPLAY_H,
            )
        } else if entry.method == zip::METHOD_STORED {
            // stored PNG: stream directly from SD
            let svc_ref = &*svc;
            smol_epub::png::decode_png_sd(
                |off, buf| svc_ref.read_file_chunk(epub_name, off, buf),
                data_offset,
                entry.uncomp_size,
                TEXT_W as u16,
                IMAGE_DISPLAY_H,
            )
        } else {
            // deflate PNG: stream-decompress + decode
            let svc_ref = &*svc;
            smol_epub::png::decode_png_deflate_sd(
                |off, buf| svc_ref.read_file_chunk(epub_name, off, buf),
                data_offset,
                entry.comp_size,
                TEXT_W as u16,
                IMAGE_DISPLAY_H,
            )
        };

        match result {
            Ok(img) => {
                log::info!(
                    "reader: decoded {}x{} image ({} bytes 1-bit)",
                    img.width,
                    img.height,
                    img.data.len()
                );
                if let Err(e) = save_cached_image(svc, dir, img_file, &img) {
                    log::warn!("reader: image cache write failed: {}", e);
                } else {
                    log::info!("reader: cached image as {}", img_file);
                }
                self.page_img = Some(img);
            }
            Err(e) => {
                log::warn!("reader: image decode failed: {}", e);
            }
        }
    }

    // parse ZIP EOCD + central directory; heap freed on return
    fn epub_init_zip<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let epub_size = svc.file_size(name)?;
        if epub_size < 22 {
            return Err("epub: file too small");
        }
        self.epub_file_size = epub_size;
        self.epub_name_hash = cache::fnv1a(name.as_bytes());
        self.cache_dir = cache::dir_name_for_hash(self.epub_name_hash);

        let tail_size = (epub_size as usize).min(EOCD_TAIL);
        let tail_offset = epub_size - tail_size as u32;
        let n = svc.read_file_chunk(name, tail_offset, &mut self.buf[..tail_size])?;
        let (cd_offset, cd_size) = ZipIndex::parse_eocd(&self.buf[..n], epub_size)?;

        log::info!(
            "epub: CD at offset {} size {} ({} file bytes)",
            cd_offset,
            cd_size,
            epub_size
        );

        let mut cd_buf = vec![0u8; cd_size as usize];
        read_full(svc, name, cd_offset, &mut cd_buf)?;
        self.zip.clear();
        self.zip.parse_central_directory(&cd_buf)?;
        drop(cd_buf);

        log::info!("epub: {} entries in ZIP", self.zip.count());

        Ok(())
    }

    // container.xml -> OPF -> spine + metadata; heap freed between steps
    fn epub_init_opf<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let container_idx = self
            .zip
            .find("META-INF/container.xml")
            .ok_or("epub: no container.xml")?;
        let container_data = extract_zip_entry(svc, name, &self.zip, container_idx)?;

        let mut opf_path_buf = [0u8; epub::OPF_PATH_CAP];
        let opf_path_len = epub::parse_container(&container_data, &mut opf_path_buf)?;
        drop(container_data);

        let opf_path = core::str::from_utf8(&opf_path_buf[..opf_path_len])
            .map_err(|_| "epub: bad opf path")?;

        log::info!("epub: OPF at {}", opf_path);

        let opf_idx = self
            .zip
            .find(opf_path)
            .or_else(|| self.zip.find_icase(opf_path))
            .ok_or("epub: opf not found in zip")?;
        let opf_data = extract_zip_entry(svc, name, &self.zip, opf_idx)?;

        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        epub::parse_opf(
            &opf_data,
            opf_dir,
            &self.zip,
            &mut self.meta,
            &mut self.spine,
        )?;

        // defer TOC to NeedToc to avoid stack overflow while OPF is live
        self.toc_source = epub::find_toc_source(&opf_data, opf_dir, &self.zip);
        drop(opf_data);

        log::info!(
            "epub: \"{}\" by {} — {} chapters",
            self.meta.title_str(),
            self.meta.author_str(),
            self.spine.len()
        );

        let tlen = self.meta.title_len as usize;
        if tlen > 0 {
            let n = tlen.min(self.title.len());
            self.title[..n].copy_from_slice(&self.meta.title[..n]);
            self.title_len = n;

            // persist title mapping for Files view
            if let Err(e) = svc.save_title(name, self.meta.title_str()) {
                log::warn!("epub: failed to save title mapping: {}", e);
            }
        }

        self.toc.clear();

        Ok(())
    }

    // Ok(true) = cache hit; Ok(false) = miss (subdir created, cache_chapter=0)
    fn epub_check_cache<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<bool, &'static str> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);

        // read META.BIN into self.buf (already owned) to avoid
        // ~2KB of stack temporaries that overflowed under esp-rtos
        let meta_cap = cache::META_MAX_SIZE.min(self.buf.len());
        if let Ok(n) = svc.read_pulp_sub_chunk(dir, cache::META_FILE, 0, &mut self.buf[..meta_cap])
            && let Ok(count) = cache::parse_cache_meta(
                &self.buf[..n],
                self.epub_file_size,
                self.epub_name_hash,
                self.spine.len(),
                &mut self.chapter_sizes,
            )
        {
            self.chapters_cached = true;
            log::info!("epub: cache hit ({} chapters)", count);
            return Ok(true);
        }

        log::info!("epub: building cache for {} chapters", self.spine.len());
        svc.ensure_pulp_subdir(dir)?;
        self.cache_chapter = 0;
        Ok(false)
    }

    // decompress + strip one chapter to SD; ~47KB heap freed on return.
    // Ok(true) = more remain; Ok(false) = all done (META.BIN written).
    fn epub_cache_one_chapter<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<bool, &'static str> {
        let ch = self.cache_chapter as usize;
        let spine_len = self.spine.len();

        if ch >= spine_len {
            return self.epub_finish_cache(svc);
        }

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);

        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let entry_idx = self.spine.items[ch] as usize;
        let entry = *self.zip.entry(entry_idx);

        let ch_file = cache::chapter_file_name(ch as u16);
        let ch_str = cache::chapter_file_str(&ch_file);

        svc.write_pulp_sub(dir, ch_str, &[])?; // truncate stale data
        let svc_ref = &*svc;
        let text_size = cache::stream_strip_entry(
            &entry,
            entry.local_offset,
            |offset, buf| svc_ref.read_file_chunk(epub_name, offset, buf),
            |chunk| svc_ref.append_pulp_sub(dir, ch_str, chunk),
        )?;

        self.chapter_sizes[ch] = text_size;
        log::info!("epub: cached ch{}/{} = {} bytes", ch, spine_len, text_size);

        self.cache_chapter += 1;

        if (self.cache_chapter as usize) < spine_len {
            Ok(true)
        } else {
            self.epub_finish_cache(svc)
        }
    }

    fn epub_finish_cache<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<bool, &'static str> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let spine_len = self.spine.len();

        let mut meta_buf = [0u8; cache::META_MAX_SIZE];
        let meta_len = cache::encode_cache_meta(
            self.epub_file_size,
            self.epub_name_hash,
            &self.chapter_sizes[..spine_len],
            &mut meta_buf,
        );
        svc.write_pulp_sub(dir, cache::META_FILE, &meta_buf[..meta_len])?;

        self.chapters_cached = true;
        log::info!("epub: cache complete");
        Ok(false)
    }

    fn epub_index_chapter(&mut self) {
        self.reset_paging();
        let ch = self.chapter as usize;
        self.file_size = if ch < cache::MAX_CACHE_CHAPTERS {
            self.chapter_sizes[ch]
        } else {
            0
        };
        log::info!(
            "epub: index chapter {}/{} ({} bytes cached text)",
            self.chapter + 1,
            self.spine.len(),
            self.file_size,
        );
    }

    // load current chapter into ch_cache; returns true on success, false = fall back to paged SD
    fn try_cache_chapter<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> bool {
        if !self.is_epub || !self.chapters_cached {
            return false;
        }

        let ch = self.chapter as usize;
        let ch_size = if ch < cache::MAX_CACHE_CHAPTERS {
            self.chapter_sizes[ch] as usize
        } else {
            return false;
        };

        if ch_size == 0 || ch_size > CHAPTER_CACHE_MAX {
            self.ch_cache.clear();
            return false;
        }

        // reuse existing buffer if it already holds this chapter's data
        // (e.g. font-size change -> NeedIndex for the same chapter)
        if self.ch_cache.len() == ch_size {
            log::info!("chapter cache: reusing {} bytes in RAM", ch_size);
            return true;
        }

        // reserve exact capacity; bail on OOM
        self.ch_cache.clear();
        if self.ch_cache.try_reserve_exact(ch_size).is_err() {
            log::info!("chapter cache: OOM for {} bytes", ch_size);
            return false;
        }
        self.ch_cache.resize(ch_size, 0);

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let ch_file = cache::chapter_file_name(self.chapter);
        let ch_str = cache::chapter_file_str(&ch_file);

        let mut pos = 0usize;
        while pos < ch_size {
            let chunk = (ch_size - pos).min(PAGE_BUF);
            match svc.read_pulp_sub_chunk(
                dir,
                ch_str,
                pos as u32,
                &mut self.ch_cache[pos..pos + chunk],
            ) {
                Ok(n) if n > 0 => pos += n,
                Ok(_) => break,
                Err(e) => {
                    log::info!("chapter cache: SD read failed at {}: {}", pos, e);
                    self.ch_cache.clear();
                    return false;
                }
            }
        }

        log::info!(
            "chapter cache: loaded ch{} ({} bytes) into RAM",
            self.chapter,
            ch_size,
        );
        true
    }

    // compute all page offsets from cached chapter text; CPU-only, no SD I/O
    fn preindex_all_pages(&mut self) {
        if self.ch_cache.is_empty() {
            return;
        }

        let total = self.ch_cache.len();
        self.offsets[0] = 0;
        self.total_pages = 1;

        let mut offset = 0usize;
        while offset < total && self.total_pages < MAX_PAGES {
            let end = (offset + PAGE_BUF).min(total);
            let n = end - offset;
            self.buf[..n].copy_from_slice(&self.ch_cache[offset..end]);
            self.buf_len = n;

            let consumed = self.wrap_lines_counted(n);
            let next_offset = offset + consumed;

            if self.line_count >= self.max_lines && next_offset < total {
                self.offsets[self.total_pages] = next_offset as u32;
                self.total_pages += 1;
                offset = next_offset;
            } else {
                break;
            }
        }

        self.fully_indexed = true;
        log::info!("chapter pre-indexed: {} pages", self.total_pages);
    }

    fn scan_to_last_page<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        while !self.fully_indexed && self.total_pages < MAX_PAGES {
            self.page = self.total_pages - 1;
            self.load_and_prefetch(svc)?;
            if self.page + 1 < self.total_pages {
                self.page += 1;
            } else {
                break;
            }
        }
        if self.total_pages > 0 {
            self.page = self.total_pages - 1;
        }
        self.prefetch_page = NO_PREFETCH;
        self.load_and_prefetch(svc)
    }

    fn page_forward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }

        if self.page + 1 < self.total_pages {
            self.page += 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub && self.fully_indexed {
            if (self.chapter as usize + 1) < self.spine.len() {
                self.chapter += 1;
                self.goto_last_page = false;
                self.state = State::NeedIndex;
                return true;
            }
        }

        false
    }

    fn page_backward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }

        if self.page > 0 {
            self.page -= 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub && self.chapter > 0 {
            self.chapter -= 1;
            self.goto_last_page = true;
            self.state = State::NeedIndex;
            return true;
        }

        false
    }

    // next chapter (EPUB) or +10 pages (TXT)
    fn jump_forward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }
        if self.is_epub {
            if (self.chapter as usize + 1) < self.spine.len() {
                self.chapter += 1;
                self.goto_last_page = false;
                self.state = State::NeedIndex;
                return true;
            }
        } else {
            let last = if self.total_pages > 0 {
                self.total_pages - 1
            } else {
                0
            };
            let target = (self.page + 10).min(last);
            if target != self.page {
                self.page = target;
                self.state = State::NeedPage;
                return true;
            }
        }
        false
    }

    // prev chapter (EPUB) or -10 pages (TXT)
    fn jump_backward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }
        if self.is_epub {
            if self.chapter > 0 {
                self.chapter -= 1;
                self.goto_last_page = false;
                self.state = State::NeedIndex;
                return true;
            }
        } else {
            let target = self.page.saturating_sub(10);
            if target != self.page {
                self.page = target;
                self.state = State::NeedPage;
                return true;
            }
        }
        false
    }
}

// helpers

fn trim_trailing_cr(buf: &[u8], start: usize, end: usize) -> usize {
    if end > start && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

fn wrap_proportional(
    buf: &[u8],
    n: usize,
    fonts: &fonts::FontSet,
    lines: &mut [LineSpan],
    max_lines: usize,
    max_width_px: u32,
) -> (usize, usize) {
    let max_l = max_lines.min(lines.len());
    let base_max_w = max_width_px;
    let mut lc: usize = 0;
    let mut ls: usize = 0;
    let mut px: u32 = 0;
    let mut sp: usize = 0;
    let mut sp_px: u32 = 0;

    // style state; carried across lines, updated by markers
    let mut bold = false;
    let mut italic = false;
    let mut heading = false;
    let mut indent: u8 = 0;
    let mut max_w = base_max_w;

    #[inline]
    fn current_style(bold: bool, italic: bool, heading: bool) -> fonts::Style {
        if heading {
            fonts::Style::Heading
        } else if bold {
            fonts::Style::Bold
        } else if italic {
            fonts::Style::Italic
        } else {
            fonts::Style::Regular
        }
    }

    macro_rules! emit {
        ($start:expr, $end:expr) => {
            if lc < max_l {
                let e = trim_trailing_cr(buf, $start, $end);
                lines[lc] = LineSpan {
                    start: ($start) as u16,
                    len: (e - ($start)) as u16,
                    flags: LineSpan::pack_flags(bold, italic, heading),
                    indent,
                };
                lc += 1;
            }
        };
    }

    let mut i = 0;
    while i < n {
        let b = buf[i];

        // 2-byte style markers: [MARKER, tag]; zero width, update state
        if b == MARKER && i + 1 < n {
            // image reference: [MARKER, IMG_REF, len, path...]
            if buf[i + 1] == IMG_REF && i + 2 < n {
                let path_len = buf[i + 2] as usize;
                let path_start = i + 3;
                if path_start + path_len <= n && path_len > 0 {
                    // flush text accumulated on current line
                    if ls < i {
                        emit!(ls, i);
                        if lc >= max_l {
                            return (i, lc);
                        }
                    }

                    // how many text-line slots does the image occupy?
                    let line_h = fonts.line_height(fonts::Style::Regular);
                    let img_lines = (IMAGE_DISPLAY_H / line_h).max(1) as usize;

                    // origin line; carries the src-path location
                    if lc < max_l {
                        lines[lc] = LineSpan {
                            start: path_start as u16,
                            len: path_len as u16,
                            flags: LineSpan::FLAG_IMAGE,
                            indent: 0,
                        };
                        lc += 1;
                    }

                    // continuation lines (empty; reserve vertical space)
                    for _ in 1..img_lines {
                        if lc >= max_l {
                            break;
                        }
                        lines[lc] = LineSpan {
                            start: 0,
                            len: 0,
                            flags: LineSpan::FLAG_IMAGE,
                            indent: 0,
                        };
                        lc += 1;
                    }

                    i = path_start + path_len;
                    ls = i;
                    px = 0;
                    sp = ls;
                    sp_px = 0;
                    if lc >= max_l {
                        return (ls, lc);
                    }
                    continue;
                }
            }

            match buf[i + 1] {
                BOLD_ON => bold = true,
                BOLD_OFF => bold = false,
                ITALIC_ON => italic = true,
                ITALIC_OFF => italic = false,
                HEADING_ON => heading = true,
                HEADING_OFF => heading = false,
                QUOTE_ON => {
                    indent = indent.saturating_add(1);
                    max_w = base_max_w.saturating_sub(INDENT_PX * indent as u32);
                }
                QUOTE_OFF => {
                    indent = indent.saturating_sub(1);
                    max_w = base_max_w.saturating_sub(INDENT_PX * indent as u32);
                }
                _ => {}
            }
            i += 2;
            continue;
        }

        if b == b'\r' {
            i += 1;
            continue;
        }

        if b == b'\n' {
            emit!(ls, i);
            ls = i + 1;
            px = 0;
            sp = ls;
            sp_px = 0;
            if lc >= max_l {
                return (ls, lc);
            }
            i += 1;
            continue;
        }

        let sty = current_style(bold, italic, heading);
        let adv = fonts.advance_byte(b, sty) as u32;

        if b == b' ' {
            px += adv;
            sp = i + 1;
            sp_px = px;
            if px > max_w {
                emit!(ls, i);
                ls = i + 1;
                px = 0;
                sp = ls;
                sp_px = 0;
                if lc >= max_l {
                    return (ls, lc);
                }
            }
            i += 1;
            continue;
        }

        px += adv;
        if px > max_w {
            if sp > ls {
                // word-wrap at last space
                emit!(ls, sp);
                px -= sp_px;
                ls = sp;
            } else {
                // no space; character-wrap
                emit!(ls, i);
                ls = i;
                px = adv;
            }
            sp = ls;
            sp_px = 0;
            if lc >= max_l {
                return (ls, lc);
            }
        }

        i += 1;
    }

    if ls < n && lc < max_l {
        let e = trim_trailing_cr(buf, ls, n);
        if e > ls {
            lines[lc] = LineSpan {
                start: ls as u16,
                len: (e - ls) as u16,
                flags: LineSpan::pack_flags(bold, italic, heading),
                indent,
            };
            lc += 1;
        }
    }

    (n, lc)
}

fn read_full<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> Result<(), &'static str> {
    let mut total = 0usize;
    while total < buf.len() {
        let n = svc.read_file_chunk(name, offset + total as u32, &mut buf[total..])?;
        if n == 0 {
            return Err("epub: unexpected EOF");
        }
        total += n;
    }
    Ok(())
}

// tiny stack buffer for formatted text
struct FmtBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> FmtBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0u8; N],
            len: 0,
        }
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}

// draw text with bitmap font (FONT_6X13 fallback), clearing region background
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
        let tw = f.measure_str(text) as u32;
        let th = f.line_height as u32;
        let pos = align.position(region, Size::new(tw, th));
        let baseline = pos.y + f.ascent as i32;
        f.draw_str(strip, text, pos.x, baseline);
    } else {
        let tw = text.len() as u32 * 6;
        let pos = align.position(region, Size::new(tw, 13));
        let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);
        Text::new(text, Point::new(pos.x, pos.y + 13), style)
            .draw(strip)
            .unwrap();
    }
}

// image cache helpers

// 8.3 filename for a cached 1-bit image: IMXXXXXX.BIN (lower 24 bits of hash)
fn img_cache_name(hash: u32) -> [u8; 12] {
    let h = hash & 0x00FF_FFFF;
    let mut n = *b"IM000000.BIN";
    for i in 0..6 {
        let nibble = ((h >> (20 - i * 4)) & 0xF) as u8;
        n[2 + i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    n
}

#[inline]
fn img_cache_str(buf: &[u8; 12]) -> &str {
    core::str::from_utf8(buf).unwrap_or("IM000000.BIN")
}

// load a cached 1-bit image from SD; format: [u16 LE width][u16 LE height][1-bit data]
fn load_cached_image<SPI: embedded_hal::spi::SpiDevice>(
    svc: &Services<'_, SPI>,
    dir: &str,
    name: &str,
) -> Result<DecodedImage, &'static str> {
    let size = svc
        .file_size_pulp_sub(dir, name)
        .map_err(|_| "no cache file")?;
    if size < 5 {
        return Err("cache file too small");
    }
    let mut header = [0u8; 4];
    svc.read_pulp_sub_chunk(dir, name, 0, &mut header)
        .map_err(|_| "read header failed")?;
    let width = u16::from_le_bytes([header[0], header[1]]);
    let height = u16::from_le_bytes([header[2], header[3]]);
    if width == 0 || height == 0 {
        return Err("zero dimensions in cache");
    }
    let stride = (width as usize + 7) / 8;
    let data_len = stride * height as usize;
    if size as usize != 4 + data_len {
        return Err("cache size mismatch");
    }
    let mut data = Vec::new();
    data.try_reserve_exact(data_len)
        .map_err(|_| "OOM for cached image")?;
    data.resize(data_len, 0);
    svc.read_pulp_sub_chunk(dir, name, 4, &mut data)
        .map_err(|_| "read data failed")?;
    Ok(DecodedImage {
        width,
        height,
        data,
        stride,
    })
}

// write a decoded 1-bit image to SD cache
fn save_cached_image<SPI: embedded_hal::spi::SpiDevice>(
    svc: &Services<'_, SPI>,
    dir: &str,
    name: &str,
    img: &DecodedImage,
) -> Result<(), &'static str> {
    let mut header = [0u8; 4];
    header[0..2].copy_from_slice(&img.width.to_le_bytes());
    header[2..4].copy_from_slice(&img.height.to_le_bytes());
    svc.write_pulp_sub(dir, name, &header)?;
    svc.append_pulp_sub(dir, name, &img.data)?;
    Ok(())
}

fn extract_zip_entry<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    name: &str,
    zip_index: &ZipIndex,
    entry_idx: usize,
) -> Result<Vec<u8>, &'static str> {
    let entry = zip_index.entry(entry_idx);

    zip::extract_entry(entry, entry.local_offset, |offset, buf| {
        svc.read_file_chunk(name, offset, buf)
    })
}

impl App for ReaderApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        let msg = ctx.message();
        let len = msg.len().min(32);
        self.filename[..len].copy_from_slice(&msg[..len]);
        self.filename_len = len;

        let n = self.filename_len.min(self.title.len());
        self.title[..n].copy_from_slice(&self.filename[..n]);
        self.title_len = n;

        self.is_epub = epub::is_epub_filename(self.name());
        self.rebuild_quick_actions();
        self.reset_paging();
        self.ch_cache.clear();
        self.file_size = 0;
        self.chapter = 0;
        self.error = None;
        self.show_position = false;
        self.goto_last_page = false;
        self.restore_offset = None;

        self.apply_font_metrics();

        self.state = State::NeedBookmark;

        log::info!("reader: opening {}", self.name());

        ctx.mark_dirty(PAGE_REGION);
    }

    fn on_exit(&mut self) {
        self.line_count = 0;
        self.buf_len = 0;
        self.prefetch_page = NO_PREFETCH;
        self.prefetch_len = 0;
        self.restore_offset = None;
        self.show_position = false;
        self.ch_cache.clear();
        self.page_img = None;

        if self.is_epub {
            self.toc.clear();
            self.toc_source = None;
        }
    }

    fn on_suspend(&mut self) {}

    fn on_resume(&mut self, ctx: &mut AppContext) {
        let font_changed = self.book_font_size_idx != self.applied_font_idx;
        self.apply_font_metrics();
        if font_changed {
            self.reset_paging();
            // page offsets depend on line height; SD cache is font-independent
            if self.is_epub && self.chapters_cached {
                self.state = State::NeedIndex;
            } else {
                self.state = State::NeedPage;
            }
        }
        ctx.mark_dirty(PAGE_REGION);
    }

    fn needs_work(&self) -> bool {
        matches!(
            self.state,
            State::NeedBookmark
                | State::NeedInit
                | State::NeedOpf
                | State::NeedToc
                | State::NeedCache
                | State::NeedCacheChapter
                | State::NeedIndex
                | State::NeedPage
        )
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        loop {
            match self.state {
                State::NeedBookmark => {
                    self.bookmark_load(svc.bookmarks());

                    let _ = svc.write_pulp(RECENT_FILE, &self.filename[..self.filename_len]);

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

                State::NeedInit => match self.epub_init_zip(svc) {
                    Ok(()) => {
                        self.state = State::NeedOpf; // yield; CD heap freed
                    }
                    Err(e) => {
                        log::info!("reader: epub init (zip) failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedOpf => match self.epub_init_opf(svc) {
                    Ok(()) => {
                        self.state = State::NeedToc; // yield; OPF heap freed
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

                        match extract_zip_entry(svc, name, &self.zip, toc_idx) {
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
                            Err(e) => {
                                log::warn!("epub: failed to read TOC: {}", e);
                            }
                        }
                    }
                    self.rebuild_quick_actions();
                    self.state = State::NeedCache;
                    continue;
                }

                State::NeedCache => match self.epub_check_cache(svc) {
                    Ok(true) => {
                        self.state = State::NeedIndex;
                        continue;
                    }
                    Ok(false) => {
                        self.state = State::NeedCacheChapter; // yield; more chapters remain
                        ctx.mark_dirty(LOADING_REGION);
                    }
                    Err(e) => {
                        log::info!("reader: cache check failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedCacheChapter => match self.epub_cache_one_chapter(svc) {
                    Ok(true) => {
                        // all cached; update loading indicator
                        ctx.mark_dirty(LOADING_REGION);
                    }
                    Ok(false) => {
                        self.state = State::NeedIndex;
                        continue;
                    }
                    Err(e) => {
                        log::info!("reader: cache ch{} failed: {}", self.cache_chapter, e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedIndex => {
                    let want_last = self.goto_last_page;
                    self.goto_last_page = false;

                    self.epub_index_chapter();

                    // try to load entire chapter into RAM; if it fits,
                    // preindex all pages (~5ms for 50KB) for zero-SD-I/O turns
                    if self.try_cache_chapter(svc) {
                        self.preindex_all_pages();
                    }

                    if want_last {
                        match self.scan_to_last_page(svc) {
                            Ok(()) => {
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
                        // restore: scan to saved byte offset
                        self.page = 0;
                        loop {
                            match self.load_and_prefetch(svc) {
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
                            self.state = State::Ready;
                            ctx.mark_dirty(PAGE_REGION);
                        }
                    } else {
                        match self.load_and_prefetch(svc) {
                            Ok(()) => {
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
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        // TOC navigation
        if self.state == State::ShowToc {
            match event {
                ActionEvent::Press(Action::Back) => {
                    self.state = State::Ready;
                    ctx.mark_dirty(PAGE_REGION);
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
                        ctx.mark_dirty(PAGE_REGION);
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
                        ctx.mark_dirty(PAGE_REGION);
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

        // normal reader navigation
        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            // long press Next/Prev: rapid paging + position overlay
            ActionEvent::LongPress(Action::Next) => {
                if self.state == State::Ready {
                    self.show_position = true;
                }
                self.page_forward();
                Transition::None
            }
            ActionEvent::LongPress(Action::Prev) => {
                if self.state == State::Ready {
                    self.show_position = true;
                }
                self.page_backward();
                Transition::None
            }

            // release clears position overlay
            ActionEvent::Release(Action::Next) | ActionEvent::Release(Action::Prev) => {
                if self.show_position {
                    self.show_position = false;
                    ctx.mark_dirty(POSITION_OVERLAY);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                self.page_forward();
                Transition::None
            }

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                self.page_backward();
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) | ActionEvent::Repeat(Action::NextJump) => {
                self.jump_forward();
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) | ActionEvent::Repeat(Action::PrevJump) => {
                self.jump_backward();
                Transition::None
            }

            _ => Transition::None,
        }
    }

    fn help_text(&self) -> &'static str {
        if self.state == State::ShowToc {
            "Prev/Next: move  Jump: select  Back: close"
        } else if self.is_epub {
            "Prev/Next: page  Jump: chapter  Menu: options"
        } else {
            "Prev/Next: page  Jump: +/-10  Menu: options"
        }
    }

    fn quick_actions(&self) -> &[QuickAction] {
        &self.qa_buf[..self.qa_count]
    }

    fn on_quick_trigger(&mut self, id: u8, ctx: &mut AppContext) {
        match id {
            QA_SAVE_BOOKMARK => {
                log::info!("reader: bookmark save requested via quick menu");
            }
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
                    // pre-select current chapter
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
            let mut sbuf = FmtBuf::<32>::new();
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
            draw_chrome_text(
                strip,
                STATUS_REGION,
                sbuf.as_str(),
                Alignment::CenterRight,
                cf,
            );
        } else if self.file_size > 0 {
            let mut sbuf = FmtBuf::<24>::new();
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

        if let Some(msg) = self.error {
            draw_chrome_text(strip, LOADING_REGION, msg, Alignment::CenterLeft, cf);
            return;
        }

        if self.state != State::Ready && self.state != State::Error && self.state != State::ShowToc
        {
            // loading indicator during work states
            let mut lbuf = FmtBuf::<48>::new();
            match self.state {
                State::NeedCache | State::NeedCacheChapter => {
                    let _ = write!(
                        lbuf,
                        "Caching ch {}/{}...",
                        self.cache_chapter + 1,
                        self.spine.len()
                    );
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
            return;
        }

        // table of contents screen
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
                        Rectangle::new(Point::new(0, y_top), Size::new(480, line_h as u32))
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
            for i in 0..self.line_count {
                let span = self.lines[i];

                // inline image
                if span.is_image() {
                    if span.is_image_origin() {
                        let y_top = TEXT_Y as i32 + i as i32 * line_h;
                        if let Some(ref img) = self.page_img {
                            // centre horizontally
                            let img_x =
                                MARGIN as i32 + ((TEXT_W as i32 - img.width as i32) / 2).max(0);
                            strip.blit_1bpp(
                                &img.data,
                                0,
                                img.width as usize,
                                img.height as usize,
                                img.stride,
                                img_x,
                                y_top,
                                true,
                            );
                        } else {
                            // placeholder when image could not be decoded
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
                    // continuation lines (and origin after blit) are blank
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
                    let ch = if (0x20..=0x7E).contains(&b) {
                        b as char
                    } else {
                        j += 1;
                        continue; // non-printable
                    };
                    cx += fs.draw_char(strip, ch, sty, cx, baseline) as i32;
                    j += 1;
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

        // progress bar
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

        // position overlay (long-press feedback)
        if self.show_position && self.state == State::Ready {
            if POSITION_OVERLAY.intersects(strip.logical_window()) {
                let mut pbuf = FmtBuf::<48>::new();
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

                // inverted banner: black bg, white text
                POSITION_OVERLAY
                    .to_rect()
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                    .draw(strip)
                    .unwrap();
                let text = pbuf.as_str();
                if let Some(f) = cf {
                    let tw = f.measure_str(text) as u32;
                    let th = f.line_height as u32;
                    let pos = Alignment::Center.position(POSITION_OVERLAY, Size::new(tw, th));
                    let baseline = pos.y + f.ascent as i32;
                    f.draw_str_fg(strip, text, BinaryColor::Off, pos.x, baseline);
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
}
