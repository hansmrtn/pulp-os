// Plain text and EPUB reader
//
// TXT: lazy indexed with prefetch; page 1 after a single SD read.
//
// EPUB: ZIP/OPF parsed once; chapters are stream-decompressed, HTML-
// stripped, and written to an SD-card cache as styled text files.
// After caching, EPUB pages are read from SD identically to TXT —
// the chapter_text heap Vec is gone and the reading path is unified.
// Cache is validated by file size + filename hash so re-opening a
// previously read book skips decompression entirely.
//
// Proportional fonts via build-time rasterised bitmaps in flash.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;

use crate::apps::{App, AppContext, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::formats::cache;
use crate::formats::epub::{self, EpubMeta, EpubSpine, EpubToc, TocSource};
use crate::formats::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, ITALIC_OFF, ITALIC_ON, MARKER, QUOTE_OFF, QUOTE_ON,
};
use crate::formats::zip::{self, ZipIndex};
use crate::ui::quick_menu::QuickAction;
use crate::ui::{Alignment, CONTENT_TOP, DynamicLabel, Label, Region};

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

// full reader content area (header + text body); used by on_work for partial DU refreshes
const PAGE_REGION: Region = Region::new(0, HEADER_Y, 480, 800 - HEADER_Y);

const NO_PREFETCH: usize = usize::MAX;

const TEXT_W: f32 = (480 - 2 * MARGIN) as f32;
const TEXT_AREA_H: u16 = 800 - TEXT_Y - MARGIN;
const EOCD_TAIL: usize = 512;
const INDENT_PX: u32 = 24; // pixels per blockquote indent level

// ── Progress bar ──────────────────────────────────────────────────────
const PROGRESS_H: u16 = 2;
const PROGRESS_Y: u16 = 800 - PROGRESS_H - 1;
const PROGRESS_W: u16 = 480 - 2 * MARGIN;

// ── Quick-action IDs ──────────────────────────────────────────────────────
const QA_FONT_SIZE: u8 = 1;
const QA_SAVE_BOOKMARK: u8 = 2;
const QA_PREV_CHAPTER: u8 = 3;
const QA_NEXT_CHAPTER: u8 = 4;
const QA_TOC: u8 = 5;

const QA_FONT_OPTIONS: &[&str] = &["Small", "Medium", "Large"];
const QA_MAX: usize = 5;

#[derive(Clone, Copy, PartialEq)]
enum State {
    NeedBookmark,
    NeedInit,
    NeedToc,
    NeedCache,
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
    /// Style flags at the start of this line (carried from wrap state).
    /// bit 0: bold, bit 1: italic, bit 2: heading
    flags: u8,
    /// Blockquote indent depth (0 = none, 1+ = nested quotes).
    /// Each level indents the line further.
    indent: u8,
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

    // epub
    is_epub: bool,
    zip: ZipIndex,
    meta: EpubMeta,
    spine: EpubSpine,
    chapter: u16,
    goto_last_page: bool,

    // epub chapter cache (SD-backed, no heap for chapter text)
    cache_dir: [u8; 8],
    epub_name_hash: u32,
    epub_file_size: u32,
    chapter_sizes: [u32; cache::MAX_CACHE_CHAPTERS],
    chapters_cached: bool,

    // table of contents
    toc: EpubToc,
    toc_source: Option<TocSource>,
    toc_selected: usize,
    toc_scroll: usize,

    // fonts (None → FONT_6X13 fallback)
    fonts: Option<fonts::FontSet>,
    font_line_h: u16,
    font_ascent: u16,
    max_lines: usize,

    // persisted preference — set by main before on_enter
    book_font_size_idx: u8,
    applied_font_idx: u8,

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

            is_epub: false,
            zip: ZipIndex::new(),
            meta: EpubMeta::new(),
            spine: EpubSpine::new(),
            chapter: 0,
            goto_last_page: false,

            cache_dir: [0u8; 8],
            epub_name_hash: 0,
            epub_file_size: 0,
            chapter_sizes: [0u32; cache::MAX_CACHE_CHAPTERS],
            chapters_cached: false,

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

