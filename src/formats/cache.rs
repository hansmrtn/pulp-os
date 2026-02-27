// EPUB chapter cache — streaming decompress + HTML strip
//
// Streams ZIP entry data through a DEFLATE decompressor and an HTML
// stripper, producing plain text chunks via a caller-provided callback.
// No persistent heap allocation — the decompressor and its 32 KB
// dictionary window are temporary and freed when the function returns.
//
// Cache layout (per book, in a subdirectory of root):
//
//   _XXXXXXX/          8.3 dir name: '_' + 7 hex chars of FNV-1a hash
//     META.BIN          validation header + per-chapter text sizes
//     CH000.TXT         stripped plain text, chapter 0
//     CH001.TXT         stripped plain text, chapter 1
//     ...
//
// META.BIN format (little-endian):
//
//   [0..4)   magic:   0x504C5043 ("PLPC")
//   [4)      version: 1
//   [5)      chapter_count: 0–255
//   [6..8)   reserved: 0
//   [8..12)  epub_file_size: u32
//   [12..16) epub_name_hash: u32
//   [16..)   chapter_sizes: [u32; chapter_count]
//
// Memory during cache building (per chapter):
//
//   ~11 KB heap  DecompressorOxide   (freed on return)
//    32 KB heap  DEFLATE window       (freed on return)
//     4 KB heap  compressed read buf  (freed on return)
//     4 KB stack strip output buf
//    ~64 B stack HtmlStripStream state
//   ─────────
//   ~51 KB total (temporary)
//
// After caching: 0 bytes of heap.  Pages are read directly from the
// cached plain-text files on SD.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::formats::html_strip::HtmlStripStream;
use crate::formats::zip::{METHOD_DEFLATE, METHOD_STORED, ZipEntry, ZipIndex};

// ── Cache metadata constants ──────────────────────────────────────────────

const CACHE_MAGIC: u32 = 0x504C_5043; // "PLPC"
const CACHE_VERSION: u8 = 1;
const META_HEADER: usize = 16;

/// Maximum chapters in a single cached EPUB.
pub const MAX_CACHE_CHAPTERS: usize = 256;

/// Maximum encoded size of a META.BIN file.
pub const META_MAX_SIZE: usize = META_HEADER + 4 * MAX_CACHE_CHAPTERS;

// ── Streaming decompression constants ─────────────────────────────────────

/// DEFLATE sliding window — must be a power of two and ≥ 32 768.
///
/// miniz_oxide in wrapping mode uses this as a circular dictionary.
/// The decompressor returns `HasMoreOutput` when `out_pos` reaches
/// `WINDOW_SIZE`, at which point the caller processes the output and
/// resets `out_pos` to 0.  Back-references are resolved through the
/// window via bitmasking, so old data remains valid.
const WINDOW_SIZE: usize = 32768;

/// Compressed data read chunk size (from SD card).
const READ_BUF_SIZE: usize = 4096;

/// HTML stripper output accumulator.  Flushed to the output callback
/// when it reaches `FLUSH_THRESHOLD` bytes.
const STRIP_BUF_SIZE: usize = 4096;
const FLUSH_THRESHOLD: usize = STRIP_BUF_SIZE - 128;

// ── Public types ──────────────────────────────────────────────────────────

/// Validated cache metadata read from META.BIN.
pub struct CacheInfo {
    pub chapter_count: usize,
    pub chapter_sizes: [u32; MAX_CACHE_CHAPTERS],
}

// ── Hash function ─────────────────────────────────────────────────────────

