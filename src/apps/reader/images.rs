// image decode, cache, and dispatch

extern crate alloc;

use alloc::vec::Vec;
use core::cell::RefCell;

use smol_epub::DecodedImage;
use smol_epub::cache;
use smol_epub::epub;
use smol_epub::html_strip::{IMG_REF, MARKER};
use smol_epub::zip::{self, ZipIndex};

use crate::kernel::KernelHandle;
use crate::kernel::work_queue;

use super::{
    IMAGE_DISPLAY_H, NO_PREFETCH, PAGE_BUF, PRECACHE_IMG_MAX, ReaderApp, TEXT_AREA_H, TEXT_W,
};

impl ReaderApp {
    pub(super) fn decode_page_images(&mut self, k: &mut KernelHandle<'_>) {
        self.page_img = None;
        self.fullscreen_img = false;

        if !self.is_epub || self.spine.is_empty() {
            return;
        }

        {
            let mut has_img = false;
            let mut has_text = false;
            for i in 0..self.line_count {
                if self.lines[i].is_image() {
                    if self.lines[i].is_image_origin() {
                        has_img = true;
                    }
                } else if self.lines[i].len > 0 {
                    has_text = true;
                }
            }
            self.fullscreen_img = has_img && !has_text;
        }

        // copy src path to a local buf to avoid borrowing self.buf below
        let mut src_buf = [0u8; 128];
        let mut src_len = 0usize;
        for i in 0..self.line_count {
            if self.lines[i].is_image_origin() {
                let start = self.lines[i].start as usize;
                let len = self.lines[i].len as usize;
                if start + len <= self.buf_len {
                    let n = len.min(src_buf.len());
                    src_buf[..n].copy_from_slice(&self.buf[start..start + n]);
                    src_len = n;
                }
                break;
            }
        }

        if src_len == 0 {
            return;
        }

        let src_str = match core::str::from_utf8(&src_buf[..src_len]) {
            Ok(s) => s,
            Err(_) => return,
        };

        log::info!("reader: decoding image: {}", src_str);

        let ch_zip_idx = self.spine.items[self.chapter as usize] as usize;
        let ch_path = self.zip.entry_name(ch_zip_idx);
        let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

        let mut path_buf = [0u8; 512];
        let path_len = epub::resolve_path(ch_dir, src_str, &mut path_buf);
        let full_path = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return,
        };

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let img_name = img_cache_name(cache::fnv1a(full_path.as_bytes()));
        let img_file = img_cache_str(&img_name);

        if let Ok(img) = load_cached_image(k, dir, img_file) {
            log::info!(
                "reader: image cache hit {} ({}x{})",
                img_file,
                img.width,
                img.height
            );
            self.page_img = Some(img);
            return;
        }

        let zip_idx = match self
            .zip
            .find(full_path)
            .or_else(|| self.zip.find_icase(full_path))
        {
            Some(idx) => idx,
            None => {
                log::warn!("reader: image not in ZIP: {}", full_path);
                return;
            }
        };

        let entry = *self.zip.entry(zip_idx);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let data_offset = {
            let mut hdr = [0u8; 30];
            if k.sync_read_chunk(epub_name, entry.local_offset, &mut hdr)
                .is_err()
            {
                log::warn!("reader: failed to read ZIP local header");
                return;
            }
            match ZipIndex::local_header_data_skip(&hdr) {
                Ok(skip) => entry.local_offset + skip,
                Err(e) => {
                    log::warn!("reader: {}", e);
                    return;
                }
            }
        };

        let ext_jpeg = full_path.ends_with(".jpg")
            || full_path.ends_with(".jpeg")
            || full_path.ends_with(".JPG")
            || full_path.ends_with(".JPEG");
        let ext_png = full_path.ends_with(".png") || full_path.ends_with(".PNG");

        let (is_jpeg, is_png) = if ext_jpeg || ext_png {
            (ext_jpeg, ext_png)
        } else if entry.method == zip::METHOD_STORED {
            let mut magic = [0u8; 8];
            let n = k
                .sync_read_chunk(epub_name, data_offset, &mut magic)
                .unwrap_or(0);
            (
                n >= 2 && magic[0] == 0xFF && magic[1] == 0xD8,
                n >= 8 && magic[..8] == [137, 80, 78, 71, 13, 10, 26, 10],
            )
        } else {
            (false, false)
        };

