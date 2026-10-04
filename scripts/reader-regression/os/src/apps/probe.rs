// Read-only view of the real ReaderApp's state for the host tests. This module
// sits in `apps` (the parent of `reader`) so it can see the `pub(super)` fields
// of the real struct; it adds no behaviour.
use super::reader::{ReaderApp, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready,
    Toc,
    Error,
}

pub fn phase(a: &ReaderApp) -> Phase {
    match a.state {
        State::Ready => Phase::Ready,
        State::ShowToc => Phase::Toc,
        State::Error => Phase::Error,
        _ => Phase::Loading,
    }
}

pub fn state_name(a: &ReaderApp) -> String {
    format!("{:?}", a.state)
}

pub fn error_kind(a: &ReaderApp) -> Option<crate::error::ErrorKind> {
    a.error.as_ref().map(|e| e.kind())
}

pub fn page(a: &ReaderApp) -> usize {
    a.pg.page
}
pub fn total_pages(a: &ReaderApp) -> usize {
    a.pg.total_pages
}
pub fn fully_indexed(a: &ReaderApp) -> bool {
    a.pg.fully_indexed
}
pub fn offsets(a: &ReaderApp) -> Vec<u32> {
    a.pg.offsets[..a.pg.total_pages].to_vec()
}
pub fn file_size(a: &ReaderApp) -> u32 {
    a.file_size
}
pub fn max_lines(a: &ReaderApp) -> usize {
    a.max_lines as usize
}
pub fn text_w(a: &ReaderApp) -> u32 {
    a.text_w
}
pub fn text_margin(a: &ReaderApp) -> u16 {
    a.text_margin
}
pub fn text_y(a: &ReaderApp) -> u16 {
    a.text_y
}
pub fn font_line_h(a: &ReaderApp) -> u16 {
    a.font_line_h
}
pub fn text_area_h(a: &ReaderApp) -> u16 {
    a.text_area_h
}
pub fn theme_idx(a: &ReaderApp) -> u8 {
    a.reading_theme_idx
}
pub fn spine_len(a: &ReaderApp) -> usize {
    a.epub.spine.len()
}
pub fn chapter_cached_count(a: &ReaderApp) -> usize {
    a.epub.ch_cached.iter().filter(|&&c| c).count()
}
pub fn toc_len(a: &ReaderApp) -> usize {
    a.epub.toc.as_ref().map_or(0, |t| t.len())
}
pub fn toc_titles(a: &ReaderApp) -> Vec<String> {
    a.epub
        .toc
        .as_ref()
        .map(|t| t.entries[..t.len()].iter().map(|e| e.title_str().to_string()).collect())
        .unwrap_or_default()
}
pub fn toc_spine_idx(a: &ReaderApp) -> Vec<u16> {
    a.epub
        .toc
        .as_ref()
        .map(|t| t.entries[..t.len()].iter().map(|e| e.spine_idx).collect())
        .unwrap_or_default()
}
pub fn toc_selected(a: &ReaderApp) -> usize {
    a.epub.toc_selected
}
pub fn epub_title(a: &ReaderApp) -> String {
    a.epub.meta.title_str().to_string()
}
pub fn qa_count(a: &ReaderApp) -> usize {
    a.qa_count as usize
}

// one laid-out line: (start, len, flags, indent, raw bytes)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub start: u16,
    pub len: u16,
    pub flags: u8,
    pub indent: u8,
    pub bytes: Vec<u8>,
}

pub fn lines(a: &ReaderApp) -> Vec<Line> {
    a.pg.lines[..a.pg.line_count]
        .iter()
        .map(|l| Line {
            start: l.start,
            len: l.len,
            flags: l.flags,
            indent: l.indent,
            bytes: a.pg.buf[l.start as usize..(l.start + l.len) as usize].to_vec(),
        })
        .collect()
}

// the page's text with style markers removed, one String per line (image lines
// are returned as "[image]")
pub fn page_lines(a: &ReaderApp) -> Vec<String> {
    lines(a)
        .into_iter()
        .map(|l| {
            if l.flags & 8 != 0 {
                return "[image]".to_string();
            }
            let mut out = Vec::new();
            let mut i = 0;
            while i < l.bytes.len() {
                if l.bytes[i] == smol_epub::html_strip::MARKER && i + 1 < l.bytes.len() {
                    i += 2;
                    continue;
                }
                out.push(l.bytes[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).into_owned()
        })
        .collect()
}

pub fn line_count(a: &ReaderApp) -> usize {
    a.pg.line_count
}