/// FNV-1a 32-bit hash (same algorithm as the bookmark hash in reader.rs).
#[inline]
pub fn fnv1a(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ── Naming helpers ────────────────────────────────────────────────────────

/// Compute the 8.3 cache directory name from a filename hash.
///
/// Returns an 8-byte ASCII array: `_` + 7 uppercase hex digits of
/// the lower 28 bits of `name_hash`.  Example: `_A1B2C3D`.
pub fn dir_name_for_hash(name_hash: u32) -> [u8; 8] {
    let h = name_hash & 0x0FFF_FFFF;
    let mut buf = [0u8; 8];
    buf[0] = b'_';
    for i in 0..7 {
        let nibble = ((h >> (24 - i * 4)) & 0xF) as u8;
        buf[1 + i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    buf
}

/// Convert an 8-byte directory name to a `&str`.
///
/// Always succeeds because `dir_name_for_hash` produces valid ASCII.
#[inline]
pub fn dir_name_str(buf: &[u8; 8]) -> &str {
    // Safety: dir_name_for_hash only produces ASCII bytes.
    core::str::from_utf8(buf).unwrap_or("_0000000")
}

/// Compute the 8.3 filename for a cached chapter.
///
/// Returns a 9-byte ASCII array like `CH000.TXT`.  Valid for indices
/// 0–255 (three decimal digits suffice for MAX_CACHE_CHAPTERS = 256).
pub fn chapter_file_name(idx: u16) -> [u8; 9] {
    let mut n = *b"CH000.TXT";
    n[2] = b'0' + ((idx / 100) % 10) as u8;
    n[3] = b'0' + ((idx / 10) % 10) as u8;
    n[4] = b'0' + (idx % 10) as u8;
    n
}

/// Convert a 9-byte chapter filename to a `&str`.
#[inline]
pub fn chapter_file_str(buf: &[u8; 9]) -> &str {
    core::str::from_utf8(buf).unwrap_or("CH000.TXT")
}

/// META.BIN filename constant.
pub const META_FILE: &str = "META.BIN";

// ── Meta encoding / parsing ───────────────────────────────────────────────

/// Encode cache metadata into `buf`.
///
/// Returns the number of bytes written.  The caller should provide a
/// buffer of at least `META_HEADER + 4 * chapter_sizes.len()` bytes
/// (or use `META_MAX_SIZE` for the upper bound).
pub fn encode_cache_meta(
    epub_size: u32,
    name_hash: u32,
    chapter_sizes: &[u32],
    buf: &mut [u8],
) -> usize {
    let count = chapter_sizes.len().min(MAX_CACHE_CHAPTERS);
    let total = META_HEADER + count * 4;
    debug_assert!(
        buf.len() >= total,
        "meta buffer too small: {} < {}",
        buf.len(),
        total
    );

    buf[0..4].copy_from_slice(&CACHE_MAGIC.to_le_bytes());
    buf[4] = CACHE_VERSION;
    buf[5] = count as u8;
    buf[6] = 0;
    buf[7] = 0;
    buf[8..12].copy_from_slice(&epub_size.to_le_bytes());
    buf[12..16].copy_from_slice(&name_hash.to_le_bytes());

    for (i, &size) in chapter_sizes.iter().enumerate().take(count) {
        let off = META_HEADER + i * 4;
        buf[off..off + 4].copy_from_slice(&size.to_le_bytes());
    }

    total
}

/// Parse and validate a META.BIN file.
///
/// `data` is the raw bytes of META.BIN (at least `META_HEADER` bytes).
/// `epub_size` and `name_hash` are checked against the stored values.
/// `expected_chapters` is the spine length from the current OPF parse;
/// a mismatch means the cache is stale.
///
/// Returns `CacheInfo` with chapter count and per-chapter text sizes
/// on success, or a diagnostic error string on failure.
pub fn parse_cache_meta(
    data: &[u8],
    epub_size: u32,
    name_hash: u32,
    expected_chapters: usize,
) -> Result<CacheInfo, &'static str> {
    if data.len() < META_HEADER {
        return Err("cache: meta too short");
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != CACHE_MAGIC {
        return Err("cache: bad magic");
    }

    if data[4] != CACHE_VERSION {
        return Err("cache: version mismatch");
    }

    let stored_size = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
    let stored_hash = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);

    if stored_size != epub_size {
        return Err("cache: epub size changed");
    }
    if stored_hash != name_hash {
        return Err("cache: epub hash changed");
    }

    let count = data[5] as usize;
    if count != expected_chapters {
        return Err("cache: chapter count mismatch");
    }

    let needed = META_HEADER + count * 4;
    if data.len() < needed {
        return Err("cache: meta truncated");
    }

    let mut info = CacheInfo {
        chapter_count: count,
        chapter_sizes: [0u32; MAX_CACHE_CHAPTERS],
    };

    for (i, size) in info.chapter_sizes.iter_mut().enumerate().take(count) {
        let off = META_HEADER + i * 4;
        *size = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    }

    Ok(info)
}

// ── Streaming entry extraction ────────────────────────────────────────────

/// Stream-decompress a ZIP entry, strip HTML, and emit plain-text chunks.
///
/// Reads compressed data from the EPUB via `read_fn(file_offset, buf)`,
/// decompresses it, runs the HTML stripper, and delivers stripped text
/// to `output_fn(chunk)` in roughly 4 KB pieces.
///
/// Returns the total number of stripped text bytes produced.
///
/// # Memory
///
/// DEFLATE entries use ~47 KB of temporary heap (DecompressorOxide +
/// 32 KB window + 4 KB read buffer).  STORED entries use only ~4 KB
/// of stack.  All heap allocations are freed before this returns.
pub fn stream_strip_entry<E>(
    entry: &ZipEntry,
    local_offset: u32,
    mut read_fn: impl FnMut(u32, &mut [u8]) -> Result<usize, E>,
    mut output_fn: impl FnMut(&[u8]) -> Result<(), &'static str>,
) -> Result<u32, &'static str> {
    // Read local file header to determine where entry data begins.
    let mut header = [0u8; 30];
    read_fn(local_offset, &mut header).map_err(|_| "cache: read local header failed")?;
    let skip = ZipIndex::local_header_data_skip(&header)?;
    let data_offset = local_offset + skip;

    match entry.method {
        METHOD_STORED => stream_stored(entry, data_offset, &mut read_fn, &mut output_fn),
        METHOD_DEFLATE => stream_deflate(entry, data_offset, &mut read_fn, &mut output_fn),
        _ => Err("cache: unsupported compression method"),
    }
}

// ── STORED entries ────────────────────────────────────────────────────────

/// Stream a STORED (uncompressed) ZIP entry through the HTML stripper.
///
/// No decompression needed — reads raw bytes from SD, strips HTML,
/// writes stripped text via the output callback.  Stack only, no heap.
fn stream_stored<E>(
    entry: &ZipEntry,
    data_offset: u32,
    read_fn: &mut impl FnMut(u32, &mut [u8]) -> Result<usize, E>,
    output_fn: &mut impl FnMut(&[u8]) -> Result<(), &'static str>,
) -> Result<u32, &'static str> {
    let mut stripper = HtmlStripStream::new();
    let mut read_buf = [0u8; READ_BUF_SIZE];
    let mut strip_buf = [0u8; STRIP_BUF_SIZE];
    let mut strip_pos: usize = 0;
    let mut total_written: u32 = 0;

    let size = entry.uncomp_size;
    let mut file_pos = data_offset;
    let mut remaining = size;

    log::info!("cache: streaming stored entry ({} bytes)", size);

    while remaining > 0 {
        let want = (remaining as usize).min(READ_BUF_SIZE);
        let n =
            read_fn(file_pos, &mut read_buf[..want]).map_err(|_| "cache: read failed (stored)")?;
        if n == 0 {
            return Err("cache: unexpected EOF in stored entry");
        }
        file_pos += n as u32;
        remaining -= n as u32;

        feed_and_flush(
            &mut stripper,
            &read_buf[..n],
            &mut strip_buf,
            &mut strip_pos,
            &mut total_written,
            output_fn,
        )?;
    }

    // Flush any trailing stripper state (deferred newlines, etc.)
    let trailing = stripper.finish(&mut strip_buf[strip_pos..]);
    strip_pos += trailing;
    if strip_pos > 0 {
        output_fn(&strip_buf[..strip_pos])?;
        total_written += strip_pos as u32;
    }

    Ok(total_written)
}