        if !is_jpeg && !is_png {
            log::warn!("reader: unsupported image format: {}", full_path);
            return;
        }

        let img_max_h = if self.fullscreen_img {
            TEXT_AREA_H
        } else {
            IMAGE_DISPLAY_H
        };

        let do_decode = |k_ref: &mut KernelHandle<'_>| -> Result<DecodedImage, &'static str> {
            let k_cell = RefCell::new(k_ref);
            if is_jpeg && entry.method == zip::METHOD_STORED {
                smol_epub::jpeg::decode_jpeg_sd(
                    |off, buf| k_cell.borrow_mut().sync_read_chunk(epub_name, off, buf),
                    data_offset,
                    entry.uncomp_size,
                    TEXT_W as u16,
                    img_max_h,
                )
            } else if is_jpeg {
                smol_epub::jpeg::decode_jpeg_deflate_sd(
                    |off, buf| k_cell.borrow_mut().sync_read_chunk(epub_name, off, buf),
                    data_offset,
                    entry.comp_size,
                    entry.uncomp_size,
                    TEXT_W as u16,
                    img_max_h,
                )
            } else if entry.method == zip::METHOD_STORED {
                smol_epub::png::decode_png_sd(
                    |off, buf| k_cell.borrow_mut().sync_read_chunk(epub_name, off, buf),
                    data_offset,
                    entry.uncomp_size,
                    TEXT_W as u16,
                    img_max_h,
                )
            } else {
                smol_epub::png::decode_png_deflate_sd(
                    |off, buf| k_cell.borrow_mut().sync_read_chunk(epub_name, off, buf),
                    data_offset,
                    entry.comp_size,
                    TEXT_W as u16,
                    img_max_h,
                )
            }
        };

        let result = do_decode(k);

        // OOM fallback: release chapter cache and retry
        let result = match result {
            Ok(img) => Ok(img),
            Err(e) if !self.ch_cache.is_empty() => {
                log::info!(
                    "reader: decode failed ({}), releasing {} KB chapter cache and retrying",
                    e,
                    self.ch_cache.len() / 1024,
                );
                self.ch_cache = Vec::new();
                do_decode(k)
            }
            Err(e) => Err(e),
        };

