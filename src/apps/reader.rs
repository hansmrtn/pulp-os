// Plain text and EPUB reader
//
// TXT: lazy indexed with prefetch; page 1 after a single SD read.
// EPUB: ZIP/OPF parsed once, chapters streamed and HTML-stripped
// into a heap Vec. Same paging engine for both modes.
// Proportional fonts via build-time rasterised bitmaps in flash.

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;

use crate::apps::{App, AppContext, Services, Transition};
use crate::board::button::Button as HwButton;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::formats::epub::{self, EpubMeta, EpubSpine};
use crate::formats::html_strip;
use crate::formats::zip::{self, ZipIndex};
use crate::ui::{Alignment, CONTENT_TOP, DynamicLabel, Label, Region, Widget};

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

#[derive(Clone, Copy, PartialEq)]
enum State {
    NeedInit,
    NeedChapter,
    NeedPage,
    Ready,
    Error,
}

#[derive(Clone, Copy)]
struct LineSpan {
    start: u16,
    len: u16,
}

impl LineSpan {
    const EMPTY: Self = Self { start: 0, len: 0 };
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
    chapter_text: Vec<u8>,
    goto_last_page: bool,

    // fonts (None → FONT_6X13 fallback)
    fonts: Option<fonts::FontSet>,
    font_line_h: u16,
    font_ascent: u16,
    max_lines: usize,

    // persisted preference — set by main before on_enter
    book_font_size_idx: u8,
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
            chapter_text: Vec::new(),
            goto_last_page: false,

            fonts: None,
            font_line_h: LINE_H,
            font_ascent: LINE_H,
            max_lines: LINES_PER_PAGE,

