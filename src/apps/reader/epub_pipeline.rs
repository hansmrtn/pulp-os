// epub init, chapter cache pipeline, and background cache state machine

use alloc::vec::Vec;
use core::cell::RefCell;

use smol_epub::cache;
use smol_epub::epub;

use crate::kernel::KernelHandle;
use crate::kernel::work_queue;

use super::{BgCacheState, CHAPTER_CACHE_MAX, EOCD_TAIL, PAGE_BUF, ReaderApp, ZipIndex};

impl ReaderApp {
    pub(super) fn epub_init_zip(&mut self, k: &mut KernelHandle<'_>) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let epub_size = k.sync_file_size(name)?;
        if epub_size < 22 {
            return Err("epub: file too small");
        }
        self.epub_file_size = epub_size;
        self.epub_name_hash = cache::fnv1a(name.as_bytes());
        self.cache_dir = cache::dir_name_for_hash(self.epub_name_hash);

        let tail_size = (epub_size as usize).min(EOCD_TAIL);
        let tail_offset = epub_size - tail_size as u32;
        let n = k.sync_read_chunk(name, tail_offset, &mut self.buf[..tail_size])?;
        let (cd_offset, cd_size) = ZipIndex::parse_eocd(&self.buf[..n], epub_size)?;

        log::info!(
            "epub: CD at offset {} size {} ({} file bytes)",
            cd_offset,
            cd_size,
            epub_size
        );

        let mut cd_buf = Vec::new();
        cd_buf
            .try_reserve_exact(cd_size as usize)
            .map_err(|_| "epub: CD too large for memory")?;
        cd_buf.resize(cd_size as usize, 0);
        super::read_full(k, name, cd_offset, &mut cd_buf)?;
        self.zip.clear();
        self.zip.parse_central_directory(&cd_buf)?;
        drop(cd_buf);

        log::info!("epub: {} entries in ZIP", self.zip.count());

        Ok(())
    }

    pub(super) fn epub_init_opf(&mut self, k: &mut KernelHandle<'_>) -> Result<(), &'static str> {
        let (nb, nl) = self.name_copy();
        let name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let mut opf_path_buf = [0u8; epub::OPF_PATH_CAP];
        let opf_path_len = if let Some(container_idx) = self.zip.find("META-INF/container.xml") {
            let container_data = super::extract_zip_entry(k, name, &self.zip, container_idx)?;
            let len = epub::parse_container(&container_data, &mut opf_path_buf)?;
            drop(container_data);
            len
        } else {
            log::warn!("epub: no container.xml, scanning for .opf");
            epub::find_opf_in_zip(&self.zip, &mut opf_path_buf)?
        };

        let opf_path = core::str::from_utf8(&opf_path_buf[..opf_path_len])
            .map_err(|_| "epub: bad opf path")?;

        log::info!("epub: OPF at {}", opf_path);

        let opf_idx = self
            .zip
            .find(opf_path)
            .or_else(|| self.zip.find_icase(opf_path))
            .ok_or("epub: opf not found in zip")?;
        let opf_data = super::extract_zip_entry(k, name, &self.zip, opf_idx)?;

        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        epub::parse_opf(
            &opf_data,
            opf_dir,
            &self.zip,
            &mut self.meta,
            &mut self.spine,
        )?;

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

            if let Err(e) = k.sync_save_title(name, self.meta.title_str()) {
                log::warn!("epub: failed to save title mapping: {}", e);
            }
        }

        self.toc.clear();

        Ok(())
    }

    pub(super) fn epub_check_cache(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<bool, &'static str> {
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);