        match result {
            Ok(img) => {
                log::info!(
                    "reader: decoded {}x{} image ({} bytes 1-bit)",
                    img.width,
                    img.height,
                    img.data.len()
                );
                if let Err(e) = save_cached_image(k, dir, img_file, &img) {
                    log::warn!("reader: image cache write failed: {}", e);
                } else {
                    log::info!("reader: cached image as {}", img_file);
                }
                self.page_img = Some(img);
            }
            Err(e) => {
                log::warn!("reader: image decode failed: {}", e);
            }
        }
    }

    // scan cached chapters for uncached images, dispatch to worker
    pub(super) fn epub_find_and_dispatch_image(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<bool, &'static str> {
        // image scanning uses the prefetch buffer as scratch; invalidate
        // it so it gets reloaded on the next page turn
        self.prefetch_page = NO_PREFETCH;

        let spine_len = self.spine.len();
        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        while (self.img_cache_ch as usize) < spine_len {
            // after wrapping, stop at the original start chapter
            if self.img_scan_wrapped && self.img_cache_ch >= self.chapter {
                break;
            }

            let ch = self.img_cache_ch as usize;
            let ch_size = if ch < cache::MAX_CACHE_CHAPTERS {
                self.chapter_sizes[ch] as usize
            } else {
                0
            };

            let ch_file = cache::chapter_file_name(self.img_cache_ch);
            let ch_str = cache::chapter_file_str(&ch_file);
            let mut offset = self.img_cache_offset as usize;

            while offset < ch_size {
                let read_len = PAGE_BUF.min(ch_size - offset);
                let n = k.sync_read_app_subdir_chunk(
                    dir,
                    ch_str,
                    offset as u32,
                    &mut self.prefetch[..read_len],
                )?;
                if n == 0 {
                    break;
                }

                let mut i = 0;
                while i + 2 < n {
                    if self.prefetch[i] != MARKER || self.prefetch[i + 1] != IMG_REF {
                        i += 1;
                        continue;
                    }

                    let path_len = self.prefetch[i + 2] as usize;
                    let path_start = i + 3;
                    if path_len == 0 || path_start + path_len > n {
                        i += 1;
                        continue;
                    }

                    let mut src_buf = [0u8; 128];
                    let src_n = path_len.min(src_buf.len());
                    src_buf[..src_n]
                        .copy_from_slice(&self.prefetch[path_start..path_start + src_n]);
                    let src_str = match core::str::from_utf8(&src_buf[..src_n]) {
                        Ok(s) if !s.is_empty() => s,
                        _ => {
                            i = path_start + path_len;
                            continue;
                        }
                    };

                    let mut path_buf = [0u8; 512];
                    let plen = {
                        let ch_zip_idx = self.spine.items[ch] as usize;
                        let ch_path = self.zip.entry_name(ch_zip_idx);
                        let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                        epub::resolve_path(ch_dir, src_str, &mut path_buf)
                    };
                    let full_path = match core::str::from_utf8(&path_buf[..plen]) {
                        Ok(s) => s,
                        Err(_) => {
                            i = path_start + path_len;
                            continue;
                        }
                    };

                    let path_hash = cache::fnv1a(full_path.as_bytes());
                    let img_name = img_cache_name(path_hash);
                    let img_file = img_cache_str(&img_name);

                    // already cached or skip-marked
                    if k.sync_file_size_app_subdir(dir, img_file).is_ok() {
                        i = path_start + path_len;
                        continue;
                    }

                    let is_jpeg = is_image_ext_jpeg(full_path);
                    let is_png = is_image_ext_png(full_path);

                    if !is_jpeg && !is_png {
                        log::info!("precache: skip unsupported: {}", full_path);
                        let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                        i = path_start + path_len;
                        continue;
                    }

                    // save resume position past this image ref
                    self.img_cache_offset = (offset + path_start + path_len) as u32;

                    let zip_idx = match self
                        .zip
                        .find(full_path)
                        .or_else(|| self.zip.find_icase(full_path))
                    {
                        Some(idx) => idx,
                        None => {
                            log::warn!("precache: image not in ZIP: {}", full_path);
                            return Ok(true);
                        }
                    };

                    let entry = *self.zip.entry(zip_idx);

                    // large images: decode via streaming SD reads
                    if entry.uncomp_size > PRECACHE_IMG_MAX {
                        log::info!(
                            "precache: streaming {} ({} bytes)",
                            full_path,
                            entry.uncomp_size,
                        );
                        match decode_image_streaming(
                            k,
                            epub_name,
                            &entry,
                            is_jpeg,
                            TEXT_W as u16,
                            TEXT_AREA_H,
                        ) {
                            Ok(img) => {
                                log::info!(
                                    "precache: decoded {}x{} ({}B 1-bit)",
                                    img.width,
                                    img.height,
                                    img.data.len(),
                                );
                                let _ = save_cached_image(k, dir, img_file, &img);
                            }
                            Err(e) => {
                                log::warn!("precache: streaming failed: {}", e);
                                let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                            }
                        }
                        self.img_cache_offset = (offset + path_start + path_len) as u32;
                        return Ok(true); // yield -- one image per tick
                    }

                    // small images: extract to memory for worker
                    log::info!("precache: extracting {}", full_path);
                    let data = match super::extract_zip_entry(k, epub_name, &self.zip, zip_idx) {
                        Ok(d) => d,
                        Err(e) => {
                            log::warn!("precache: extract failed: {}", e);
                            let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                            self.img_cache_offset = (offset + path_start + path_len) as u32;
                            return Ok(true);
                        }
                    };

                    log::info!(
                        "precache: dispatch {} ({} bytes) to worker",
                        full_path,
                        data.len()
                    );

                    let task = work_queue::WorkTask::DecodeImage {
                        path_hash,
                        data,
                        is_jpeg,
                        max_w: TEXT_W as u16,
                        max_h: TEXT_AREA_H,
                    };
                    if !work_queue::submit(self.work_gen, task) {
                        return Err("cache: worker channel full");
                    }
                    return Ok(true);
                }

                // advance with overlap so markers at chunk boundaries are not missed
                if offset + n >= ch_size {
                    break;
                }
                offset += n.saturating_sub(128).max(1);
            }

            self.img_cache_ch += 1;
            self.img_cache_offset = 0;
        }

        // wrap around: if we started mid-book, scan chapters before the start
        if !self.img_scan_wrapped && self.chapter > 0 {
            log::info!(
                "precache: wrapping image scan to ch0 (started at ch{})",
                self.chapter,
            );
            self.img_cache_ch = 0;
            self.img_cache_offset = 0;
            self.img_scan_wrapped = true;
            return Ok(true);
        }

        log::info!("precache: all images scanned");
        Ok(false)
    }

    // poll worker for a completed image-decode result
    pub(super) fn epub_recv_image_result(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> Result<Option<bool>, &'static str> {
        let result = match work_queue::try_recv() {
            Some(r) if r.is_current() => r,
            Some(_) => return Ok(None), // stale generation -- discard
            None => return Ok(None),
        };

        match result.outcome {
            work_queue::WorkOutcome::ImageReady { path_hash, image } => {
                let dir_buf = self.cache_dir;
                let dir = cache::dir_name_str(&dir_buf);
                let img_name = img_cache_name(path_hash);
                let img_file = img_cache_str(&img_name);

                log::info!(
                    "precache: decoded {}x{} ({}B 1-bit)",
                    image.width,
                    image.height,
                    image.data.len()
                );

                if let Err(e) = save_cached_image(k, dir, img_file, &image) {
                    log::warn!("precache: save failed: {}", e);
                }

                Ok(Some(true))
            }
            work_queue::WorkOutcome::ImageFailed { path_hash, error } => {
                log::warn!("precache: image {:#010X} failed: {}", path_hash, error);
                Ok(Some(true))
            }
            _ => {
                log::warn!("precache: unexpected result while waiting for image decode");
                Ok(None)
            }
        }
    }

    // dispatch one uncached image from chapter ch to the worker
    pub(super) fn dispatch_one_image_in_chapter(
        &mut self,
        k: &mut KernelHandle<'_>,
        ch: usize,
    ) -> bool {
        if !self.ch_cached[ch] || ch >= cache::MAX_CACHE_CHAPTERS {
            return false;
        }
        let ch_size = self.chapter_sizes[ch] as usize;
        if ch_size == 0 {
            return false;
        }

        let dir_buf = self.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let ch_file = cache::chapter_file_name(ch as u16);
        let ch_str = cache::chapter_file_str(&ch_file);

        self.prefetch_page = NO_PREFETCH;

        let mut offset = 0usize;
        while offset < ch_size {
            let read_len = PAGE_BUF.min(ch_size - offset);
            let n = match k.sync_read_app_subdir_chunk(
                dir,
                ch_str,
                offset as u32,
                &mut self.prefetch[..read_len],
            ) {
                Ok(n) if n > 0 => n,
                _ => break,
            };

            let mut i = 0;
            while i + 2 < n {
                if self.prefetch[i] != MARKER || self.prefetch[i + 1] != IMG_REF {
                    i += 1;
                    continue;
                }

                let path_len = self.prefetch[i + 2] as usize;
                let path_start = i + 3;
                if path_len == 0 || path_start + path_len > n {
                    i += 1;
                    continue;
                }

                let mut src_buf = [0u8; 128];
                let src_n = path_len.min(src_buf.len());
                src_buf[..src_n].copy_from_slice(&self.prefetch[path_start..path_start + src_n]);
                let src_str = match core::str::from_utf8(&src_buf[..src_n]) {
                    Ok(s) if !s.is_empty() => s,
                    _ => {
                        i = path_start + path_len;
                        continue;
                    }
                };

                let mut path_buf = [0u8; 512];
                let plen = {
                    let ch_zip_idx = self.spine.items[ch] as usize;
                    let ch_path = self.zip.entry_name(ch_zip_idx);
                    let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                    epub::resolve_path(ch_dir, src_str, &mut path_buf)
                };
                let full_path = match core::str::from_utf8(&path_buf[..plen]) {
                    Ok(s) => s,
                    Err(_) => {
                        i = path_start + path_len;
                        continue;
                    }
                };

                let path_hash = cache::fnv1a(full_path.as_bytes());
                let img_name = img_cache_name(path_hash);
                let img_file = img_cache_str(&img_name);

                // already in SD cache
                if k.sync_file_size_app_subdir(dir, img_file).is_ok() {
                    i = path_start + path_len;
                    continue;
                }

                // check format by extension before the expensive ZIP extraction
                let is_jpeg = is_image_ext_jpeg(full_path);
                let is_png = is_image_ext_png(full_path);

                if !is_jpeg && !is_png {
                    log::info!("precache: skip unsupported: {}", full_path);
                    // zero-byte skip marker so future scans skip instantly
                    let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                    i = path_start + path_len;
                    continue;
                }

                let zip_idx = match self
                    .zip
                    .find(full_path)
                    .or_else(|| self.zip.find_icase(full_path))
                {
                    Some(idx) => idx,
                    None => {
                        i = path_start + path_len;
                        continue;
                    }
                };

                let entry = self.zip.entry(zip_idx);

                // large images: decode via streaming SD reads
                if entry.uncomp_size > PRECACHE_IMG_MAX {
                    log::info!(
                        "precache: nearby streaming {} ({} bytes)",
                        full_path,
                        entry.uncomp_size,
                    );
                    match decode_image_streaming(
                        k,
                        epub_name,
                        entry,
                        is_jpeg,
                        TEXT_W as u16,
                        TEXT_AREA_H,
                    ) {
                        Ok(img) => {
                            log::info!(
                                "precache: decoded {}x{} ({}B)",
                                img.width,
                                img.height,
                                img.data.len(),
                            );
                            let _ = save_cached_image(k, dir, img_file, &img);
                        }
                        Err(e) => {
                            log::warn!("precache: nearby streaming failed: {}", e);
                            let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                        }
                    }
                    // decoded synchronously -- no worker dispatch
                    return false;
                }

                // small images: extract to memory for worker dispatch
                let data = match super::extract_zip_entry(k, epub_name, &self.zip, zip_idx) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("precache: nearby extract failed: {}", e);
                        // skip marker prevents retrying every tick
                        let _ = k.sync_write_app_subdir(dir, img_file, &[]);
                        i = path_start + path_len;
                        continue;
                    }
                };

                log::info!(
                    "precache: nearby ch{} image {} ({} bytes)",
                    ch,
                    full_path,
                    data.len(),
                );

                let task = work_queue::WorkTask::DecodeImage {
                    path_hash,
                    data,
                    is_jpeg,
                    max_w: TEXT_W as u16,
                    max_h: TEXT_AREA_H,
                };
                return work_queue::submit(self.work_gen, task);
            }

            if offset + n >= ch_size {
                break;
            }
            offset += n.saturating_sub(128).max(1);
        }

        false
    }

    // dispatch one uncached image from chapters near the current position
    pub(super) fn try_dispatch_nearby_image(&mut self, k: &mut KernelHandle<'_>) -> bool {
        let r = self.chapter as usize;
        let spine_len = self.spine.len();
        for &ch in &[r, r + 1, r.saturating_sub(1), r + 2, r.saturating_sub(2)] {
            if ch < spine_len && self.ch_cached[ch] {
                if self.dispatch_one_image_in_chapter(k, ch) {
                    return true;
                }
            }
        }
        false
    }
}

