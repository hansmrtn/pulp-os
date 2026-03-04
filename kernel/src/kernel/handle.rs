// kernel handle: synchronous syscall boundary for apps
//
// every storage method calls a single storage::* function and returns
// the unified Error result. apps call these directly.
//
// app-specific logic (bookmarks, title scan, etc.) accesses the
// underlying caches directly via bookmark_cache() / dir_cache_mut()
// rather than through dedicated handle methods.

use crate::drivers::storage::{self, DirEntry, DirPage};
use crate::error::{Error, Result};
use crate::kernel::bookmarks::BookmarkCache;
use crate::kernel::dir_cache::DirCache;
use crate::kernel::wake::uptime_secs;

// synchronous API surface for apps
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

    // smol-epub sync reader bridge
    //
    // smol-epub performs I/O through closures that return
    // Result<usize, &'static str>; these adapters convert
    // Error → &'static str at the boundary via the From impl.

    pub fn with_sync_reader<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(
            &mut dyn FnMut(&str, u32, &mut [u8]) -> core::result::Result<usize, &'static str>,
        ) -> R,
    {
        let sd = &self.kernel.sd;
        let mut reader = |name: &str, offset: u32, buf: &mut [u8]| {
            storage::read_file_chunk(sd, name, offset, buf)
                .map_err(|e: Error| -> &'static str { e.into() })
        };
        f(&mut reader)
    }

    pub fn with_sync_reader_app_subdir<F, R>(&mut self, dir: &str, f: F) -> R
    where
        F: FnOnce(
            &mut dyn FnMut(&str, u32, &mut [u8]) -> core::result::Result<usize, &'static str>,
        ) -> R,
    {
        let sd = &self.kernel.sd;
        let mut reader = |name: &str, offset: u32, buf: &mut [u8]| {
            storage::read_chunk_in_pulp_subdir(sd, dir, name, offset, buf)
                .map_err(|e: Error| -> &'static str { e.into() })
        };
        f(&mut reader)
    }

    // storage primitives
    //
    // each calls a single storage::* function; return type is
    // Result<T> (unified Error) throughout

    #[inline]
    pub fn file_size(&mut self, name: &str) -> Result<u32> {
        storage::file_size(&self.kernel.sd, name)
    }

    #[inline]
    pub fn read_chunk(&mut self, name: &str, offset: u32, buf: &mut [u8]) -> Result<usize> {
        storage::read_file_chunk(&self.kernel.sd, name, offset, buf)
    }

    #[inline]
    pub fn read_file_start(&mut self, name: &str, buf: &mut [u8]) -> Result<(u32, usize)> {
        storage::read_file_start(&self.kernel.sd, name, buf)
    }

    #[inline]
    pub fn save_title(&mut self, filename: &str, title: &str) -> Result<()> {
        storage::save_title(&self.kernel.sd, filename, title)
    }

    #[inline]
    pub fn read_app_data_start(&mut self, name: &str, buf: &mut [u8]) -> Result<(u32, usize)> {
        storage::read_file_start_in_dir(&self.kernel.sd, storage::PULP_DIR, name, buf)
    }

    #[inline]
    pub fn write_app_data(&mut self, name: &str, data: &[u8]) -> Result<()> {
        storage::write_file_in_dir(&self.kernel.sd, storage::PULP_DIR, name, data)
    }

    #[inline]
    pub fn ensure_app_subdir(&mut self, dir: &str) -> Result<()> {
        storage::ensure_pulp_subdir(&self.kernel.sd, dir)
    }

    #[inline]
    pub fn read_app_subdir_chunk(
        &mut self,
        dir: &str,
        name: &str,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize> {
        storage::read_chunk_in_pulp_subdir(&self.kernel.sd, dir, name, offset, buf)
    }

    #[inline]
    pub fn write_app_subdir(&mut self, dir: &str, name: &str, data: &[u8]) -> Result<()> {
        storage::write_in_pulp_subdir(&self.kernel.sd, dir, name, data)
    }

    #[inline]
    pub fn append_app_subdir(&mut self, dir: &str, name: &str, data: &[u8]) -> Result<()> {
        storage::append_in_pulp_subdir(&self.kernel.sd, dir, name, data)
    }

    #[inline]
    pub fn file_size_app_subdir(&mut self, dir: &str, name: &str) -> Result<u32> {
        storage::file_size_in_pulp_subdir(&self.kernel.sd, dir, name)
    }

    #[inline]
    pub fn delete_app_subdir(&mut self, dir: &str, name: &str) -> Result<()> {
        storage::delete_in_pulp_subdir(&self.kernel.sd, dir, name)
    }

    pub fn dir_page(&mut self, offset: usize, buf: &mut [DirEntry]) -> Result<DirPage> {
        let k = &mut *self.kernel;
        k.dir_cache.ensure_loaded(&k.sd)?;
        Ok(k.dir_cache.page(offset, buf))
    }

    pub fn invalidate_dir_cache(&mut self) {
        self.kernel.dir_cache.invalidate();
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
