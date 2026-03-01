// SD card file operations and directory cache.
// with_dir! opens volume/root/subdirs; do_*! macros handle file ops.
// DirCache reads root entries once into RAM, serves pages from there.

use embedded_sdmmc::{Mode, VolumeIdx};

use crate::drivers::sdcard::SdStorage;

pub const PULP_DIR: &str = "_PULP";

// title index: append-only lines of "FILENAME.EXT\tTitle Text\n"
pub const TITLES_FILE: &str = "TITLES.BIN";

pub const TITLE_CAP: usize = 48;

#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name: [u8; 13],
    pub name_len: u8,
    pub is_dir: bool,
    pub size: u32,
    // parsed display title from EPUB OPF metadata; empty = unavailable
    pub title: [u8; TITLE_CAP],
    pub title_len: u8,
}

impl DirEntry {
    pub const EMPTY: Self = Self {
        name: [0u8; 13],
        name_len: 0,
        is_dir: false,
        size: 0,
        title: [0u8; TITLE_CAP],
        title_len: 0,
    };

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("?")
    }

    pub fn display_name(&self) -> &str {
        if self.title_len > 0 {
            core::str::from_utf8(&self.title[..self.title_len as usize]).unwrap_or(self.name_str())
        } else {
            self.name_str()
        }
    }

    pub fn set_title(&mut self, s: &[u8]) {
        let n = s.len().min(TITLE_CAP);
        self.title[..n].copy_from_slice(&s[..n]);
        self.title_len = n as u8;
    }
}

pub struct DirPage {
    pub total: usize,
    pub count: usize,
}

const MAX_DIR_ENTRIES: usize = 128;

// with_dir!: open volume -> root, optionally descend 1 or 2 subdirs
macro_rules! with_dir {
    // root only
    ($sd:expr, |$dir:ident| $body:expr) => {{
        let volume = $sd
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| "open volume failed")?;
        let $dir = volume.open_root_dir().map_err(|_| "open root dir failed")?;
        $body
    }};
    // one subdirectory
    ($sd:expr, $d1:expr, |$dir:ident| $body:expr) => {{
        let volume = $sd
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| "open volume failed")?;
        let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
        let $dir = root.open_dir($d1).map_err(|_| "open dir failed")?;
        $body
    }};
    // two subdirectories
    ($sd:expr, $d1:expr, $d2:expr, |$dir:ident| $body:expr) => {{
        let volume = $sd
            .volume_mgr
            .open_volume(VolumeIdx(0))
            .map_err(|_| "open volume failed")?;
        let root = volume.open_root_dir().map_err(|_| "open root dir failed")?;
        let mid = root.open_dir($d1).map_err(|_| "open dir failed")?;
        let $dir = mid.open_dir($d2).map_err(|_| "open dir failed")?;
        $body
    }};
}

// inner-op macros; always invoked inside with_dir!

