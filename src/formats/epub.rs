// EPUB structure parser — container.xml and OPF spine/metadata
//
// An EPUB is a ZIP archive with a defined structure:
//
//   META-INF/container.xml
//     → points to the OPF package document (e.g. OEBPS/content.opf)
//
//   *.opf (package document)
//     → <metadata>  title, author
//     → <manifest>  id → href mapping for every file in the book
//     → <spine>     ordered list of idrefs defining reading order
//
// This module parses both XML files using the minimal scanner in
// `crate::formats::xml`, producing an `EpubMeta` (title/author)
// and `EpubSpine` (ordered ZIP entry indices for reading).
//
// All parsing uses borrowed slices from caller-provided buffers.
// Temporary heap Vecs are used during OPF manifest resolution and
// freed before returning.

use alloc::vec::Vec;

use crate::formats::xml;
use crate::formats::zip::ZipIndex;

// ── Public types ────────────────────────────────────────────────

/// Maximum title length we store (bytes). Titles longer than this
/// are silently truncated.
pub const TITLE_CAP: usize = 96;

/// Maximum author length we store.
pub const AUTHOR_CAP: usize = 64;

/// Maximum number of spine items (chapters) we track.
pub const MAX_SPINE: usize = 256;

/// Maximum length of the OPF path extracted from container.xml.
pub const OPF_PATH_CAP: usize = 256;

/// Book metadata extracted from the OPF `<metadata>` block.
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

    /// Get the title as a string, or `""` if empty.
    pub fn title_str(&self) -> &str {
        core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or("")
    }

    /// Get the author as a string, or `""` if empty.
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

/// Reading-order spine: an ordered list of ZIP entry indices.
///
/// Each entry in `items` is an index into the `ZipIndex` that was
/// used during parsing. The caller can use this to sequentially
/// extract chapter XHTML from the ZIP.
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

    /// Number of chapters in reading order.
    #[inline]
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Is the spine empty?
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

// ── container.xml parsing ───────────────────────────────────────

/// Parse `META-INF/container.xml` to find the OPF package path.
///
/// Writes the path into `out` and returns its length, or an error
/// if no `<rootfile>` with `full-path` is found.
///
/// Typical content:
/// ```xml
/// <container>
///   <rootfiles>
///     <rootfile full-path="OEBPS/content.opf"
///               media-type="application/oebps-package+xml"/>
///   </rootfiles>
/// </container>
/// ```
pub fn parse_container(data: &[u8], out: &mut [u8; OPF_PATH_CAP]) -> Result<usize, &'static str> {
    let mut found_len: Option<usize> = None;

    xml::for_each_tag(data, b"rootfile", |tag_bytes| {
        if found_len.is_some() {
            return; // already found
        }
        if let Some(path) = xml::get_attr(tag_bytes, b"full-path") {
            let n = path.len().min(OPF_PATH_CAP);
            out[..n].copy_from_slice(&path[..n]);
            found_len = Some(n);
        }
    });

    found_len.ok_or("epub: no rootfile full-path in container.xml")
}

// ── OPF parsing ─────────────────────────────────────────────────

/// Temporary manifest entry used during OPF parsing.
/// Heap-allocated, freed after spine resolution.
struct ManifestItem {
    id: Vec<u8>,
    href: Vec<u8>,
}

