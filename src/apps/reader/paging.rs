// text wrapping, page navigation, and load/prefetch

use smol_epub::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, IMG_REF, ITALIC_OFF, ITALIC_ON, MARKER, QUOTE_OFF,
    QUOTE_ON,
};

use crate::fonts;
use crate::fonts::bitmap::FIRST_CHAR;
use crate::kernel::KernelHandle;

use super::{
    DEFAULT_IMG_H, INDENT_PX, LINES_PER_PAGE, LineSpan, MAX_PAGES, NO_PREFETCH, PAGE_BUF,
    ReaderApp, State, decode_utf8_char,
};

impl ReaderApp {
    pub(super) fn wrap_lines_counted(&mut self, n: usize) -> usize {
        let fonts_copy = self.fonts;

        if let Some(fs) = fonts_copy {
            let heights = &self.img_heights[..self.img_height_count as usize];
            let (c, count) = wrap_proportional(
                &self.pg.buf,
                n,
                &fs,
                &mut self.pg.lines,
                self.max_lines as usize,
                self.text_w,
                heights,
            );
            self.pg.line_count = count;
            c
        } else {
            self.wrap_monospace(n)
        }
    }

    pub(super) fn wrap_monospace(&mut self, n: usize) -> usize {
        use super::CHARS_PER_LINE;

        let max = self.max_lines as usize;
        self.pg.line_count = 0;
        let mut col: usize = 0;
        let mut line_start: usize = 0;

        for i in 0..n {
            let b = self.pg.buf[i];
            match b {
                b'\r' => {}
                b'\n' => {
                    let end = trim_trailing_cr(&self.pg.buf, line_start, i);
                    self.push_line(line_start, end);
                    line_start = i + 1;
                    col = 0;
                    if self.pg.line_count >= max {
                        return line_start;
                    }
                }
                _ => {
                    col += 1;
                    if col >= CHARS_PER_LINE {
                        self.push_line(line_start, i + 1);
                        line_start = i + 1;
                        col = 0;
                        if self.pg.line_count >= max {
                            return line_start;
                        }
                    }
                }
            }
        }

        if line_start < n && self.pg.line_count < max {
            let end = trim_trailing_cr(&self.pg.buf, line_start, n);
            self.push_line(line_start, end);
        }

        n
    }

    pub(super) fn push_line(&mut self, start: usize, end: usize) {
        if self.pg.line_count < LINES_PER_PAGE {
            self.pg.lines[self.pg.line_count] = LineSpan {
                start: start as u16,
                len: (end - start) as u16,
                flags: 0,
                indent: 0,
            };
            self.pg.line_count += 1;
        }
    }

    pub(super) fn reset_paging(&mut self) {
        self.pg.page = 0;
        self.pg.offsets[0] = 0;
        self.pg.total_pages = 1;
        self.pg.fully_indexed = false;
        self.pg.buf_len = 0;
        self.pg.line_count = 0;
        self.pg.prefetch_page = NO_PREFETCH;
        self.pg.prefetch_len = 0;
        self.page_img = None;
        self.fullscreen_img = false;
    }

