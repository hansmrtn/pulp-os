// Document format support for the reader
//
// zip        — streaming ZIP central directory parser + entry extraction
// xml        — minimal XML tag/attribute scanner for EPUB metadata
// epub       — EPUB structure parser (container.xml, OPF spine)
// html_strip — single-pass HTML to plain text converter

pub mod epub;
pub mod html_strip;
pub mod xml;
pub mod zip;
