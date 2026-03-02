# smol-epub

Minimal `no_std` EPUB parser with streaming decompression, HTML stripping,
CSS resolution, and optional 1-bit image decoders.

Designed for memory-constrained embedded targets (≥ 140 KB heap), but works
anywhere `alloc` is available.

## Features

| Module | Purpose |
|--------|---------|
| `zip` | ZIP central-directory parser, streaming DEFLATE extraction |
| `xml` | Minimal XML tag / attribute scanner (EPUB metadata) |
| `css` | CSS property parser for EPUB stylesheets |
| `epub` | EPUB structure: `container.xml` → OPF → spine / metadata / TOC |
| `html_strip` | Single-pass, streaming HTML-to-styled-text converter |
| `cache` | Chapter decompress-and-strip pipeline with cache metadata |
| `png` | PNG decoder → 1-bit Floyd–Steinberg dithered bitmap *(feature `images`)* |
| `jpeg` | JPEG decoder → 1-bit Floyd–Steinberg dithered bitmap *(feature `images`)* |

## Feature flags

| Flag | Default | Description |
|------|---------|-------------|
| `images` | ✓ | Enable `png` and `jpeg` image decoders |

## Quick start

```rust
use smol_epub::zip::{self, ZipIndex};
use smol_epub::epub::{self, EpubMeta, EpubSpine, EpubToc};

// 1. Build ZIP index from the EPUB file's central directory
let mut zip = ZipIndex::new();
let (cd_offset, cd_size) = ZipIndex::parse_eocd(&tail_buf, file_size)?;
// ... read the central directory bytes into `cd_buf` ...
zip.parse_central_directory(&cd_buf)?;

// 2. Parse EPUB structure
let container = zip::extract_entry(
    zip.entry(zip.find("META-INF/container.xml").unwrap()),
    zip.entry(zip.find("META-INF/container.xml").unwrap()).local_offset,
    |off, buf| read_fn(off, buf),
)?;
let mut opf_path = [0u8; epub::OPF_PATH_CAP];
let opf_len = epub::parse_container(&container, &mut opf_path)?;

// 3. Extract metadata and reading-order spine
let mut meta = EpubMeta::new();
let mut spine = EpubSpine::new();
epub::parse_opf(&opf_data, opf_dir, &zip, &mut meta, &mut spine)?;
println!("{} by {}", meta.title_str(), meta.author_str());

// 4. Optionally parse the table of contents
let mut toc = EpubToc::new();
if let Some(src) = epub::find_toc_source(&opf_data, opf_dir, &zip) {
    epub::parse_toc(src, &toc_data, toc_dir, &spine, &zip, &mut toc);
}

// 5. Stream-decompress + HTML-strip a chapter
let bytes_written = smol_epub::cache::stream_strip_entry(
    &entry, local_offset,
    |off, buf| read_fn(off, buf),     // read closure
    |chunk| { output.extend(chunk); Ok(()) },  // output closure
)?;
```

## Streaming I/O model

All functions that read from an external byte source accept a generic
closure:

```rust
FnMut(offset: u32, buf: &mut [u8]) -> Result<usize, E>
```

This works with SD cards, flash memory, `std::fs::File`, in-memory buffers,
or any other random-access byte store — the crate never assumes a specific
storage backend.

## Image decoders

The `png` and `jpeg` modules decode images to 1-bit monochrome bitmaps
using Floyd–Steinberg dithering, ideal for e-ink displays. Three decoder
variants are provided for each format:

| Function | Input |
|----------|-------|
| `decode_{png,jpeg}_fit` | In-memory `&[u8]` buffer |
| `decode_{png,jpeg}_streaming` | Stored (uncompressed) ZIP entry via read closure |
| `decode_{png,jpeg}_deflate_streaming` | DEFLATE-compressed ZIP entry via read closure |

All variants accept `max_w` / `max_h` parameters and integer-downscale
the image to fit.

## Memory budget

Typical peak heap usage on an embedded target:

| Operation | Peak heap |
|-----------|-----------|
| ZIP index parse | ~5 KB |
| Chapter stream-strip (DEFLATE) | ~51 KB |
| PNG streaming decode | ~90 KB |
| JPEG streaming decode | ~30 KB |
| JPEG DEFLATE streaming decode | ~79 KB |

Stack usage is kept low throughout; large structs like `DecompressorOxide`
(~11 KB) are always heap-allocated via `Box`.

## License

Licensed under either of

- [MIT license](http://opensource.org/licenses/MIT)
- [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)

at your option.