            qa_buf: [QuickAction::trigger(0, "", ""); QA_MAX],
            qa_count: 0,
        }
    }

    // 0 = Small (14 px), 1 = Medium (21 px), 2 = Large (30 px)
    pub fn set_book_font_size(&mut self, idx: u8) {
        self.book_font_size_idx = idx;
        self.apply_font_metrics();
        self.rebuild_quick_actions();
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

        // chapter nav only available for multi-chapter epubs
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

    // ── Bookmarks ─────────────────────────────────────────────────────────────
    //
    // File: "BOOKMARKS" on the SD root — a flat array of 20-byte records.
    // Layout:
    //   name_hash  u32     bytes 0–3    FNV-1a of filename
    //   page       u32     bytes 4–7    page index (within chapter for epub)
    //   chapter    u16     bytes 8–9    epub chapter; 0 for txt
    //   flags      u16     bytes 10–11  bit 0 = valid
    //   name_pfx   [u8;8]  bytes 12–19  filename prefix for collision safety
    //
    // Lookup matches on hash AND name prefix to avoid silent wrong-position
    // restores when two filenames collide in FNV-1a.

    const BOOKMARK_FILE: &'static str = "BOOKMARKS";
    const BOOKMARK_SLOTS: usize = 32;
    const BOOKMARK_RECORD_LEN: usize = 20;
    const BOOKMARK_FILE_LEN: usize = Self::BOOKMARK_SLOTS * Self::BOOKMARK_RECORD_LEN;
    const NAME_PFX_LEN: usize = 8;

    // FNV-1a 32-bit hash of the current filename
    fn bookmark_key(&self) -> u32 {
        let mut h: u32 = 0x811c_9dc5;
        for &b in &self.filename[..self.filename_len] {
            h ^= b as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
        h
    }

    fn name_prefix(&self) -> [u8; Self::NAME_PFX_LEN] {
        let mut pfx = [0u8; Self::NAME_PFX_LEN];
        let n = self.filename_len.min(Self::NAME_PFX_LEN);
        pfx[..n].copy_from_slice(&self.filename[..n]);
        pfx
    }

    fn bookmark_encode(&self) -> [u8; Self::BOOKMARK_RECORD_LEN] {
        let key = self.bookmark_key();
        let page = self.page as u32;
        let chapter = self.chapter;
        let flags: u16 = 1;
        let pfx = self.name_prefix();
        let mut rec = [0u8; Self::BOOKMARK_RECORD_LEN];
        rec[0..4].copy_from_slice(&key.to_le_bytes());
        rec[4..8].copy_from_slice(&page.to_le_bytes());
        rec[8..10].copy_from_slice(&chapter.to_le_bytes());
        rec[10..12].copy_from_slice(&flags.to_le_bytes());
        rec[12..20].copy_from_slice(&pfx);
        rec
    }

    // save bookmark; called synchronously by main on nav events
    pub fn save_position<SPI: embedded_hal::spi::SpiDevice>(&self, svc: &mut Services<'_, SPI>) {
        if self.state == State::Ready {
            self.bookmark_save(svc);
        }
    }

    fn bookmark_save<SPI: embedded_hal::spi::SpiDevice>(&self, svc: &mut Services<'_, SPI>) {
        let key = self.bookmark_key();
        let pfx = self.name_prefix();
        let mut file_buf = [0u8; Self::BOOKMARK_FILE_LEN];
        let mut slot_count = 0usize;

        if let Ok((_, n)) = svc.read_file_start(Self::BOOKMARK_FILE, &mut file_buf) {
            slot_count = (n / Self::BOOKMARK_RECORD_LEN).min(Self::BOOKMARK_SLOTS);
        }

        let mut target_slot = slot_count;
        for i in 0..slot_count {
            let base = i * Self::BOOKMARK_RECORD_LEN;
            let flags = u16::from_le_bytes([file_buf[base + 10], file_buf[base + 11]]);
            if flags & 1 == 0 {
                if target_slot == slot_count {
                    target_slot = i;
                }
                continue;
            }
            let stored_key = u32::from_le_bytes([
                file_buf[base],
                file_buf[base + 1],
                file_buf[base + 2],
                file_buf[base + 3],
            ]);
            if stored_key == key && file_buf[base + 12..base + 20] == pfx {
                target_slot = i;
                break;
            }
        }

        if target_slot >= Self::BOOKMARK_SLOTS {
            // All slots full; overwrite slot 0 (LRU would be nicer but costly).
            target_slot = 0;
        }

        let base = target_slot * Self::BOOKMARK_RECORD_LEN;
        let rec = self.bookmark_encode();
        file_buf[base..base + Self::BOOKMARK_RECORD_LEN].copy_from_slice(&rec);

        let new_len = ((target_slot + 1).max(slot_count)) * Self::BOOKMARK_RECORD_LEN;

        match svc.write_file(Self::BOOKMARK_FILE, &file_buf[..new_len]) {
            Ok(_) => log::info!(
                "bookmark: saved page={} ch={} for key={:#010x}",
                self.page,
                self.chapter,
                key
            ),
            Err(e) => log::warn!("bookmark: save failed: {}", e),
        }
    }

    // restore a saved bookmark; returns true if found
    fn bookmark_load<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> bool {
        let key = self.bookmark_key();
        let pfx = self.name_prefix();
        let mut file_buf = [0u8; Self::BOOKMARK_FILE_LEN];

        let slot_count = match svc.read_file_start(Self::BOOKMARK_FILE, &mut file_buf) {
            Ok((_, n)) => (n / Self::BOOKMARK_RECORD_LEN).min(Self::BOOKMARK_SLOTS),
            Err(_) => return false,
        };

        for i in 0..slot_count {
            let base = i * Self::BOOKMARK_RECORD_LEN;
            let flags = u16::from_le_bytes([file_buf[base + 10], file_buf[base + 11]]);
            if flags & 1 == 0 {
                continue;
            }
            let stored_key = u32::from_le_bytes([
                file_buf[base],
                file_buf[base + 1],
                file_buf[base + 2],
                file_buf[base + 3],
            ]);
            if stored_key != key || file_buf[base + 12..base + 20] != pfx {
                continue;
            }

            let page = u32::from_le_bytes([
                file_buf[base + 4],
                file_buf[base + 5],
                file_buf[base + 6],
                file_buf[base + 7],
            ]) as usize;
            let chapter = u16::from_le_bytes([file_buf[base + 8], file_buf[base + 9]]);

            log::info!(
                "bookmark: restoring page={} ch={} for key={:#010x}",
                page,
                chapter,
                key
            );

            self.page = page;
            self.chapter = chapter;
            return true;
        }

        false
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

            // Last page of the last chapter → 100%
            if ch + 1 >= spine_len && self.fully_indexed && self.page + 1 >= self.total_pages {
                return 100;
            }

            // Within-chapter progress (0–100)
            let in_ch = if self.file_size == 0 {
                0u64
            } else {
                let pos = self.offsets[self.page] as u64;
                let size = self.file_size as u64;
                ((pos * 100) / size).min(100)
            };

            // Overall: (chapter * 100 + in_chapter_pct) / spine_len
            let overall = (ch * 100 + in_ch) / spine_len;
            return overall.min(100) as u8;
        }

        // TXT path
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
    }

    fn load_and_prefetch<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        if self.prefetch_page == self.page {
            // prefetch hit, zero SD I/O
            core::mem::swap(&mut self.buf, &mut self.prefetch);
            self.buf_len = self.prefetch_len;
            self.prefetch_page = NO_PREFETCH;
            self.prefetch_len = 0;
        } else if self.is_epub && self.chapters_cached {
            // EPUB: read from cached styled-text file on SD
            let dir_buf = self.cache_dir;
            let dir = cache::dir_name_str(&dir_buf);
            let ch_file = cache::chapter_file_name(self.chapter);
            let ch_str = cache::chapter_file_str(&ch_file);
            let n = svc.read_chunk_in_dir(dir, ch_str, self.offsets[self.page], &mut self.buf)?;
            self.buf_len = n;
        } else if self.file_size == 0 {
            // TXT first load; read_file_start folds size + read into one open
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
            // TXT cache miss (backward nav, etc.)
            let n = svc.read_file_chunk(name, self.offsets[self.page], &mut self.buf)?;
            self.buf_len = n;
        }

        // wrap lines and discover next page offset
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

        // prefetch next page
        if self.page + 1 < self.total_pages {
            let pf_offset = self.offsets[self.page + 1];
            let pf_result = if self.is_epub && self.chapters_cached {
                let dir_buf = self.cache_dir;
                let dir = cache::dir_name_str(&dir_buf);
                let ch_file = cache::chapter_file_name(self.chapter);
                let ch_str = cache::chapter_file_str(&ch_file);
                svc.read_chunk_in_dir(dir, ch_str, pf_offset, &mut self.prefetch)
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

        Ok(())
    }

    fn epub_init<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        // 1. Get file size and compute cache identifiers
        let epub_size = svc.file_size(name)?;
        if epub_size < 22 {
            return Err("epub: file too small");
        }
        self.epub_file_size = epub_size;
        self.epub_name_hash = cache::fnv1a(name.as_bytes());
        self.cache_dir = cache::dir_name_for_hash(self.epub_name_hash);

        // 2. Read EOCD from tail of file
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

        // 3. Read central directory (heap temporary)
        let mut cd_buf = vec![0u8; cd_size as usize];
        read_full(svc, name, cd_offset, &mut cd_buf)?;
        self.zip.clear();
        self.zip.parse_central_directory(&cd_buf)?;
        drop(cd_buf);

        log::info!("epub: {} entries in ZIP", self.zip.count());

        // 4. Read container.xml
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

        // 5. Read and parse OPF
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

        // Discover TOC source while OPF bytes are still available;
        // actual extraction is deferred to NeedToc to avoid stack overflow.
        self.toc_source = epub::find_toc_source(&opf_data, opf_dir, &self.zip);
        drop(opf_data);

        log::info!(
            "epub: \"{}\" by {} — {} chapters",
            self.meta.title_str(),
            self.meta.author_str(),
            self.spine.len()
        );

        // Set display title from metadata (inline to avoid borrow conflict)
        let tlen = self.meta.title_len as usize;
        if tlen > 0 {
            let n = tlen.min(self.title.len());
            self.title[..n].copy_from_slice(&self.meta.title[..n]);
            self.title_len = n;
        }

        self.toc.clear();

        Ok(())
    }

    /// Validate or build the SD chapter cache for the entire book.
    ///
    /// On first open, each chapter is stream-decompressed, HTML-stripped
    /// (with style markers), and written to a cache file under
    /// `_XXXXXXX/CHXXX.TXT`.  A `META.BIN` header records epub size,
    /// name hash, and per-chapter text sizes for fast validation on
    /// subsequent opens.
    ///
    /// Temporary heap: ~47 KB (decompressor + window + read buffer),
    /// freed after each chapter.  No persistent heap allocation.
    fn epub_ensure_cache<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);

        // Try to load and validate existing cache
        let mut meta_buf = [0u8; cache::META_MAX_SIZE];
        if let Ok(n) = svc.read_chunk_in_dir(dir, cache::META_FILE, 0, &mut meta_buf)
            && let Ok(info) = cache::parse_cache_meta(
                &meta_buf[..n],
                self.epub_file_size,
                self.epub_name_hash,
                self.spine.len(),
            )
        {
            self.chapter_sizes[..info.chapter_count]
                .copy_from_slice(&info.chapter_sizes[..info.chapter_count]);
            self.chapters_cached = true;
            log::info!("epub: cache hit ({} chapters)", info.chapter_count);
            return Ok(());
        }

        // Cache miss — stream each chapter to SD
        log::info!("epub: building cache for {} chapters", self.spine.len());
        svc.ensure_dir(dir)?;

        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let spine_len = self.spine.len();
        for ch in 0..spine_len {
            let entry_idx = self.spine.items[ch] as usize;
            let entry = *self.zip.entry(entry_idx);

            let ch_file = cache::chapter_file_name(ch as u16);
            let ch_str = cache::chapter_file_str(&ch_file);

            // Create empty file (truncate if stale)
            svc.write_in_dir(dir, ch_str, &[])?;

            // Stream: decompress → HTML strip → append to cache file.
            // Temporary heap (~47 KB) is allocated inside stream_strip_entry
            // and freed when it returns.
            let svc_ref = &*svc;
            let text_size = cache::stream_strip_entry(
                &entry,
                entry.local_offset,
                |offset, buf| svc_ref.read_file_chunk(epub_name, offset, buf),
                |chunk| svc_ref.append_in_dir(dir, ch_str, chunk),
            )?;

            self.chapter_sizes[ch] = text_size;
            log::info!("epub: cached ch{} = {} bytes", ch, text_size);
        }

        // Write META.BIN so subsequent opens skip decompression
        let meta_len = cache::encode_cache_meta(
            self.epub_file_size,
            self.epub_name_hash,
            &self.chapter_sizes[..spine_len],
            &mut meta_buf,
        );
        svc.write_in_dir(dir, cache::META_FILE, &meta_buf[..meta_len])?;

        self.chapters_cached = true;
        log::info!("epub: cache complete");
        Ok(())
    }

    /// Set up paging state for the current chapter from the SD cache.
    ///
    /// Resets the page offset table and sets `file_size` to the cached
    /// chapter's text size.  No SD I/O — just index bookkeeping.
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

    fn scan_to_last_page<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        // Scan forward, building the page offset table, until the
        // entire chapter / file is indexed.
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
            // Normal page turn within the current content
            self.page += 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub && self.fully_indexed {
            // At the end of a chapter — try next chapter
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
            // At the start of a chapter — go to last page of prev chapter
            self.chapter -= 1;
            self.goto_last_page = true;
            self.state = State::NeedIndex;
            return true;
        }

        false
    }

    // next chapter (epub) or +10 pages (txt)
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

    // prev chapter (epub) or -10 pages (txt)
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