pub(super) fn img_cache_name(hash: u32) -> [u8; 12] {
    let h = hash & 0x00FF_FFFF;
    let mut n = *b"IM000000.BIN";
    for i in 0..6 {
        let nibble = ((h >> (20 - i * 4)) & 0xF) as u8;
        n[2 + i] = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    n
}

#[inline]
pub(super) fn img_cache_str(buf: &[u8; 12]) -> &str {
    core::str::from_utf8(buf).unwrap_or("IM000000.BIN")
}

pub(super) fn is_image_ext_jpeg(path: &str) -> bool {
    let p = path.as_bytes();
    let len = p.len();
    if len >= 4 && p[len - 4] == b'.' {
        let ext = &p[len - 3..];
        return ext.eq_ignore_ascii_case(b"jpg");
    }
    if len >= 5 && p[len - 5] == b'.' {
        let ext = &p[len - 4..];
        return ext.eq_ignore_ascii_case(b"jpeg");
    }
    false
}

pub(super) fn is_image_ext_png(path: &str) -> bool {
    let p = path.as_bytes();
    let len = p.len();
    if len >= 4 && p[len - 4] == b'.' {
        let ext = &p[len - 3..];
        return ext.eq_ignore_ascii_case(b"png");
    }
    false
}

// decode image directly from EPUB ZIP via streaming 4 KB SD reads;
// large-image path -- worker can't stream from SD, so main loop does it
pub(super) fn decode_image_streaming(
    k: &mut KernelHandle<'_>,
    epub_name: &str,
    entry: &smol_epub::zip::ZipEntry,
    is_jpeg: bool,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str> {
    let mut hdr = [0u8; 30];
    k.sync_read_chunk(epub_name, entry.local_offset, &mut hdr)
        .map_err(|_| "read local header failed")?;
    let skip = ZipIndex::local_header_data_skip(&hdr)?;
    let data_offset = entry.local_offset + skip;

    if is_jpeg && entry.method == zip::METHOD_STORED {
        smol_epub::jpeg::decode_jpeg_sd(
            |off, buf| k.sync_read_chunk(epub_name, off, buf),
            data_offset,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else if is_jpeg {
        smol_epub::jpeg::decode_jpeg_deflate_sd(
            |off, buf| k.sync_read_chunk(epub_name, off, buf),
            data_offset,
            entry.comp_size,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else if entry.method == zip::METHOD_STORED {
        smol_epub::png::decode_png_sd(
            |off, buf| k.sync_read_chunk(epub_name, off, buf),
            data_offset,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else {
        smol_epub::png::decode_png_deflate_sd(
            |off, buf| k.sync_read_chunk(epub_name, off, buf),
            data_offset,
            entry.comp_size,
            max_w,
            max_h,
        )
    }
}

pub(super) fn load_cached_image(
    k: &mut KernelHandle<'_>,
    dir: &str,
    name: &str,
) -> Result<DecodedImage, &'static str> {
    let size = k
        .sync_file_size_app_subdir(dir, name)
        .map_err(|_| "no cache file")?;
    if size < 5 {
        return Err("cache file too small");
    }
    let mut header = [0u8; 4];
    k.sync_read_app_subdir_chunk(dir, name, 0, &mut header)
        .map_err(|_| "read header failed")?;
    let width = u16::from_le_bytes([header[0], header[1]]);
    let height = u16::from_le_bytes([header[2], header[3]]);
    if width == 0 || height == 0 {
        return Err("zero dimensions in cache");
    }
    let stride = (width as usize).div_ceil(8);
    let data_len = stride * height as usize;
    if size as usize != 4 + data_len {
        return Err("cache size mismatch");
    }
    let mut data = Vec::new();
    data.try_reserve_exact(data_len)
        .map_err(|_| "OOM for cached image")?;
    data.resize(data_len, 0);
    k.sync_read_app_subdir_chunk(dir, name, 4, &mut data)
        .map_err(|_| "read data failed")?;
    Ok(DecodedImage {
        width,
        height,
        data,
        stride,
    })
}

pub(super) fn save_cached_image(
    k: &mut KernelHandle<'_>,
    dir: &str,
    name: &str,
    img: &DecodedImage,
) -> Result<(), &'static str> {
    let mut header = [0u8; 4];
    header[0..2].copy_from_slice(&img.width.to_le_bytes());
    header[2..4].copy_from_slice(&img.height.to_le_bytes());
    k.sync_write_app_subdir(dir, name, &header)?;
    k.sync_append_app_subdir(dir, name, &img.data)?;
    Ok(())
}
