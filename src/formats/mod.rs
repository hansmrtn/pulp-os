// Document format support for the reader.
// All parsing and decoding lives in the smol-epub crate.
// Re-exports everything so pulp-os can use crate::formats::* paths unchanged.

pub use smol_epub::cache;
pub use smol_epub::css;
pub use smol_epub::epub;
pub use smol_epub::html_strip;
pub use smol_epub::jpeg;
pub use smol_epub::png;
pub use smol_epub::xml;
pub use smol_epub::zip;
