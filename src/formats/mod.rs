// Document format support for the reader
//
// zip        — ZIP central directory parser, streaming DEFLATE extraction
// xml        — minimal XML tag/attribute scanner for EPUB metadata
// epub       — EPUB structure parser (container.xml, OPF spine)
// html_strip — single-pass HTML to styled-text converter (in-place + streaming)
// cache      — EPUB chapter cache: streaming decompress + strip to SD
// css        — minimal CSS parser for EPUB stylesheet resolution

pub mod cache;
pub mod css;
pub mod epub;
pub mod html_strip;
pub mod jpeg;
pub mod png;
pub mod xml;
pub mod zip;
