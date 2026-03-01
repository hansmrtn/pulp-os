// smol-epub: minimal no_std EPUB parser with streaming image decoders.
// zip:        ZIP central directory parser, streaming DEFLATE extraction
// xml:        minimal XML tag/attribute scanner for EPUB metadata
// css:        minimal CSS parser for EPUB stylesheet resolution
// epub:       EPUB structure parser (container.xml, OPF spine, TOC)
// html_strip: single-pass HTML to styled-text converter (streaming)
// cache:      EPUB chapter cache: streaming decompress + strip
// png:        PNG decoder, 1-bit Floyd-Steinberg dithered bitmap
// jpeg:       JPEG decoder, 1-bit Floyd-Steinberg dithered bitmap

#![no_std]

extern crate alloc;

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
