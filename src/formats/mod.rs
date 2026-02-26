// Document format support for the reader
//
// zip        — ZIP central directory parser, streaming DEFLATE extraction
// xml        — minimal XML tag/attribute scanner for EPUB metadata
// epub       — EPUB structure parser (container.xml, OPF spine)
// html_strip — single-pass HTML to plain text converter

pub mod epub;
pub mod html_strip;
pub mod xml;
pub mod zip;
