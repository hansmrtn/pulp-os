// ZIP central directory parser and entry extractor
//
// Designed for reading EPUB archives from SD card in a no_std
// environment.  The caller handles all I/O (via Services); this
// module operates purely on byte slices.
//
// Workflow:
//   1. Caller reads last ~512 bytes of file → parse_eocd()
//   2. Caller reads central directory bytes  → parse_central_directory()
//   3. Caller looks up entries by name       → find()
//   4. Caller reads local file header        → local_header_data_skip()
//   5. Caller reads entry data, optionally decompresses → extract_entry()
//
// Stack footprint: ZipIndex is ~5KB (256 entries inline, names on heap).
// Extraction uses streaming DEFLATE — only a 4KB read buffer on the
// stack plus the output Vec on the heap.  Compressed data is never
// held in RAM in full; it streams directly from SD through the
// decompressor.  All heap allocations use try_reserve so OOM produces
// a clean error instead of a panic.

use alloc::vec::Vec;

// ── ZIP signatures ──────────────────────────────────────────────

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

/// Compression: stored verbatim.
pub const METHOD_STORED: u16 = 0;
/// Compression: DEFLATE.
pub const METHOD_DEFLATE: u16 = 8;

// ── Little-endian helpers ───────────────────────────────────────

#[inline]
fn le_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

#[inline]
fn le_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

// ── ZipEntry ────────────────────────────────────────────────────

/// Compact metadata for one file inside the ZIP.
#[derive(Clone, Copy)]
pub struct ZipEntry {
    /// Byte offset into `ZipIndex::names` where this entry's name starts.
    pub name_start: u16,
    /// Length of the name in bytes.
    pub name_len: u16,
    /// Offset of the *local file header* in the ZIP file.
    pub local_offset: u32,
    /// Compressed size in bytes.
    pub comp_size: u32,
    /// Uncompressed size in bytes.
    pub uncomp_size: u32,
    /// Compression method (0 = stored, 8 = deflate).
    pub method: u16,
}

impl ZipEntry {
    const EMPTY: Self = Self {
        name_start: 0,
        name_len: 0,
        local_offset: 0,
        comp_size: 0,
        uncomp_size: 0,
        method: 0,
    };
}

// ── ZipIndex ────────────────────────────────────────────────────

/// Maximum number of entries we track.  Large technical EPUBs can
/// have 200+ files (chapters, images, CSS, fonts, metadata).
pub const MAX_ENTRIES: usize = 256;

/// In-memory index of a ZIP file's central directory.
///
/// Built from two caller-provided reads (EOCD tail + CD bytes).
/// After construction, supports O(n) lookup by filename.
///
/// The entry metadata array is inline (~5KB) but filenames are
/// stored in a heap-allocated `Vec` that grows during parsing,
/// keeping the stack footprint fixed regardless of path lengths.
pub struct ZipIndex {
    entries: [ZipEntry; MAX_ENTRIES],
    count: u16,
    /// Concatenated filenames for all entries.  Each entry's
    /// `name_start` / `name_len` indexes into this buffer.
    /// Heap-allocated, empty (zero bytes) until `parse_central_directory`.
    names: Vec<u8>,
}

impl ZipIndex {
    pub const fn new() -> Self {
        Self {
            entries: [ZipEntry::EMPTY; MAX_ENTRIES],
            count: 0,
            names: Vec::new(), // const — no allocation
        }
    }

    /// Reset to empty state, freeing the names heap allocation.
    pub fn clear(&mut self) {
        self.count = 0;
        self.names = Vec::new(); // drops + frees backing memory
    }

    // ── Phase 1: locate the central directory ───────────────────

    /// Parse the End-of-Central-Directory record from the tail of the
    /// file.
    ///
    /// `tail` should be the last 256–512 bytes of the ZIP file.
    /// `file_size` is the total file size (needed to validate offsets).
    ///
    /// Returns `(cd_offset, cd_size)` — the byte offset and size of
    /// the central directory within the ZIP file.
    pub fn parse_eocd(tail: &[u8], file_size: u32) -> Result<(u32, u32), &'static str> {
        if tail.len() < 22 {
            return Err("zip: tail too short for EOCD");
        }