// ── Helpers ─────────────────────────────────────────────────────

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
    max_width_px: f32,
) -> (usize, usize) {
    let max_l = max_lines.min(lines.len());
    let base_max_w = max_width_px as u32;
    let mut lc: usize = 0;
    let mut ls: usize = 0;
    let mut px: u32 = 0;
    let mut sp: usize = 0;
    let mut sp_px: u32 = 0;

    // Style state — carried across lines, updated by markers
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

        // 2-byte style markers: [MARKER, tag] — zero width, update state
        if b == MARKER && i + 1 < n {
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
                // Word-wrap at last space
                emit!(ls, sp);
                px -= sp_px;
                ls = sp;
            } else {
                // No space on this line — character-wrap
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

        // Default display title = filename (inline to avoid borrow conflict)
        let n = self.filename_len.min(self.title.len());
        self.title[..n].copy_from_slice(&self.filename[..n]);
        self.title_len = n;

        self.is_epub = epub::is_epub_filename(self.name());
        self.rebuild_quick_actions();
        self.reset_paging();
        self.file_size = 0;
        self.chapter = 0;
        self.error = None;
        self.goto_last_page = false;

        self.apply_font_metrics();

        // Load the bookmark first; the actual book init follows in on_work
        // after we know the saved position.
        self.state = State::NeedBookmark;

        log::info!("reader: opening {}", self.name());

        // Full GC refresh for the initial screen transition.
        // Subsequent page turns in on_work use mark_dirty (DU partial).
        ctx.request_screen_redraw();
    }

    fn on_exit(&mut self) {
        self.line_count = 0;
        self.buf_len = 0;
        self.prefetch_page = NO_PREFETCH;
        self.prefetch_len = 0;

        if self.is_epub {
            self.toc.clear();
            self.toc_source = None;
        }
    }

    fn on_suspend(&mut self) {
        // Position is saved synchronously by main.rs via save_position()
        // before this is called, so nothing extra is needed here.
    }

    fn on_resume(&mut self, ctx: &mut AppContext) {
        let font_changed = self.book_font_size_idx != self.applied_font_idx;
        self.apply_font_metrics();
        if font_changed {
            self.reset_paging();
            // Cache files are font-independent (styled text), but page
            // offsets depend on line height / advance widths.  For EPUB
            // rebuild the chapter's page index from the cached text;
            // for TXT just re-page from the file.
            if self.is_epub && self.chapters_cached {
                self.state = State::NeedIndex;
            } else {
                self.state = State::NeedPage;
            }
        }
        ctx.request_screen_redraw();
    }

    fn needs_work(&self) -> bool {
        matches!(
            self.state,
            State::NeedBookmark
                | State::NeedInit
                | State::NeedToc
                | State::NeedCache
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
                    let found = self.bookmark_load(svc);
                    if self.is_epub {
                        if found {
                            // chapter was restored; NeedInit will load that chapter
                            // and page_forward will position within it.
                        }
                        self.zip.clear();
                        self.meta = EpubMeta::new();
                        self.spine = EpubSpine::new();
                        self.chapters_cached = false;
                        // goto_last_page only when bookmark says page > 0
                        self.goto_last_page = false;
                        self.state = State::NeedInit;
                    } else {
                        // For TXT, page index maps directly to offsets[].
                        // We'll seek to offsets[page] once NeedPage runs and
                        // the offset table is populated. If page == 0 nothing
                        // special is needed.
                        self.state = State::NeedPage;
                    }
                    continue;
                }

                State::NeedInit => match self.epub_init(svc) {
                    Ok(()) => {
                        self.state = State::NeedToc;
                        continue;
                    }
                    Err(e) => {
                        log::info!("reader: epub init failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedToc => {
                    // Runs in its own on_work cycle so the epub_init
                    // stack frames are fully unwound before we allocate
                    // the decompression buffer for the TOC file.
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

                State::NeedCache => match self.epub_ensure_cache(svc) {
                    Ok(()) => {
                        self.state = State::NeedIndex;
                        continue;
                    }
                    Err(e) => {
                        log::info!("reader: cache build failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedIndex => {
                    // epub_index_chapter calls reset_paging() which zeroes
                    // self.page.  Save the target values first so bookmark
                    // restore and backward-nav-to-last-page still work.
                    let target_page = self.page;
                    let want_last = self.goto_last_page;
                    self.goto_last_page = false;

                    self.epub_index_chapter();

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
                    } else if target_page > 0 {
                        // Bookmark requested a non-zero page within this
                        // chapter. Scan forward until we reach it.
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
                            if self.page >= target_page || self.page + 1 >= self.total_pages {
                                break;
                            }
                            self.page += 1;
                        }
                        if self.state != State::Error {
                            self.state = State::Ready;
                            ctx.mark_dirty(PAGE_REGION);
                        }
                    } else {
                        self.state = State::NeedPage;
                        continue;
                    }
                }

                State::NeedPage => {
                    // Unified for TXT and cached EPUB — both read from SD.
                    let target_page = self.page;
                    if target_page > 0 && self.offsets[target_page] == 0 {
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
                            if self.page >= target_page || self.page + 1 >= self.total_pages {
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
        // ── TOC navigation ────────────────────────────────────────
        if self.state == State::ShowToc {
            match event {
                ActionEvent::Press(Action::Back) => {
                    self.state = State::Ready;
                    ctx.mark_dirty(PAGE_REGION);
                    return Transition::None;
                }
                ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                    if self.toc_selected + 1 < self.toc.len() {
                        self.toc_selected += 1;
                        let vis = (TEXT_AREA_H / self.font_line_h) as usize;
                        if self.toc_selected >= self.toc_scroll + vis {
                            self.toc_scroll = self.toc_selected + 1 - vis;
                        }
                        ctx.mark_dirty(PAGE_REGION);
                    }
                    return Transition::None;
                }
                ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                    if self.toc_selected > 0 {
                        self.toc_selected -= 1;
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

        // ── Normal reader navigation ──────────────────────────────
        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

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
                // SD flush handled by main.rs via save_position
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
                    // Pre-select the current chapter in the TOC list
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
            // Cache files are styled text — font-independent.  Only the
            // page offset table needs rebuilding on font change.
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
        Label::new(HEADER_REGION, self.display_name(), &FONT_6X13)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.state == State::ShowToc {
            let mut status = DynamicLabel::<32>::new(STATUS_REGION, &FONT_6X13)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "Contents");
            status.draw(strip).unwrap();
        } else if self.is_epub && !self.spine.is_empty() {
            let mut status = DynamicLabel::<32>::new(STATUS_REGION, &FONT_6X13)
                .alignment(Alignment::CenterRight);

            if self.spine.len() > 1 {
                if self.fully_indexed {
                    let _ = write!(
                        status,
                        "Ch{}/{} {}/{}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1,
                        self.total_pages
                    );
                } else {
                    let _ = write!(
                        status,
                        "Ch{}/{} p{}",
                        self.chapter + 1,
                        self.spine.len(),
                        self.page + 1
                    );
                }
            } else if self.fully_indexed {
                let _ = write!(status, "{}/{}", self.page + 1, self.total_pages);
            } else {
                let _ = write!(status, "p{}", self.page + 1);
            }

            status.draw(strip).unwrap();
        } else if self.file_size > 0 {
            let mut status = DynamicLabel::<24>::new(STATUS_REGION, &FONT_6X13)
                .alignment(Alignment::CenterRight);
            if self.fully_indexed {
                let _ = write!(status, "{}/{}", self.page + 1, self.total_pages);
            } else {
                let _ = write!(status, "{} | {}%", self.page + 1, self.progress_pct());
            }
            status.draw(strip).unwrap();
        }

        if let Some(msg) = self.error {
            let r = Region::new(MARGIN, TEXT_Y, 464, 20);
            Label::new(r, msg, &FONT_6X13)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        if self.state != State::Ready && self.state != State::Error && self.state != State::ShowToc
        {
            return;
        }

        // ── Table of Contents screen ──────────────────────────────
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
                let start = span.start as usize;
                let end = start + span.len as usize;
                let baseline = TEXT_Y as i32 + i as i32 * line_h + ascent;
                let x_indent = INDENT_PX as i32 * span.indent as i32;

                // Style-marker-aware rendering: walk bytes, switch
                // fonts on [MARKER, tag] pairs, draw everything else.
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
                        continue; // skip non-printable (stray marker bytes, etc.)
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

        // ── Progress bar ──────────────────────────────────────────────
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
    }
}
