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

impl Default for EpubMeta {
    fn default() -> Self {
        Self::new()
    }
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

impl Default for EpubSpine {
    fn default() -> Self {
        Self::new()
    }
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

// ── Table of Contents ─────────────────────────────────────────────────────

pub const MAX_TOC: usize = 128;
pub const TOC_TITLE_CAP: usize = 48;

#[derive(Clone, Copy)]
pub struct TocEntry {
    pub title: [u8; TOC_TITLE_CAP],
    pub title_len: u8,
    /// Index into `EpubSpine::items`; `0xFFFF` = unresolved / not in spine.
    pub spine_idx: u16,
}

impl TocEntry {
    pub const EMPTY: Self = Self {
        title: [0u8; TOC_TITLE_CAP],
        title_len: 0,
        spine_idx: 0xFFFF,
    };

    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("")
    }
}

pub struct EpubToc {
    pub entries: [TocEntry; MAX_TOC],
    pub count: u16,
}

impl Default for EpubToc {
    fn default() -> Self {
        Self::new()
    }
}

impl EpubToc {
    pub const fn new() -> Self {
        Self {
            entries: [TocEntry::EMPTY; MAX_TOC],
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count as usize
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn push(&mut self, title: &[u8], spine_idx: u16) {
        if (self.count as usize) >= MAX_TOC {
            return;
        }
        let i = self.count as usize;
        let n = title.len().min(TOC_TITLE_CAP);
        self.entries[i] = TocEntry::EMPTY;
        self.entries[i].title[..n].copy_from_slice(&title[..n]);
        self.entries[i].title_len = n as u8;
        self.entries[i].spine_idx = spine_idx;
        self.count += 1;
    }
}

/// Where the TOC data lives inside the EPUB ZIP.
#[derive(Clone, Copy, Debug)]
pub enum TocSource {
    /// EPUB 2 NCX file (zip entry index).
    Ncx(usize),
    /// EPUB 3 navigation document (zip entry index).
    Nav(usize),
}

impl TocSource {
    pub fn zip_index(&self) -> usize {
        match *self {
            TocSource::Ncx(i) | TocSource::Nav(i) => i,
        }
    }
}

// parse container.xml to find the OPF path; writes into out
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

// parse OPF: extract metadata and build the reading-order spine as indices into ZipIndex
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

        if let Some(idx) = zip.find(full_path).or_else(|| zip.find_icase(full_path))
            && (spine.count as usize) < MAX_SPINE
        {
            spine.items[spine.count as usize] = idx as u16;
            spine.count += 1;
        }
    }

    if spine.count == 0 {
        return Err("epub: spine is empty after resolution");
    }

