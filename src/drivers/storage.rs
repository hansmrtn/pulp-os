// SD card file operations and directory cache
//
// FAT directory iteration has no seek; every listing scans from entry 0.
// DirCache reads all entries once into RAM and serves pages from there.
// 128 entries * 20 bytes = 2.5KB of SRAM.
//
// read_file_start: single open, returns (file_size, bytes_read) from offset 0.
// Avoids the separate file_size + read_file_chunk round-trip on first access.

use embedded_sdmmc::{Mode, VolumeIdx};

use crate::board::sdcard::SdStorage;

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

pub struct DirPage {
    pub total: usize,
    pub count: usize,
}

pub const MAX_DIR_ENTRIES: usize = 128;

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

    pub fn invalidate(&mut self) {
        self.valid = false;
    }
}

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
    let file = root
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

// open file, return (size, bytes_read) from offset 0 in a single open
pub fn read_file_start<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    buf: &mut [u8],
) -> Result<(u32, usize), &'static str>
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

    let file_size = file.length();

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file.read(&mut buf[total..]).map_err(|_| "read failed")?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok((file_size, total))
}

fn format_83_name(sfn: &embedded_sdmmc::ShortFileName, out: &mut [u8; 13]) -> usize {
    let base = sfn.base_name();
    let ext = sfn.extension();

    let mut pos = 0;

    for &b in base.iter() {
        if b == b' ' {
            break;
        }
        out[pos] = b;
        pos += 1;
    }

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
