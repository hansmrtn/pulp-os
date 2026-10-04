// host SD shim: an in-memory FAT-less file store behind the same `SdStorage`
// name the kernel uses. Directory layout mirrors the card: files in the root,
// `_PULP/` (settings, bookmarks, caches) and `_PULP/<sub>/`.
//
// Semantics follow kernel/src/drivers/storage.rs over embedded-sdmmc (see the
// op_* macros there): missing dir -> OpenDir, missing file -> OpenFile, read at
// offset > size -> SeekFailed, read at offset == size -> Ok(0), write truncates,
// append creates, write_at seeks within [0, size] (else SeekFailed).
// 8.3 names are compared case-insensitively (FAT).
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ErrorKind};

#[derive(Default, Clone)]
pub struct FakeFs {
    files: BTreeMap<String, Vec<u8>>,
    dirs: BTreeSet<String>,
}

fn key(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_ascii_uppercase()
    } else {
        format!("{}/{}", dir.to_ascii_uppercase(), name.to_ascii_uppercase())
    }
}

impl FakeFs {
    pub fn new() -> Self {
        Self::default()
    }

    // host-side set-up / inspection (not part of the firmware API)
    pub fn put(&mut self, dir: &str, name: &str, data: &[u8]) {
        let mut acc = String::new();
        for part in dir.split('/').filter(|p| !p.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(&part.to_ascii_uppercase());
            self.dirs.insert(acc.clone());
        }
        self.files.insert(key(dir, name), data.to_vec());
    }

    pub fn get(&self, dir: &str, name: &str) -> Option<&Vec<u8>> {
        self.files.get(&key(dir, name))
    }

    pub fn remove(&mut self, dir: &str, name: &str) -> bool {
        self.files.remove(&key(dir, name)).is_some()
    }

    pub fn names(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    fn has_dir(&self, dir: &str) -> bool {
        dir.is_empty() || self.dirs.contains(&dir.to_ascii_uppercase())
    }

    pub fn mkdir(&mut self, dir: &str) {
        self.dirs.insert(dir.to_ascii_uppercase());
    }

    pub(crate) fn read(
        &self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        let Some(f) = self.files.get(&key(dir, name)) else {
            return Err(Error::new(ErrorKind::OpenFile, "read_chunk"));
        };
        let off = offset as usize;
        if off > f.len() {
            return Err(Error::new(ErrorKind::SeekFailed, "read_chunk"));
        }
        let n = buf.len().min(f.len() - off);
        buf[..n].copy_from_slice(&f[off..off + n]);
        Ok(n)
    }

    pub(crate) fn read_start(
        &self,
        dir: &str,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        let Some(f) = self.files.get(&key(dir, name)) else {
            return Err(Error::new(ErrorKind::OpenFile, "read_start"));
        };
        let n = buf.len().min(f.len());
        buf[..n].copy_from_slice(&f[..n]);
        Ok((f.len() as u32, n))
    }

    pub(crate) fn size(&self, dir: &str, name: &str) -> Result<u32, Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        self.files
            .get(&key(dir, name))
            .map(|f| f.len() as u32)
            .ok_or(Error::new(ErrorKind::OpenFile, "file_size"))
    }

    pub(crate) fn write(&mut self, dir: &str, name: &str, data: &[u8]) -> Result<(), Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        self.files.insert(key(dir, name), data.to_vec());
        Ok(())
    }

    pub(crate) fn append(&mut self, dir: &str, name: &str, data: &[u8]) -> Result<(), Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        self.files
            .entry(key(dir, name))
            .or_default()
            .extend_from_slice(data);
        Ok(())
    }

    pub(crate) fn write_at(
        &mut self,
        dir: &str,
        name: &str,
        offset: u32,
        data: &[u8],
    ) -> Result<(), Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        let f = self.files.entry(key(dir, name)).or_default();
        let off = offset as usize;
        if off > f.len() {
            return Err(Error::new(ErrorKind::SeekFailed, "write_at"));
        }
        if off + data.len() > f.len() {
            f.resize(off + data.len(), 0);
        }
        f[off..off + data.len()].copy_from_slice(data);
        Ok(())
    }

    pub(crate) fn delete(&mut self, dir: &str, name: &str) -> Result<(), Error> {
        if !self.has_dir(dir) {
            return Err(Error::new(ErrorKind::OpenDir, "in_dir"));
        }
        self.files
            .remove(&key(dir, name))
            .map(|_| ())
            .ok_or(Error::new(ErrorKind::DeleteFailed, "delete"))
    }
}

// Same name and role as the kernel's SdStorage: the object every storage call
// borrows. `empty()` is the no-card case (NoCard on every call).
pub struct SdStorage {
    fs: RefCell<Option<FakeFs>>,
    reads: Cell<u64>,
    fail_reads: Cell<u32>,
}

impl SdStorage {
    pub fn mounted(fs: FakeFs) -> Self {
        Self {
            fs: RefCell::new(Some(fs)),
            reads: Cell::new(0),
            fail_reads: Cell::new(0),
        }
    }

    pub fn empty() -> Self {
        Self {
            fs: RefCell::new(None),
            reads: Cell::new(0),
            fail_reads: Cell::new(0),
        }
    }

    pub fn is_mounted(&self) -> bool {
        self.fs.borrow().is_some()
    }

    // host-side: inspect or edit the card contents between steps
    pub fn with_fs<R>(&self, f: impl FnOnce(&mut FakeFs) -> R) -> Option<R> {
        self.fs.borrow_mut().as_mut().map(f)
    }

    // host-side: copy of the card contents (what a reboot finds on the card)
    pub fn snapshot(&self) -> Option<FakeFs> {
        self.fs.borrow().clone()
    }

    // host-side: pull the card (subsequent calls fail with NoCard)
    pub fn eject(&self) {
        *self.fs.borrow_mut() = None;
    }

    // host-side: make the next `n` reads fail with ReadFailed
    pub fn fail_next_reads(&self, n: u32) {
        self.fail_reads.set(n);
    }

    pub fn read_count(&self) -> u64 {
        self.reads.get()
    }

    pub(crate) fn run<R>(&self, f: impl FnOnce(&mut FakeFs) -> Result<R, Error>) -> Result<R, Error> {
        let mut g = self.fs.borrow_mut();
        match g.as_mut() {
            Some(fs) => f(fs),
            None => Err(Error::new(ErrorKind::NoCard, "storage::borrow")),
        }
    }

    pub(crate) fn run_read<R>(
        &self,
        f: impl FnOnce(&FakeFs) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let g = self.fs.borrow();
        match g.as_ref() {
            Some(fs) => {
                self.reads.set(self.reads.get() + 1);
                let left = self.fail_reads.get();
                if left > 0 {
                    self.fail_reads.set(left - 1);
                    return Err(Error::new(ErrorKind::ReadFailed, "read_chunk"));
                }
                f(fs)
            }
            None => Err(Error::new(ErrorKind::NoCard, "storage::borrow")),
        }
    }
}
