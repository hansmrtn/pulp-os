// text wrapping, page navigation, and load/prefetch

use smol_epub::cache;
use smol_epub::html_strip::{
    BOLD_OFF, BOLD_ON, HEADING_OFF, HEADING_ON, IMG_REF, ITALIC_OFF, ITALIC_ON, MARKER, QUOTE_OFF,
    QUOTE_ON,
};

use crate::fonts;
use crate::kernel::KernelHandle;

use super::{
    IMAGE_DISPLAY_H, INDENT_PX, LINES_PER_PAGE, LineSpan, MAX_PAGES, NO_PREFETCH, PAGE_BUF,
    ReaderApp, State, TEXT_W,
};

impl ReaderApp {
    pub(super) fn wrap_lines_counted(&mut self, n: usize) -> usize {
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

    pub(super) fn wrap_monospace(&mut self, n: usize) -> usize {
        use super::CHARS_PER_LINE;

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

    pub(super) fn push_line(&mut self, start: usize, end: usize) {
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

    pub(super) fn reset_paging(&mut self) {
        self.page = 0;
        self.offsets[0] = 0;
        self.total_pages = 1;
        self.fully_indexed = false;
        self.buf_len = 0;
        self.line_count = 0;
        self.prefetch_page = NO_PREFETCH;
        self.prefetch_len = 0;
        self.page_img = None;
        self.fullscreen_img = false;
    }

    pub(super) fn load_and_prefetch(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<()> {
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
            self.wrap_lines_counted(n);
            self.decode_page_images(k);
            return Ok(());
        }

        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        if self.prefetch_page == self.page {
            core::mem::swap(&mut self.buf, &mut self.prefetch);
            self.buf_len = self.prefetch_len;
            self.prefetch_page = NO_PREFETCH;
            self.prefetch_len = 0;
        } else if self.is_epub && self.chapters_cached {
            let dir_buf = self.cache_dir;
            let dir = cache::dir_name_str(&dir_buf);
            let ch_file = cache::chapter_file_name(self.chapter);
            let ch_str = cache::chapter_file_str(&ch_file);
            let n = k.read_app_subdir_chunk(dir, ch_str, self.offsets[self.page], &mut self.buf)?;
            self.buf_len = n;
        } else if self.file_size == 0 {
            let (size, n) = k.read_file_start(name, &mut self.buf)?;
            self.file_size = size;
            self.buf_len = n;
            log::info!("reader: opened {} ({} bytes)", name, size);

            if size == 0 {
                self.fully_indexed = true;
                self.line_count = 0;
                return Ok(());
            }
        } else {
            let n = k.read_chunk(name, self.offsets[self.page], &mut self.buf)?;
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
                k.read_app_subdir_chunk(dir, ch_str, pf_offset, &mut self.prefetch)
            } else {
                k.read_chunk(name, pf_offset, &mut self.prefetch)
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

        self.decode_page_images(k);
        Ok(())
    }

    pub(super) fn preindex_all_pages(&mut self) {
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

    pub(super) fn scan_to_last_page(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<()> {
        while !self.fully_indexed && self.total_pages < MAX_PAGES {
            self.page = self.total_pages - 1;
            self.load_and_prefetch(k)?;
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
        self.load_and_prefetch(k)
    }

    pub(super) fn page_forward(&mut self) -> bool {
        if self.state != State::Ready {
            return false;
        }

        if self.page + 1 < self.total_pages {
            self.page += 1;
            self.state = State::NeedPage;
            return true;
        }

        if self.is_epub && self.fully_indexed && (self.chapter as usize + 1) < self.spine.len() {
            self.chapter += 1;
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
    pub(super) fn jump_forward(&mut self) -> bool {
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
    pub(super) fn jump_backward(&mut self) -> bool {
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

// decode one utf-8 character starting at buf[pos]
// returns (char, byte_length); malformed input yields ('\u{FFFD}', consumed)
pub(super) fn decode_utf8_char(buf: &[u8], pos: usize) -> (char, usize) {
    let b0 = buf[pos];
    let (mut cp, expected) = if b0 < 0xE0 {
        ((b0 as u32) & 0x1F, 2)
    } else if b0 < 0xF0 {
        ((b0 as u32) & 0x0F, 3)
    } else {
        ((b0 as u32) & 0x07, 4)
    };
    let len = buf.len();
    if pos + expected > len {
        return ('\u{FFFD}', len - pos);
    }
    for i in 1..expected {
        let cont = buf[pos + i];
        if cont & 0xC0 != 0x80 {
            return ('\u{FFFD}', i);
        }
        cp = (cp << 6) | (cont as u32 & 0x3F);
    }
    let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
    (ch, expected)
}

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
) -> (usize, usize) {
    let max_l = max_lines.min(lines.len());
    let base_max_w = max_width_px;
    let mut lc: usize = 0;
    let mut ls: usize = 0;
    let mut px: u32 = 0;
    let mut sp: usize = 0;
    let mut sp_px: u32 = 0;

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

        if b == MARKER && i + 1 < n {
            if buf[i + 1] == IMG_REF && i + 2 < n {
                let path_len = buf[i + 2] as usize;
                let path_start = i + 3;
                if path_start + path_len <= n && path_len > 0 {
                    if ls < i {
                        emit!(ls, i);
                        if lc >= max_l {
                            return (i, lc);
                        }
                    }

                    let line_h = fonts.line_height(fonts::Style::Regular);
                    let img_lines = (IMAGE_DISPLAY_H / line_h).max(1) as usize;

                    if lc < max_l {
                        lines[lc] = LineSpan {
                            start: path_start as u16,
                            len: path_len as u16,
                            flags: LineSpan::FLAG_IMAGE,
                            indent: 0,
                        };
                        lc += 1;
                    }

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

        // UTF-8 multi-byte: decode the full character and measure it
        // using the font's extended glyph tables
        if b >= 0xC0 {
            let (ch, seq_len) = decode_utf8_char(buf, i);

            // soft hyphen (U+00AD): zero-width break opportunity
            if ch == '\u{00AD}' {
                sp = i + seq_len;
                sp_px = px;
                i += seq_len;
                continue;
            }

            // NBSP and regular spaces: word-break opportunity
            if is_wrap_space(ch) {
                let sty = current_style(bold, italic, heading);
                px += fonts.advance(' ', sty) as u32;
                sp = i + seq_len;
                sp_px = px;
                if px > max_w {
                    emit!(ls, i);
                    ls = i + seq_len;
                    px = 0;
                    sp = ls;
                    sp_px = 0;
                    if lc >= max_l {
                        return (ls, lc);
                    }
                }
                i += seq_len;
                continue;
            }

            let sty = current_style(bold, italic, heading);
            let adv = fonts.advance(ch, sty) as u32;
            px += adv;
            if px > max_w {
                if sp > ls {
                    emit!(ls, sp);
                    px -= sp_px;
                    ls = sp;
                } else {
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
            i += seq_len;
            continue;
        }
        if b >= 0x80 {
            // stray continuation byte
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
                emit!(ls, sp);
                px -= sp_px;
                ls = sp;
            } else {
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
