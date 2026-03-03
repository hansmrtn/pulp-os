// kernel handle: async syscall boundary for apps.
//
// every storage method does a synchronous operation then yields to
// the executor, giving other tasks a scheduling opportunity.
//
// app-specific logic (bookmarks, title scan, etc.) accesses the
// underlying caches directly via bookmark_cache() / dir_cache_mut()
// rather than through dedicated handle methods.

use crate::drivers::storage::{self, DirEntry, DirPage};
use crate::kernel::bookmarks::BookmarkCache;
use crate::kernel::dir_cache::DirCache;
use crate::kernel::wake::uptime_secs;

// hides SPI generics from app code; detailed diagnostics go to log::warn
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageError {
    OpenVolume,
    OpenDir,
    OpenFile,
    ReadFailed,
    WriteFailed,
    SeekFailed,
    DeleteFailed,
    DirFull,
    NotFound,
}

impl core::fmt::Display for StorageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OpenVolume => write!(f, "open volume failed"),
            Self::OpenDir => write!(f, "open dir failed"),
            Self::OpenFile => write!(f, "open file failed"),
            Self::ReadFailed => write!(f, "read failed"),
            Self::WriteFailed => write!(f, "write failed"),
            Self::SeekFailed => write!(f, "seek failed"),
            Self::DeleteFailed => write!(f, "delete failed"),
            Self::DirFull => write!(f, "directory full"),
            Self::NotFound => write!(f, "not found"),
        }
    }
}

// yield to executor after a synchronous storage call
macro_rules! yield_op {
    ($e:expr) => {{
        let r = $e;
        embassy_futures::yield_now().await;
        r
    }};
}

fn map_read_err(e: &'static str) -> StorageError {
    if e.contains("volume") {
        StorageError::OpenVolume
    } else if e.contains("dir") {
        StorageError::OpenDir
    } else if e.contains("open file") {
        StorageError::OpenFile
    } else if e.contains("seek") {
        StorageError::SeekFailed
    } else {
        StorageError::ReadFailed
    }
}

fn map_write_err(e: &'static str) -> StorageError {
    if e.contains("volume") {
        StorageError::OpenVolume
    } else if e.contains("dir") && !e.contains("make") {
        StorageError::OpenDir
    } else if e.contains("make dir") {
        StorageError::WriteFailed
    } else if e.contains("open file") || e.contains("create") {
        StorageError::OpenFile
    } else {
        StorageError::WriteFailed
    }
}

// async API surface for apps -- the syscall boundary
//
// borrows the Kernel for the duration of an app lifecycle method;
// no SPI, no generics, no driver types visible to apps
pub struct KernelHandle<'k> {
    pub(crate) kernel: &'k mut super::Kernel,
}

impl<'k> KernelHandle<'k> {
    pub(crate) fn new(kernel: &'k mut super::Kernel) -> Self {
        Self { kernel }
    }

    // root file operations

    pub async fn file_size(&mut self, name: &str) -> Result<u32, StorageError> {
        yield_op!(self.sync_file_size(name).map_err(map_read_err))
    }

    pub async fn read_file_chunk(
        &mut self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, StorageError> {
        yield_op!(
            self.sync_read_chunk(name, offset, buf)
                .map_err(map_read_err)
        )
    }

    pub async fn read_file_start(
        &mut self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), StorageError> {
        yield_op!(self.sync_read_file_start(name, buf).map_err(map_read_err))
    }

    pub async fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), StorageError> {
        yield_op!(storage::write_file(&self.kernel.sd, name, data).map_err(map_write_err))
    }

    pub async fn delete_file(&mut self, name: &str) -> Result<(), StorageError> {
        yield_op!(
            storage::delete_file(&self.kernel.sd, name).map_err(|_| StorageError::DeleteFailed)
        )
    }

    // directory listing (cached)

