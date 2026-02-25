//! High-level file operations for the SD card.
//!
//! Uses embedded-sdmmc 0.9's RAII handles (Volume, Directory, File)
//! which close automatically on drop.

use embedded_sdmmc::{Mode, VolumeIdx};

use crate::board::sdcard::SdStorage;

/// A single directory entry, small enough to keep a page on the stack.
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name: [u8; 13],
    pub name_len: u8,
    pub is_dir: bool,
    pub size: u32,
}

impl DirEntry {
    pub const EMPTY: Self = Self {
        name: [0u8; 13],
        name_len: 0,
        is_dir: false,
        size: 0,
    };

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }
}

/// Result of a paginated directory listing.
pub struct DirPage {
    pub total: usize,
    pub count: usize,
}

// ── Directory cache ────────────────────────────────────────────
//
// FAT directory iteration has no seek — every list_page() must scan
// from the first entry and skip. For scroll position 40, that's
// 40 entries read and discarded. With 100ms+ per SD transaction,
// scrolling feels sluggish.
//
// The cache reads ALL entries once and serves pages from RAM.
// Subsequent scrolls are pure memory copies — instant.
//
// Memory: 128 entries × 20 bytes = 2.5KB (of 400KB SRAM).

/// Maximum directory entries we'll cache.
pub const MAX_DIR_ENTRIES: usize = 128;

/// In-memory cache of a directory's entries.
///
/// Created once in main.rs, lives for the lifetime of the program.
/// `ensure_loaded()` fills it from SD on first access; `page()`
/// serves slices without touching hardware.
pub struct DirCache {
    entries: [DirEntry; MAX_DIR_ENTRIES],
    count: usize,
    valid: bool,
}

impl DirCache {
    pub const fn new() -> Self {
        Self {
            entries: [DirEntry::EMPTY; MAX_DIR_ENTRIES],
            count: 0,
            valid: false,
        }
    }

    /// Load all entries from the root directory if not already cached.
    /// Returns Ok(()) if cache is warm (already valid), or after a
    /// successful SD read. Returns Err only on SD failure.
    pub fn ensure_loaded<SPI>(&mut self, sd: &SdStorage<SPI>) -> Result<(), &'static str>
    where
        SPI: embedded_hal::spi::SpiDevice,
    {
        if self.valid {
            return Ok(());
        }

        let volume = sd
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| "open volume failed")?;
        let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

        let mut count = 0usize;
        root.iterate_dir(|entry| {
            if entry.name.base_name()[0] == b'.' {
                return;
            }
            if count < MAX_DIR_ENTRIES {
                let mut name_buf = [0u8; 13];
                let name_len = format_83_name(&entry.name, &mut name_buf);
                self.entries[count] = DirEntry {
                    name: name_buf,
                    name_len: name_len as u8,
                    is_dir: entry.attributes.is_directory(),
                    size: entry.size,
                };
                count += 1;
            }
        })
        .map_err(|_| "iterate dir failed")?;

        self.count = count;
        self.valid = true;
        Ok(())
    }

    /// Copy a page of entries into `buf`, starting at `skip`.
    /// Pure memory operation — no SD access.
    pub fn page(&self, skip: usize, buf: &mut [DirEntry]) -> DirPage {
        let available = self.count.saturating_sub(skip);
        let count = available.min(buf.len());
        if count > 0 {
            buf[..count].copy_from_slice(&self.entries[skip..skip + count]);
        }
        DirPage {
            total: self.count,
            count,
        }
    }

    /// Mark cache as stale. Next `ensure_loaded()` will re-read from SD.
    pub fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Total cached entries (0 if not loaded).
    pub fn total(&self) -> usize {
        self.count
    }

    /// Whether the cache has been loaded.
    pub fn is_valid(&self) -> bool {
        self.valid
    }
}

/// List one page of entries from root directory.
///
/// Skips the first `skip` entries, then fills `buf` with up to `buf.len()` entries.
/// Returns total entry count and how many were written to buf.
pub fn list_page<SPI>(
    sd: &SdStorage<SPI>,
    skip: usize,
    buf: &mut [DirEntry],
) -> Result<DirPage, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    let mut total = 0usize;
    let mut written = 0usize;
    let page_size = buf.len();

    root.iterate_dir(|entry| {
        // Skip dot entries
        if entry.name.base_name()[0] == b'.' {
            return;
        }

        if total >= skip && written < page_size {
            let mut name_buf = [0u8; 13];
            let name_len = format_83_name(&entry.name, &mut name_buf);
            buf[written] = DirEntry {
                name: name_buf,
                name_len: name_len as u8,
                is_dir: entry.attributes.is_directory(),
                size: entry.size,
            };
            written += 1;
        }
        total += 1;
    })
    .map_err(|_| "iterate dir failed")?;

    Ok(DirPage {
        total,
        count: written,
    })
}

/// List files in the root directory, calling `cb` for each entry.
/// Returns the number of entries found.
pub fn list_root_dir<SPI>(
    sd: &SdStorage<SPI>,
    mut cb: impl FnMut(&str, bool, u32),
) -> Result<usize, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    let mut count = 0usize;
    root.iterate_dir(|entry| {
        let mut name_buf = [0u8; 13];
        let name_len = format_83_name(&entry.name, &mut name_buf);
        if let Ok(name) = core::str::from_utf8(&name_buf[..name_len]) {
            cb(name, entry.attributes.is_directory(), entry.size);
            count += 1;
        }
    })
    .map_err(|_| "iterate dir failed")?;

    Ok(count)
}

/// Get the size of a file in the root directory.
pub fn file_size<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let file = root
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file failed")?;

    Ok(file.length())
}

/// Read an entire file (or up to buf.len() bytes) into a buffer.
/// Returns the number of bytes read.
pub fn read_file<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    buf: &mut [u8],
) -> Result<usize, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let mut file = root
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file failed")?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file.read(&mut buf[total..]).map_err(|_| "read failed")?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok(total)
}

/// Read a chunk of a file starting at `offset`.
/// Returns the number of bytes read.
pub fn read_file_chunk<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> Result<usize, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let mut file = root
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file failed")?;

    file.seek_from_start(offset).map_err(|_| "seek failed")?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file.read(&mut buf[total..]).map_err(|_| "read failed")?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok(total)
}

/// Write data to a file (create or truncate).
pub fn write_file<SPI>(sd: &SdStorage<SPI>, name: &str, data: &[u8]) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let mut file = root
        .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "open file for write failed")?;

    file.write(data).map_err(|_| "write failed")?;
    file.flush().map_err(|_| "flush failed")?;

    Ok(())
}

/// Format a ShortFileName (8.3) into a human-readable "NAME.EXT" string.
/// Returns the number of bytes written to `out`.
fn format_83_name(sfn: &embedded_sdmmc::ShortFileName, out: &mut [u8; 13]) -> usize {
    let base = sfn.base_name();
    let ext = sfn.extension();

    let mut pos = 0;

    // Copy base name, trimming trailing spaces
    for &b in base.iter() {
        if b == b' ' {
            break;
        }
        out[pos] = b;
        pos += 1;
    }

    // Add extension if non-empty
    let ext_trimmed: &[u8] = &ext[..ext.iter().position(|&b| b == b' ').unwrap_or(ext.len())];
    if !ext_trimmed.is_empty() {
        out[pos] = b'.';
        pos += 1;
        for &b in ext_trimmed {
            out[pos] = b;
            pos += 1;
        }
    }

    pos
}