    Ok(())
}

// ── TOC discovery and parsing ─────────────────────────────────────────────

/// Scan the OPF to locate the TOC file in the ZIP.
///
/// Checks EPUB 3 first (`<item properties="nav">`), then falls back to
/// EPUB 2 (`<spine toc="ncx_id">` → manifest lookup).
pub fn find_toc_source(opf: &[u8], opf_dir: &str, zip: &ZipIndex) -> Option<TocSource> {
    let mut path_buf = [0u8; 512];

    // ── EPUB 3: manifest item whose properties contain "nav" ────
    let mut nav_href_buf = [0u8; 256];
    let mut nav_href_len: usize = 0;
    xml::for_each_tag(opf, b"item", |tag_bytes| {
        if nav_href_len > 0 {
            return;
        }
        if let Some(props) = xml::get_attr(tag_bytes, b"properties") {
            if props.split(|&b| b == b' ').any(|w| w == b"nav") {
                if let Some(href) = xml::get_attr(tag_bytes, b"href") {
                    let n = href.len().min(nav_href_buf.len());
                    nav_href_buf[..n].copy_from_slice(&href[..n]);
                    nav_href_len = n;
                }
            }
        }
    });

    if nav_href_len > 0 {
        let decoded = percent_decode(&nav_href_buf[..nav_href_len]);
        let href_str = core::str::from_utf8(&decoded).unwrap_or("");
        let full_len = resolve_path(opf_dir, href_str, &mut path_buf);
        let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");
        if let Some(idx) = zip.find(full_path).or_else(|| zip.find_icase(full_path)) {
            log::info!("epub: TOC source = EPUB3 nav (zip #{})", idx);
            return Some(TocSource::Nav(idx));
        }
    }

    // ── EPUB 2: <spine toc="id"> → manifest item href ───────────
    let mut toc_id = [0u8; 64];
    let mut toc_id_len: usize = 0;
    xml::for_each_tag(opf, b"spine", |tag_bytes| {
        if toc_id_len > 0 {
            return;
        }
        if let Some(attr) = xml::get_attr(tag_bytes, b"toc") {
            let n = attr.len().min(toc_id.len());
            toc_id[..n].copy_from_slice(&attr[..n]);
            toc_id_len = n;
        }
    });

    if toc_id_len > 0 {
        let target_id = &toc_id[..toc_id_len];
        let mut ncx_href_buf = [0u8; 256];
        let mut ncx_href_len: usize = 0;
        xml::for_each_tag(opf, b"item", |tag_bytes| {
            if ncx_href_len > 0 {
                return;
            }
            if let Some(id) = xml::get_attr(tag_bytes, b"id") {
                if id == target_id {
                    if let Some(href) = xml::get_attr(tag_bytes, b"href") {
                        let n = href.len().min(ncx_href_buf.len());
                        ncx_href_buf[..n].copy_from_slice(&href[..n]);
                        ncx_href_len = n;
                    }
                }
            }
        });

        if ncx_href_len > 0 {
            let decoded = percent_decode(&ncx_href_buf[..ncx_href_len]);
            let href_str = core::str::from_utf8(&decoded).unwrap_or("");
            let full_len = resolve_path(opf_dir, href_str, &mut path_buf);
            let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");
            if let Some(idx) = zip.find(full_path).or_else(|| zip.find_icase(full_path)) {
                log::info!(
                    "epub: TOC source = EPUB2 NCX via spine toc attr (zip #{})",
                    idx
                );
                return Some(TocSource::Ncx(idx));
            }
        }
    }

    // ── Fallback: find NCX by media-type in manifest ────────────
    // Many EPUB 2 books omit the toc attribute on <spine> but still
    // include a toc.ncx referenced in the manifest with the NCX
    // media-type.
    let mut ncx_fb_href = [0u8; 256];
    let mut ncx_fb_len: usize = 0;
    xml::for_each_tag(opf, b"item", |tag_bytes| {
        if ncx_fb_len > 0 {
            return;
        }
        if let Some(mt) = xml::get_attr(tag_bytes, b"media-type") {
            if mt == b"application/x-dtbncx+xml" {
                if let Some(href) = xml::get_attr(tag_bytes, b"href") {
                    let n = href.len().min(ncx_fb_href.len());
                    ncx_fb_href[..n].copy_from_slice(&href[..n]);
                    ncx_fb_len = n;
                }
            }
        }
    });

    if ncx_fb_len > 0 {
        let decoded = percent_decode(&ncx_fb_href[..ncx_fb_len]);
        let href_str = core::str::from_utf8(&decoded).unwrap_or("");
        let full_len = resolve_path(opf_dir, href_str, &mut path_buf);
        let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");
        if let Some(idx) = zip.find(full_path).or_else(|| zip.find_icase(full_path)) {
            log::info!(
                "epub: TOC source = NCX via media-type fallback (zip #{})",
                idx
            );
            return Some(TocSource::Ncx(idx));
        }
    }

    log::warn!("epub: no TOC source found in OPF");
    None
}

/// Convenience dispatcher — parses TOC data using the correct format.
///
/// `data` is the decompressed TOC file content; `toc_dir` is the
/// directory containing the TOC file (for resolving relative hrefs).
pub fn parse_toc(
    source: TocSource,
    data: &[u8],
    toc_dir: &str,
    spine: &EpubSpine,
    zip: &ZipIndex,
    toc: &mut EpubToc,
) {
    match source {
        TocSource::Ncx(_) => parse_ncx_toc(data, toc_dir, spine, zip, toc),
        TocSource::Nav(_) => parse_nav_toc(data, toc_dir, spine, zip, toc),
    }
}

/// Parse an EPUB 2 NCX file (`toc.ncx`) into flat TOC entries.
///
/// Linear scan: tracks the most-recent `<text>` content and pairs it
/// with the next `<content src="...">` to produce each entry.  Nested
/// `<navPoint>` elements are flattened automatically.
pub fn parse_ncx_toc(
    ncx: &[u8],
    ncx_dir: &str,
    spine: &EpubSpine,
    zip: &ZipIndex,
    toc: &mut EpubToc,
) {
    toc.clear();
    let mut pos: usize = 0;
    let mut title_buf = [0u8; TOC_TITLE_CAP];
    let mut title_len: usize = 0;

    while pos < ncx.len() {
        let Some(lt) = toc_find_byte(ncx, pos, b'<') else {
            break;
        };
        pos = lt + 1;
        if pos >= ncx.len() {
            break;
        }

        // skip comments, PIs
        if ncx[pos] == b'!' || ncx[pos] == b'?' {
            pos = toc_skip_to_gt(ncx, pos);
            continue;
        }

        // skip closing tags
        let is_close = ncx[pos] == b'/';
        if is_close {
            pos = toc_skip_to_gt(ncx, pos + 1);
            continue;
        }

        // read tag name
        let name_start = pos;
        while pos < ncx.len() && !is_toc_delim(ncx[pos]) {
            pos += 1;
        }
        let name = &ncx[name_start..pos];

        // <text>: capture label for the next <content>
        if name.eq_ignore_ascii_case(b"text") {
            pos = toc_skip_to_gt(ncx, pos);
            let text_start = pos;
            while pos < ncx.len() && ncx[pos] != b'<' {
                pos += 1;
            }
            let raw = toc_trim_ws(&ncx[text_start..pos]);
            title_len = raw.len().min(TOC_TITLE_CAP);
            title_buf[..title_len].copy_from_slice(&raw[..title_len]);
            continue;
        }

        // <content src="...">: emit a TOC entry
        if name.eq_ignore_ascii_case(b"content") {
            let gt = toc_find_byte(ncx, pos, b'>').unwrap_or(ncx.len());
            let tag_bytes = &ncx[name_start..gt];
            if let Some(src) = xml::get_attr(tag_bytes, b"src") {
                let sidx = href_to_spine_idx(src, ncx_dir, spine, zip);
                toc.push(&title_buf[..title_len], sidx);
            }
            pos = if gt < ncx.len() { gt + 1 } else { gt };
            continue;
        }

        pos = toc_skip_to_gt(ncx, pos);
    }

    let unresolved = (0..toc.len())
        .filter(|&i| toc.entries[i].spine_idx == 0xFFFF)
        .count();
    if unresolved > 0 {
        log::warn!(
            "epub: NCX TOC: {} of {} entries unresolved",
            unresolved,
            toc.len()
        );
    }
}

/// Parse an EPUB 3 navigation document (XHTML with `<nav epub:type="toc">`).
///
/// Locates the `<nav>` element with `epub:type="toc"`, then extracts
/// `<a href="...">Title</a>` entries from within it.  Nested `<ol>`
/// lists (sub-chapters) are flattened.
pub fn parse_nav_toc(
    nav: &[u8],
    nav_dir: &str,
    spine: &EpubSpine,
    zip: &ZipIndex,
    toc: &mut EpubToc,
) {
    toc.clear();

    // Restrict scanning to the <nav epub:type="toc"> ... </nav> region
    let Some((region_start, region_end)) = find_nav_toc_region(nav) else {
        log::warn!("epub: nav document has no <nav epub:type=\"toc\"> region");
        return;
    };
    let region = &nav[region_start..region_end];

    let mut pos: usize = 0;
    while pos < region.len() {
        let Some(lt) = toc_find_byte(region, pos, b'<') else {
            break;
        };
        pos = lt + 1;
        if pos >= region.len() {
            break;
        }

        if region[pos] == b'!' || region[pos] == b'?' || region[pos] == b'/' {
            pos = toc_skip_to_gt(region, pos);
            continue;
        }

        let name_start = pos;
        while pos < region.len() && !is_toc_delim(region[pos]) {
            pos += 1;
        }
        let name = &region[name_start..pos];

        if !name.eq_ignore_ascii_case(b"a") {
            pos = toc_skip_to_gt(region, pos);
            continue;
        }

        // Found <a ...>: extract href attribute
        let gt = toc_find_byte(region, pos, b'>').unwrap_or(region.len());
        let tag_bytes = &region[name_start..gt];
        let href = xml::get_attr(tag_bytes, b"href");
        pos = if gt < region.len() { gt + 1 } else { gt };

        // Read text until </a>, stripping any nested tags (e.g. <span>)
        let mut title_buf = [0u8; TOC_TITLE_CAP];
        let mut title_len: usize = 0;
        while pos < region.len() {
            if region[pos] == b'<' {
                // Check for </a>
                if pos + 1 < region.len() && region[pos + 1] == b'/' {
                    let cs = pos + 2;
                    let mut ce = cs;
                    while ce < region.len() && !is_toc_delim(region[ce]) {
                        ce += 1;
                    }
                    if region[cs..ce].eq_ignore_ascii_case(b"a") {
                        pos = toc_skip_to_gt(region, ce);
                        break;
                    }
                }
                // Skip other nested tag
                pos = toc_skip_to_gt(region, pos + 1);
                continue;
            }
            // Accumulate text, collapsing whitespace
            if title_len < TOC_TITLE_CAP {
                let b = region[pos];
                if is_toc_ws(b) {
                    if title_len > 0 && title_buf[title_len - 1] != b' ' {
                        title_buf[title_len] = b' ';
                        title_len += 1;
                    }
                } else {
                    title_buf[title_len] = b;
                    title_len += 1;
                }
            }
            pos += 1;
        }

        // trim trailing space
        while title_len > 0 && title_buf[title_len - 1] == b' ' {
            title_len -= 1;
        }

        if let Some(href) = href {
            let sidx = href_to_spine_idx(href, nav_dir, spine, zip);
            toc.push(&title_buf[..title_len], sidx);
        }
    }

    let unresolved = (0..toc.len())
        .filter(|&i| toc.entries[i].spine_idx == 0xFFFF)
        .count();
    if unresolved > 0 {
        log::warn!(
            "epub: nav TOC: {} of {} entries unresolved",
            unresolved,
            toc.len()
        );
    }
}

/// Find the byte range of the `<nav epub:type="toc">` content.
///
/// Returns `(content_start, content_end)` — the bytes between `>`
/// and the matching `</nav>`.
fn find_nav_toc_region(data: &[u8]) -> Option<(usize, usize)> {
    let mut pos: usize = 0;
    while pos < data.len() {
        let Some(lt) = toc_find_byte(data, pos, b'<') else {
            break;
        };
        pos = lt + 1;
        if pos >= data.len() {
            break;
        }
        if data[pos] == b'!' || data[pos] == b'?' || data[pos] == b'/' {
            pos = toc_skip_to_gt(data, pos);
            continue;
        }

        let name_start = pos;
        while pos < data.len() && !is_toc_delim(data[pos]) {
            pos += 1;
        }
        let name = &data[name_start..pos];

        if !name.eq_ignore_ascii_case(b"nav") {
            pos = toc_skip_to_gt(data, pos);
            continue;
        }

        // Check for epub:type="toc" or type="toc"
        let gt = toc_find_byte(data, pos, b'>').unwrap_or(data.len());
        let tag_bytes = &data[name_start..gt];

        // epub:type can contain space-separated tokens, e.g. "toc landmarks"
        let is_toc = if let Some(t) = xml::get_attr(tag_bytes, b"epub:type") {
            t == b"toc" || t.split(|&b| b == b' ').any(|w| w == b"toc")
        } else {
            xml::get_attr(tag_bytes, b"type")
                .map(|t| t == b"toc" || t.split(|&b| b == b' ').any(|w| w == b"toc"))
                .unwrap_or(false)
        };

        if !is_toc {
            pos = if gt < data.len() { gt + 1 } else { gt };
            continue;
        }

        let content_start = if gt < data.len() { gt + 1 } else { gt };

        // Find closing </nav> (case-insensitive)
        let mut search = content_start;
        while search < data.len() {
            if data[search] == b'<' && search + 2 < data.len() && data[search + 1] == b'/' {
                let ts = search + 2;
                let mut te = ts;
                while te < data.len() && !is_toc_delim(data[te]) {
                    te += 1;
                }
                if data[ts..te].eq_ignore_ascii_case(b"nav") {
                    return Some((content_start, search));
                }
            }
            search += 1;
        }
        // no closing tag — use rest of document
        return Some((content_start, data.len()));
    }
    None
}

/// Resolve a TOC href to a spine index.
///
/// Strips the fragment identifier, percent-decodes, resolves relative to
/// `base_dir`, looks up the ZIP entry, and maps it to a spine position.
/// Returns `0xFFFF` if the href cannot be resolved.
fn href_to_spine_idx(href: &[u8], base_dir: &str, spine: &EpubSpine, zip: &ZipIndex) -> u16 {
    let decoded = percent_decode(href);
    let href_str = core::str::from_utf8(&decoded).unwrap_or("");
    // strip fragment (#section-id)
    let href_no_frag = href_str.split('#').next().unwrap_or(href_str);
    if href_no_frag.is_empty() {
        return 0xFFFF;
    }

    let mut path_buf = [0u8; 512];
    let full_len = resolve_path(base_dir, href_no_frag, &mut path_buf);
    let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");

    // 1. Try exact path match, then case-insensitive
    let zip_idx = zip
        .find(full_path)
        .or_else(|| zip.find_icase(full_path))
        .or_else(|| {
            // 2. Fallback: match by filename component only.
            //    Handles cases where the TOC and OPF resolve paths
            //    differently (different base dirs, leading ./, etc.)
            let filename = href_no_frag.rsplit('/').next().unwrap_or(href_no_frag);
            if filename.is_empty() {
                return None;
            }
            let fname = filename.as_bytes();
            for i in 0..zip.count() {
                let entry_name = zip.entry_name(i).as_bytes();
                let entry_fname = entry_name
                    .rsplit(|&b| b == b'/')
                    .next()
                    .unwrap_or(entry_name);
                if entry_fname.eq_ignore_ascii_case(fname) {
                    return Some(i);
                }
            }
            None
        });

    let Some(zip_idx) = zip_idx else {
        return 0xFFFF;
    };

    // 3. Map zip entry index → spine position
    for i in 0..spine.len() {
        if spine.items[i] as usize == zip_idx {
            return i as u16;
        }
    }

    // 4. Fallback: match by filename against spine entry names.
    //    Covers rare cases where the same content appears under
    //    slightly different paths in the manifest vs the TOC.
    let target_fname = zip
        .entry_name(zip_idx)
        .as_bytes()
        .rsplit(|&b| b == b'/')
        .next()
        .unwrap_or(b"");
    if !target_fname.is_empty() {
        for i in 0..spine.len() {
            let se = spine.items[i] as usize;
            let se_name = zip.entry_name(se).as_bytes();
            let se_fname = se_name.rsplit(|&b| b == b'/').next().unwrap_or(se_name);
            if se_fname.eq_ignore_ascii_case(target_fname) {
                return i as u16;
            }
        }
    }

    0xFFFF
}

// ── TOC scanning helpers (private) ────────────────────────────────────────

fn toc_find_byte(data: &[u8], start: usize, needle: u8) -> Option<usize> {
    data[start..]
        .iter()
        .position(|&b| b == needle)
        .map(|i| start + i)
}

fn toc_skip_to_gt(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() {
        if data[pos] == b'>' {
            return pos + 1;
        }
        pos += 1;
    }
    data.len()
}

#[inline]
fn is_toc_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

#[inline]
fn is_toc_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn toc_trim_ws(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|b| !is_toc_ws(*b))
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !is_toc_ws(*b))
        .map(|p| p + 1)
        .unwrap_or(start);
    if start >= end { &[] } else { &data[start..end] }
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

// check if a filename looks like an EPUB (.epub or .epu for FAT 8.3)
pub fn is_epub_filename(name: &str) -> bool {
    let b = name.as_bytes();

    // ".epub"
    if b.len() >= 5 && b[b.len() - 5] == b'.' {
        return b[b.len() - 4..].eq_ignore_ascii_case(b"epub");
    }
    // FAT 8.3 truncation: ".epu"
    if b.len() >= 4 && b[b.len() - 4] == b'.' {
        return b[b.len() - 3..].eq_ignore_ascii_case(b"epu");
    }

    false
}