macro_rules! read_loop {
    ($file:expr, $buf:expr) => {{
        let mut total = 0usize;
        while !$file.is_eof() && total < $buf.len() {
            let n = $file.read(&mut $buf[total..]).map_err(|_| "read failed")?;
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    }};
}

macro_rules! write_flush {
    ($file:expr, $data:expr) => {{
        if !$data.is_empty() {
            $file.write($data).map_err(|_| "write failed")?;
        }
        $file.flush().map_err(|_| "flush failed")?;
    }};
}

macro_rules! do_read_chunk {
    ($dir:expr, $name:expr, $offset:expr, $buf:expr) => {{
        let file = $dir
            .open_file_in_dir($name, Mode::ReadOnly)
            .map_err(|_| "open file failed")?;
        file.seek_from_start($offset).map_err(|_| "seek failed")?;
        Ok(read_loop!(file, $buf))
    }};
}

macro_rules! do_read_start {
    ($dir:expr, $name:expr, $buf:expr) => {{
        let file = $dir
            .open_file_in_dir($name, Mode::ReadOnly)
            .map_err(|_| "open file failed")?;
        let size = file.length();
        let n = read_loop!(file, $buf);
        Ok((size, n))
    }};
}

macro_rules! do_write {
    ($dir:expr, $name:expr, $data:expr) => {{
        let file = $dir
            .open_file_in_dir($name, Mode::ReadWriteCreateOrTruncate)
            .map_err(|_| "create file failed")?;
        write_flush!(file, $data);
        Ok(())
    }};
}

macro_rules! do_append {
    ($dir:expr, $name:expr, $data:expr) => {{
        let file = $dir
            .open_file_in_dir($name, Mode::ReadWriteCreateOrAppend)
            .map_err(|_| "open file for append failed")?;
        write_flush!(file, $data);
        Ok(())
    }};
}

macro_rules! do_file_size {
    ($dir:expr, $name:expr) => {{
        let file = $dir
            .open_file_in_dir($name, Mode::ReadOnly)
            .map_err(|_| "open file failed")?;
        Ok(file.length())
    }};
}

macro_rules! do_delete {
    ($dir:expr, $name:expr) => {{
        let _ = $dir.delete_file_in_dir($name);
        Ok(())
    }};
}

macro_rules! do_ensure_subdir {
    ($dir:expr, $name:expr) => {{
        if $dir.open_dir($name).is_err() {
            $dir.make_dir_in_dir($name).map_err(|_| "make dir failed")?;
        }
        Ok(())
    }};
}

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

        // volume/root handles must be dropped before load_titles opens its own
        with_dir!(sd, |root| {
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
                        title: [0u8; TITLE_CAP],
                        title_len: 0,
                    };
                    count += 1;
                }
            })
            .map_err(|_| "iterate dir failed")?;

            self.count = count;
            sort_entries(&mut self.entries[..count]);
            self.valid = true;
            Ok(())
        })?;

        self.load_titles(sd);

        Ok(())
    }

    // read _PULP/TITLES.BIN and apply parsed titles to matching entries;
    // append-only so later lines override earlier ones for the same name
    fn load_titles<SPI>(&mut self, sd: &SdStorage<SPI>)
    where
        SPI: embedded_hal::spi::SpiDevice,
    {
        let mut buf = [0u8; 1024];
        let mut offset: u32 = 0;
        let mut leftover = 0usize;

        loop {
            let space = buf.len() - leftover;
            if space == 0 {
                // line longer than buffer; skip to next newline
                leftover = 0;
                loop {
                    let n = match read_pulp_file_chunk(sd, TITLES_FILE, offset, &mut buf) {
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    if n == 0 {
                        break;
                    }
                    offset += n as u32;
                    if let Some(nl) = buf[..n].iter().position(|&b| b == b'\n') {
                        let rest = n - (nl + 1);
                        if rest > 0 {
                            buf.copy_within(nl + 1..n, 0);
                        }
                        leftover = rest;
                        break;
                    }
                }
                continue;
            }

            let n = match read_pulp_file_chunk(sd, TITLES_FILE, offset, &mut buf[leftover..]) {
                Ok(n) => n,
                Err(_) => break,
            };
            if n == 0 {
                if leftover > 0 {
                    self.apply_title_line(&buf[..leftover]);
                }
                break;
            }

            offset += n as u32;
            let total = leftover + n;

            let mut start = 0;
            for i in 0..total {
                if buf[i] == b'\n' {
                    if i > start {
                        self.apply_title_line(&buf[start..i]);
                    }
                    start = i + 1;
                }
            }

            if start < total {
                buf.copy_within(start..total, 0);
                leftover = total - start;
            } else {
                leftover = 0;
            }
        }
    }

    // parse "FILENAME.EXT\tTitle Text"; last-writer-wins for duplicate names
    fn apply_title_line(&mut self, line: &[u8]) {
        let tab_pos = match line.iter().position(|&b| b == b'\t') {
            Some(p) => p,
            None => return,
        };
        let name_part = &line[..tab_pos];
        let title_part = &line[tab_pos + 1..];
        if title_part.is_empty() {
            return;
        }

        for entry in self.entries[..self.count].iter_mut() {
            let elen = entry.name_len as usize;
            if elen == name_part.len() && entry.name[..elen].eq_ignore_ascii_case(name_part) {
                entry.set_title(title_part);
                break;
            }
        }
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

// insertion sort: dirs first, then filenames case-insensitive
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

// root file operations

pub fn file_size<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, |root| do_file_size!(root, name))
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
    with_dir!(sd, |root| do_read_chunk!(root, name, offset, buf))
}

