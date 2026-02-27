// ZIP central directory parser and streaming entry extraction
//
// Reads EPUB archives from SD without holding compressed data in RAM.
// Caller provides all I/O; this module operates on byte slices.
//
// Workflow: parse_eocd → parse_central_directory → find → extract_entry
//
// ZipIndex is ~5KB inline (256 entries); names are heap-allocated
// during parsing and freed on clear(). Streaming DEFLATE reads
// compressed data in 4KB chunks from SD through the decompressor;
// only the output buffer (uncompressed size) lives on the heap.
// All allocations use try_reserve for graceful OOM.

use alloc::vec::Vec;

// refuse to extract entries larger than this to avoid OOM
const MAX_ENTRY_SIZE: u32 = 192 * 1024;

const EOCD_SIG: u32 = 0x0605_4b50;
const CD_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;

pub const METHOD_STORED: u16 = 0;
pub const METHOD_DEFLATE: u16 = 8;

#[inline]
fn le_u16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

#[inline]
fn le_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

#[derive(Clone, Copy)]
pub struct ZipEntry {
    pub name_start: u16,
    pub name_len: u16,
    pub local_offset: u32,
    pub comp_size: u32,
    pub uncomp_size: u32,
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

pub const MAX_ENTRIES: usize = 256;

pub struct ZipIndex {
    entries: [ZipEntry; MAX_ENTRIES],
    count: u16,
    names: Vec<u8>,
}

impl Default for ZipIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ZipIndex {
    pub const fn new() -> Self {
        Self {
            entries: [ZipEntry::EMPTY; MAX_ENTRIES],
            count: 0,
            names: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.names = Vec::new();
    }

    // parse EOCD from the tail of the file; returns (cd_offset, cd_size)
    pub fn parse_eocd(tail: &[u8], file_size: u32) -> Result<(u32, u32), &'static str> {
        if tail.len() < 22 {
            return Err("zip: tail too short for EOCD");
        }

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

    // parse central directory bytes into the entry index
    pub fn parse_central_directory(&mut self, cd: &[u8]) -> Result<(), &'static str> {
        self.count = 0;
        self.names.clear();
        let _ = self.names.try_reserve(cd.len().min(8192));

        let mut pos = 0;

        while pos + 46 <= cd.len() {
            if le_u32(cd, pos) != CD_SIG {
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
            }

            pos = entry_end;
        }

        if self.count == 0 {
            return Err("zip: no entries in CD");
        }

        Ok(())
    }

    #[inline]
    pub fn count(&self) -> usize {
        self.count as usize
    }

    #[inline]
    pub fn entry(&self, idx: usize) -> &ZipEntry {
        assert!(idx < self.count as usize);
        &self.entries[idx]
    }

    pub fn entry_name(&self, idx: usize) -> &str {
        let e = self.entry(idx);
        let start = e.name_start as usize;
        let end = start + e.name_len as usize;
        core::str::from_utf8(&self.names[start..end]).unwrap_or("")
    }

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

    pub fn find_icase(&self, name: &str) -> Option<usize> {
        let target = name.as_bytes();
        for i in 0..self.count as usize {
            let e = &self.entries[i];
            let start = e.name_start as usize;
            let end = start + e.name_len as usize;
            let entry_name = &self.names[start..end];
            if entry_name.eq_ignore_ascii_case(target) {
                return Some(i);
            }
        }
        None
    }

    // bytes to skip past a local file header to reach entry data
    pub fn local_header_data_skip(header: &[u8]) -> Result<u32, &'static str> {
        if header.len() < 30 {
            return Err("zip: local header too short");
        }
        if le_u32(header, 0) != LOCAL_SIG {
            return Err("zip: bad local header signature");
        }
        let name_len = le_u16(header, 26) as u32;
        let extra_len = le_u16(header, 28) as u32;
        Ok(30 + name_len + extra_len)
    }
}

// -- entry extraction (requires alloc) --

pub fn extract_entry<E, F>(
    entry: &ZipEntry,
    local_offset: u32,
    mut read_fn: F,
) -> Result<Vec<u8>, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, E>,
{
    let mut header = [0u8; 30];
    read_fn(local_offset, &mut header).map_err(|_| "zip: read local header failed")?;
    let skip = ZipIndex::local_header_data_skip(&header)?;
    let data_offset = local_offset + skip;

    if entry.uncomp_size > MAX_ENTRY_SIZE {
        return Err("zip: entry too large");
    }

    match entry.method {
        METHOD_STORED => extract_stored(entry, data_offset, &mut read_fn),
        METHOD_DEFLATE => extract_deflate(entry, data_offset, &mut read_fn),
        _ => Err("zip: unsupported compression method"),
    }
}

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

const DEFLATE_READ_BUF: usize = 4096;

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

    let mut output = Vec::new();
    output
        .try_reserve_exact(uncomp_size)
        .map_err(|_| "zip: chapter too large for memory")?;
    output.resize(uncomp_size, 0);

    let mut decomp = DecompressorOxide::new();
    let mut out_pos: usize = 0;

    let mut rbuf = [0u8; DEFLATE_READ_BUF];
    let mut in_avail: usize = 0;
    let mut file_pos = data_offset;
    let mut comp_left = comp_size;

    loop {
        // top up read buffer from SD
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
                    comp_left = 0;
                }
                Err(_) => return Err("zip: read failed during deflate"),
            }
        }

        if in_avail == 0 && out_pos == 0 {
            return Err("zip: empty deflate stream");
        }

        let flags = inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF
            | if comp_left > 0 {
                inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
            } else {
                0
            };

        let (status, consumed, produced) =
            decompress(&mut decomp, &rbuf[..in_avail], &mut output, out_pos, flags);

        out_pos += produced;

        if consumed > 0 && consumed < in_avail {
            rbuf.copy_within(consumed..in_avail, 0);
        }
        in_avail -= consumed;

        match status {
            TINFLStatus::Done => break,
            TINFLStatus::NeedsMoreInput => {
                if comp_left == 0 && in_avail == 0 {
                    return Err("zip: truncated deflate stream");
                }
                if consumed == 0 && produced == 0 && in_avail >= DEFLATE_READ_BUF {
                    return Err("zip: deflate stream stuck");
                }
            }
            TINFLStatus::HasMoreOutput => {
                return Err("zip: deflate output exceeds declared size");
            }
            _ => return Err("zip: deflate decompression error"),
        }
    }

    output.truncate(out_pos);
    Ok(output)
}

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
