// epub init, chapter cache pipeline, and background cache state machine

use alloc::vec::Vec;
use core::cell::RefCell;

use smol_epub::cache;
use smol_epub::epub;

use crate::error::{Error, ErrorKind};
use crate::kernel::KernelHandle;
use crate::kernel::work_queue;

use super::{BgCacheState, CHAPTER_CACHE_MAX, EOCD_TAIL, PAGE_BUF, ReaderApp, ZipIndex};

// one cell shared between reader and writer; safe because
// stream_strip_entry_async never borrows both simultaneously
struct CellReader<'a, 'k>(&'a RefCell<&'a mut KernelHandle<'k>>, &'a str);
struct CellWriter<'a, 'k>(&'a RefCell<&'a mut KernelHandle<'k>>, &'a str, &'a str);

impl smol_epub::async_io::AsyncReadAt for CellReader<'_, '_> {
    async fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.0
            .borrow_mut()
            .read_chunk(self.1, offset, buf)
            .map_err(|e: Error| -> &'static str { e.into() })
    }
}

impl smol_epub::async_io::AsyncWriteChunk for CellWriter<'_, '_> {
    async fn write_chunk(&mut self, data: &[u8]) -> Result<(), &'static str> {
        self.0
            .borrow_mut()
            .append_app_subdir(self.1, self.2, data)
            .map_err(|e: Error| -> &'static str { e.into() })
    }
}

impl ReaderApp {
    pub(super) fn epub_init_zip(&mut self, k: &mut KernelHandle<'_>) -> crate::error::Result<()> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let epub_size = k.file_size(name)?;
        if epub_size < 22 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "epub_init_zip: too small",
            ));
        }
        self.epub_file_size = epub_size;
        self.epub_name_hash = cache::fnv1a(name.as_bytes());
        self.cache_dir = cache::dir_name_for_hash(self.epub_name_hash);

        let tail_size = (epub_size as usize).min(EOCD_TAIL);
        let tail_offset = epub_size - tail_size as u32;
        let n = k.read_chunk(name, tail_offset, &mut self.buf[..tail_size])?;
        let (cd_offset, cd_size) = ZipIndex::parse_eocd(&self.buf[..n], epub_size)
            .map_err(|_| Error::new(ErrorKind::ParseFailed, "epub_init_zip: EOCD"))?;

        log::info!(
            "epub: CD at offset {} size {} ({} file bytes)",
            cd_offset,
            cd_size,
            epub_size
        );

        let mut cd_buf = Vec::new();
        cd_buf
            .try_reserve_exact(cd_size as usize)
            .map_err(|_| Error::new(ErrorKind::OutOfMemory, "epub_init_zip: CD alloc"))?;
        cd_buf.resize(cd_size as usize, 0);
        super::read_full(k, name, cd_offset, &mut cd_buf)?;
        self.zip.clear();
        self.zip
            .parse_central_directory(&cd_buf)
            .map_err(|_| Error::new(ErrorKind::ParseFailed, "epub_init_zip: CD parse"))?;
        drop(cd_buf);

        log::info!("epub: {} entries in ZIP", self.zip.count());

        Ok(())
    }

    pub(super) fn epub_init_opf(&mut self, k: &mut KernelHandle<'_>) -> crate::error::Result<()> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let mut opf_path_buf = [0u8; epub::OPF_PATH_CAP];
        let opf_path_len = if let Some(container_idx) = self.zip.find("META-INF/container.xml") {
            let container_data = super::extract_zip_entry(k, name, &self.zip, container_idx)
                .map_err(|_| Error::new(ErrorKind::ReadFailed, "epub_init_opf: container read"))?;
            let len = epub::parse_container(&container_data, &mut opf_path_buf).map_err(|_| {
                Error::new(ErrorKind::ParseFailed, "epub_init_opf: container parse")
            })?;
            drop(container_data);
            len
        } else {
            log::warn!("epub: no container.xml, scanning for .opf");
            epub::find_opf_in_zip(&self.zip, &mut opf_path_buf)
                .map_err(|_| Error::new(ErrorKind::NotFound, "epub_init_opf: no .opf in zip"))?
        };

        let opf_path = core::str::from_utf8(&opf_path_buf[..opf_path_len])
            .map_err(|_| Error::new(ErrorKind::BadEncoding, "epub_init_opf: OPF path"))?;

        log::info!("epub: OPF at {}", opf_path);

        let opf_idx = self
            .zip
            .find(opf_path)
            .or_else(|| self.zip.find_icase(opf_path))
            .ok_or(Error::new(ErrorKind::NotFound, "epub_init_opf: OPF entry"))?;
        let opf_data = super::extract_zip_entry(k, name, &self.zip, opf_idx)
            .map_err(|_| Error::new(ErrorKind::ReadFailed, "epub_init_opf: OPF read"))?;

        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        epub::parse_opf(
            &opf_data,
            opf_dir,
            &self.zip,
            &mut self.meta,
            &mut self.spine,
        )
        .map_err(|_| Error::new(ErrorKind::ParseFailed, "epub_init_opf: OPF parse"))?;

        // defer TOC to NeedToc to avoid stack overflow while OPF is live
        self.toc_source = epub::find_toc_source(&opf_data, opf_dir, &self.zip);
        drop(opf_data);

        log::info!(
            "epub: \"{}\" by {} -- {} chapters",
            self.meta.title_str(),
            self.meta.author_str(),
            self.spine.len()
        );

        let tlen = self.meta.title_len as usize;
        if tlen > 0 {
            let n = tlen.min(self.title.len());
            self.title[..n].copy_from_slice(&self.meta.title[..n]);
            self.title_len = n;

            if let Err(e) = k.save_title(name, self.meta.title_str()) {
                log::warn!("epub: failed to save title mapping: {}", e);
            }
        }

        self.toc.clear();

        Ok(())
    }

    pub(super) fn epub_check_cache(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<bool> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);

        // read into self.buf to avoid ~2 KB stack temporaries
        let meta_cap = cache::META_MAX_SIZE.min(self.buf.len());
        if let Ok(n) = k.read_app_subdir_chunk(dir, cache::META_FILE, 0, &mut self.buf[..meta_cap])
            && let Ok(count) = cache::parse_cache_meta(
                &self.buf[..n],
                self.epub_file_size,
                self.epub_name_hash,
                self.spine.len(),
                &mut self.chapter_sizes,
            )
        {
            self.chapters_cached = true;
            for i in 0..count {
                self.ch_cached[i] = true;
            }
            log::info!("epub: cache hit ({} chapters)", count);
            return Ok(true);
        }

        log::info!("epub: building cache for {} chapters", self.spine.len());
        k.ensure_app_subdir(dir)?;
        self.cache_chapter = 0;
        Ok(false)
    }

    pub(super) fn epub_finish_cache(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<bool> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let spine_len = self.spine.len();

        let mut meta_buf = [0u8; cache::META_MAX_SIZE];
        let meta_len = cache::encode_cache_meta(
            self.epub_file_size,
            self.epub_name_hash,
            &self.chapter_sizes[..spine_len],
            &mut meta_buf,
        );
        k.write_app_subdir(dir, cache::META_FILE, &meta_buf[..meta_len])?;

        self.chapters_cached = true;
        log::info!("epub: cache complete");
        Ok(false)
    }

    // async streaming chapter cache; used for both initial and background
    // caching. decompresses, strips html, and writes chunks to sd without
    // ever holding full xhtml in ram. yields between decompression
    // iterations so the scheduler's select(run_background, input) can
    // interrupt on user input (e.g. pressing back during book open)
    pub(super) async fn epub_cache_chapter_async(
        &mut self,
        k: &mut KernelHandle<'_>,
        ch: usize,
    ) -> crate::error::Result<()> {
        if ch >= self.spine.len() || self.ch_cached[ch] {
            return Ok(());
        }

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");
        let entry_idx = self.spine.items[ch] as usize;
        let entry = *self.zip.entry(entry_idx);
        let ch_file = cache::chapter_file_name(ch as u16);
        let ch_str = cache::chapter_file_str(&ch_file);

        // truncate stale data before streaming begins
        k.write_app_subdir(dir, ch_str, &[])?;

        let k_cell = RefCell::new(&mut *k);

        let mut reader = CellReader(&k_cell, epub_name);
        let mut writer = CellWriter(&k_cell, dir, ch_str);

        let text_size = smol_epub::async_io::stream_strip_entry_async(
            &entry,
            entry.local_offset,
            &mut reader,
            &mut writer,
        )
        .await
        .map_err(|msg| Error::from(msg).with_source("epub_cache_chapter_async: stream"))?;

        self.chapter_sizes[ch] = text_size;
        self.ch_cached[ch] = true;

        log::info!(
            "epub: cached ch{}/{} = {} bytes",
            ch,
            self.spine.len(),
            text_size
        );
        Ok(())
    }

    pub(super) fn epub_index_chapter(&mut self) {
        self.reset_paging();
        // force reload; ch_cache may hold a different chapter's data
        // with the same byte count (try_cache_chapter only checks len)
        self.ch_cache = Vec::new();
        let ch = self.chapter as usize;
        self.file_size = if ch < cache::MAX_CACHE_CHAPTERS {
            self.chapter_sizes[ch]
        } else {
            0
        };
        log::info!(
            "epub: index chapter {}/{} ({} bytes cached text)",
            self.chapter + 1,
            self.spine.len(),
            self.file_size,
        );
    }

    pub(super) fn try_cache_chapter(&mut self, k: &mut KernelHandle<'_>) -> bool {
        if !self.is_epub || !self.chapters_cached {
            return false;
        }

        let ch = self.chapter as usize;
        let ch_size = if ch < cache::MAX_CACHE_CHAPTERS {
            self.chapter_sizes[ch] as usize
        } else {
            return false;
        };

        if ch_size == 0 || ch_size > CHAPTER_CACHE_MAX {
            self.ch_cache = Vec::new();
            return false;
        }

        if self.ch_cache.len() == ch_size {
            log::info!("chapter cache: reusing {} bytes in RAM", ch_size);
            return true;
        }

        self.ch_cache = Vec::new();
        if self.ch_cache.try_reserve_exact(ch_size).is_err() {
            log::info!("chapter cache: OOM for {} bytes", ch_size);
            return false;
        }
        self.ch_cache.resize(ch_size, 0);

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let ch_file = cache::chapter_file_name(self.chapter);
        let ch_str = cache::chapter_file_str(&ch_file);

        let mut pos = 0usize;
        while pos < ch_size {
            let chunk = (ch_size - pos).min(PAGE_BUF);
            match k.read_app_subdir_chunk(
                dir,
                ch_str,
                pos as u32,
                &mut self.ch_cache[pos..pos + chunk],
            ) {
                Ok(n) if n > 0 => pos += n,
                Ok(_) => break,
                Err(e) => {
                    log::info!("chapter cache: SD read failed at {}: {}", pos, e);
                    self.ch_cache = Vec::new();
                    return false;
                }
            }
        }

        log::info!(
            "chapter cache: loaded ch{} ({} bytes) into RAM",
            self.chapter,
            ch_size,
        );
        true
    }

    // run one step of background caching; async because CacheChapter
    // awaits epub_cache_chapter_async which yields during deflate
    pub(super) async fn bg_cache_step(&mut self, k: &mut KernelHandle<'_>) {
        match self.bg_cache {
            BgCacheState::CacheChapter => {
                let spine_len = self.spine.len();

                // skip chapters already cached
                while (self.cache_chapter as usize) < spine_len
                    && self.ch_cached[self.cache_chapter as usize]
                {
                    self.cache_chapter += 1;
                }

                // priority: cache chapters adjacent to reading position
                // before continuing the sequential scan; forward/backward
                // nav stays instant
                let reading_ch = self.chapter as usize;
                for &adj in &[reading_ch + 1, reading_ch.saturating_sub(1)] {
                    if adj < spine_len && adj != reading_ch && !self.ch_cached[adj] {
                        log::info!(
                            "epub: priority cache ch{} (adjacent to ch{})",
                            adj,
                            reading_ch,
                        );
                        if let Err(e) = self.epub_cache_chapter_async(k, adj).await {
                            log::warn!("epub: priority ch{} failed: {}", adj, e);
                        }
                    }
                }

                let ch = self.cache_chapter as usize;
                if ch >= spine_len {
                    let _ = self.epub_finish_cache(k);
                    self.img_cache_ch = self.chapter;
                    self.img_cache_offset = 0;
                    self.img_scan_wrapped = false;
                    self.bg_cache = BgCacheState::CacheImage;
                    return;
                }

                match self.epub_cache_chapter_async(k, ch).await {
                    Ok(()) => {
                        self.cache_chapter += 1;
                        // try nearby image dispatch before next chapter
                        if self.try_dispatch_nearby_image(k) {
                            self.bg_cache = BgCacheState::WaitNearbyImage;
                        }
                        // else stay in CacheChapter
                    }
                    Err(e) => {
                        log::warn!("bg: ch{} failed: {}, skipping", ch, e);
                        self.cache_chapter += 1;
                    }
                }
            }

            BgCacheState::WaitNearbyImage => {
                match self.epub_recv_image_result(k) {
                    Ok(Some(_)) => {
                        if self.try_dispatch_nearby_image(k) {
                            // stay in WaitNearbyImage
                        } else {
                            self.bg_cache = BgCacheState::CacheChapter;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("bg: nearby image error: {}, continuing", e);
                        self.bg_cache = BgCacheState::CacheChapter;
                    }
                }
            }
            BgCacheState::CacheImage => {
                match self.epub_find_and_dispatch_image(k) {
                    Ok(true) => {
                        // worker busy: dispatched a small image, wait
                        // worker idle: decoded inline, scan next tick
                        if !work_queue::is_idle() {
                            self.bg_cache = BgCacheState::WaitImage;
                        }
                    }
                    Ok(false) => self.bg_cache = BgCacheState::Idle,
                    Err(e) => {
                        log::warn!("bg: image error: {}, continuing", e);
                        // stay in CacheImage; next tick scans for the next one
                    }
                }
            }
            BgCacheState::WaitImage => match self.epub_recv_image_result(k) {
                Ok(Some(_)) => self.bg_cache = BgCacheState::CacheImage,
                Ok(None) => {}
                Err(e) => {
                    log::warn!("bg: image recv error: {}", e);
                    self.bg_cache = BgCacheState::CacheImage;
                }
            },
            BgCacheState::Idle => {}
        }
    }
}
