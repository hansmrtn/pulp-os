// sd card file operations
//
// all I/O through embedded-sdmmc AsyncVolumeManager; functions are
// synchronous, wrapping async ops with poll_once (SPI bus is blocking
// so every .await resolves immediately)
//
// returns the unified Error type (re-exported as StorageError for
// backward compat); apps receive it through KernelHandle

use core::ops::ControlFlow;

use embedded_sdmmc::Mode;

use crate::drivers::sdcard::{SdStorage, SdStorageInner, poll_once};
use crate::error::{Error, ErrorKind};

pub const PULP_DIR: &str = "_PULP";
pub const TITLES_FILE: &str = "TITLES.BIN";
pub const TITLE_CAP: usize = 48;

// backward-compatible alias
pub type StorageError = Error;

#[derive(Clone, Copy)]
pub struct DirEntry {
    pub name: [u8; 13],
    pub name_len: u8,
    pub is_dir: bool,
    pub size: u32,
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

fn ext_eq(name: &[u8], target: &[u8]) -> bool {
    let dot = match name.iter().rposition(|&b| b == b'.') {
        Some(p) => p,
        None => return false,
    };
    let ext = &name[dot + 1..];
    ext.len() == target.len() && ext.eq_ignore_ascii_case(target)
}

fn has_supported_ext(name: &[u8]) -> bool {
    ext_eq(name, b"TXT") || ext_eq(name, b"EPUB") || ext_eq(name, b"EPU") || ext_eq(name, b"MD")
}

// build "NAME.EXT" bytes from a ShortFileName

fn sfn_to_bytes(name: &embedded_sdmmc::ShortFileName, out: &mut [u8; 13]) -> u8 {
    let base = name.base_name();
    let ext = name.extension();
    let mut pos = 0usize;
    let blen = base.len().min(8);
    out[..blen].copy_from_slice(&base[..blen]);
    pos += blen;
    if !ext.is_empty() {
        out[pos] = b'.';
        pos += 1;
        let elen = ext.len().min(3);
        out[pos..pos + elen].copy_from_slice(&ext[..elen]);
        pos += elen;
    }
    pos as u8
}

// file-operation macros; each evaluates to Result<T, Error>
// none use ? internally so caller cleanup is never bypassed

macro_rules! op_file_size {
    ($inner:expr, $dir:expr, $name:expr) => {
        $inner
            .mgr
            .find_directory_entry($dir, $name)
            .await
            .map(|e| e.size)
            .map_err(|_| Error::new(ErrorKind::OpenFile, "file_size"))
    };
}

macro_rules! op_read_chunk {
    ($inner:expr, $dir:expr, $name:expr, $offset:expr, $buf:expr) => {
        match $inner
            .mgr
            .open_file_in_dir($dir, $name, Mode::ReadOnly)
            .await
        {
            Err(_) => Err(Error::new(ErrorKind::OpenFile, "read_chunk")),
            Ok(file) => {
                let result = match $inner.mgr.file_seek_from_start(file, $offset) {
                    Ok(()) => $inner
                        .mgr
                        .read(file, $buf)
                        .await
                        .map_err(|_| Error::new(ErrorKind::ReadFailed, "read_chunk")),
                    Err(_) => Err(Error::new(ErrorKind::SeekFailed, "read_chunk")),
                };
                let _ = $inner.mgr.close_file(file).await;
                result
            }
        }
    };
}

macro_rules! op_read_start {
    ($inner:expr, $dir:expr, $name:expr, $buf:expr) => {
        match $inner
            .mgr
            .open_file_in_dir($dir, $name, Mode::ReadOnly)
            .await
        {
            Err(_) => Err(Error::new(ErrorKind::OpenFile, "read_start")),
            Ok(file) => {
                let size = $inner.mgr.file_length(file).unwrap_or(0);
                let result = $inner
                    .mgr
                    .read(file, $buf)
                    .await
                    .map_err(|_| Error::new(ErrorKind::ReadFailed, "read_start"));
                let _ = $inner.mgr.close_file(file).await;
                result.map(|n| (size, n))
            }
        }
    };
}

macro_rules! op_write {
    ($inner:expr, $dir:expr, $name:expr, $data:expr) => {
        match $inner
            .mgr
            .open_file_in_dir($dir, $name, Mode::ReadWriteCreateOrTruncate)
            .await
        {
            Err(_) => Err(Error::new(ErrorKind::OpenFile, "write")),
            Ok(file) => {
                let result = if ($data).is_empty() {
                    Ok(())
                } else {
                    $inner
                        .mgr
                        .write(file, $data)
                        .await
                        .map_err(|_| Error::new(ErrorKind::WriteFailed, "write"))
                };
                let _ = $inner.mgr.close_file(file).await;
                result
            }
        }
    };
}

macro_rules! op_append {
    ($inner:expr, $dir:expr, $name:expr, $data:expr) => {
        match $inner
            .mgr
            .open_file_in_dir($dir, $name, Mode::ReadWriteCreateOrAppend)
            .await
        {
            Err(_) => Err(Error::new(ErrorKind::OpenFile, "append")),
            Ok(file) => {
                let result = if ($data).is_empty() {
                    Ok(())
                } else {
                    $inner
                        .mgr
                        .write(file, $data)
                        .await
                        .map_err(|_| Error::new(ErrorKind::WriteFailed, "append"))
                };
                let _ = $inner.mgr.close_file(file).await;
                result
            }
        }
    };
}

macro_rules! op_delete {
    ($inner:expr, $dir:expr, $name:expr) => {{
        $inner
            .mgr
            .delete_entry_in_dir($dir, $name)
            .await
            .map_err(|_| Error::new(ErrorKind::DeleteFailed, "delete"))
    }};
}

// dir-scoping macros; open subdir, execute body, close handle

macro_rules! in_dir {
    ($inner:expr, $dirname:expr, |$dir:ident| $body:expr) => {
        match $inner.mgr.open_dir($inner.root, $dirname).await {
            Err(_) => Err(Error::new(ErrorKind::OpenDir, "in_dir")),
            Ok($dir) => {
                let _r = $body;
                let _ = $inner.mgr.close_dir($dir);
                _r
            }
        }
    };
}

macro_rules! in_subdir {
    ($inner:expr, $d1:expr, $d2:expr, |$dir:ident| $body:expr) => {
        match $inner.mgr.open_dir($inner.root, $d1).await {
            Err(_) => Err(Error::new(ErrorKind::OpenDir, "in_subdir")),
            Ok(_mid) => match $inner.mgr.open_dir(_mid, $d2).await {
                Err(_) => {
                    let _ = $inner.mgr.close_dir(_mid);
                    Err(Error::new(ErrorKind::OpenDir, "in_subdir"))
                }
                Ok($dir) => {
                    let _r = $body;
                    let _ = $inner.mgr.close_dir($dir);
                    let _ = $inner.mgr.close_dir(_mid);
                    _r
                }
            },
        }
    };
}

fn borrow(sd: &SdStorage) -> core::result::Result<core::cell::RefMut<'_, SdStorageInner>, Error> {
    sd.borrow_inner()
        .ok_or(Error::new(ErrorKind::NoCard, "storage::borrow"))
}