            book_font_size_idx: 0,
        }
    }

    // 0 = Small (~14 px), 1 = Medium (~18 px), 2 = Large (~24 px)
    pub fn set_book_font_size(&mut self, idx: u8) {
        self.book_font_size_idx = idx;
        self.apply_font_metrics();
    }

    /// (Re-)initialise all font-derived metrics from `book_font_size_idx`.
    /// Call whenever the index changes *or* on resume so the reader always
    /// uses the currently selected size, even if it was changed in Settings
    /// while the reader was suspended in the nav stack.
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
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.filename[..self.filename_len]).unwrap_or("???")
    }

    fn name_copy(&self) -> ([u8; 32], usize) {
        let mut buf = [0u8; 32];
        buf[..self.filename_len].copy_from_slice(&self.filename[..self.filename_len]);
        (buf, self.filename_len)
    }

    fn display_name(&self) -> &str {
        if self.title_len > 0 {
            core::str::from_utf8(&self.title[..self.title_len]).unwrap_or(self.name())
        } else {
            self.name()
        }
    }

    fn progress_pct(&self) -> u8 {
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

        let consumed = if let Some(fs) = fonts_copy {
            let (c, count) =
                wrap_proportional(&self.buf, n, &fs, &mut self.lines, self.max_lines, TEXT_W);
            self.line_count = count;
            c
        } else {
            self.wrap_monospace(n)
        };

        consumed
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

    fn load_page_from_memory(&mut self) {
        let ct_len = self.chapter_text.len();
        let offset = self.offsets[self.page] as usize;
        let remaining = ct_len.saturating_sub(offset);
        let n = remaining.min(PAGE_BUF);

        if n > 0 {
            self.buf[..n].copy_from_slice(&self.chapter_text[offset..offset + n]);
        }
        self.buf_len = n;

        let consumed = self.wrap_lines_counted(self.buf_len);
        let next_offset = offset + consumed;

        if self.page + 1 >= self.total_pages && !self.fully_indexed {
            if self.line_count >= self.max_lines && next_offset < ct_len {
                if self.total_pages < MAX_PAGES {
                    self.offsets[self.total_pages] = next_offset as u32;
                    self.total_pages += 1;
                } else {
                    self.fully_indexed = true;
                }
            } else {
                self.fully_indexed = true;
            }
        }

        // No prefetch needed — data is in memory
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
        } else if self.file_size == 0 {
            // first load; read_file_start folds size + read into one open
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
            // cache miss (backward nav, etc.)
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
            match svc.read_file_chunk(name, pf_offset, &mut self.prefetch) {
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

        // 1. Get file size
        let epub_size = svc.file_size(name)?;
        if epub_size < 22 {
            return Err("epub: file too small");
        }

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

        self.chapter = 0;
        self.goto_last_page = false;

        Ok(())
    }

    fn epub_load_chapter<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
    ) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let entry_idx = self.spine.items[self.chapter as usize] as usize;

        log::info!(
            "epub: loading chapter {}/{} (zip entry {} = {})",
            self.chapter + 1,
            self.spine.len(),
            entry_idx,
            self.zip.entry_name(entry_idx)
        );

        // Free the previous chapter's heap allocation *before* we
        // allocate for the new one.  .clear() keeps the backing
        // memory; replacing with an empty Vec actually frees it,
        // giving extract_zip_entry the full heap to work with.
        let old_cap = self.chapter_text.capacity();
        self.chapter_text = Vec::new();
        if old_cap > 0 {
            log::info!("epub: freed previous chapter buffer ({}KB)", old_cap / 1024);
        }

        // Decompress into a single buffer, then strip HTML in place.
        // Peak heap = just this one Vec (the uncompressed XHTML).
        // No second allocation — the stripped text overwrites the
        // same buffer since it is always shorter.
        let mut content = extract_zip_entry(svc, name, &self.zip, entry_idx)?;
        let raw_len = content.len();
        html_strip::strip_html_inplace(&mut content);
        self.chapter_text = content;

        log::info!(
            "epub: chapter {} — {}KB xhtml -> {}KB text",
            self.chapter + 1,
            raw_len / 1024,
            self.chapter_text.len() / 1024
        );

        // Reset paging for this chapter
        self.reset_paging();
        self.file_size = self.chapter_text.len() as u32;

        Ok(())
    }

    fn scan_to_last_page(&mut self) {
        // Load pages sequentially until fully indexed
        while !self.fully_indexed && self.total_pages < MAX_PAGES {
            let next_page = self.total_pages - 1;
            self.page = next_page;
            self.load_page_from_memory();
        }
        // Now go to the actual last page
        if self.total_pages > 0 {
            self.page = self.total_pages - 1;
        }
        // Reload the last page into buf for display
        self.load_page_from_memory();
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
                self.state = State::NeedChapter;
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
            self.state = State::NeedChapter;
            return true;
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
    let max_w = max_width_px as u32;
    let mut lc: usize = 0;
    let mut ls: usize = 0;
    let mut px: u32 = 0;
    let mut sp: usize = 0;
    let mut sp_px: u32 = 0;

    macro_rules! emit {
        ($start:expr, $end:expr) => {
            if lc < max_l {
                let e = trim_trailing_cr(buf, $start, $end);
                lines[lc] = LineSpan {
                    start: ($start) as u16,
                    len: (e - ($start)) as u16,
                };
                lc += 1;
            }
        };
    }

    let sty = fonts::Style::Regular;

    for i in 0..n {
        let b = buf[i];

        if b == b'\r' {
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
            continue;
        }

        let adv = fonts.advance_byte(b, sty) as u32;

        if b == b' ' {
            px += adv;
            sp = i + 1;
            sp_px = px;
            // Space itself pushed us over — break before it
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
    }

    if ls < n && lc < max_l {
        let e = trim_trailing_cr(buf, ls, n);
        if e > ls {
            lines[lc] = LineSpan {
                start: ls as u16,
                len: (e - ls) as u16,
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
        self.reset_paging();
        self.file_size = 0;
        self.error = None;
        self.goto_last_page = false;

        self.apply_font_metrics();

        if self.is_epub {
            self.zip.clear();
            self.meta = EpubMeta::new();
            self.spine = EpubSpine::new();
            self.chapter = 0;
            self.chapter_text.clear();
            self.state = State::NeedInit;
            log::info!("reader: opening epub {}", self.name());
        } else {
            self.state = State::NeedPage;
            log::info!("reader: opening txt {}", self.name());
        }

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
            self.chapter_text = Vec::new();
        }
    }

    fn on_suspend(&mut self) {}

    fn on_resume(&mut self, ctx: &mut AppContext) {
        // Re-apply font metrics in case book_font_size_idx was updated via
        // set_book_font_size() while this app was suspended under Settings.
        self.apply_font_metrics();
        // A font size change also invalidates the current page layout; force
        // a full re-wrap so line breaks match the new metrics.
        self.reset_paging();
        ctx.request_screen_redraw();
    }

    fn needs_work(&self) -> bool {
        matches!(
            self.state,
            State::NeedInit | State::NeedChapter | State::NeedPage
        )
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        loop {
            match self.state {
                State::NeedInit => match self.epub_init(svc) {
                    Ok(()) => {
                        self.state = State::NeedChapter;
                        continue;
                    }
                    Err(e) => {
                        log::info!("reader: epub init failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedChapter => match self.epub_load_chapter(svc) {
                    Ok(()) => {
                        if self.goto_last_page {
                            self.goto_last_page = false;
                            self.scan_to_last_page();
                            self.state = State::Ready;
                            ctx.mark_dirty(PAGE_REGION);
                        } else {
                            self.load_page_from_memory();
                            self.state = State::Ready;
                            ctx.mark_dirty(PAGE_REGION);
                        }
                    }
                    Err(e) => {
                        log::info!("reader: chapter load failed: {}", e);
                        self.error = Some(e);
                        self.state = State::Error;
                        ctx.mark_dirty(PAGE_REGION);
                    }
                },

                State::NeedPage => {
                    if self.is_epub {
                        self.load_page_from_memory();
                        self.state = State::Ready;
                        ctx.mark_dirty(PAGE_REGION);
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

    fn on_event(&mut self, event: Event, _ctx: &mut AppContext) -> Transition {
        match event {
            Event::Press(HwButton::Back) => Transition::Pop,

            Event::Press(HwButton::Right | HwButton::VolDown)
            | Event::Repeat(HwButton::Right | HwButton::VolDown) => {
                self.page_forward();
                Transition::None
            }

            Event::Press(HwButton::Left | HwButton::VolUp)
            | Event::Repeat(HwButton::Left | HwButton::VolUp) => {
                self.page_backward();
                Transition::None
            }

            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        Label::new(HEADER_REGION, self.display_name(), &FONT_6X13)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.is_epub && self.spine.len() > 0 {
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

        if self.state != State::Ready && self.state != State::Error {
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
                fs.draw_bytes(
                    strip,
                    &self.buf[start..end],
                    fonts::Style::Regular,
                    MARGIN as i32,
                    baseline,
                );
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
    }
}
