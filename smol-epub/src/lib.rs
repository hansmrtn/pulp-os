//! # smol-epub
//!
//! Minimal `no_std` EPUB parser with streaming decompression, HTML
//! stripping, CSS resolution, and optional 1-bit image decoders.
//!
//! Designed for memory-constrained embedded targets (≥ 140 KB heap),
//! but works anywhere `alloc` is available.
//!
//! ## Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`zip`] | ZIP central-directory parser, streaming DEFLATE extraction |
//! | [`xml`] | Minimal XML tag / attribute scanner (EPUB metadata) |
//! | [`css`] | CSS property parser for EPUB stylesheets |
//! | [`epub`] | EPUB structure: `container.xml` → OPF → spine / metadata / TOC |
//! | [`html_strip`] | Single-pass, streaming HTML-to-styled-text converter |
//! | [`cache`] | Chapter decompress-and-strip pipeline with cache metadata |
//! | [`png`] | PNG decoder → 1-bit Floyd–Steinberg dithered bitmap *(feature `images`)* |
//! | [`jpeg`] | JPEG decoder → 1-bit Floyd–Steinberg dithered bitmap *(feature `images`)* |
//!
//! ## Feature flags
//!
//! | Flag | Default | Description |
//! |------|---------|-------------|
//! | `images` | ✓ | Enable [`png`] and [`jpeg`] image decoders |
//!
//! ## Streaming I/O model
//!
//! Functions that read from an external byte source accept a generic
//! closure with signature:
//!
//! ```text
//! FnMut(offset: u32, buf: &mut [u8]) -> Result<usize, E>
//! ```
//!
//! This works with SD cards, flash, `std::fs::File`, in-memory
//! buffers, or any other random-access byte store.
//!
//! ## Quick start
//!
//! ```rust,ignore
//! use smol_epub::zip::ZipIndex;
//! use smol_epub::epub::{self, EpubMeta, EpubSpine, EpubToc};
//!
//! // 1. Build ZIP index from the file's central directory
//! let mut zip = ZipIndex::new();
//! // ... parse_eocd, read CD, parse_central_directory ...
//!
//! // 2. Parse EPUB structure
//! let container = smol_epub::zip::extract_entry(/* ... */)?;
//! let mut opf_path = [0u8; epub::OPF_PATH_CAP];
//! let opf_len = epub::parse_container(&container, &mut opf_path)?;
//!
//! // 3. Extract metadata and reading-order spine
//! let mut meta = EpubMeta::new();
//! let mut spine = EpubSpine::new();
//! epub::parse_opf(&opf_data, opf_dir, &zip, &mut meta, &mut spine)?;
//!
//! // 4. Optionally parse the table of contents
//! let mut toc = EpubToc::new();
//! if let Some(src) = epub::find_toc_source(&opf_data, opf_dir, &zip) {
//!     epub::parse_toc(src, &toc_data, toc_dir, &spine, &zip, &mut toc);
//! }
//!
//! // 5. Stream-decompress + HTML-strip chapters via cache module
//! let bytes_written = smol_epub::cache::stream_strip_entry(
//!     &entry, local_offset, read_fn, output_fn,
//! )?;
//! ```

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

use alloc::vec::Vec;

// ── public modules ──────────────────────────────────────────────────

pub mod cache;
pub mod css;
pub mod epub;
pub mod html_strip;
pub mod xml;
pub mod zip;

#[cfg(feature = "images")]
pub mod jpeg;
#[cfg(feature = "images")]
pub mod png;

// ── shared types ────────────────────────────────────────────────────

/// A decoded 1-bit monochrome image, packed MSB-first, row-major.
///
/// A **set** bit (1) represents black (ink); a **clear** bit (0) represents
/// white (paper). This convention matches most e-ink controllers directly.
///
/// Produced by the [`png`] and [`jpeg`] decoders when the `images`
/// feature is enabled.
///
/// # Layout
///
/// ```text
/// stride = ceil(width / 8)   bytes per row
/// data.len() == stride * height
/// ```
///
/// Pixel (x, y) is bit `(7 - x % 8)` of byte `data[y * stride + x / 8]`.
#[derive(Clone)]
pub struct DecodedImage {
    /// Image width in pixels.
    pub width: u16,
    /// Image height in pixels.
    pub height: u16,
    /// Packed 1-bit pixel data, `stride * height` bytes.
    pub data: Vec<u8>,
    /// Bytes per row (`ceil(width / 8)`).
    pub stride: usize,
}

impl core::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("data_len", &self.data.len())
            .finish()
    }
}