    pub async fn list_dir(
        &mut self,
        offset: usize,
        buf: &mut [DirEntry],
    ) -> Result<DirPage, StorageError> {
        {
            let k = &mut *self.kernel;
            k.dir_cache.ensure_loaded(&k.sd).map_err(map_read_err)?;
        }
        let page = self.kernel.dir_cache.page(offset, buf);
        embassy_futures::yield_now().await;
        Ok(page)
    }

    pub fn invalidate_dir_cache(&mut self) {
        self.kernel.dir_cache.invalidate();
    }

    // _PULP app-data directory

    pub async fn ensure_app_dir(&mut self) -> Result<(), StorageError> {
        yield_op!(storage::ensure_dir(&self.kernel.sd, storage::PULP_DIR).map_err(map_write_err))
    }

    pub async fn read_app_data_start(
        &mut self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), StorageError> {
        yield_op!(
            self.sync_read_app_data_start(name, buf)
                .map_err(map_read_err)
        )
    }

    pub async fn read_app_data(
        &mut self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, StorageError> {
        yield_op!(
            storage::read_file_chunk_in_dir(&self.kernel.sd, storage::PULP_DIR, name, offset, buf)
                .map_err(map_read_err)
        )
    }

    pub async fn write_app_data(&mut self, name: &str, data: &[u8]) -> Result<(), StorageError> {
        yield_op!(self.sync_write_app_data(name, data).map_err(map_write_err))
    }

    pub async fn ensure_app_subdir(&mut self, dir: &str) -> Result<(), StorageError> {
        yield_op!(self.sync_ensure_app_subdir(dir).map_err(map_write_err))
    }

    pub async fn read_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, StorageError> {
        yield_op!(
            self.sync_read_app_subdir_chunk(dir, name, offset, buf)
                .map_err(map_read_err)
        )
    }

    pub async fn write_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), StorageError> {
        yield_op!(
            self.sync_write_app_subdir(dir, name, data)
                .map_err(map_write_err)
        )
    }

