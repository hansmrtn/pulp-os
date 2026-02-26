// EPUB structure parser (container.xml → OPF → spine + metadata)
//
// Parses the two XML files that define an EPUB's reading order:
// container.xml gives the OPF path; the OPF gives metadata, a
// manifest (id→href), and a spine (ordered idrefs). Spine idrefs
// are resolved through the manifest to ZIP entry indices.

use alloc::vec::Vec;

use crate::formats::xml;
use crate::formats::zip::ZipIndex;

pub const TITLE_CAP: usize = 96;
pub const AUTHOR_CAP: usize = 64;
pub const MAX_SPINE: usize = 256;
pub const OPF_PATH_CAP: usize = 256;

pub struct EpubMeta {
    pub title: [u8; TITLE_CAP],
    pub title_len: u8,
    pub author: [u8; AUTHOR_CAP],
    pub author_len: u8,
}

impl EpubMeta {
    pub const fn new() -> Self {
        Self {
            title: [0u8; TITLE_CAP],
            title_len: 0,
            author: [0u8; AUTHOR_CAP],
            author_len: 0,
        }
    }

    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("")
    }

    pub fn author_str(&self) -> &str {
        core::str::from_utf8(&self.author[..self.author_len as usize]).unwrap_or("")
    }

    fn set_title(&mut self, s: &[u8]) {
        let n = s.len().min(TITLE_CAP);
        self.title[..n].copy_from_slice(&s[..n]);
        self.title_len = n as u8;
    }

    fn set_author(&mut self, s: &[u8]) {
        let n = s.len().min(AUTHOR_CAP);
        self.author[..n].copy_from_slice(&s[..n]);
        self.author_len = n as u8;
    }
}

pub struct EpubSpine {
    pub items: [u16; MAX_SPINE],
    pub count: u16,
}