        // Scan backwards for the EOCD signature
        let mut i = tail.len() - 22;
        loop {
            if le_u32(tail, i) == EOCD_SIG {
                break;
            }
            if i == 0 {
                return Err("zip: EOCD signature not found");
            }
            i -= 1;
        }

        let cd_size = le_u32(tail, i + 12);
        let cd_offset = le_u32(tail, i + 16);

        if cd_offset.saturating_add(cd_size) > file_size {
            return Err("zip: CD extends past EOF");
        }

        Ok((cd_offset, cd_size))
    }

    // ── Phase 2: parse central directory entries ────────────────

    /// Parse central directory bytes into the entry index.
    ///
    /// `cd` must contain the full central directory (read from the
    /// offset and size returned by `parse_eocd`).  Entries beyond
    /// `MAX_ENTRIES` are silently dropped.  Name allocation failures
    /// cause the individual entry to be skipped.
    pub fn parse_central_directory(&mut self, cd: &[u8]) -> Result<(), &'static str> {
        self.count = 0;
        self.names.clear();

        // Pre-allocate a reasonable guess for the names buffer.
        let _ = self.names.try_reserve(cd.len().min(8192));

        let mut pos = 0;

        while pos + 46 <= cd.len() {
            let sig = le_u32(cd, pos);
            if sig != CD_SIG {
                break;
            }

            let method = le_u16(cd, pos + 10);
            let comp_size = le_u32(cd, pos + 20);
            let uncomp_size = le_u32(cd, pos + 24);
            let name_len = le_u16(cd, pos + 28) as usize;
            let extra_len = le_u16(cd, pos + 30) as usize;
            let comment_len = le_u16(cd, pos + 32) as usize;
            let local_offset = le_u32(cd, pos + 42);

            let name_start_in_cd = pos + 46;
            let entry_end = name_start_in_cd + name_len + extra_len + comment_len;

            if entry_end > cd.len() {
                return Err("zip: CD entry extends past buffer");
            }

            let idx = self.count as usize;
            if idx < MAX_ENTRIES {
                let ns = self.names.len();

                // u16 offset — names buffer must stay under 64KB
                if ns + name_len <= u16::MAX as usize && self.names.try_reserve(name_len).is_ok() {
                    self.names
                        .extend_from_slice(&cd[name_start_in_cd..name_start_in_cd + name_len]);

                    self.entries[idx] = ZipEntry {
                        name_start: ns as u16,
                        name_len: name_len as u16,
                        local_offset,
                        comp_size,
                        uncomp_size,
                        method,
                    };
                    self.count += 1;
                }
                // If try_reserve or u16 check fails, skip entry silently
            }

            pos = entry_end;
        }

        if self.count == 0 {
            return Err("zip: no entries in CD");
        }

        Ok(())
    }

    // ── Accessors ───────────────────────────────────────────────

    /// Number of entries in the index.
    #[inline]
    pub fn count(&self) -> usize {
        self.count as usize
    }

    /// Get entry by index.
    ///
    /// # Panics
    /// Panics if `idx >= count()`.
    #[inline]
    pub fn entry(&self, idx: usize) -> &ZipEntry {
        assert!(idx < self.count as usize);
        &self.entries[idx]
    }

    /// Get the filename of entry `idx` as a UTF-8 string.
    pub fn entry_name(&self, idx: usize) -> &str {
        let e = self.entry(idx);
        let start = e.name_start as usize;
        let end = start + e.name_len as usize;
        core::str::from_utf8(&self.names[start..end]).unwrap_or("")
    }

    /// Find an entry by exact filename.  Returns the entry index.
    ///
    /// Case-sensitive linear scan (ZIP filenames in EPUBs are
    /// case-sensitive per the OCF spec).
    pub fn find(&self, name: &str) -> Option<usize> {
        let name_bytes = name.as_bytes();
        for i in 0..self.count as usize {
            let e = &self.entries[i];
            let start = e.name_start as usize;
            let end = start + e.name_len as usize;
            if &self.names[start..end] == name_bytes {
                return Some(i);
            }
        }
        None
    }

    /// Find an entry by case-insensitive filename match.
    /// Fallback when exact match fails (some EPUBs have
    /// inconsistent casing between the OPF manifest and actual paths).
    pub fn find_icase(&self, name: &str) -> Option<usize> {
        let target = name.as_bytes();
        for i in 0..self.count as usize {
            let e = &self.entries[i];
            let start = e.name_start as usize;
            let end = start + e.name_len as usize;
            let entry_name = &self.names[start..end];
            if entry_name.len() == target.len()
                && entry_name
                    .iter()
                    .zip(target.iter())
                    .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
            {
                return Some(i);
            }
        }
        None
    }

    // ── Phase 3: reading entry data ─────────────────────────────

    /// Compute the number of bytes to skip past the local file header
    /// to reach the actual file data.
    ///
    /// `header` must contain at least 30 bytes read from the entry's
    /// `local_offset`.  Returns the total header size
    /// (30 + name_len + extra_len).
    pub fn local_header_data_skip(header: &[u8]) -> Result<u32, &'static str> {
        if header.len() < 30 {
            return Err("zip: local header too short");
        }
        let sig = le_u32(header, 0);
        if sig != LOCAL_SIG {
            return Err("zip: bad local header signature");
        }
        let name_len = le_u16(header, 26) as u32;
        let extra_len = le_u16(header, 28) as u32;
        Ok(30 + name_len + extra_len)
    }
}