    pub async fn append_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), StorageError> {
        yield_op!(
            self.sync_append_app_subdir(dir, name, data)
                .map_err(map_write_err)
        )
    }

    pub async fn file_size_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
    ) -> Result<u32, StorageError> {
        yield_op!(
            self.sync_file_size_app_subdir(dir, name)
                .map_err(map_read_err)
        )
    }

    pub async fn delete_app_subdir(&mut self, dir: &str, name: &str) -> Result<(), StorageError> {
        yield_op!(
            storage::delete_in_pulp_subdir(&self.kernel.sd, dir, name)
                .map_err(|_| StorageError::DeleteFailed)
        )
    }

    // arbitrary subdirectory reads (non-_PULP)

    pub async fn read_chunk_in_dir(
        &mut self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, StorageError> {
        let result = storage::read_file_chunk_in_dir(&self.kernel.sd, dir, name, offset, buf)
            .map_err(map_read_err);
        embassy_futures::yield_now().await;
        result
    }

    // SD card health

    pub async fn check_sd(&mut self) -> bool {
        let ok = self.kernel.sd.probe_ok();
        self.kernel.sd_ok = ok;
        yield_op!(ok)
    }

    // system info (sync, no I/O)

    #[inline]
    pub fn battery_mv(&self) -> u16 {
        self.kernel.cached_battery_mv
    }

    #[inline]
    pub fn uptime_secs(&self) -> u32 {
        uptime_secs()
    }

    #[inline]
    pub fn sd_ok(&self) -> bool {
        self.kernel.sd_ok
    }

    // smol-epub sync reader bridge
    //
    // smol-epub performs I/O through closures that cannot be async;
    // this provides a scoped sync reader that completes before
    // returning -- no borrows held across any .await point

    pub fn with_sync_reader<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut dyn FnMut(&str, u32, &mut [u8]) -> Result<usize, &'static str>) -> R,
    {
        let sd = &self.kernel.sd;
        let mut reader = |name: &str, offset: u32, buf: &mut [u8]| {
            storage::read_file_chunk(sd, name, offset, buf)
        };
        f(&mut reader)
    }

    pub fn with_sync_reader_app_subdir<F, R>(&mut self, dir: &str, f: F) -> R
    where
        F: FnOnce(&mut dyn FnMut(&str, u32, &mut [u8]) -> Result<usize, &'static str>) -> R,
    {
        let sd = &self.kernel.sd;
        let mut reader = |name: &str, offset: u32, buf: &mut [u8]| {
            storage::read_chunk_in_pulp_subdir(sd, dir, name, offset, buf)
        };
        f(&mut reader)
    }

    // synchronous storage primitives
    //
    // each calls a single storage::* function and returns the raw
    // &'static str error; the async methods above delegate to these
    // adding map_err + yield_now

    #[inline]
    pub fn sync_file_size(&mut self, name: &str) -> Result<u32, &'static str> {
        storage::file_size(&self.kernel.sd, name)
    }

    #[inline]
    pub fn sync_read_chunk(
        &mut self,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_file_chunk(&self.kernel.sd, name, offset, buf)
    }

    #[inline]
    pub fn sync_read_file_start(
        &mut self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), &'static str> {
        storage::read_file_start(&self.kernel.sd, name, buf)
    }

    #[inline]
    pub fn sync_save_title(&mut self, filename: &str, title: &str) -> Result<(), &'static str> {
        storage::save_title(&self.kernel.sd, filename, title)
    }

    #[inline]
    pub fn sync_read_app_data_start(
        &mut self,
        name: &str,
        buf: &mut [u8],
    ) -> Result<(u32, usize), &'static str> {
        storage::read_file_start_in_dir(&self.kernel.sd, storage::PULP_DIR, name, buf)
    }

    #[inline]
    pub fn sync_write_app_data(&mut self, name: &str, data: &[u8]) -> Result<(), &'static str> {
        storage::write_file_in_dir(&self.kernel.sd, storage::PULP_DIR, name, data)
    }

    #[inline]
    pub fn sync_ensure_app_subdir(&mut self, dir: &str) -> Result<(), &'static str> {
        storage::ensure_pulp_subdir(&self.kernel.sd, dir)
    }

    #[inline]
    pub fn sync_read_app_subdir_chunk(
        &mut self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        storage::read_chunk_in_pulp_subdir(&self.kernel.sd, dir, name, offset, buf)
    }

    #[inline]
    pub fn sync_write_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), &'static str> {
        storage::write_in_pulp_subdir(&self.kernel.sd, dir, name, data)
    }

    #[inline]
    pub fn sync_append_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
        data: &[u8],
    ) -> Result<(), &'static str> {
        storage::append_in_pulp_subdir(&self.kernel.sd, dir, name, data)
    }

    #[inline]
    pub fn sync_file_size_app_subdir(
        &mut self,
        dir: &str,
        name: &str,
    ) -> Result<u32, &'static str> {
        storage::file_size_in_pulp_subdir(&self.kernel.sd, dir, name)
    }

    pub fn sync_dir_page(
        &mut self,
        offset: usize,
        buf: &mut [DirEntry],
    ) -> Result<DirPage, &'static str> {
        let k = &mut *self.kernel;
        k.dir_cache.ensure_loaded(&k.sd)?;
        Ok(k.dir_cache.page(offset, buf))
    }

    // direct cache accessors

    #[inline]
    pub fn bookmark_cache(&self) -> &BookmarkCache {
        &*self.kernel.bm_cache
    }

    #[inline]
    pub fn bookmark_cache_mut(&mut self) -> &mut BookmarkCache {
        &mut *self.kernel.bm_cache
    }

    #[inline]
    pub fn dir_cache_mut(&mut self) -> &mut DirCache {
        &mut *self.kernel.dir_cache
    }
}