// ── DEFLATE entries ───────────────────────────────────────────────────────

/// Stream a DEFLATE-compressed ZIP entry through the HTML stripper.
///
/// Uses miniz_oxide in circular-buffer (wrapping) mode with a 32 KB
/// dictionary window.  The decompressor returns `HasMoreOutput` when
/// `out_pos` reaches `WINDOW_SIZE`; the caller processes the output
/// and resets `out_pos` to 0.  Back-references remain valid because
/// the window data is never cleared — only overwritten naturally as
/// the circular position advances.
///
/// Temporary heap: ~47 KB (DecompressorOxide + window + read buffer).
fn stream_deflate<E>(
    entry: &ZipEntry,
    data_offset: u32,
    read_fn: &mut impl FnMut(u32, &mut [u8]) -> Result<usize, E>,
    output_fn: &mut impl FnMut(&[u8]) -> Result<(), &'static str>,
) -> Result<u32, &'static str> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress, inflate_flags};

    let comp_size = entry.comp_size as usize;
    let uncomp_size = entry.uncomp_size;

    log::info!(
        "cache: streaming deflate {} -> {} bytes",
        comp_size,
        uncomp_size
    );

    // ── Heap allocations ──────────────────────────────────────────
    //
    // DecompressorOxide is ~11 KB (Huffman tables, state machine).
    // Box::new() would construct on the stack first and memcpy,
    // overflowing the ESP32-C3 stack.  Allocate zeroed directly.
    // Safety: DecompressorOxide::default() is all-zeros.

    let decomp_ptr =
        unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<DecompressorOxide>()) };
    if decomp_ptr.is_null() {
        return Err("cache: OOM for decompressor");
    }
    let mut decomp = unsafe { Box::from_raw(decomp_ptr as *mut DecompressorOxide) };

    // 32 KB circular dictionary window.
    let mut window = Vec::new();
    window
        .try_reserve_exact(WINDOW_SIZE)
        .map_err(|_| "cache: OOM for window")?;
    window.resize(WINDOW_SIZE, 0);

    // 4 KB compressed-data read buffer.
    let mut rbuf = Vec::new();
    rbuf.try_reserve_exact(READ_BUF_SIZE)
        .map_err(|_| "cache: OOM for read buffer")?;
    rbuf.resize(READ_BUF_SIZE, 0);

    // ── Stack allocations ─────────────────────────────────────────

    let mut stripper = HtmlStripStream::new();
    let mut strip_buf = [0u8; STRIP_BUF_SIZE];
    let mut strip_pos: usize = 0;
    let mut total_written: u32 = 0;

    let mut in_avail: usize = 0;
    let mut file_pos = data_offset;
    let mut comp_left = comp_size;
    let mut out_pos: usize = 0; // logical position in the circular window

    loop {
        // ── Top up read buffer from SD ────────────────────────────
        if in_avail < READ_BUF_SIZE && comp_left > 0 {
            let space = READ_BUF_SIZE - in_avail;
            let want = space.min(comp_left);
            match read_fn(file_pos, &mut rbuf[in_avail..in_avail + want]) {
                Ok(n) if n > 0 => {
                    file_pos += n as u32;
                    comp_left -= n;
                    in_avail += n;
                }
                Ok(_) => {
                    comp_left = 0;
                }
                Err(_) => return Err("cache: SD read failed during deflate"),
            }
        }

        if in_avail == 0 && out_pos == 0 {
            return Err("cache: empty deflate stream");
        }

        // ── Decompress ────────────────────────────────────────────
        //
        // Do NOT set TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF.
        // This enables circular-buffer mode: the window wraps at
        // WINDOW_SIZE and the decompressor returns HasMoreOutput
        // when out_pos reaches the buffer boundary.

        let flags = if comp_left > 0 {
            inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            0
        };

        let old_out_pos = out_pos;
        let (status, consumed, produced) =
            decompress(&mut decomp, &rbuf[..in_avail], &mut window, out_pos, flags);

        // ── Feed new output to the HTML stripper ──────────────────
        //
        // In wrapping mode, decompress stops at the WINDOW_SIZE
        // boundary, so `old_out_pos + produced <= WINDOW_SIZE`.
        // The output is always a contiguous slice within the window.

        if produced > 0 {
            let end = old_out_pos + produced;
            debug_assert!(
                end <= WINDOW_SIZE,
                "deflate produced past window boundary: {} > {}",
                end,
                WINDOW_SIZE
            );

            feed_and_flush(
                &mut stripper,
                &window[old_out_pos..end],
                &mut strip_buf,
                &mut strip_pos,
                &mut total_written,
                output_fn,
            )?;
        }

        out_pos += produced;

        // ── Shift remaining compressed input ──────────────────────

        if consumed > 0 && consumed < in_avail {
            rbuf.copy_within(consumed..in_avail, 0);
        }
        in_avail -= consumed;

        // ── Handle status ─────────────────────────────────────────

        match status {
            TINFLStatus::Done => break,

            TINFLStatus::HasMoreOutput => {
                // Window is full — the decompressor stopped at
                // out_pos == WINDOW_SIZE.  Reset to 0 so the next
                // call starts writing at the beginning of the
                // circular buffer.  The window data stays intact
                // for back-reference resolution.
                out_pos = 0;
            }

            TINFLStatus::NeedsMoreInput => {
                if comp_left == 0 && in_avail == 0 {
                    return Err("cache: truncated deflate stream");
                }
                if consumed == 0 && produced == 0 && in_avail >= READ_BUF_SIZE {
                    return Err("cache: deflate stream stuck");
                }
            }

            _ => return Err("cache: deflate decompression error"),
        }
    }

    // ── Flush remaining stripped text ──────────────────────────────

    let trailing = stripper.finish(&mut strip_buf[strip_pos..]);
    strip_pos += trailing;
    if strip_pos > 0 {
        output_fn(&strip_buf[..strip_pos])?;
        total_written += strip_pos as u32;
    }

    // decomp, window, rbuf dropped here → heap freed
    Ok(total_written)
}

