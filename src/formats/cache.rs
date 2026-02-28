// EPUB chapter cache — streaming decompress + HTML strip
//
// Stream ZIP entries through DEFLATE + HTML stripper to plain text
// on SD. No persistent heap; ~51KB temporary per chapter (freed on
// return). Cache dir: _XXXXXXX/ with META.BIN + CHnnn.TXT files.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::formats::html_strip::HtmlStripStream;
use crate::formats::zip::{METHOD_DEFLATE, METHOD_STORED, ZipEntry, ZipIndex};

const CACHE_MAGIC: u32 = 0x504C_5043; // "PLPC"
const CACHE_VERSION: u8 = 1;
const META_HEADER: usize = 16;

pub const MAX_CACHE_CHAPTERS: usize = 256;
pub const META_MAX_SIZE: usize = META_HEADER + 4 * MAX_CACHE_CHAPTERS;

const WINDOW_SIZE: usize = 32768; // DEFLATE sliding window
const READ_BUF_SIZE: usize = 4096; // compressed read chunk
const STRIP_BUF_SIZE: usize = 4096; // strip output accumulator
const FLUSH_THRESHOLD: usize = STRIP_BUF_SIZE - 128;

#[inline]
pub fn fnv1a(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// 8.3 cache dir name: '_' + 7 hex digits of lower 28 bits
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

#[inline]
pub fn dir_name_str(buf: &[u8; 8]) -> &str {
    core::str::from_utf8(buf).unwrap_or("_0000000")
}

// 8.3 chapter filename: CH000.TXT .. CH255.TXT
pub fn chapter_file_name(idx: u16) -> [u8; 9] {
    let mut n = *b"CH000.TXT";
    n[2] = b'0' + ((idx / 100) % 10) as u8;
    n[3] = b'0' + ((idx / 10) % 10) as u8;
    n[4] = b'0' + (idx % 10) as u8;
    n
}

#[inline]
pub fn chapter_file_str(buf: &[u8; 9]) -> &str {
    core::str::from_utf8(buf).unwrap_or("CH000.TXT")
}

pub const META_FILE: &str = "META.BIN";

// encode cache metadata into buf; returns bytes written
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

// parse and validate META.BIN; write chapter sizes into caller's slice
pub fn parse_cache_meta(
    data: &[u8],
    epub_size: u32,
    name_hash: u32,
    expected_chapters: usize,
    chapter_sizes_out: &mut [u32],
) -> Result<usize, &'static str> {
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

    if chapter_sizes_out.len() < count {
        return Err("cache: output slice too small");
    }

    for i in 0..count {
        let off = META_HEADER + i * 4;
        chapter_sizes_out[i] =
            u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
    }

    Ok(count)
}

// stream-decompress ZIP entry, strip HTML, emit plain-text chunks; ~47KB temp heap
pub fn stream_strip_entry<E>(
    entry: &ZipEntry,
    local_offset: u32,
    mut read_fn: impl FnMut(u32, &mut [u8]) -> Result<usize, E>,
    mut output_fn: impl FnMut(&[u8]) -> Result<(), &'static str>,
) -> Result<u32, &'static str> {
    // skip past local file header to entry data
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

// stored entry: read raw, strip HTML, write via callback; stack only
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

// deflate entry: decompress in 32KB circular window, strip HTML; ~47KB temp heap
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

    // ~11KB DecompressorOxide; alloc zeroed directly — Box::new() overflows stack

    let decomp_ptr =
        unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<DecompressorOxide>()) };
    if decomp_ptr.is_null() {
        return Err("cache: OOM for decompressor");
    }
    let mut decomp = unsafe { Box::from_raw(decomp_ptr as *mut DecompressorOxide) };

    // 32KB circular dictionary window
    let mut window = Vec::new();
    window
        .try_reserve_exact(WINDOW_SIZE)
        .map_err(|_| "cache: OOM for window")?;
    window.resize(WINDOW_SIZE, 0);

    // 4KB compressed-data read buffer
    let mut rbuf = Vec::new();
    rbuf.try_reserve_exact(READ_BUF_SIZE)
        .map_err(|_| "cache: OOM for read buffer")?;
    rbuf.resize(READ_BUF_SIZE, 0);

    let mut stripper = HtmlStripStream::new();
    let mut strip_buf = [0u8; STRIP_BUF_SIZE];
    let mut strip_pos: usize = 0;
    let mut total_written: u32 = 0;

    let mut in_avail: usize = 0;
    let mut file_pos = data_offset;
    let mut comp_left = comp_size;
    let mut out_pos: usize = 0; // logical position in the circular window

    loop {
        // top up read buffer from SD
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

        // circular-buffer mode: do NOT set TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
        let flags = if comp_left > 0 {
            inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            0
        };

        let old_out_pos = out_pos;
        let (status, consumed, produced) =
            decompress(&mut decomp, &rbuf[..in_avail], &mut window, out_pos, flags);

        // feed new output to HTML stripper; always contiguous within window
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

        if consumed > 0 && consumed < in_avail {
            rbuf.copy_within(consumed..in_avail, 0);
        }
        in_avail -= consumed;

        match status {
            TINFLStatus::Done => break,

            TINFLStatus::HasMoreOutput => {
                // window full; reset to 0, data stays for back-references
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

    let trailing = stripper.finish(&mut strip_buf[strip_pos..]);
    strip_pos += trailing;
    if strip_pos > 0 {
        output_fn(&strip_buf[..strip_pos])?;
        total_written += strip_pos as u32;
    }

    Ok(total_written)
}

// feed input through stripper; flush to output_fn at FLUSH_THRESHOLD
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
            // no progress: flush if we have data, otherwise skip byte
            if *strip_pos > 0 {
                output_fn(&strip_buf[..*strip_pos])?;
                *total_written += *strip_pos as u32;
                *strip_pos = 0;
            } else {
                // safety net: advance to avoid infinite loop
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