// ── Entry extraction (requires alloc) ───────────────────────────
//
// All extraction paths use `try_reserve` so OOM returns
// `Err("zip: ...")` instead of panicking.

/// Read and decompress a ZIP entry using caller-provided I/O.
///
/// `read_fn` takes `(offset: u32, buf: &mut [u8]) -> Result<usize, E>`
/// and reads from the underlying ZIP file on SD.
///
/// Returns a `Vec<u8>` with the uncompressed entry data.
pub fn extract_entry<E, F>(
    entry: &ZipEntry,
    local_offset: u32,
    mut read_fn: F,
) -> Result<Vec<u8>, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, E>,
{
    // Read local file header to find where data starts
    let mut header = [0u8; 30];
    read_fn(local_offset, &mut header).map_err(|_| "zip: read local header failed")?;
    let skip = ZipIndex::local_header_data_skip(&header)?;
    let data_offset = local_offset + skip;

    match entry.method {
        METHOD_STORED => extract_stored(entry, data_offset, &mut read_fn),
        METHOD_DEFLATE => extract_deflate(entry, data_offset, &mut read_fn),
        _ => Err("zip: unsupported compression method"),
    }
}

// ── STORED extraction ───────────────────────────────────────────

/// Extract a STORED (uncompressed) entry.
fn extract_stored<E, F>(
    entry: &ZipEntry,
    data_offset: u32,
    read_fn: &mut F,
) -> Result<Vec<u8>, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, E>,
{
    let size = entry.uncomp_size as usize;

    log::info!("zip: stored entry ({} bytes)", size);

    let mut out = Vec::new();
    out.try_reserve_exact(size)
        .map_err(|_| "zip: chapter too large for memory")?;
    out.resize(size, 0);
    read_all(data_offset, &mut out, read_fn)?;
    Ok(out)
}

// ── DEFLATE streaming extraction ────────────────────────────────
//
// Reads compressed data in 4KB chunks from SD and feeds them into
// the miniz_oxide streaming decompressor.  Only the output buffer
// (= uncompressed size) lives on the heap.  The compressed data is
// never held in RAM in full.
//
// Peak heap = uncomp_size.
// Peak stack ≈ 4KB (read buffer) + ~11KB (DecompressorOxide).

/// Size of the SD read buffer used during streaming decompression.
const DEFLATE_READ_BUF: usize = 4096;

