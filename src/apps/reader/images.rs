// image decode, cache, and dispatch
//
// scan_chapter_for_image is the shared core: reads chapter data in
// chunks, finds IMG_REF markers, resolves paths, checks cache, and
// either decodes inline (large images) or dispatches to the worker
// (small images); both epub_find_and_dispatch_image (background scan)
// and dispatch_one_image_in_chapter (nearby prefetch) call through it

use alloc::vec::Vec;
use core::cell::RefCell;

use smol_epub::cache;
use smol_epub::epub;
use smol_epub::html_strip::{IMG_REF, MARKER};
use smol_epub::zip::{self, ZipIndex};
use smol_epub::DecodedImage;

use crate::error::{Error, ErrorKind};
use crate::kernel::work_queue;
use crate::kernel::KernelHandle;

use super::{
    ReaderApp, IMAGE_DISPLAY_H, NO_PREFETCH, PAGE_BUF, PRECACHE_IMG_MAX, TEXT_AREA_H, TEXT_W,
};

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

        let img_max_h = if self.fullscreen_img {
            TEXT_AREA_H
        } else {
            IMAGE_DISPLAY_H
        };

        let do_decode = |k_ref: &mut KernelHandle<'_>| -> Result<DecodedImage, &'static str> {
            let k_cell = RefCell::new(k_ref);
            let read_err = |e: Error| -> &'static str { e.into() };
            if is_jpeg && entry.method == zip::METHOD_STORED {
                smol_epub::jpeg::decode_jpeg_sd(
                    |off, buf| {
                        k_cell
                            .borrow_mut()
                            .read_chunk(epub_name, off, buf)
                            .map_err(read_err)
                    },
                    data_offset,
                    entry.uncomp_size,
                    TEXT_W as u16,
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
                    TEXT_W as u16,
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
                    TEXT_W as u16,
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
                    TEXT_W as u16,
                    img_max_h,
                )
            }
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
        let ch_size = self.epub.chapter_sizes[ch] as usize;
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

                // wait for worker to have capacity before expensive
                // extraction; caller sees Dispatched + worker busy
                // and transitions to WaitImage, retrying after drain
                if !work_queue::is_idle() {
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
                    max_w: TEXT_W as u16,
                    max_h: TEXT_AREA_H,
                };
                if work_queue::submit(self.epub.work_gen, task) {
                    return Ok(ScanResult::Dispatched {
                        resume_offset: resume,
                    });
                }
                // queue full despite idle check; skip this image,
                // it will be decoded on demand if the user views it
                log::warn!("precache: worker queue full, skipping {}", full_path);
                i = path_start + path_len;
                continue;
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
                let dir_buf = self.epub.cache_dir;
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
            if ch < spine_len && self.epub.ch_cached[ch] {
                if self.dispatch_one_image_in_chapter(k, ch) {
                    return true;
                }
            }
        }
        false
    }
}

pub(super) fn img_cache_name(hash: u32) -> [u8; 12] {
    let mut n = *b"00000000.BIN";
    for i in 0..8 {
        let nibble = ((hash >> (28 - i * 4)) & 0xF) as u8;
        n[i] = if nibble < 10 {
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
    result.map_err(|msg| Error::from(msg).with_source("decode_image_streaming"))
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