/// Parse the OPF package document, extracting metadata and building
/// the reading-order spine.
///
/// # Arguments
/// * `opf`     — raw bytes of the OPF XML
/// * `opf_dir` — directory containing the OPF file (e.g. `"OEBPS"`),
///               used to resolve relative hrefs in the manifest.
///               Pass `""` if the OPF is in the ZIP root.
/// * `zip`     — the ZIP index, used to map resolved paths to entry indices
/// * `meta`    — output: filled with title/author
/// * `spine`   — output: filled with ordered ZIP entry indices
pub fn parse_opf(
    opf: &[u8],
    opf_dir: &str,
    zip: &ZipIndex,
    meta: &mut EpubMeta,
    spine: &mut EpubSpine,
) -> Result<(), &'static str> {
    // Reset outputs
    *meta = EpubMeta::new();
    spine.count = 0;

    // ── Extract metadata ────────────────────────────────────────
    if let Some(title) = xml::tag_text(opf, b"title") {
        meta.set_title(title);
    }
    if let Some(author) = xml::tag_text(opf, b"creator") {
        meta.set_author(author);
    }

    // ── Build manifest: id → href ───────────────────────────────
    // Heap-allocated, freed at end of this function.
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

    // ── Build spine: ordered idrefs ─────────────────────────────
    // Collect idrefs first, then resolve through manifest + zip.
    let mut idrefs: Vec<Vec<u8>> = Vec::with_capacity(64);

    xml::for_each_tag(opf, b"itemref", |tag_bytes| {
        if let Some(idref) = xml::get_attr(tag_bytes, b"idref") {
            idrefs.push(Vec::from(idref));
        }
    });

    // ── Resolve each idref → manifest href → zip entry index ────
    let mut path_buf = [0u8; 512];

    for idref in &idrefs {
        // Find this idref in the manifest
        let Some(item) = manifest.iter().find(|m| m.id == *idref) else {
            // Unknown idref — skip silently (could be a non-content item)
            continue;
        };

        // Decode percent-encoded characters in the href
        let decoded_href = percent_decode(&item.href);
        let href_str = core::str::from_utf8(&decoded_href).unwrap_or("");

        // Resolve the href relative to the OPF directory
        let full_len = resolve_path(opf_dir, href_str, &mut path_buf);
        let full_path = core::str::from_utf8(&path_buf[..full_len]).unwrap_or("");

        // Look up in the ZIP index
        let entry_idx = zip.find(full_path).or_else(|| zip.find_icase(full_path));

        if let Some(idx) = entry_idx {
            if (spine.count as usize) < MAX_SPINE {
                spine.items[spine.count as usize] = idx as u16;
                spine.count += 1;
            }
        }
    }

    // manifest, idrefs, and all their inner Vecs are dropped here

    if spine.count == 0 {
        return Err("epub: spine is empty after resolution");
    }

    Ok(())
}

// ── Path helpers ────────────────────────────────────────────────

/// Resolve a relative `href` against a `base_dir`.
///
/// If `href` starts with `/`, it's treated as absolute (returned as-is).
/// Otherwise it's joined: `"{base_dir}/{href}"`.
///
/// Writes the result into `out` and returns the number of bytes written.
fn resolve_path(base_dir: &str, href: &str, out: &mut [u8; 512]) -> usize {
    // Strip any fragment (#...) from href
    let href = href.split('#').next().unwrap_or(href);

    if href.starts_with('/') || base_dir.is_empty() {
        // Absolute path or OPF is at root — use href directly
        let href = href.trim_start_matches('/');
        let n = href.len().min(out.len());
        out[..n].copy_from_slice(&href.as_bytes()[..n]);
        return n;
    }

    let base = base_dir.as_bytes();
    let rel = href.as_bytes();

    // Count how many "../" segments the href has
    let mut rel_pos = 0;
    let mut base_end = base.len();

    while rel_pos + 3 <= rel.len() && &rel[rel_pos..rel_pos + 3] == b"../" {
        rel_pos += 3;
        // Walk base_dir up one level
        if let Some(slash) = base[..base_end].iter().rposition(|&b| b == b'/') {
            base_end = slash;
        } else {
            base_end = 0;
        }
    }

    // Also handle a lone ".." at the end (no trailing slash)
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

    // Strip "./" prefix if present
    if rel_pos + 2 <= rel.len() && &rel[rel_pos..rel_pos + 2] == b"./" {
        rel_pos += 2;
    }

    let remaining = &rel[rel_pos..];

    if base_end == 0 {
        // Base is root
        let n = remaining.len().min(out.len());
        out[..n].copy_from_slice(&remaining[..n]);
        n
    } else {
        // base_dir[..base_end] / remaining
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

/// Decode percent-encoded bytes in a URL path.
/// E.g. `"chapter%201.xhtml"` → `"chapter 1.xhtml"`.
///
/// Returns the input unchanged if no percent encoding is found.
fn percent_decode(input: &[u8]) -> Vec<u8> {
    // Fast path: no percent signs at all
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

// ── Extension detection ─────────────────────────────────────────

/// Check if a filename looks like an EPUB.
///
/// Handles both long filenames (`.epub`) and FAT 8.3 short names
/// where the 4-char extension is truncated to 3 (`.EPU`).
pub fn is_epub_filename(name: &str) -> bool {
    let b = name.as_bytes();

    // Check ".epub" (5 chars) — case-insensitive
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

    // Check ".epu" (4 chars) — FAT 8.3 truncation of ".epub"
    if b.len() >= 4 {
        let e = &b[b.len() - 4..];
        if e[0] == b'.' && (e[1] | 0x20) == b'e' && (e[2] | 0x20) == b'p' && (e[3] | 0x20) == b'u' {
            return true;
        }
    }

    false
}