/// Extract a DEFLATE-compressed entry via streaming decompression.
fn extract_deflate<E, F>(
    entry: &ZipEntry,
    data_offset: u32,
    read_fn: &mut F,
) -> Result<Vec<u8>, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, E>,
{
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::DecompressorOxide;
    use miniz_oxide::inflate::core::decompress;
    use miniz_oxide::inflate::core::inflate_flags;

    let comp_size = entry.comp_size as usize;
    let uncomp_size = entry.uncomp_size as usize;

    log::info!("zip: deflate stream {} -> {} bytes", comp_size, uncomp_size);

    // Pre-allocate the output buffer (only the uncompressed data).
    let mut output = Vec::new();
    output
        .try_reserve_exact(uncomp_size)
        .map_err(|_| "zip: chapter too large for memory")?;
    output.resize(uncomp_size, 0);

    // The DecompressorOxide is ~11KB (Huffman tables).  It lives on
    // the stack here — acceptable because we shaved 12KB off the
    // ZipIndex that used to be inline in ReaderApp.
    let mut decomp = DecompressorOxide::new();
    let mut out_pos: usize = 0;

    // Streaming read buffer — compressed data flows from SD through
    // here in 4KB chunks, never fully resident in RAM.
    let mut rbuf = [0u8; DEFLATE_READ_BUF];
    let mut in_avail: usize = 0; // valid bytes at front of rbuf
    let mut file_pos = data_offset;
    let mut comp_left = comp_size; // compressed bytes still on SD

    loop {
        // ── Top up the read buffer from SD ──────────────────────
        if in_avail < DEFLATE_READ_BUF && comp_left > 0 {
            let space = DEFLATE_READ_BUF - in_avail;
            let want = space.min(comp_left);
            match read_fn(file_pos, &mut rbuf[in_avail..in_avail + want]) {
                Ok(n) if n > 0 => {
                    file_pos += n as u32;
                    comp_left -= n;
                    in_avail += n;
                }
                Ok(_) => {
                    // SD returned 0 bytes — treat as end of compressed data
                    comp_left = 0;
                }
                Err(_) => return Err("zip: read failed during deflate"),
            }
        }

        // Nothing to feed and nothing produced yet — empty stream
        if in_avail == 0 && out_pos == 0 {
            return Err("zip: empty deflate stream");
        }

        // ── Decompress one step ─────────────────────────────────
        let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
            | if comp_left > 0 {
                inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
            } else {
                0
            };

        let (status, consumed, produced) =
            decompress(&mut decomp, &rbuf[..in_avail], &mut output, out_pos, flags);

        out_pos += produced;

        // Shift unconsumed input to front of buffer
        if consumed > 0 && consumed < in_avail {
            rbuf.copy_within(consumed..in_avail, 0);
        }
        in_avail -= consumed;

        // ── Handle decompressor status ──────────────────────────
        match status {
            TINFLStatus::Done => break,

            TINFLStatus::NeedsMoreInput => {
                if comp_left == 0 && in_avail == 0 {
                    // No more data anywhere — truncated stream
                    return Err("zip: truncated deflate stream");
                }
                if consumed == 0 && produced == 0 {
                    // Decompressor made no progress
                    if in_avail >= DEFLATE_READ_BUF {
                        // Buffer is full and nothing was consumed — stuck
                        return Err("zip: deflate stream stuck");
                    }
                    // Otherwise: buffer is partially full, will be
                    // topped up on next iteration.
                }
            }

            TINFLStatus::HasMoreOutput => {
                // Output buffer is full but decompressor has more data.
                // The ZIP header's uncompressed size was too small.
                return Err("zip: deflate output exceeds declared size");
            }

            // All negative statuses are errors
            _ => return Err("zip: deflate decompression error"),
        }
    }

    output.truncate(out_pos);
    Ok(output)
}

// ── Shared helpers ──────────────────────────────────────────────

/// Read exactly `buf.len()` bytes from the given offset, using
/// multiple calls to `read_fn` if needed.
fn read_all<E, F>(offset: u32, buf: &mut [u8], read_fn: &mut F) -> Result<(), &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, E>,
{
    let mut total = 0usize;
    while total < buf.len() {
        let n =
            read_fn(offset + total as u32, &mut buf[total..]).map_err(|_| "zip: read failed")?;
        if n == 0 {
            return Err("zip: unexpected EOF");
        }
        total += n;
    }
    Ok(())
}