// ── Stripper feed + flush helper ──────────────────────────────────────────

/// Feed `input` bytes through the HTML stripper, accumulating output in
/// `strip_buf`.  When the buffer reaches `FLUSH_THRESHOLD`, it is flushed
/// to `output_fn` and reset.
///
/// This drives the stripper to completion for the given input: it loops
/// until all bytes are consumed, flushing as needed when the output
/// buffer fills up.
fn feed_and_flush(
    stripper: &mut HtmlStripStream,
    input: &[u8],
    strip_buf: &mut [u8; STRIP_BUF_SIZE],
    strip_pos: &mut usize,
    total_written: &mut u32,
    output_fn: &mut impl FnMut(&[u8]) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let mut ip: usize = 0;

    while ip < input.len() {
        let avail_out = STRIP_BUF_SIZE - *strip_pos;
        if avail_out == 0 {
            // Output buffer completely full — flush before continuing.
            output_fn(&strip_buf[..*strip_pos])?;
            *total_written += *strip_pos as u32;
            *strip_pos = 0;
            continue;
        }

        let (consumed, written) = stripper.feed(
            &input[ip..],
            &mut strip_buf[*strip_pos..*strip_pos + avail_out],
        );
        ip += consumed;
        *strip_pos += written;

        if consumed == 0 && written == 0 {
            // No progress.  If the strip buffer has data, flush it to
            // make room; otherwise the input byte genuinely produced
            // no output (e.g. a whitespace-only tag).
            if *strip_pos > 0 {
                output_fn(&strip_buf[..*strip_pos])?;
                *total_written += *strip_pos as u32;
                *strip_pos = 0;
            } else {
                // Stripper consumed nothing and produced nothing with
                // an empty output buffer.  This can happen if the input
                // byte is inside a tag or entity that needs more bytes.
                // Since we always pass a non-zero output slice above,
                // this means the stripper is stuck waiting for more
                // input context.  Advance ip to avoid an infinite loop.
                //
                // In practice this should not happen because the
                // stripper always consumes at least one input byte when
                // given output space.  Safety net only.
                ip += 1;
            }
            continue;
        }

        // Flush when we have accumulated a good-sized chunk.
        if *strip_pos >= FLUSH_THRESHOLD {
            output_fn(&strip_buf[..*strip_pos])?;
            *total_written += *strip_pos as u32;
            *strip_pos = 0;
        }
    }

    Ok(())
}