impl EpubSpine {
    pub const fn new() -> Self {
        Self {
            items: [0u16; MAX_SPINE],
            count: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Parse container.xml to find the OPF path. Writes into `out`.
pub fn parse_container(data: &[u8], out: &mut [u8; OPF_PATH_CAP]) -> Result<usize, &'static str> {
    let mut found_len: Option<usize> = None;

    xml::for_each_tag(data, b"rootfile", |tag_bytes| {
        if found_len.is_some() {
            return;
        }
        if let Some(path) = xml::get_attr(tag_bytes, b"full-path") {
            let n = path.len().min(OPF_PATH_CAP);
            out[..n].copy_from_slice(&path[..n]);
            found_len = Some(n);
        }
    });

    found_len.ok_or("epub: no rootfile full-path in container.xml")
}

struct ManifestItem {
    id: Vec<u8>,
    href: Vec<u8>,
}

/// Parse OPF: extract metadata and build the reading-order spine
/// as indices into the provided ZipIndex.
pub fn parse_opf(
    opf: &[u8],
    opf_dir: &str,
    zip: &ZipIndex,
    meta: &mut EpubMeta,
    spine: &mut EpubSpine,
) -> Result<(), &'static str> {
    *meta = EpubMeta::new();
    spine.count = 0;

    if let Some(title) = xml::tag_text(opf, b"title") {
        meta.set_title(title);
    }
    if let Some(author) = xml::tag_text(opf, b"creator") {
        meta.set_author(author);
    }

    // build manifest: id → href (heap temporary, freed at end)
    let mut manifest: Vec<ManifestItem> = Vec::with_capacity(64);
    xml::for_each_tag(opf, b"item", |tag_bytes| {
        let id = xml::get_attr(tag_bytes, b"id");
        let href = xml::get_attr(tag_bytes, b"href");
        if let (Some(id), Some(href)) = (id, href) {
            manifest.push(ManifestItem {
                id: Vec::from(id),
                href: Vec::from(href),
            });
        }
    });

    // collect spine idrefs
    let mut idrefs: Vec<Vec<u8>> = Vec::with_capacity(64);
    xml::for_each_tag(opf, b"itemref", |tag_bytes| {
        if let Some(idref) = xml::get_attr(tag_bytes, b"idref") {
            idrefs.push(Vec::from(idref));
        }
    });

    // resolve each idref → manifest href → zip entry index
    let mut path_buf = [0u8; 512];
    for idref in &idrefs {
        let Some(item) = manifest.iter().find(|m| m.id == *idref) else {
            continue;
        };

        let decoded_href = percent_decode(&item.href);
        let href_str = core::str::from_utf8(&decoded_href).unwrap_or("");
        let full_len = resolve_path(opf_dir, href_str, &mut path_buf);
        let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");

        if let Some(idx) = zip.find(full_path).or_else(|| zip.find_icase(full_path)) {
            if (spine.count as usize) < MAX_SPINE {
                spine.items[spine.count as usize] = idx as u16;
                spine.count += 1;
            }
        }
    }

    if spine.count == 0 {
        return Err("epub: spine is empty after resolution");
    }

    Ok(())
}

// -- path helpers --

fn resolve_path(base_dir: &str, href: &str, out: &mut [u8; 512]) -> usize {
    let href = href.split('#').next().unwrap_or(href);

    if href.starts_with('/') || base_dir.is_empty() {
        let href = href.trim_start_matches('/');
        let n = href.len().min(out.len());
        out[..n].copy_from_slice(&href.as_bytes()[..n]);
        return n;
    }

    let base = base_dir.as_bytes();
    let rel = href.as_bytes();

    let mut rel_pos = 0;
    let mut base_end = base.len();

    while rel_pos + 3 <= rel.len() && &rel[rel_pos..rel_pos + 3] == b"../" {
        rel_pos += 3;
        if let Some(slash) = base[..base_end].iter().rposition(|&b| b == b'/') {
            base_end = slash;
        } else {
            base_end = 0;
        }
    }

    if rel_pos + 2 <= rel.len()
        && &rel[rel_pos..rel_pos + 2] == b".."
        && (rel_pos + 2 == rel.len() || rel[rel_pos + 2] == b'/')
    {
        rel_pos += 2;
        if rel_pos < rel.len() && rel[rel_pos] == b'/' {
            rel_pos += 1;
        }
        if let Some(slash) = base[..base_end].iter().rposition(|&b| b == b'/') {
            base_end = slash;
        } else {
            base_end = 0;
        }
    }

    if rel_pos + 2 <= rel.len() && &rel[rel_pos..rel_pos + 2] == b"./" {
        rel_pos += 2;
    }

    let remaining = &rel[rel_pos..];

    if base_end == 0 {
        let n = remaining.len().min(out.len());
        out[..n].copy_from_slice(&remaining[..n]);
        n
    } else {
        let total = base_end + 1 + remaining.len();
        let n = total.min(out.len());

        let mut w = 0;
        let copy_base = base_end.min(n);
        out[..copy_base].copy_from_slice(&base[..copy_base]);
        w += copy_base;

        if w < n {
            out[w] = b'/';
            w += 1;
        }

        let copy_rem = remaining.len().min(n.saturating_sub(w));
        out[w..w + copy_rem].copy_from_slice(&remaining[..copy_rem]);
        w += copy_rem;

        w
    }
}

fn percent_decode(input: &[u8]) -> Vec<u8> {
    if !input.contains(&b'%') {
        return Vec::from(input);
    }

    let mut out = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' && i + 2 < input.len() {
            let hi = hex_nibble(input[i + 1]);
            let lo = hex_nibble(input[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(input[i]);
        i += 1;
    }
    out
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Check if a filename looks like an EPUB (.epub or .epu for FAT 8.3).
pub fn is_epub_filename(name: &str) -> bool {
    let b = name.as_bytes();

    if b.len() >= 5 {
        let e = &b[b.len() - 5..];
        if e[0] == b'.'
            && (e[1] | 0x20) == b'e'
            && (e[2] | 0x20) == b'p'
            && (e[3] | 0x20) == b'u'
            && (e[4] | 0x20) == b'b'
        {
            return true;
        }
    }

    if b.len() >= 4 {
        let e = &b[b.len() - 4..];
        if e[0] == b'.' && (e[1] | 0x20) == b'e' && (e[2] | 0x20) == b'p' && (e[3] | 0x20) == b'u' {
            return true;
        }
    }

    false
}