        // read into self.buf to avoid ~2 KB stack temporaries
        let meta_cap = cache::META_MAX_SIZE.min(self.buf.len());
        if let Ok(n) =
            k.sync_read_app_subdir_chunk(dir, cache::META_FILE, 0, &mut self.buf[..meta_cap])
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
        k.sync_ensure_app_subdir(dir)?;
        self.cache_chapter = 0;
        Ok(false)
    }

    pub(super) fn epub_finish_cache(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<bool, &'static str> {
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
        k.sync_write_app_subdir(dir, cache::META_FILE, &meta_buf[..meta_len])?;

        self.chapters_cached = true;
        log::info!("epub: cache complete");
        Ok(false)
    }

    // synchronously cache a single chapter by index; skipped if already cached
    pub(super) fn epub_cache_single_chapter(
        &mut self,
        k: &mut KernelHandle<'_>,
        ch: usize,
    ) -> Result<(), &'static str> {
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

        k.sync_write_app_subdir(dir, ch_str, &[])?;
        let text_size = {
            let k_cell = RefCell::new(&mut *k);
            cache::stream_strip_entry(
                &entry,
                entry.local_offset,
                |offset, buf| k_cell.borrow_mut().sync_read_chunk(epub_name, offset, buf),
                |chunk| {
                    k_cell
                        .borrow_mut()
                        .sync_append_app_subdir(dir, ch_str, chunk)
                },
            )?
        };

        self.chapter_sizes[ch] = text_size;
        self.ch_cached[ch] = true;

        log::info!(
            "epub: sync-cached ch{}/{} = {} bytes",
            ch,
            self.spine.len(),
            text_size
        );
        Ok(())
    }

    // extract chapter XHTML from ZIP and dispatch to worker for HTML stripping
    pub(super) fn epub_dispatch_chapter_strip(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<bool, &'static str> {
        let spine_len = self.spine.len();

        // advance past chapters that were already sync-cached
        while (self.cache_chapter as usize) < spine_len
            && self.ch_cached[self.cache_chapter as usize]
        {
            self.cache_chapter += 1;
        }

        // priority: sync-cache chapters adjacent to the reading position
        // before continuing the sequential scan, so forward/backward
        // chapter navigation is always instant
        let reading_ch = self.chapter as usize;
        for &adj in &[reading_ch + 1, reading_ch.saturating_sub(1)] {
            if adj < spine_len && adj != reading_ch && !self.ch_cached[adj] {
                log::info!(
                    "epub: priority cache ch{} (adjacent to ch{})",
                    adj,
                    reading_ch,
                );
                if let Err(e) = self.epub_cache_single_chapter(k, adj) {
                    log::warn!("epub: priority cache ch{} failed: {}", adj, e);
                }
            }
        }

        let ch = self.cache_chapter as usize;
        if ch >= spine_len {
            return self.epub_finish_cache(k);
        }

        // large chapters need ~2x their uncompressed size in heap
        // (extract Vec + strip output Vec simultaneously); on a 140 KB
        // heap anything over ~32 KB risks OOM in the worker; fall back
        // to the streaming pipeline which uses fixed ~51 KB overhead
        const ASYNC_THRESHOLD: u32 = 32768;
        let entry_idx = self.spine.items[ch] as usize;
        let uncomp = self.zip.entry(entry_idx).uncomp_size;
        if uncomp > ASYNC_THRESHOLD {
            log::info!(
                "epub: ch{}/{} large ({} bytes), sync-caching",
                ch,
                spine_len,
                uncomp,
            );
            self.epub_cache_single_chapter(k, ch)?;
            self.cache_chapter += 1;
            return Ok(true);
        }

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let ch_file = cache::chapter_file_name(ch as u16);
        let ch_str = cache::chapter_file_str(&ch_file);

        // truncate any stale data before the worker produces output
        k.sync_write_app_subdir(dir, ch_str, &[])?;

        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        // extract full XHTML into memory; if OOM fall back to sync
        let xhtml = match super::extract_zip_entry(k, epub_name, &self.zip, entry_idx) {
            Ok(data) => data,
            Err(e) => {
                log::info!(
                    "epub: ch{}/{} extract failed ({}), sync-caching",
                    ch,
                    spine_len,
                    e,
                );
                self.epub_cache_single_chapter(k, ch)?;
                self.cache_chapter += 1;
                return Ok(true);
            }
        };

        log::info!(
            "epub: dispatch ch{}/{} ({} bytes XHTML) to worker",
            ch,
            spine_len,
            xhtml.len()
        );

        let task = work_queue::WorkTask::StripChapter {
            chapter_idx: ch as u16,
            xhtml,
        };
        if !work_queue::submit(self.work_gen, task) {
            return Err("cache: worker channel full");
        }
        Ok(true)
    }

    // poll worker for a completed chapter-strip result
    pub(super) fn epub_recv_chapter_strip(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<Option<bool>, &'static str> {
        let result = match work_queue::try_recv() {
            Some(r) if r.is_current() => r,
            Some(_) => return Ok(None), // stale generation -- discard
            None => return Ok(None),    // worker still busy
        };

        match result.outcome {
            work_queue::WorkOutcome::ChapterReady { chapter_idx, text } => {
                let ch = chapter_idx as usize;
                let text_size = text.len() as u32;

                // if the user sync-cached this chapter while the worker
                // was processing, skip the SD write
                if !self.ch_cached[ch] {
                    let dir_buf = self.cache_dir;
                    let dir = cache::dir_name_str(&dir_buf);
                    let ch_file = cache::chapter_file_name(chapter_idx);
                    let ch_str = cache::chapter_file_str(&ch_file);

                    k.sync_write_app_subdir(dir, ch_str, &text)?;
                    self.chapter_sizes[ch] = text_size;
                }
                self.ch_cached[ch] = true;
                drop(text);

                log::info!(
                    "epub: cached ch{}/{} = {} bytes",
                    ch,
                    self.spine.len(),
                    text_size
                );

                self.cache_chapter += 1;

                if (self.cache_chapter as usize) < self.spine.len() {
                    Ok(Some(true))
                } else {
                    self.epub_finish_cache(k)?;
                    Ok(Some(false))
                }
            }
            work_queue::WorkOutcome::ChapterFailed { chapter_idx, error } => {
                let ch = chapter_idx as usize;
                log::warn!(
                    "epub: worker failed ch{} ({}), falling back to sync",
                    ch,
                    error,
                );
                // streaming pipeline uses fixed ~51 KB overhead -- won't OOM
                if let Err(e) = self.epub_cache_single_chapter(k, ch) {
                    log::warn!("epub: sync fallback also failed ch{}: {}", ch, e);
                }
                self.cache_chapter += 1;

                if (self.cache_chapter as usize) < self.spine.len() {
                    Ok(Some(true))
                } else {
                    self.epub_finish_cache(k)?;
                    Ok(Some(false))
                }
            }
            _ => {
                // unexpected result type -- discard and keep waiting
                log::warn!("epub: unexpected result while waiting for chapter strip");
                Ok(None)
            }
        }
    }

    pub(super) fn epub_index_chapter(&mut self) {
        self.reset_paging();
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
            match k.sync_read_app_subdir_chunk(
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

    // run one step of background caching; returns true if self.buf was dirtied
    pub(super) fn bg_cache_step(&mut self, k: &mut KernelHandle<'_>) -> bool {
        match self.bg_cache {
            BgCacheState::CacheChapter => {
                match self.epub_dispatch_chapter_strip(k) {
                    Ok(true) => self.bg_cache = BgCacheState::WaitChapter,
                    Ok(false) => {
                        // all chapters cached; start image scan from
                        // the current reading chapter
                        self.img_cache_ch = self.chapter;
                        self.img_cache_offset = 0;
                        self.img_scan_wrapped = false;
                        self.bg_cache = BgCacheState::CacheImage;
                    }
                    Err(e) => {
                        log::warn!("bg: ch dispatch failed: {}, skipping", e);
                        self.cache_chapter += 1;
                        // stay in CacheChapter; next tick tries the next one
                    }
                }
                false
            }
            BgCacheState::WaitChapter => {
                match self.epub_recv_chapter_strip(k) {
                    Ok(Some(true)) => {
                        // after caching a chapter, try dispatching a nearby
                        // image before continuing with the next chapter
                        if self.try_dispatch_nearby_image(k) {
                            self.bg_cache = BgCacheState::WaitNearbyImage;
                        } else {
                            self.bg_cache = BgCacheState::CacheChapter;
                        }
                    }
                    Ok(Some(false)) => {
                        self.img_cache_ch = self.chapter;
                        self.img_cache_offset = 0;
                        self.img_scan_wrapped = false;
                        self.bg_cache = BgCacheState::CacheImage;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("bg: ch recv failed: {}, continuing", e);
                        self.bg_cache = BgCacheState::CacheChapter;
                    }
                }
                false
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
                false
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
                // image scanning uses the prefetch buffer, leaving
                // self.buf (current page data) untouched
                false
            }
            BgCacheState::WaitImage => {
                match self.epub_recv_image_result(k) {
                    Ok(Some(_)) => self.bg_cache = BgCacheState::CacheImage,
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("bg: image recv error: {}", e);
                        self.bg_cache = BgCacheState::CacheImage;
                    }
                }
                false
            }
            BgCacheState::Idle => false,
        }
    }
}
