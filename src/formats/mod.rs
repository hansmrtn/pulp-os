// Document format support for the reader
//
// All parsing and decoding lives in the `smol-epub` crate.
// This module re-exports everything so the rest of pulp-os
// can continue to use `crate::formats::*` paths unchanged.

pub use smol_epub::cache;
pub use smol_epub::css;
pub use smol_epub::epub;
pub use smol_epub::html_strip;
pub use smol_epub::jpeg;
pub use smol_epub::png;
pub use smol_epub::xml;
pub use smol_epub::zip;
