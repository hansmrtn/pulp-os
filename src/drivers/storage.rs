// SD card file operations and directory cache
//
// DirCache reads all entries once into RAM, serves pages from there.
// Subdirectory ops support the EPUB chapter cache pipeline.

use embedded_sdmmc::{Mode, VolumeIdx};

use crate::drivers::sdcard::SdStorage;

// all app data lives under this directory on the SD root
pub const PULP_DIR: &str = "_PULP";

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

impl Default for DirCache {
    fn default() -> Self {
        Self::new()
    }
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
            if matches!(entry.name.base_name()[0], b'.' | b'_') {
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
        sort_entries(&mut self.entries[..count]);
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

// insertion sort: dirs first, then case-insensitive
fn sort_entries(entries: &mut [DirEntry]) {
    for i in 1..entries.len() {
        let key = entries[i];
        let mut j = i;
        while j > 0 && entry_gt(&entries[j - 1], &key) {
            entries[j] = entries[j - 1];
            j -= 1;
        }
        entries[j] = key;
    }
}

fn entry_gt(a: &DirEntry, b: &DirEntry) -> bool {
    if a.is_dir != b.is_dir {
        return !a.is_dir;
    }
    let an = &a.name[..a.name_len as usize];
    let bn = &b.name[..b.name_len as usize];
    for (&ab, &bb) in an.iter().zip(bn.iter()) {
        let al = ab.to_ascii_lowercase();
        let bl = bb.to_ascii_lowercase();
        match al.cmp(&bl) {
            core::cmp::Ordering::Less => return false,
            core::cmp::Ordering::Greater => return true,
            core::cmp::Ordering::Equal => {}
        }
    }
    an.len() > bn.len()
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

// open file inside a subdir, return (size, bytes_read) from offset 0
pub fn read_file_start_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
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
    let sub = root.open_dir(dir).map_err(|_| "open dir failed")?;
    let file = sub
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

pub fn write_file<SPI>(sd: &SdStorage<SPI>, name: &str, data: &[u8]) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    // ReadWriteCreateOrTruncate handles both first write and updates
    let file = root
        .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "open file for write failed")?;

    file.write(data).map_err(|_| "write failed")?;
    file.flush().map_err(|_| "flush failed")?;

    Ok(())
}

/// Create (or truncate) a file in the SD root and write an initial chunk.
/// Use [`append_root_file`] for subsequent chunks.
pub fn create_or_truncate_root<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    let file = root
        .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "create file failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "write failed")?;
    }
    file.flush().map_err(|_| "flush failed")?;

    Ok(())
}

/// Append a chunk to an existing file in the SD root.
/// The file must already exist (created via [`create_or_truncate_root`]).
pub fn append_root_file<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    let file = root
        .open_file_in_dir(name, Mode::ReadWriteCreateOrAppend)
        .map_err(|_| "open file for append failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "append write failed")?;
    }
    file.flush().map_err(|_| "append flush failed")?;

    Ok(())
}

// ── Subdirectory operations (EPUB chapter cache) ──────────────────────────

// create dir in root if it doesn't already exist
pub fn ensure_dir<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;

    // already exists → done
    if root.open_dir(name).is_ok() {
        return Ok(());
    }

    // create it
    root.make_dir_in_dir(name).map_err(|_| "make dir failed")?;

    Ok(())
}

// write (create-or-truncate) a file inside a subdirectory of root
pub fn write_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let sub = root.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "create file in dir failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "write in dir failed")?;
    }
    file.flush().map_err(|_| "flush in dir failed")?;

    Ok(())
}

// append to file (or create) inside a subdirectory
pub fn append_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let sub = root.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadWriteCreateOrAppend)
        .map_err(|_| "open file for append failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "append write failed")?;
    }
    file.flush().map_err(|_| "append flush failed")?;

    Ok(())
}

// read chunk from file in subdir at `offset`
pub fn read_file_chunk_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
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
    let sub = root.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file in dir failed")?;

    file.seek_from_start(offset)
        .map_err(|_| "seek in dir failed")?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file
            .read(&mut buf[total..])
            .map_err(|_| "read in dir failed")?;
        if n == 0 {
            break;
        }
        total += n;
    }

    Ok(total)
}

// file size in subdir, or Err if not found
pub fn file_size_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let sub = root.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file in dir for size failed")?;

    Ok(file.length())
}

// delete file in subdir; no-op if absent
pub fn delete_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let sub = root.open_dir(dir).map_err(|_| "open cache dir failed")?;

    // idempotent: ignore "not found"
    let _ = sub.delete_file_in_dir(name);

    Ok(())
}

// ── _PULP app-data directory ──────────────────────────────────────────────

// create _PULP in root if it doesn't already exist
pub fn ensure_pulp_dir<SPI>(sd: &SdStorage<SPI>) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    ensure_dir(sd, PULP_DIR)
}

// write (create-or-truncate) a file directly inside _PULP/
pub fn write_pulp_file<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    write_file_in_dir(sd, PULP_DIR, name, data)
}

// read chunk from a file directly inside _PULP/
pub fn read_pulp_file_chunk<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> Result<usize, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    read_file_chunk_in_dir(sd, PULP_DIR, name, offset, buf)
}

// open file in _PULP/, return (size, bytes_read) from offset 0
pub fn read_pulp_file_start<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    buf: &mut [u8],
) -> Result<(u32, usize), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    read_file_start_in_dir(sd, PULP_DIR, name, buf)
}

// ── Nested subdirectory operations (_PULP/<sub>/) ─────────────────────────

// create _PULP/<name>/ (ensures _PULP exists first)
pub fn ensure_pulp_subdir<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;

    if pulp.open_dir(name).is_ok() {
        return Ok(());
    }
    pulp.make_dir_in_dir(name)
        .map_err(|_| "make subdir failed")?;
    Ok(())
}

// write (create-or-truncate) a file inside _PULP/<dir>/
pub fn write_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;
    let sub = pulp.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadWriteCreateOrTruncate)
        .map_err(|_| "create file in dir failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "write in dir failed")?;
    }
    file.flush().map_err(|_| "flush in dir failed")?;
    Ok(())
}

// append to file (or create) inside _PULP/<dir>/
pub fn append_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;
    let sub = pulp.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadWriteCreateOrAppend)
        .map_err(|_| "open file for append failed")?;

    if !data.is_empty() {
        file.write(data).map_err(|_| "append write failed")?;
    }
    file.flush().map_err(|_| "append flush failed")?;
    Ok(())
}

// read chunk from _PULP/<dir>/<name> at offset
pub fn read_chunk_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
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
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;
    let sub = pulp.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file in dir failed")?;

    file.seek_from_start(offset)
        .map_err(|_| "seek in dir failed")?;

    let mut total = 0;
    while !file.is_eof() && total < buf.len() {
        let n = file
            .read(&mut buf[total..])
            .map_err(|_| "read in dir failed")?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

// file size inside _PULP/<dir>/<name>
pub fn file_size_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;
    let sub = pulp.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let file = sub
        .open_file_in_dir(name, Mode::ReadOnly)
        .map_err(|_| "open file for size failed")?;
    Ok(file.length())
}

// delete file inside _PULP/<dir>/; no-op if absent
pub fn delete_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let volume = sd
        .volume_mgr
        .open_volume(VolumeIdx(0))
        .map_err(|_| "open volume failed")?;
    let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
    let pulp = root.open_dir(PULP_DIR).map_err(|_| "open _PULP failed")?;
    let sub = pulp.open_dir(dir).map_err(|_| "open cache dir failed")?;

    let _ = sub.delete_file_in_dir(name);
    Ok(())
}
