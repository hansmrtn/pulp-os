// image decode, cache, and dispatch
//
// scan_chapter_for_image is the shared core: reads chapter data in
// chunks, finds IMG_REF markers, resolves paths, checks cache, and
// either decodes inline (large images) or dispatches to the worker
// (small images); both epub_find_and_dispatch_image (background scan)
// and dispatch_one_image_in_chapter (nearby prefetch) call through it

use alloc::vec::Vec;
use core::cell::RefCell;

use crate::kernel::work_queue::DecodedImage;
use smol_epub::cache;
use smol_epub::epub;
use smol_epub::html_strip::{IMG_REF, MARKER};
use smol_epub::zip::{self, ZipIndex};

use crate::error::{Error, ErrorKind};
use crate::kernel::KernelHandle;
use crate::kernel::work_queue;

use super::{
    DEFAULT_IMG_H, MAX_IMAGES_PER_PAGE, NO_PREFETCH, PAGE_BUF, PRECACHE_IMG_MAX, ReaderApp,
};

fn from_smol_image(img: smol_epub::DecodedImage) -> DecodedImage {
    DecodedImage {
        width: img.width,
        height: img.height,
        data: img.data,
        stride: img.stride,
    }
}

// result of scanning a chapter for the next uncached image
enum ScanResult {
    // small image dispatched to background worker
    Dispatched { resume_offset: u32 },
    // large image decoded inline via streaming SD reads
    DecodedInline { resume_offset: u32 },
    // no uncached images found from the given offset
    NoneFound,
}