    pub(super) fn load_and_prefetch(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<()> {
        if !self.epub.ch_cache.is_empty() {
            let start = (self.pg.offsets[self.pg.page] as usize).min(self.epub.ch_cache.len());
            let end = (start + PAGE_BUF).min(self.epub.ch_cache.len());
            let n = end - start;
            if n > 0 {
                self.pg.buf[..n].copy_from_slice(&self.epub.ch_cache[start..end]);
            }
            self.pg.buf_len = n;
            self.pg.prefetch_page = NO_PREFETCH;
            self.pg.prefetch_len = 0;
            self.prescan_image_heights(k, n);
            self.wrap_lines_counted(n);
            self.decode_page_images(k);
            return Ok(());
        }

        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        if self.pg.prefetch_page == self.pg.page {
            let pf_len = self.pg.prefetch_len;
            self.pg.buf[..pf_len].copy_from_slice(&self.pg.prefetch[..pf_len]);
            self.pg.buf_len = pf_len;
            self.pg.prefetch_page = NO_PREFETCH;
            self.pg.prefetch_len = 0;
        } else if self.is_epub && self.epub.chapters_cached {
            let cf_str = self.epub.cache_file_str();
            let ch = self.epub.chapter as usize;
            let ch_base = self.epub.chapter_table[ch].0;
            let n = k.read_cache_chunk(
                cf_str,
                ch_base + self.pg.offsets[self.pg.page],
                &mut self.pg.buf,
            )?;
            self.pg.buf_len = n;
        } else if self.file_size == 0 {
            let (size, n) = k.read_file_start(name, &mut self.pg.buf)?;
            self.file_size = size;
            self.pg.buf_len = n;
            log::info!("reader: opened {} ({} bytes)", name, size);

            if size == 0 {
                self.pg.fully_indexed = true;
                self.pg.line_count = 0;
                return Ok(());
            }
        } else {
            let n = k.read_chunk(name, self.pg.offsets[self.pg.page], &mut self.pg.buf)?;
            self.pg.buf_len = n;
        }

        self.prescan_image_heights(k, self.pg.buf_len);
        let consumed = self.wrap_lines_counted(self.pg.buf_len);
        let next_offset = self.pg.offsets[self.pg.page] + consumed as u32;

        if self.pg.page + 1 >= self.pg.total_pages && !self.pg.fully_indexed {
            if self.pg.line_count >= self.max_lines as usize && next_offset < self.file_size {
                if self.pg.total_pages < MAX_PAGES {
                    self.pg.offsets[self.pg.total_pages] = next_offset;
                    self.pg.total_pages += 1;
                } else {
                    self.pg.fully_indexed = true;
                }
            } else {
                self.pg.fully_indexed = true;
            }
        }

        if self.pg.page + 1 < self.pg.total_pages {
            if self.pg.prefetch.len() < PAGE_BUF {
                self.pg.prefetch.resize(PAGE_BUF, 0);
            }
            let pf_offset = self.pg.offsets[self.pg.page + 1];
            let pf_result = if self.is_epub && self.epub.chapters_cached {
                let cf_str = self.epub.cache_file_str();
                let ch = self.epub.chapter as usize;
                let ch_base = self.epub.chapter_table[ch].0;
                k.read_cache_chunk(cf_str, ch_base + pf_offset, &mut self.pg.prefetch)
            } else {
                k.read_chunk(name, pf_offset, &mut self.pg.prefetch)
            };
            match pf_result {
                Ok(n) => {
                    self.pg.prefetch_len = n;
                    self.pg.prefetch_page = self.pg.page + 1;
                }
                Err(_) => {
                    self.pg.prefetch_page = NO_PREFETCH;
                    self.pg.prefetch_len = 0;
                }
            }
        } else {
            self.pg.prefetch_page = NO_PREFETCH;
            self.pg.prefetch_len = 0;
        }

        self.decode_page_images(k);
        Ok(())
    }

    pub(super) fn preindex_all_pages(&mut self) {
        if self.epub.ch_cache.is_empty() {
            return;
        }

        let total = self.epub.ch_cache.len();
        self.pg.offsets[0] = 0;
        self.pg.total_pages = 1;

        let mut offset = 0usize;
        while offset < total && self.pg.total_pages < MAX_PAGES {
            let end = (offset + PAGE_BUF).min(total);
            let n = end - offset;
            self.pg.buf[..n].copy_from_slice(&self.epub.ch_cache[offset..end]);
            self.pg.buf_len = n;

            let consumed = self.wrap_lines_counted(n);
            let next_offset = offset + consumed;

            if self.pg.line_count >= self.max_lines as usize && next_offset < total {
                self.pg.offsets[self.pg.total_pages] = next_offset as u32;
                self.pg.total_pages += 1;
                offset = next_offset;
            } else {
                break;
            }
        }

        self.pg.fully_indexed = true;
        log::info!("chapter pre-indexed: {} pages", self.pg.total_pages);
    }

    pub(super) fn scan_to_last_page(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<()> {
        while !self.pg.fully_indexed && self.pg.total_pages < MAX_PAGES {
            self.pg.page = self.pg.total_pages - 1;
            self.load_and_prefetch(k)?;
            if self.pg.page + 1 < self.pg.total_pages {
                self.pg.page += 1;
            } else {
                break;
            }
        }
        if self.pg.total_pages > 0 {
            self.pg.page = self.pg.total_pages - 1;
        }
        self.pg.prefetch_page = NO_PREFETCH;
        self.load_and_prefetch(k)
    }

    pub(super) fn page_forward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }

        if self.pg.page + 1 < self.pg.total_pages {
            self.pg.page += 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub
            && self.pg.fully_indexed
            && (self.epub.chapter as usize + 1) < self.epub.spine.len()
        {
            self.epub.chapter += 1;
            self.goto_last_page = false;
            self.state = State::NeedIndex;
            return true;
        }

        false
    }

    pub(super) fn page_backward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }

        if self.pg.page > 0 {
            self.pg.page -= 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub && self.epub.chapter > 0 {
            self.epub.chapter -= 1;
            self.goto_last_page = true;
            self.state = State::NeedIndex;
            return true;
        }

        false
    }