pub fn read_file_start<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    buf: &mut [u8],
) -> Result<(u32, usize), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, |root| do_read_start!(root, name, buf))
}

pub fn write_file<SPI>(sd: &SdStorage<SPI>, name: &str, data: &[u8]) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, |root| do_write!(root, name, data))
}

pub fn append_root_file<SPI>(
    sd: &SdStorage<SPI>,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, |root| do_append!(root, name, data))
}

// subdirectory operations

pub fn ensure_dir<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, |root| do_ensure_subdir!(root, name))
}

pub fn write_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, dir, |sub| do_write!(sub, name, data))
}

pub fn append_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, dir, |sub| do_append!(sub, name, data))
}

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
    with_dir!(sd, dir, |sub| do_read_chunk!(sub, name, offset, buf))
}

pub fn read_file_start_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    buf: &mut [u8],
) -> Result<(u32, usize), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, dir, |sub| do_read_start!(sub, name, buf))
}

pub fn file_size_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, dir, |sub| do_file_size!(sub, name))
}

pub fn delete_file_in_dir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, dir, |sub| do_delete!(sub, name))
}

// _PULP app-data directory

pub fn ensure_pulp_dir<SPI>(sd: &SdStorage<SPI>) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    ensure_dir(sd, PULP_DIR)
}

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

// _PULP subdirectory operations

pub fn ensure_pulp_subdir<SPI>(sd: &SdStorage<SPI>, name: &str) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, PULP_DIR, |pulp| do_ensure_subdir!(pulp, name))
}

pub fn write_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, PULP_DIR, dir, |sub| do_write!(sub, name, data))
}

pub fn append_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
    data: &[u8],
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, PULP_DIR, dir, |sub| do_append!(sub, name, data))
}

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
    with_dir!(sd, PULP_DIR, dir, |sub| do_read_chunk!(
        sub, name, offset, buf
    ))
}

pub fn file_size_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<u32, &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, PULP_DIR, dir, |sub| do_file_size!(sub, name))
}

pub fn delete_in_pulp_subdir<SPI>(
    sd: &SdStorage<SPI>,
    dir: &str,
    name: &str,
) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    with_dir!(sd, PULP_DIR, dir, |sub| do_delete!(sub, name))
}

// append title mapping line to _PULP/TITLES.BIN
pub fn save_title<SPI>(sd: &SdStorage<SPI>, filename: &str, title: &str) -> Result<(), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let name_bytes = filename.as_bytes();
    let title_bytes = title.as_bytes();
    let title_len = title_bytes.len().min(TITLE_CAP);
    let line_len = name_bytes.len() + 1 + title_len + 1; // name + \t + title + \n
    if line_len > 128 {
        return Err("title line too long");
    }
    let mut line = [0u8; 128];
    line[..name_bytes.len()].copy_from_slice(name_bytes);
    line[name_bytes.len()] = b'\t';
    line[name_bytes.len() + 1..name_bytes.len() + 1 + title_len]
        .copy_from_slice(&title_bytes[..title_len]);
    line[name_bytes.len() + 1 + title_len] = b'\n';

    append_file_in_dir(sd, PULP_DIR, TITLES_FILE, &line[..line_len])
}