// root file operations

pub fn file_size(sd: &SdStorage, name: &str) -> crate::error::Result<u32> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_file_size!(inner, inner.root, name)
    })
}

pub fn read_file_chunk(
    sd: &SdStorage,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> crate::error::Result<usize> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_read_chunk!(inner, inner.root, name, offset, buf)
    })
}

pub fn read_file_start(
    sd: &SdStorage,
    name: &str,
    buf: &mut [u8],
) -> crate::error::Result<(u32, usize)> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_read_start!(inner, inner.root, name, buf)
    })
}

pub fn write_file(sd: &SdStorage, name: &str, data: &[u8]) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_write!(inner, inner.root, name, data)
    })
}

pub fn append_root_file(sd: &SdStorage, name: &str, data: &[u8]) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_append!(inner, inner.root, name, data)
    })
}

pub fn delete_file(sd: &SdStorage, name: &str) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        op_delete!(inner, inner.root, name)
    })
}

// directory listing

pub fn list_root_files(sd: &SdStorage, buf: &mut [DirEntry]) -> crate::error::Result<usize> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;

        let mut count = 0usize;
        let mut total = 0usize;

        inner
            .mgr
            .iterate_dir(inner.root, |entry| {
                if entry.attributes.is_volume() || entry.attributes.is_directory() {
                    return ControlFlow::Continue(());
                }

                let mut name_buf = [0u8; 13];
                let name_len = sfn_to_bytes(&entry.name, &mut name_buf);
                let sfn = &name_buf[..name_len as usize];

                if sfn.is_empty() || sfn[0] == b'.' || sfn[0] == b'_' {
                    return ControlFlow::Continue(());
                }
                if !has_supported_ext(sfn) {
                    return ControlFlow::Continue(());
                }

                total += 1;

                if count < buf.len() {
                    buf[count] = DirEntry {
                        name: name_buf,
                        name_len,
                        is_dir: false,
                        size: entry.size,
                        title: [0u8; TITLE_CAP],
                        title_len: 0,
                    };
                    count += 1;
                }
                ControlFlow::Continue(())
            })
            .await
            .map_err(|_| Error::new(ErrorKind::ReadFailed, "list_root_files"))?;

        if total > count {
            log::warn!(
                "dir: {} supported files on SD, only {} fit in buffer (max {})",
                total,
                count,
                buf.len(),
            );
        }
        Ok(count)
    })
}

// directory management

pub fn ensure_dir(sd: &SdStorage, name: &str) -> crate::error::Result<()> {
    // two poll_once calls so the large make_dir future never shares
    // a stack frame with open_dir, halving peak stack usage
    let exists = poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        match inner.mgr.open_dir(inner.root, name).await {
            Ok(dir) => {
                let _ = inner.mgr.close_dir(dir);
                Ok::<_, Error>(true)
            }
            Err(_) => Ok(false),
        }
    })?;

    if exists {
        return Ok(());
    }

    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        match inner.mgr.make_dir_in_dir(inner.root, name).await {
            Ok(()) => Ok(()),
            Err(embedded_sdmmc::Error::DirAlreadyExists) => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::WriteFailed, "ensure_dir")),
        }
    })
}