    // next chapter (EPUB) or +10 pages (TXT)
    pub(super) fn jump_forward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }
        if self.is_epub {
            if (self.epub.chapter as usize + 1) < self.epub.spine.len() {
                self.epub.chapter += 1;
                self.goto_last_page = false;
                self.state = State::NeedIndex;
                return true;
            }
        } else {
            let last = if self.pg.total_pages > 0 {
                self.pg.total_pages - 1
            } else {
                0
            };
            let target = (self.pg.page + 10).min(last);
            if target != self.pg.page {
                self.pg.page = target;
                self.state = State::NeedPage;
                return true;
            }
        }
        false
    }

    // prev chapter (EPUB) or -10 pages (TXT)
    pub(super) fn jump_backward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }
        if self.is_epub {
            if self.epub.chapter > 0 {
                self.epub.chapter -= 1;
                self.goto_last_page = false;
                self.state = State::NeedIndex;
                return true;
            }
        } else {
            let target = self.pg.page.saturating_sub(10);
            if target != self.pg.page {
                self.pg.page = target;
                self.state = State::NeedPage;
                return true;
            }
        }
        false
    }
}

// UTF-8 decoding is provided by pulp_kernel::util::decode_utf8_char
// (re-exported via super::decode_utf8_char)

pub(super) fn trim_trailing_cr(buf: &[u8], start: usize, end: usize) -> usize {
    if end > start && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

// true if ch is a word-separator for line-wrapping (space, NBSP, etc)
#[inline]
fn is_wrap_space(ch: char) -> bool {
    matches!(ch, ' ' | '\u{00A0}')
}

pub(super) fn wrap_proportional(
    buf: &[u8],
    n: usize,
    fonts: &fonts::FontSet,
    lines: &mut [LineSpan],
    max_lines: usize,
    max_width_px: u32,
    img_heights: &[u16],
) -> (usize, usize) {
    let max_l = max_lines.min(lines.len());
    let base_max_w = max_width_px;
    let mut line_count: usize = 0;
    let mut line_start: usize = 0;
    let mut cursor_x: u32 = 0;
    let mut last_space: usize = 0;
    let mut cursor_at_space: u32 = 0;

    let mut bold = false;
    let mut italic = false;
    let mut heading = false;
    let mut indent: u8 = 0;
    let mut max_w = base_max_w;
    let mut img_idx: usize = 0;

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
            if line_count < max_l {
                let e = trim_trailing_cr(buf, $start, $end);
                lines[line_count] = LineSpan {
                    start: ($start) as u16,
                    len: (e - ($start)) as u16,
                    flags: LineSpan::pack_flags(bold, italic, heading),
                    indent,
                };
                line_count += 1;
            }
        };
    }

    let mut i = 0;
    while i < n {
        let b = buf[i];

        if b == MARKER && i + 1 < n {
            if buf[i + 1] == IMG_REF && i + 2 < n {
                let path_len = buf[i + 2] as usize;
                let path_start = i + 3;
                if path_start + path_len <= n && path_len > 0 {
                    if line_start < i {
                        emit!(line_start, i);
                        if line_count >= max_l {
                            return (i, line_count);
                        }
                    }

                    let line_h = fonts.line_height(fonts::Style::Regular);
                    // use pre-scanned height if available, else default
                    let img_h = if img_idx < img_heights.len() && img_heights[img_idx] > 0 {
                        img_heights[img_idx]
                    } else {
                        DEFAULT_IMG_H
                    };
                    img_idx += 1;
                    // ceiling division: ensure reserved lines fully cover image height
                    let img_lines = img_h.div_ceil(line_h).max(1) as usize;

                    if line_count < max_l {
                        lines[line_count] = LineSpan {
                            start: path_start as u16,
                            len: path_len as u16,
                            flags: LineSpan::FLAG_IMAGE,
                            indent: 0,
                        };
                        line_count += 1;
                    }

                    for _ in 1..img_lines {
                        if line_count >= max_l {
                            break;
                        }
                        lines[line_count] = LineSpan {
                            start: 0,
                            len: 0,
                            flags: LineSpan::FLAG_IMAGE,
                            indent: 0,
                        };
                        line_count += 1;
                    }

                    i = path_start + path_len;
                    line_start = i;
                    cursor_x = 0;
                    last_space = line_start;
                    cursor_at_space = 0;
                    if line_count >= max_l {
                        return (line_start, line_count);
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
            emit!(line_start, i);
            line_start = i + 1;
            cursor_x = 0;
            last_space = line_start;
            cursor_at_space = 0;
            if line_count >= max_l {
                return (line_start, line_count);
            }
            i += 1;
            continue;
        }

        // UTF-8 multi-byte: decode the full character and measure it
        // using the font's extended glyph tables
        if b >= 0xC0 {
            let (ch, seq_len) = decode_utf8_char(buf, i);

            // soft hyphen (U+00AD): zero-width break opportunity
            if ch == '\u{00AD}' {
                last_space = i + seq_len;
                cursor_at_space = cursor_x;
                i += seq_len;
                continue;
            }

            // NBSP and regular spaces: word-break opportunity
            if is_wrap_space(ch) {
                let sty = current_style(bold, italic, heading);
                cursor_x += fonts.advance(' ', sty) as u32;
                last_space = i + seq_len;
                cursor_at_space = cursor_x;
                if cursor_x > max_w {
                    emit!(line_start, i);
                    line_start = i + seq_len;
                    cursor_x = 0;
                    last_space = line_start;
                    cursor_at_space = 0;
                    if line_count >= max_l {
                        return (line_start, line_count);
                    }
                }
                i += seq_len;
                continue;
            }

            let sty = current_style(bold, italic, heading);
            let adv = fonts.advance(ch, sty) as u32;
            cursor_x += adv;
            if cursor_x > max_w {
                if last_space > line_start {
                    emit!(line_start, last_space);
                    cursor_x -= cursor_at_space;
                    line_start = last_space;
                } else {
                    emit!(line_start, i);
                    line_start = i;
                    cursor_x = adv;
                }
                last_space = line_start;
                cursor_at_space = 0;
                if line_count >= max_l {
                    return (line_start, line_count);
                }
            }
            i += seq_len;
            continue;
        }
        if b >= 0x80 {
            // stray continuation byte
            i += 1;
            continue;
        }

        // --- ASCII fast path: batch space and word runs ---
        let sty = current_style(bold, italic, heading);
        let font = fonts.font(sty);
        let glyphs = font.glyphs;

        if b == b' ' {
            let adv = glyphs[(b' ' - FIRST_CHAR) as usize].advance as u32;
            cursor_x += adv;
            last_space = i + 1;
            cursor_at_space = cursor_x;
            if cursor_x > max_w {
                emit!(line_start, i);
                line_start = i + 1;
                cursor_x = 0;
                last_space = line_start;
                cursor_at_space = 0;
                if line_count >= max_l {
                    return (line_start, line_count);
                }
            }
            i += 1;
            continue;
        }

        // Printable non-space ASCII (0x21..=0x7E): batch-scan the word run.
        // Find end of contiguous printable non-space ASCII bytes, sum advances.
        let word_start = i;
        let remaining = max_w.saturating_sub(cursor_x);
        let mut run_adv: u32 = 0;
        let mut j = i;
        while j < n {
            let c = buf[j];
            // stop at space, control chars, MARKER, high-bit bytes
            if c <= b' ' || c > 0x7E {
                break;
            }
            let a = glyphs[(c - FIRST_CHAR) as usize].advance as u32;
            if run_adv + a > remaining && j > word_start {
                // would overflow; stop batch here so we handle break properly
                break;
            }
            run_adv += a;
            j += 1;
        }

        if j > i {
            // consumed j - i bytes as a batch
            cursor_x += run_adv;
            i = j;
            if cursor_x > max_w {
                // overflow: break at last space or at word start
                if last_space > line_start {
                    emit!(line_start, last_space);
                    cursor_x -= cursor_at_space;
                    line_start = last_space;
                } else {
                    emit!(line_start, word_start);
                    line_start = word_start;
                    // recompute cursor_x from line_start..i
                    cursor_x = 0;
                    for k in line_start..i {
                        let c = buf[k];
                        if c >= FIRST_CHAR && c <= 0x7E {
                            cursor_x += glyphs[(c - FIRST_CHAR) as usize].advance as u32;
                        }
                    }
                }
                last_space = line_start;
                cursor_at_space = 0;
                if line_count >= max_l {
                    return (line_start, line_count);
                }
            }
            continue;
        }

        // single non-printable byte that wasn't caught above; skip
        i += 1;
    }

    if line_start < n && line_count < max_l {
        let e = trim_trailing_cr(buf, line_start, n);
        if e > line_start {
            lines[line_count] = LineSpan {
                start: line_start as u16,
                len: (e - line_start) as u16,
                flags: LineSpan::pack_flags(bold, italic, heading),
                indent,
            };
            line_count += 1;
        }
    }

    (n, line_count)
}