impl ReaderApp {
    // decode the image on the current page (if any) for display
    pub(super) fn decode_page_images(&mut self, k: &mut KernelHandle<'_>) {
        self.page_img = None;
        self.fullscreen_img = false;

        if !self.is_epub || self.epub.spine.is_empty() {
            return;
        }

        {
            let mut has_img = false;
            let mut has_text = false;
            for i in 0..self.pg.line_count {
                if self.pg.lines[i].is_image() {
                    if self.pg.lines[i].is_image_origin() {
                        has_img = true;
                    }
                } else if self.pg.lines[i].len > 0 {
                    has_text = true;
                }
            }
            self.fullscreen_img = has_img && !has_text;
        }

        // copy src path to a local buf to avoid borrowing self.buf below
        let mut src_buf = [0u8; 128];
        let mut src_len = 0usize;
        for i in 0..self.pg.line_count {
            if self.pg.lines[i].is_image_origin() {
                let start = self.pg.lines[i].start as usize;
                let len = self.pg.lines[i].len as usize;
                if start + len <= self.pg.buf_len {
                    let n = len.min(src_buf.len());
                    src_buf[..n].copy_from_slice(&self.pg.buf[start..start + n]);
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

        let ch_zip_idx = self.epub.spine.items[self.epub.chapter as usize] as usize;
        let ch_path = self.epub.zip.entry_name(ch_zip_idx);
        let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

        let mut path_buf = [0u8; 512];
        let path_len = epub::resolve_path(ch_dir, src_str, &mut path_buf);
        let full_path = match core::str::from_utf8(&path_buf[..path_len]) {
            Ok(s) => s,
            Err(_) => return,
        };

        let dir_buf = self.epub.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let img_name = img_cache_name(cache::fnv1a(full_path.as_bytes()));
        let img_file = img_cache_str(&img_name);

        // inline images are capped to a fraction of the text area so
        // they feel proportional to surrounding text.  fullscreen
        // images (sole content on the page) get the full budget.
        let img_budget_h = if self.fullscreen_img {
            self.text_area_h
        } else {
            super::inline_img_max_h(self.text_area_h)
        };

        if let Ok(img) = load_cached_image(k, dir, img_file) {
            // use the cache if the image already fits the budget;
            // if the cached image is too tall (precache used full
            // text_area_h) fall through to re-decode at inline budget
            if img.height <= img_budget_h {
                log::info!(
                    "reader: image cache hit {} ({}x{})",
                    img_file,
                    img.width,
                    img.height
                );
                self.page_img = Some(img);
                return;
            }
            log::info!(
                "reader: cache {}x{} exceeds inline budget {}, re-decoding",
                img.width,
                img.height,
                img_budget_h,
            );
            // drop the oversized image before decoding a smaller one
            drop(img);
        }

        // background precache will decode this image eventually;
        // skip the blocking inline decode so the page renders
        // immediately without it. once the user is in Ready state
        // defer_image_decode is cleared and a page revisit will
        // either hit the cache or do the full decode.
        if self.defer_image_decode {
            log::info!(
                "reader: deferring image decode (bg will handle {})",
                full_path
            );
            return;
        }

        let zip_idx = match self
            .epub
            .zip
            .find(full_path)
            .or_else(|| self.epub.zip.find_icase(full_path))
        {
            Some(idx) => idx,
            None => {
                log::warn!("reader: image not in ZIP: {}", full_path);
                return;
            }
        };

        let entry = *self.epub.zip.entry(zip_idx);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let data_offset = {
            let mut hdr = [0u8; 30];
            if k.read_chunk(epub_name, entry.local_offset, &mut hdr)
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
                .read_chunk(epub_name, data_offset, &mut magic)
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

        // decode at the context-appropriate budget: inline images use
        // the capped height so they scale proportionally; fullscreen
        // images use the full text area
        let img_max_h = img_budget_h;

        let img_max_w = self.text_w as u16;
        let do_decode = |k_ref: &mut KernelHandle<'_>| -> Result<DecodedImage, &'static str> {
            let k_cell = RefCell::new(k_ref);
            let read_err = |e: Error| -> &'static str { e.into() };
            let raw = if is_jpeg && entry.method == zip::METHOD_STORED {
                smol_epub::jpeg::decode_jpeg_sd(
                    |off, buf| {
                        k_cell
                            .borrow_mut()
                            .read_chunk(epub_name, off, buf)
                            .map_err(read_err)
                    },
                    data_offset,
                    entry.uncomp_size,
                    img_max_w,
                    img_max_h,
                )
            } else if is_jpeg {
                smol_epub::jpeg::decode_jpeg_deflate_sd(
                    |off, buf| {
                        k_cell
                            .borrow_mut()
                            .read_chunk(epub_name, off, buf)
                            .map_err(read_err)
                    },
                    data_offset,
                    entry.comp_size,
                    entry.uncomp_size,
                    img_max_w,
                    img_max_h,
                )
            } else if entry.method == zip::METHOD_STORED {
                smol_epub::png::decode_png_sd(
                    |off, buf| {
                        k_cell
                            .borrow_mut()
                            .read_chunk(epub_name, off, buf)
                            .map_err(read_err)
                    },
                    data_offset,
                    entry.uncomp_size,
                    img_max_w,
                    img_max_h,
                )
            } else {
                smol_epub::png::decode_png_deflate_sd(
                    |off, buf| {
                        k_cell
                            .borrow_mut()
                            .read_chunk(epub_name, off, buf)
                            .map_err(read_err)
                    },
                    data_offset,
                    entry.comp_size,
                    img_max_w,
                    img_max_h,
                )
            };
            raw.map(from_smol_image)
        };

        let result = do_decode(k);

        // OOM fallback: release chapter cache and retry
        let result = match result {
            Ok(img) => Ok(img),
            Err(e) if !self.epub.ch_cache.is_empty() => {
                log::info!(
                    "reader: decode failed ({}), releasing {} KB chapter cache and retrying",
                    e,
                    self.epub.ch_cache.len() / 1024,
                );
                self.epub.ch_cache = Vec::new();
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

    // pre-scan the page buffer for IMG_REF markers and look up each
    // image's decoded dimensions (from cache or ZIP headers).
    // populates self.img_heights so wrap_proportional can reserve
    // the exact number of lines for each image.
    pub(super) fn prescan_image_heights(&mut self, k: &mut KernelHandle<'_>, buf_len: usize) {
        self.img_height_count = 0;

        if !self.is_epub || self.epub.spine.is_empty() {
            return;
        }

        let ch_zip_idx = self.epub.spine.items[self.epub.chapter as usize] as usize;
        let ch_path = self.epub.zip.entry_name(ch_zip_idx);
        let ch_dir = ch_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");

        let dir = self.epub.cache_dir_str();

        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let text_w = self.text_w;
        let text_area_h = self.text_area_h;
        let max_inline_h = super::inline_img_max_h(text_area_h);

        // scan for [MARKER, IMG_REF, len, path...] sequences
        let mut i = 0usize;
        while i + 2 < buf_len && (self.img_height_count as usize) < MAX_IMAGES_PER_PAGE {
            if self.pg.buf[i] != MARKER || self.pg.buf[i + 1] != IMG_REF {
                i += 1;
                continue;
            }
            let path_len = self.pg.buf[i + 2] as usize;
            let path_start = i + 3;
            if path_len == 0 || path_start + path_len > buf_len {
                i += 1;
                continue;
            }

            // resolve image path
            let mut src_buf = [0u8; 128];
            let src_n = path_len.min(src_buf.len());
            src_buf[..src_n].copy_from_slice(&self.pg.buf[path_start..path_start + src_n]);
            let src_str = match core::str::from_utf8(&src_buf[..src_n]) {
                Ok(s) if !s.is_empty() => s,
                _ => {
                    self.img_heights[self.img_height_count as usize] = DEFAULT_IMG_H;
                    self.img_height_count += 1;
                    i = path_start + path_len;
                    continue;
                }
            };

            let mut path_buf = [0u8; 512];
            let plen = epub::resolve_path(ch_dir, src_str, &mut path_buf);
            let full_path = match core::str::from_utf8(&path_buf[..plen]) {
                Ok(s) => s,
                Err(_) => {
                    self.img_heights[self.img_height_count as usize] = DEFAULT_IMG_H;
                    self.img_height_count += 1;
                    i = path_start + path_len;
                    continue;
                }
            };

            let path_hash = cache::fnv1a(full_path.as_bytes());
            let img_name = img_cache_name(path_hash);
            let img_file = img_cache_str(&img_name);

            // try 1: read cached image header (4 bytes, very fast)
            let out_h = if let Some((_w, h)) = peek_cached_image_size(k, dir, img_file) {
                // cached image is already at the final decoded size
                h
            } else {
                // try 2: peek source dimensions from the ZIP entry
                peek_source_dimensions(k, epub_name, &self.epub.zip, full_path, text_w, text_area_h)
            };

            // cap to the inline budget; fullscreen images bypass line
            // reservation entirely and use the full text_area_h
            self.img_heights[self.img_height_count as usize] = out_h.min(max_inline_h);
            self.img_height_count += 1;
            i = path_start + path_len;
        }
    }

    // scan one chapter from start_offset for the first uncached image.
    // reads chapter data in chunks via self.prefetch, finds IMG_REF
    // markers, resolves paths against the ZIP, checks the SD cache,
    // and either decodes inline (large) or dispatches to worker (small).
    fn scan_chapter_for_image(
        &mut self,
        k: &mut KernelHandle<'_>,
        ch: usize,
        start_offset: usize,
    ) -> crate::error::Result<ScanResult> {
        if ch >= self.epub.spine.len()
            || ch >= cache::MAX_CACHE_CHAPTERS
            || !self.epub.ch_cached[ch]
        {
            return Ok(ScanResult::NoneFound);
        }
        let ch_size = self.epub.chapter_size(ch) as usize;
        if ch_size == 0 {
            return Ok(ScanResult::NoneFound);
        }

        self.pg.prefetch_page = NO_PREFETCH;

        let dir_buf = self.epub.cache_dir;
        let dir = cache::dir_name_str(&dir_buf);
        let (nb, nl) = self.name_copy();
        let epub_name = core::str::from_utf8(&nb[..nl]).unwrap_or("");

        let ch_file = cache::chapter_file_name(ch as u16);
        let ch_str = cache::chapter_file_str(&ch_file);

        let mut offset = start_offset;
        while offset < ch_size {
            let read_len = PAGE_BUF.min(ch_size - offset);
            let n = k.read_app_subdir_chunk(
                dir,
                ch_str,
                offset as u32,
                &mut self.pg.prefetch[..read_len],
            )?;
            if n == 0 {
                break;
            }

            let mut i = 0;
            while i + 2 < n {
                if self.pg.prefetch[i] != MARKER || self.pg.prefetch[i + 1] != IMG_REF {
                    i += 1;
                    continue;
                }

                let path_len = self.pg.prefetch[i + 2] as usize;
                let path_start = i + 3;
                if path_len == 0 || path_start + path_len > n {
                    i += 1;
                    continue;
                }

                let mut src_buf = [0u8; 128];
                let src_n = path_len.min(src_buf.len());
                src_buf[..src_n].copy_from_slice(&self.pg.prefetch[path_start..path_start + src_n]);
                let src_str = match core::str::from_utf8(&src_buf[..src_n]) {
                    Ok(s) if !s.is_empty() => s,
                    _ => {
                        i = path_start + path_len;
                        continue;
                    }
                };

                let mut path_buf = [0u8; 512];
                let plen = {
                    let ch_zip_idx = self.epub.spine.items[ch] as usize;
                    let ch_path = self.epub.zip.entry_name(ch_zip_idx);
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
                let resume = (offset + path_start + path_len) as u32;

                // already cached or skip-marked
                if k.file_size_app_subdir(dir, img_file).is_ok() {
                    i = path_start + path_len;
                    continue;
                }

                let is_jpeg = is_image_ext_jpeg(full_path);
                let is_png = is_image_ext_png(full_path);

                if !is_jpeg && !is_png {
                    log::info!("precache: skip unsupported: {}", full_path);
                    let _ = k.write_app_subdir(dir, img_file, &[]);
                    i = path_start + path_len;
                    continue;
                }

                let zip_idx = match self
                    .epub
                    .zip
                    .find(full_path)
                    .or_else(|| self.epub.zip.find_icase(full_path))
                {
                    Some(idx) => idx,
                    None => {
                        log::warn!("precache: {} not in ZIP", full_path);
                        i = path_start + path_len;
                        continue;
                    }
                };

                let entry = *self.epub.zip.entry(zip_idx);

                // large images: decode via streaming SD reads on main loop
                if entry.uncomp_size > PRECACHE_IMG_MAX {
                    log::info!(
                        "precache: streaming {} ({} bytes)",
                        full_path,
                        entry.uncomp_size,
                    );
                    let img_w = self.text_w as u16;
                    let img_h = self.text_area_h;
                    let result =
                        decode_image_streaming(k, epub_name, &entry, is_jpeg, img_w, img_h);

                    // OOM fallback: release chapter cache and retry
                    let result = match result {
                        Ok(img) => Ok(img),
                        Err(e) if !self.epub.ch_cache.is_empty() => {
                            log::info!(
                                "precache: streaming failed ({}), releasing {} KB ch_cache and retrying",
                                e,
                                self.epub.ch_cache.len() / 1024,
                            );
                            self.epub.ch_cache = Vec::new();
                            decode_image_streaming(k, epub_name, &entry, is_jpeg, img_w, img_h)
                        }
                        Err(e) => Err(e),
                    };

                    match result {
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
                            log::warn!("precache: streaming failed: {}", e);
                            let _ = k.write_app_subdir(dir, img_file, &[]);
                        }
                    }
                    return Ok(ScanResult::DecodedInline {
                        resume_offset: resume,
                    });
                }

                // wait for worker to have capacity before the
                // expensive extraction (deflate + alloc).  check both
                // idle state and channel room to avoid extracting data
                // that can't be submitted.
                if !work_queue::is_idle() || !work_queue::can_submit() {
                    return Ok(ScanResult::Dispatched {
                        resume_offset: (offset + i) as u32,
                    });
                }

                // small images: extract to memory for worker dispatch
                let data = match super::extract_zip_entry(k, epub_name, &self.epub.zip, zip_idx) {
                    Ok(d) => d,
                    Err(e) => {
                        log::warn!("precache: extract failed: {}", e);
                        let _ = k.write_app_subdir(dir, img_file, &[]);
                        i = path_start + path_len;
                        continue;
                    }
                };

                log::info!("precache: dispatch {} ({} bytes)", full_path, data.len(),);

                let task = work_queue::WorkTask::DecodeImage {
                    path_hash,
                    data,
                    is_jpeg,
                    max_w: self.text_w as u16,
                    max_h: self.text_area_h,
                };
                if work_queue::submit(self.epub.work_gen, task) {
                    return Ok(ScanResult::Dispatched {
                        resume_offset: resume,
                    });
                }
                // rare race: channel filled between can_submit() and
                // submit().  retry on next poll instead of skipping.
                log::info!("precache: queue race, will retry {}", full_path);
                return Ok(ScanResult::Dispatched {
                    resume_offset: (offset + i) as u32,
                });
            }

            // advance with overlap so markers at chunk boundaries are not missed
            if offset + n >= ch_size {
                break;
            }
            offset += n.saturating_sub(128).max(1);
        }

        Ok(ScanResult::NoneFound)
    }

    // background image scanner: iterates across all chapters starting
    // from self.epub.img_cache_ch / self.epub.img_cache_offset, wrapping around
    // to cover chapters before the reading position
    pub(super) fn epub_find_and_dispatch_image(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<bool> {
        let spine_len = self.epub.spine.len();

        while (self.epub.img_cache_ch as usize) < spine_len {
            if self.epub.img_scan_wrapped && self.epub.img_cache_ch >= self.epub.chapter {
                break;
            }

            let ch = self.epub.img_cache_ch as usize;
            let start = self.epub.img_cache_offset as usize;

            match self.scan_chapter_for_image(k, ch, start)? {
                ScanResult::Dispatched { resume_offset }
                | ScanResult::DecodedInline { resume_offset } => {
                    self.epub.img_cache_offset = resume_offset;
                    return Ok(true);
                }
                ScanResult::NoneFound => {
                    self.epub.img_cache_ch += 1;
                    self.epub.img_cache_offset = 0;
                }
            }
        }

        // wrap around: if we started mid-book, scan chapters before the start
        if !self.epub.img_scan_wrapped && self.epub.chapter > 0 {
            log::info!(
                "precache: wrapping image scan to ch0 (started at ch{})",
                self.epub.chapter,
            );
            self.epub.img_cache_ch = 0;
            self.epub.img_cache_offset = 0;
            self.epub.img_scan_wrapped = true;
            return Ok(true);
        }

        log::info!("precache: all images scanned");
        Ok(false)
    }

    // poll worker for a completed image-decode result
    pub(super) fn epub_recv_image_result(
        &mut self,
        k: &mut KernelHandle<'_>,
    ) -> crate::error::Result<Option<bool>> {
        let result = match work_queue::try_recv() {
            Some(r) if r.is_current() => r,
            Some(_) => return Ok(None), // stale generation; discard
            None => return Ok(None),
        };

        match result.outcome {
            work_queue::WorkOutcome::ImageReady { path_hash, image } => {
                let dir = self.epub.cache_dir_str();
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
        }
    }

    // scan one chapter for the first uncached image, dispatch to worker.
    // returns true if dispatched, false if nothing found or decoded inline.
    pub(super) fn dispatch_one_image_in_chapter(
        &mut self,
        k: &mut KernelHandle<'_>,
        ch: usize,
    ) -> bool {
        matches!(
            self.scan_chapter_for_image(k, ch, 0),
            Ok(ScanResult::Dispatched { .. })
        )
    }

    // dispatch one uncached image from chapters near the current position
    pub(super) fn try_dispatch_nearby_image(&mut self, k: &mut KernelHandle<'_>) -> bool {
        let r = self.epub.chapter as usize;
        let spine_len = self.epub.spine.len();
        for &ch in &[r, r + 1, r.saturating_sub(1), r + 2, r.saturating_sub(2)] {
            if ch < spine_len
                && self.epub.ch_cached[ch]
                && self.dispatch_one_image_in_chapter(k, ch)
            {
                return true;
            }
        }
        false
    }
}

pub(super) fn img_cache_name(hash: u32) -> [u8; 12] {
    let mut n = *b"00000000.BIN";
    for (i, byte) in n.iter_mut().enumerate().take(8) {
        let nibble = ((hash >> (28 - i * 4)) & 0xF) as u8;
        *byte = if nibble < 10 {
            b'0' + nibble
        } else {
            b'A' + nibble - 10
        };
    }
    n
}

#[inline]
pub(super) fn img_cache_str(buf: &[u8; 12]) -> &str {
    core::str::from_utf8(buf).unwrap_or("00000000.BIN")
}

fn path_ext_eq(path: &str, ext: &[u8]) -> bool {
    let p = path.as_bytes();
    let need = ext.len() + 1; // dot + ext
    p.len() >= need
        && p[p.len() - need] == b'.'
        && p[p.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

pub(super) fn is_image_ext_jpeg(path: &str) -> bool {
    path_ext_eq(path, b"jpg") || path_ext_eq(path, b"jpeg")
}

pub(super) fn is_image_ext_png(path: &str) -> bool {
    path_ext_eq(path, b"png")
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
) -> crate::error::Result<DecodedImage> {
    let mut hdr = [0u8; 30];
    k.read_chunk(epub_name, entry.local_offset, &mut hdr)?;
    let skip = ZipIndex::local_header_data_skip(&hdr)
        .map_err(|_| Error::new(ErrorKind::ParseFailed, "decode_image: local header"))?;
    let data_offset = entry.local_offset + skip;

    let read_err = |_: Error| -> &'static str { "read failed" };

    let result = if is_jpeg && entry.method == zip::METHOD_STORED {
        smol_epub::jpeg::decode_jpeg_sd(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else if is_jpeg {
        smol_epub::jpeg::decode_jpeg_deflate_sd(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.comp_size,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else if entry.method == zip::METHOD_STORED {
        smol_epub::png::decode_png_sd(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.uncomp_size,
            max_w,
            max_h,
        )
    } else {
        smol_epub::png::decode_png_deflate_sd(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.comp_size,
            max_w,
            max_h,
        )
    };
    result
        .map(from_smol_image)
        .map_err(|msg| Error::from(msg).with_source("decode_image_streaming"))
}

pub(super) fn load_cached_image(
    k: &mut KernelHandle<'_>,
    dir: &str,
    name: &str,
) -> crate::error::Result<DecodedImage> {
    let size = k.file_size_app_subdir(dir, name)?;
    if size < 5 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "load_cached_image: too small",
        ));
    }
    let mut header = [0u8; 4];
    k.read_app_subdir_chunk(dir, name, 0, &mut header)?;
    let width = u16::from_le_bytes([header[0], header[1]]);
    let height = u16::from_le_bytes([header[2], header[3]]);
    if width == 0 || height == 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "load_cached_image: zero dimensions",
        ));
    }
    let stride = (width as usize).div_ceil(8);
    let data_len = stride * height as usize;
    if size as usize != 4 + data_len {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "load_cached_image: size mismatch",
        ));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(data_len)
        .map_err(|_| Error::new(ErrorKind::OutOfMemory, "load_cached_image"))?;
    data.resize(data_len, 0);
    k.read_app_subdir_chunk(dir, name, 4, &mut data)?;
    Ok(DecodedImage {
        width,
        height,
        data,
        stride,
    })
}

// read just the 4-byte header of a cached 1-bit image file to
// extract its decoded dimensions without loading the pixel data.
// returns None if the file doesn't exist, is too small, or has
// zero dimensions.
fn peek_cached_image_size(k: &mut KernelHandle<'_>, dir: &str, name: &str) -> Option<(u16, u16)> {
    let size = k.file_size_app_subdir(dir, name).ok()?;
    if size < 5 {
        return None;
    }
    let mut hdr = [0u8; 4];
    k.read_app_subdir_chunk(dir, name, 0, &mut hdr).ok()?;
    let w = u16::from_le_bytes([hdr[0], hdr[1]]);
    let h = u16::from_le_bytes([hdr[2], hdr[3]]);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

// resolve a ZIP image entry's source dimensions and compute the
// scaled output height that the decoder would produce.
//
// for stored (uncompressed) entries, peeks the source dimensions
// directly from the ZIP stream (29 bytes for PNG, up to 32 KB for
// JPEG).  for deflate-compressed entries, we can't cheaply read the
// raw pixels, so we return DEFAULT_IMG_H as a reasonable fallback
// (the actual decode will happen later and may produce a different
// height, but it's close enough for line reservation).
fn peek_source_dimensions(
    k: &mut KernelHandle<'_>,
    epub_name: &str,
    zip: &ZipIndex,
    full_path: &str,
    text_w: u32,
    text_area_h: u16,
) -> u16 {
    let zip_idx = match zip.find(full_path).or_else(|| zip.find_icase(full_path)) {
        Some(idx) => idx,
        None => return DEFAULT_IMG_H,
    };
    let entry = *zip.entry(zip_idx);

    // deflate-compressed images: can't peek dimensions cheaply
    if entry.method != zip::METHOD_STORED {
        return DEFAULT_IMG_H;
    }

    // read local header to find data offset
    let data_offset = {
        let mut hdr = [0u8; 30];
        if k.read_chunk(epub_name, entry.local_offset, &mut hdr)
            .is_err()
        {
            return DEFAULT_IMG_H;
        }
        match ZipIndex::local_header_data_skip(&hdr) {
            Ok(skip) => entry.local_offset + skip,
            Err(_) => return DEFAULT_IMG_H,
        }
    };

    let is_jpeg = is_image_ext_jpeg(full_path);
    let is_png = is_image_ext_png(full_path);

    // fall back to magic-byte detection if extension is ambiguous
    let (is_jpeg, is_png) = if is_jpeg || is_png {
        (is_jpeg, is_png)
    } else {
        let mut magic = [0u8; 8];
        let n = k
            .read_chunk(epub_name, data_offset, &mut magic)
            .unwrap_or(0);
        (
            n >= 2 && magic[0] == 0xFF && magic[1] == 0xD8,
            n >= 8 && magic[..8] == [137, 80, 78, 71, 13, 10, 26, 10],
        )
    };

    let read_err = |_: crate::error::Error| -> &'static str { "read failed" };

    let dims = if is_png {
        smol_epub::png::peek_png_dimensions_streaming(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.uncomp_size,
        )
        .map(|(w, h)| (w as u16, h as u16))
    } else if is_jpeg {
        smol_epub::jpeg::peek_jpeg_dimensions_streaming(
            |off, buf| k.read_chunk(epub_name, off, buf).map_err(read_err),
            data_offset,
            entry.uncomp_size,
        )
    } else {
        return DEFAULT_IMG_H;
    };

    match dims {
        Ok((src_w, src_h)) if src_w > 0 && src_h > 0 => {
            // replicate the decoder's integer downscale logic:
            // scale = max(ceil(src_w/max_w), ceil(src_h/max_h), 1)
            let max_w = text_w as u16;
            let max_h = text_area_h;
            let sw = src_w.div_ceil(max_w);
            let sh = src_h.div_ceil(max_h);
            let scale = sw.max(sh).max(1);
            src_h / scale
        }
        _ => DEFAULT_IMG_H,
    }
}

pub(super) fn save_cached_image(
    k: &mut KernelHandle<'_>,
    dir: &str,
    name: &str,
    img: &DecodedImage,
) -> crate::error::Result<()> {
    let mut header = [0u8; 4];
    header[0..2].copy_from_slice(&img.width.to_le_bytes());
    header[2..4].copy_from_slice(&img.height.to_le_bytes());
    k.write_app_subdir(dir, name, &header)?;
    k.append_app_subdir(dir, name, &img.data)?;
    Ok(())
}
