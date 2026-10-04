// minimal DirCache: handle.rs names the type; the reader never lists directories
use crate::drivers::sdcard::SdStorage;
use crate::drivers::storage::{DirEntry, DirPage};

pub struct DirCache;

impl DirCache {
    pub const fn new() -> Self {
        Self
    }
    pub fn ensure_loaded(&mut self, _sd: &SdStorage) -> crate::error::Result<()> {
        Ok(())
    }
    pub fn page(&self, _offset: usize, _buf: &mut [DirEntry]) -> DirPage {
        DirPage { total: 0, count: 0 }
    }
    pub fn invalidate(&mut self) {}
}