// single-directory file operations

pub fn write_file_in_dir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    data: &[u8],
) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, dir, |dir_h| op_write!(inner, dir_h, name, data))
    })
}

pub fn append_file_in_dir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    data: &[u8],
) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, dir, |dir_h| op_append!(inner, dir_h, name, data))
    })
}

pub fn read_file_chunk_in_dir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> crate::error::Result<usize> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, dir, |dir_h| op_read_chunk!(
            inner, dir_h, name, offset, buf
        ))
    })
}

pub fn read_file_start_in_dir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    buf: &mut [u8],
) -> crate::error::Result<(u32, usize)> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, dir, |dir_h| op_read_start!(inner, dir_h, name, buf))
    })
}

// async boot path (runs inside the real executor)

pub async fn ensure_pulp_dir_async(sd: &SdStorage) -> crate::error::Result<()> {
    let mut guard = borrow(sd)?;
    let inner = &mut *guard;

    if let Ok(dir) = inner.mgr.open_dir(inner.root, PULP_DIR).await {
        let _ = inner.mgr.close_dir(dir);
        return Ok(());
    }
    match inner.mgr.make_dir_in_dir(inner.root, PULP_DIR).await {
        Ok(()) => Ok(()),
        Err(embedded_sdmmc::Error::DirAlreadyExists) => Ok(()),
        Err(_) => Err(Error::new(ErrorKind::WriteFailed, "ensure_pulp_dir_async")),
    }
}

// _PULP subdirectory operations

pub fn ensure_pulp_subdir(sd: &SdStorage, name: &str) -> crate::error::Result<()> {
    let exists = poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, PULP_DIR, |pulp_h| {
            match inner.mgr.open_dir(pulp_h, name).await {
                Ok(sub) => {
                    let _ = inner.mgr.close_dir(sub);
                    Ok::<_, Error>(true)
                }
                Err(_) => Ok(false),
            }
        })
    })?;

    if exists {
        return Ok(());
    }

    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_dir!(inner, PULP_DIR, |pulp_h| {
            match inner.mgr.make_dir_in_dir(pulp_h, name).await {
                Ok(()) => Ok::<_, Error>(()),
                Err(embedded_sdmmc::Error::DirAlreadyExists) => Ok(()),
                Err(_) => Err(Error::new(ErrorKind::WriteFailed, "ensure_pulp_subdir")),
            }
        })
    })
}

pub fn write_in_pulp_subdir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    data: &[u8],
) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_subdir!(inner, PULP_DIR, dir, |sub_h| op_write!(
            inner, sub_h, name, data
        ))
    })
}

pub fn append_in_pulp_subdir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    data: &[u8],
) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_subdir!(inner, PULP_DIR, dir, |sub_h| op_append!(
            inner, sub_h, name, data
        ))
    })
}

pub fn read_chunk_in_pulp_subdir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
    offset: u32,
    buf: &mut [u8],
) -> crate::error::Result<usize> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_subdir!(inner, PULP_DIR, dir, |sub_h| op_read_chunk!(
            inner, sub_h, name, offset, buf
        ))
    })
}

pub fn file_size_in_pulp_subdir(
    sd: &SdStorage,
    dir: &str,
    name: &str,
) -> crate::error::Result<u32> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_subdir!(inner, PULP_DIR, dir, |sub_h| op_file_size!(
            inner, sub_h, name
        ))
    })
}

pub fn delete_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str) -> crate::error::Result<()> {
    poll_once(async {
        let mut guard = borrow(sd)?;
        let inner = &mut *guard;
        in_subdir!(inner, PULP_DIR, dir, |sub_h| op_delete!(inner, sub_h, name))
    })
}

// title mapping

// append a title line to _PULP/TITLES.BIN
pub fn save_title(sd: &SdStorage, filename: &str, title: &str) -> crate::error::Result<()> {
    let name_bytes = filename.as_bytes();
    let title_bytes = title.as_bytes();
    let title_len = title_bytes.len().min(TITLE_CAP);
    let line_len = name_bytes.len() + 1 + title_len + 1; // name + \t + title + \n
    if line_len > 128 {
        return Err(Error::new(
            ErrorKind::WriteFailed,
            "save_title: line too long",
        ));
    }
    let mut line = [0u8; 128];
    line[..name_bytes.len()].copy_from_slice(name_bytes);
    line[name_bytes.len()] = b'\t';
    line[name_bytes.len() + 1..name_bytes.len() + 1 + title_len]
        .copy_from_slice(&title_bytes[..title_len]);
    line[name_bytes.len() + 1 + title_len] = b'\n';

    append_file_in_dir(sd, PULP_DIR, TITLES_FILE, &line[..line_len])
}
