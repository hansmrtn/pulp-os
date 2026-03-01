// Minimal PNG decoder for monochrome e-ink display.
// Decodes to 1-bit Floyd-Steinberg dithered bitmap; streams row-by-row
// through miniz_oxide; peak RAM ~90KB (32KB dict + 11KB decomp + bitmap).
// Colour types: 0=greyscale, 2=RGB, 3=palette, 4=grey+alpha, 6=RGBA.
// Interlaced (Adam7) rejected; rare in EPUB and doubles code complexity.
// Output packed 1-bit MSB-first, row-major; pass to StripBuffer::blit_1bpp.

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

// PNG constants

const PNG_SIG: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

const CHUNK_IHDR: [u8; 4] = *b"IHDR";
const CHUNK_PLTE: [u8; 4] = *b"PLTE";
const CHUNK_IDAT: [u8; 4] = *b"IDAT";

const COLOR_GREYSCALE: u8 = 0;
const COLOR_RGB: u8 = 2;
const COLOR_PALETTE: u8 = 3;
const COLOR_GREY_ALPHA: u8 = 4;
const COLOR_RGBA: u8 = 6;

const FILTER_NONE: u8 = 0;
const FILTER_SUB: u8 = 1;
const FILTER_UP: u8 = 2;
const FILTER_AVERAGE: u8 = 3;
const FILTER_PAETH: u8 = 4;

// max total pixels we are willing to decode (memory guard)
const MAX_PIXELS: u32 = 800 * 800;

// miniz_oxide LZ dictionary size; must be a power of two >= 32768
const DICT_SIZE: usize = 32_768;

// public types

// decoded 1-bit image, packed MSB-first, row-major.
// set bit = black (ink); clear bit = white (paper).
pub struct DecodedImage {
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>, // stride * height bytes
    pub stride: usize, // bytes per row: ceil(width / 8)
}

// backward-compatible alias
pub type PngImage = DecodedImage;

// decode a PNG buffer to a 1-bit dithered bitmap;
// images wider or taller than max_w/max_h are nearest-neighbour down-scaled
pub fn decode_png(data: &[u8]) -> Result<DecodedImage, &'static str> {
    decode_png_fit(data, 800, 480)
}

// decode, scaling down by integer factor so result fits inside max_w x max_h
pub fn decode_png_fit(data: &[u8], max_w: u16, max_h: u16) -> Result<DecodedImage, &'static str> {
    let header = parse_ihdr(data)?;
    let idat = collect_idat(data)?;
    let plte = collect_plte(data)?;

    if header.width.saturating_mul(header.height) > MAX_PIXELS {
        return Err("png: image exceeds pixel limit");
    }

    // integer down-scale factor (1 = no scaling)
    let scale = {
        let sw = header
            .width
            .checked_add(max_w as u32 - 1)
            .unwrap_or(u32::MAX)
            / max_w as u32;
        let sh = header
            .height
            .checked_add(max_h as u32 - 1)
            .unwrap_or(u32::MAX)
            / max_h as u32;
        sw.max(sh).max(1) as usize
    };

    let out_w = (header.width as usize / scale).max(1);
    let out_h = (header.height as usize / scale).max(1);
    let out_stride = (out_w + 7) / 8;

    // palette -> greyscale LUT (only used for colour type 3)
    let palette_grey = build_palette_lut(header.color_type, &plte)?;

    let scanline_bytes = header.scanline_bytes();
    let bpp = header.bytes_per_pixel();
    let src_h = header.height as usize;

    // allocate working buffers

    let mut output = Vec::new();
    output
        .try_reserve_exact(out_stride * out_h)
        .map_err(|_| "png: OOM for output bitmap")?;
    output.resize(out_stride * out_h, 0u8);

    let mut prev_row = vec![0u8; scanline_bytes];
    let mut curr_row = vec![0u8; scanline_bytes];

    // Floyd-Steinberg error buffers (+2 for left/right sentinels)
    let mut err_cur = vec![0i16; out_w + 2];
    let mut err_nxt = vec![0i16; out_w + 2];

    // streaming scanline accumulator: 1 filter byte + scanline_bytes
    let row_total = 1 + scanline_bytes;
    let mut row_buf = vec![0u8; row_total];
    let mut row_pos: usize = 0;

    // streaming decompressor (~11KB heap-allocated)

    let decomp_layout = core::alloc::Layout::new::<miniz_oxide::inflate::core::DecompressorOxide>();
    let decomp_ptr = unsafe { alloc::alloc::alloc_zeroed(decomp_layout) };
    if decomp_ptr.is_null() {
        return Err("png: OOM for decompressor");
    }
    let mut decomp =
        unsafe { Box::from_raw(decomp_ptr as *mut miniz_oxide::inflate::core::DecompressorOxide) };

    // 32KB circular dictionary for wrapping-mode inflate
    let mut dict = vec![0u8; DICT_SIZE];

    let mut in_pos: usize = 0;
    let mut dict_pos: usize = 0; // cumulative output pos
    let mut src_y: usize = 0; // source row counter
    let mut out_y: usize = 0; // output row counter

    loop {
        let has_more = in_pos < idat.len();

        let flags = miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
            | if has_more {
                miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
            } else {
                0
            };

        let write_pos = dict_pos & (DICT_SIZE - 1);
        let (status, consumed, produced) = miniz_oxide::inflate::core::decompress(
            &mut *decomp,
            &idat[in_pos..],
            &mut dict,
            write_pos,
            flags,
        );

        in_pos += consumed;

        // feed decompressed bytes into the scanline accumulator
        for i in 0..produced {
            row_buf[row_pos] = dict[(write_pos + i) & (DICT_SIZE - 1)];
            row_pos += 1;

            if row_pos == row_total {
                let filter = row_buf[0];
                curr_row.copy_from_slice(&row_buf[1..]);

                unfilter_row(filter, &mut curr_row, &prev_row, bpp);

                // only dither + output rows that map to an output pixel row
                if src_y % scale == 0 && out_y < out_h {
                    dither_row(
                        &curr_row,
                        &header,
                        &palette_grey,
                        scale,
                        out_w,
                        &mut err_cur,
                        &mut err_nxt,
                        &mut output[out_y * out_stride..(out_y + 1) * out_stride],
                    );
                    out_y += 1;

                    // swap error-diffusion buffers
                    core::mem::swap(&mut err_cur, &mut err_nxt);
                    err_nxt.fill(0);
                }

                core::mem::swap(&mut prev_row, &mut curr_row);
                curr_row.fill(0);
                row_pos = 0;
                src_y += 1;
            }
        }

        dict_pos += produced;

        match status {
            miniz_oxide::inflate::TINFLStatus::Done => break,
            miniz_oxide::inflate::TINFLStatus::NeedsMoreInput => {
                if !has_more {
                    return Err("png: truncated IDAT stream");
                }
                if consumed == 0 && produced == 0 {
                    return Err("png: IDAT decompression stuck");
                }
            }
            miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                // dictionary full; circular buffer recycles automatically
                if produced == 0 && consumed == 0 {
                    return Err("png: decompression stalled (output)");
                }
            }
            _ => return Err("png: IDAT decompression error"),
        }
    }

    if src_y < src_h {
        log::warn!("png: expected {} rows, got {}", src_h, src_y);
    }

    Ok(DecodedImage {
        width: out_w as u16,
        height: out_y as u16,
        data: output,
        stride: out_stride,
    })
}

// streaming PNG decoders: decode PNG images from ZIP entries without
// extracting to a contiguous buffer; IDAT fed directly into zlib row-by-row

// chunk size for streaming SD reads
const SD_READ_BUF: usize = 4096;

// DEFLATE sliding-window for outer ZIP decompression
const ZIP_DEFLATE_WINDOW: usize = 32_768;

// sequential byte source for streaming PNG decoder
trait ReadExact {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), &'static str>;

    fn skip(&mut self, mut n: usize) -> Result<(), &'static str> {
        let mut trash = [0u8; 64];
        while n > 0 {
            let chunk = n.min(64);
            self.read_exact(&mut trash[..chunk])?;
            n -= chunk;
        }
        Ok(())
    }
}

// reads sequentially from a STORED ZIP entry on SD
struct SdSource<F> {
    read_fn: F,
    offset: u32,
    end: u32,
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> ReadExact for SdSource<F> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut done = 0usize;
        while done < buf.len() {
            if self.offset >= self.end {
                return Err("png: unexpected EOF in stored entry");
            }
            let remaining = (self.end - self.offset) as usize;
            let want = (buf.len() - done).min(remaining);
            let n = (self.read_fn)(self.offset, &mut buf[done..done + want])?;
            if n == 0 {
                return Err("png: unexpected EOF in stored entry");
            }
            self.offset += n as u32;
            done += n;
        }
        Ok(())
    }

    fn skip(&mut self, n: usize) -> Result<(), &'static str> {
        let n32 = n as u32;
        if self.offset.saturating_add(n32) > self.end {
            return Err("png: skip past end of stored entry");
        }
        self.offset += n32;
        Ok(())
    }
}

// reads sequentially from a DEFLATE-compressed ZIP entry on SD
struct DeflateSource<F> {
    read_fn: F,
    file_pos: u32,
    comp_left: usize,
    rbuf: Vec<u8>,
    in_avail: usize,
    decomp: Box<miniz_oxide::inflate::core::DecompressorOxide>,
    window: Vec<u8>,
    dict_pos: usize,
    read_pos: usize,
    avail: usize,
    done: bool,
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> DeflateSource<F> {
    fn new(read_fn: F, data_offset: u32, comp_size: u32) -> Result<Self, &'static str> {
        use miniz_oxide::inflate::core::DecompressorOxide;

        let decomp_ptr =
            unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<DecompressorOxide>()) };
        if decomp_ptr.is_null() {
            return Err("png: OOM for DEFLATE decompressor");
        }
        let decomp = unsafe { Box::from_raw(decomp_ptr as *mut DecompressorOxide) };

        let mut window = Vec::new();
        window
            .try_reserve_exact(ZIP_DEFLATE_WINDOW)
            .map_err(|_| "png: OOM for DEFLATE window")?;
        window.resize(ZIP_DEFLATE_WINDOW, 0);

        let mut rbuf = Vec::new();
        rbuf.try_reserve_exact(SD_READ_BUF)
            .map_err(|_| "png: OOM for DEFLATE read buffer")?;
        rbuf.resize(SD_READ_BUF, 0);

        Ok(Self {
            read_fn,
            file_pos: data_offset,
            comp_left: comp_size as usize,
            rbuf,
            in_avail: 0,
            decomp,
            window,
            dict_pos: 0,
            read_pos: 0,
            avail: 0,
            done: false,
        })
    }

    fn pump(&mut self) -> Result<(), &'static str> {
        use miniz_oxide::inflate::TINFLStatus;
        use miniz_oxide::inflate::core::{decompress, inflate_flags};

        if self.done {
            return Ok(());
        }

        if self.in_avail < SD_READ_BUF && self.comp_left > 0 {
            let space = SD_READ_BUF - self.in_avail;
            let want = space.min(self.comp_left);
            match (self.read_fn)(
                self.file_pos,
                &mut self.rbuf[self.in_avail..self.in_avail + want],
            ) {
                Ok(n) if n > 0 => {
                    self.file_pos += n as u32;
                    self.comp_left -= n;
                    self.in_avail += n;
                }
                Ok(_) => {
                    self.comp_left = 0;
                }
                Err(_) => return Err("png: SD read failed during DEFLATE"),
            }
        }

        let flags = if self.comp_left > 0 {
            inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            0
        };

        let write_pos = self.dict_pos & (ZIP_DEFLATE_WINDOW - 1);
        let (status, consumed, produced) = decompress(
            &mut *self.decomp,
            &self.rbuf[..self.in_avail],
            &mut self.window,
            write_pos,
            flags,
        );

        if consumed > 0 && consumed < self.in_avail {
            self.rbuf.copy_within(consumed..self.in_avail, 0);
        }
        self.in_avail -= consumed;
        self.dict_pos += produced;
        self.avail += produced;

        match status {
            TINFLStatus::Done => {
                self.done = true;
            }
            TINFLStatus::HasMoreOutput | TINFLStatus::NeedsMoreInput => {}
            _ => return Err("png: DEFLATE decompression error"),
        }

        Ok(())
    }
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> ReadExact for DeflateSource<F> {
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), &'static str> {
        let mut total = 0usize;
        while total < buf.len() {
            if self.avail == 0 {
                if self.done {
                    return Err("png: unexpected end of DEFLATE stream");
                }
                self.pump()?;
                if self.avail == 0 {
                    return Err("png: unexpected end of DEFLATE stream");
                }
            }
            let rp = self.read_pos & (ZIP_DEFLATE_WINDOW - 1);
            let contiguous = (ZIP_DEFLATE_WINDOW - rp).min(self.avail);
            let n = contiguous.min(buf.len() - total);
            buf[total..total + n].copy_from_slice(&self.window[rp..rp + n]);
            self.read_pos += n;
            self.avail -= n;
            total += n;
        }
        Ok(())
    }
}

// decode a PNG from a STORED ZIP entry by streaming from SD
pub fn decode_png_sd<F>(
    read_fn: F,
    data_offset: u32,
    data_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    let mut src = SdSource {
        read_fn,
        offset: data_offset,
        end: data_offset + data_size,
    };
    decode_png_from(&mut src, max_w, max_h)
}

// decode a PNG from a DEFLATE-compressed ZIP entry by streaming
pub fn decode_png_deflate_sd<F>(
    read_fn: F,
    data_offset: u32,
    comp_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    let mut src = DeflateSource::new(read_fn, data_offset, comp_size)?;
    decode_png_from(&mut src, max_w, max_h)
}

// core streaming PNG decoder; generic over byte source.
// reads chunks sequentially, feeds IDAT into zlib row-by-row; never holds full PNG in RAM.
fn decode_png_from<R: ReadExact>(
    src: &mut R,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str> {
    // PNG signature
    let mut sig = [0u8; 8];
    src.read_exact(&mut sig)?;
    if sig != PNG_SIG {
        return Err("png: invalid signature");
    }

    // IHDR (must be first chunk)
    let mut chunk_hdr = [0u8; 8]; // 4-byte length + 4-byte type
    src.read_exact(&mut chunk_hdr)?;
    let ihdr_len = be_u32(&chunk_hdr, 0) as usize;
    if [chunk_hdr[4], chunk_hdr[5], chunk_hdr[6], chunk_hdr[7]] != CHUNK_IHDR || ihdr_len < 13 {
        return Err("png: missing or invalid IHDR");
    }
    let mut ihdr_raw = [0u8; 13];
    src.read_exact(&mut ihdr_raw)?;
    if ihdr_len > 13 {
        src.skip(ihdr_len - 13)?;
    }
    src.skip(4)?; // skip CRC

    let header = PngHeader {
        width: be_u32(&ihdr_raw, 0),
        height: be_u32(&ihdr_raw, 4),
        bit_depth: ihdr_raw[8],
        color_type: ihdr_raw[9],
    };
    if header.width == 0 || header.height == 0 {
        return Err("png: zero dimensions");
    }
    if ihdr_raw[12] != 0 {
        return Err("png: interlaced PNGs not supported");
    }
    match (header.color_type, header.bit_depth) {
        (COLOR_GREYSCALE, 1 | 2 | 4 | 8 | 16) => {}
        (COLOR_RGB, 8 | 16) => {}
        (COLOR_PALETTE, 1 | 2 | 4 | 8) => {}
        (COLOR_GREY_ALPHA, 8 | 16) => {}
        (COLOR_RGBA, 8 | 16) => {}
        _ => return Err("png: unsupported colour type / bit depth"),
    }
    if header.width.saturating_mul(header.height) > MAX_PIXELS {
        return Err("png: image exceeds pixel limit");
    }

    // scan for PLTE, skip to first IDAT
    let mut plte: Option<Vec<u8>> = None;
    let first_idat_len: usize;
    loop {
        src.read_exact(&mut chunk_hdr)?;
        let clen = be_u32(&chunk_hdr, 0) as usize;
        let ctype = [chunk_hdr[4], chunk_hdr[5], chunk_hdr[6], chunk_hdr[7]];
        if ctype == CHUNK_IDAT {
            first_idat_len = clen;
            break;
        } else if ctype == CHUNK_PLTE && clen <= 768 && clen % 3 == 0 {
            let mut p = Vec::new();
            p.try_reserve_exact(clen).map_err(|_| "png: OOM for PLTE")?;
            p.resize(clen, 0);
            src.read_exact(&mut p)?;
            src.skip(4)?; // CRC
            plte = Some(p);
        } else {
            src.skip(clen + 4)?; // skip data + CRC
        }
    }

    let palette_grey = build_palette_lut(header.color_type, &plte)?;
    drop(plte);

    // output dimensions
    let scale = {
        let sw = header
            .width
            .checked_add(max_w as u32 - 1)
            .unwrap_or(u32::MAX)
            / max_w as u32;
        let sh = header
            .height
            .checked_add(max_h as u32 - 1)
            .unwrap_or(u32::MAX)
            / max_h as u32;
        sw.max(sh).max(1) as usize
    };
    let out_w = (header.width as usize / scale).max(1);
    let out_h = (header.height as usize / scale).max(1);
    let out_stride = (out_w + 7) / 8;
    let scanline_bytes = header.scanline_bytes();
    let bpp = header.bytes_per_pixel();
    let src_h = header.height as usize;

    log::info!(
        "png: streaming {}x{} -> {}x{} (scale {})",
        header.width,
        header.height,
        out_w,
        out_h,
        scale
    );

    // allocate working buffers
    let mut output = Vec::new();
    output
        .try_reserve_exact(out_stride * out_h)
        .map_err(|_| "png: OOM for output bitmap")?;
    output.resize(out_stride * out_h, 0u8);

    let mut prev_row = vec![0u8; scanline_bytes];
    let mut curr_row = vec![0u8; scanline_bytes];
    let mut err_cur = vec![0i16; out_w + 2];
    let mut err_nxt = vec![0i16; out_w + 2];
    let row_total = 1 + scanline_bytes;
    let mut row_buf = vec![0u8; row_total];
    let mut row_pos: usize = 0;

    // streaming zlib decompressor for IDAT data
    let decomp_layout = core::alloc::Layout::new::<miniz_oxide::inflate::core::DecompressorOxide>();
    let decomp_ptr = unsafe { alloc::alloc::alloc_zeroed(decomp_layout) };
    if decomp_ptr.is_null() {
        return Err("png: OOM for decompressor");
    }
    let mut decomp =
        unsafe { Box::from_raw(decomp_ptr as *mut miniz_oxide::inflate::core::DecompressorOxide) };
    let mut dict = vec![0u8; DICT_SIZE];
    let mut dict_pos: usize = 0;
    let mut src_y: usize = 0;
    let mut out_y: usize = 0;

    // feed IDAT chunks into zlib row-by-row
    let mut idat_buf = [0u8; SD_READ_BUF];
    let mut in_avail: usize = 0;
    let mut idat_chunk_left = first_idat_len;
    let mut more_idat = true;

    loop {
        // top up input buffer from the IDAT stream
        while in_avail < SD_READ_BUF {
            if idat_chunk_left > 0 {
                let space = SD_READ_BUF - in_avail;
                let want = idat_chunk_left.min(space);
                src.read_exact(&mut idat_buf[in_avail..in_avail + want])?;
                in_avail += want;
                idat_chunk_left -= want;
            } else if more_idat {
                src.skip(4)?; // CRC
                src.read_exact(&mut chunk_hdr)?;
                let clen = be_u32(&chunk_hdr, 0) as usize;
                let ctype = [chunk_hdr[4], chunk_hdr[5], chunk_hdr[6], chunk_hdr[7]];
                if ctype == CHUNK_IDAT {
                    idat_chunk_left = clen;
                } else {
                    more_idat = false;
                    break;
                }
            } else {
                break;
            }
        }

        let has_more = idat_chunk_left > 0 || more_idat;
        let flags = miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
            | if has_more {
                miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
            } else {
                0
            };

        let write_pos = dict_pos & (DICT_SIZE - 1);
        let (status, consumed, produced) = miniz_oxide::inflate::core::decompress(
            &mut *decomp,
            &idat_buf[..in_avail],
            &mut dict,
            write_pos,
            flags,
        );

        if consumed > 0 && consumed < in_avail {
            idat_buf.copy_within(consumed..in_avail, 0);
        }
        in_avail -= consumed;

        // feed decompressed bytes into the scanline accumulator
        for i in 0..produced {
            row_buf[row_pos] = dict[(write_pos + i) & (DICT_SIZE - 1)];
            row_pos += 1;

            if row_pos == row_total {
                let filter = row_buf[0];
                curr_row.copy_from_slice(&row_buf[1..]);

                unfilter_row(filter, &mut curr_row, &prev_row, bpp);

                if src_y % scale == 0 && out_y < out_h {
                    dither_row(
                        &curr_row,
                        &header,
                        &palette_grey,
                        scale,
                        out_w,
                        &mut err_cur,
                        &mut err_nxt,
                        &mut output[out_y * out_stride..(out_y + 1) * out_stride],
                    );
                    out_y += 1;
                    core::mem::swap(&mut err_cur, &mut err_nxt);
                    err_nxt.fill(0);
                }

                core::mem::swap(&mut prev_row, &mut curr_row);
                curr_row.fill(0);
                row_pos = 0;
                src_y += 1;
            }
        }

        dict_pos += produced;

        match status {
            miniz_oxide::inflate::TINFLStatus::Done => break,
            miniz_oxide::inflate::TINFLStatus::NeedsMoreInput => {
                if !has_more && in_avail == 0 {
                    return Err("png: truncated IDAT stream");
                }
                if consumed == 0 && produced == 0 && in_avail >= SD_READ_BUF {
                    return Err("png: IDAT decompression stuck");
                }
            }
            miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {
                if produced == 0 && consumed == 0 {
                    return Err("png: decompression stalled (output)");
                }
            }
            _ => return Err("png: IDAT decompression error"),
        }
    }

    if src_y < src_h {
        log::warn!("png: expected {} rows, got {}", src_h, src_y);
    }

    Ok(DecodedImage {
        width: out_w as u16,
        height: out_y as u16,
        data: output,
        stride: out_stride,
    })
}

// IHDR / chunk parsing

struct PngHeader {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
}

impl PngHeader {
    // bytes per complete pixel; filter stride for Sub/Paeth; 1 for sub-byte depths
    fn bytes_per_pixel(&self) -> usize {
        let channels: usize = match self.color_type {
            COLOR_GREYSCALE => 1,
            COLOR_RGB => 3,
            COLOR_PALETTE => 1,
            COLOR_GREY_ALPHA => 2,
            COLOR_RGBA => 4,
            _ => 1,
        };
        if self.bit_depth >= 8 {
            channels * (self.bit_depth as usize / 8)
        } else {
            1 // sub-byte packed
        }
    }

    // byte length of one unfiltered row (without the leading filter byte)
    fn scanline_bytes(&self) -> usize {
        let bits_per_pixel: usize = match self.color_type {
            COLOR_GREYSCALE => self.bit_depth as usize,
            COLOR_RGB => 3 * self.bit_depth as usize,
            COLOR_PALETTE => self.bit_depth as usize,
            COLOR_GREY_ALPHA => 2 * self.bit_depth as usize,
            COLOR_RGBA => 4 * self.bit_depth as usize,
            _ => self.bit_depth as usize,
        };
        (self.width as usize * bits_per_pixel + 7) / 8
    }
}

// big-endian u32 (PNG uses network byte order)
#[inline]
fn be_u32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

// iterator over PNG chunks; yields (type, data) pairs
struct ChunkIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> ChunkIter<'a> {
    fn new(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 8 || data[..8] != PNG_SIG {
            return Err("png: invalid signature");
        }
        Ok(Self { data, pos: 8 })
    }
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = ([u8; 4], &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos + 12 > self.data.len() {
            return None;
        }
        let len = be_u32(self.data, self.pos) as usize;
        let ctype: [u8; 4] = self.data[self.pos + 4..self.pos + 8].try_into().ok()?;
        let data_start = self.pos + 8;
        let data_end = data_start + len;
        if data_end + 4 > self.data.len() {
            return None;
        }
        self.pos = data_end + 4; // CRC
        Some((ctype, &self.data[data_start..data_end]))
    }
}

fn parse_ihdr(data: &[u8]) -> Result<PngHeader, &'static str> {
    let mut chunks = ChunkIter::new(data)?;
    let (ctype, cdata) = chunks.next().ok_or("png: missing IHDR")?;
    if ctype != CHUNK_IHDR || cdata.len() < 13 {
        return Err("png: invalid IHDR");
    }

    let header = PngHeader {
        width: be_u32(cdata, 0),
        height: be_u32(cdata, 4),
        bit_depth: cdata[8],
        color_type: cdata[9],
    };

    if header.width == 0 || header.height == 0 {
        return Err("png: zero dimensions");
    }
    if cdata[12] != 0 {
        return Err("png: interlaced PNGs not supported");
    }

    match (header.color_type, header.bit_depth) {
        (COLOR_GREYSCALE, 1 | 2 | 4 | 8 | 16) => {}
        (COLOR_RGB, 8 | 16) => {}
        (COLOR_PALETTE, 1 | 2 | 4 | 8) => {}
        (COLOR_GREY_ALPHA, 8 | 16) => {}
        (COLOR_RGBA, 8 | 16) => {}
        _ => return Err("png: unsupported colour type / bit depth"),
    }

    Ok(header)
}

// concatenate all IDAT chunk payloads into a single buffer
fn collect_idat(data: &[u8]) -> Result<Vec<u8>, &'static str> {
    let chunks = ChunkIter::new(data)?;
    let total: usize = chunks
        .filter(|(t, _)| *t == CHUNK_IDAT)
        .map(|(_, d)| d.len())
        .sum();
    if total == 0 {
        return Err("png: no IDAT data");
    }

    let mut idat = Vec::new();
    idat.try_reserve_exact(total)
        .map_err(|_| "png: IDAT too large for memory")?;
    for (ctype, cdata) in ChunkIter::new(data).unwrap() {
        if ctype == CHUNK_IDAT {
            idat.extend_from_slice(cdata);
        }
    }
    Ok(idat)
}

// read PLTE chunk if present; up to 768 bytes (256 x RGB)
fn collect_plte(data: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
    for (ctype, cdata) in ChunkIter::new(data)? {
        if ctype == CHUNK_PLTE {
            if cdata.len() % 3 != 0 || cdata.len() > 768 {
                return Err("png: invalid PLTE");
            }
            let mut plte = Vec::new();
            plte.try_reserve_exact(cdata.len())
                .map_err(|_| "png: OOM for PLTE")?;
            plte.extend_from_slice(cdata);
            return Ok(Some(plte));
        }
    }
    Ok(None)
}

// build a 256-entry greyscale LUT from the palette
fn build_palette_lut(color_type: u8, plte: &Option<Vec<u8>>) -> Result<[u8; 256], &'static str> {
    let mut lut = [0u8; 256];
    if color_type == COLOR_PALETTE {
        let plte_data = plte.as_ref().ok_or("png: palette image without PLTE")?;
        for i in 0..plte_data.len() / 3 {
            let r = plte_data[i * 3] as u16;
            let g = plte_data[i * 3 + 1] as u16;
            let b = plte_data[i * 3 + 2] as u16;
            // BT.601 luma: 0.299R + 0.587G + 0.114B
            lut[i] = ((r * 77 + g * 150 + b * 29) >> 8) as u8;
        }
    }
    Ok(lut)
}

// unfiltering

// reconstruct one scanline in-place given the previous unfiltered row; bpp = byte stride
fn unfilter_row(filter: u8, row: &mut [u8], prev: &[u8], bpp: usize) {
    let len = row.len();
    match filter {
        FILTER_NONE => {}
        FILTER_SUB => {
            for i in bpp..len {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        FILTER_UP => {
            for i in 0..len {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        FILTER_AVERAGE => {
            for i in 0..len {
                let a = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                let b = prev[i] as u16;
                row[i] = row[i].wrapping_add(((a + b) / 2) as u8);
            }
        }
        FILTER_PAETH => {
            for i in 0..len {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                row[i] = row[i].wrapping_add(paeth(a, b, c));
            }
        }
        _ => {} // unknown filter; treat as None (best-effort)
    }
}

#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let a = a as i16;
    let b = b as i16;
    let c = c as i16;
    let p = a + b - c;
    let pa = (p - a).unsigned_abs();
    let pb = (p - b).unsigned_abs();
    let pc = (p - c).unsigned_abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

// pixel -> greyscale conversion

// sample one pixel from an unfiltered scanline; return 0-255 grey.
// alpha pre-blended against white (e-paper background).
#[inline]
fn pixel_to_grey(row: &[u8], x: usize, hdr: &PngHeader, pal: &[u8; 256]) -> u8 {
    match (hdr.color_type, hdr.bit_depth) {
        // greyscale
        (COLOR_GREYSCALE, 8) => row[x],
        (COLOR_GREYSCALE, 16) => row[x * 2], // high byte only
        (COLOR_GREYSCALE, bd) => unpack_sub_byte(row, x, bd),

        // RGB
        (COLOR_RGB, 8) => rgb_to_grey(row[x * 3], row[x * 3 + 1], row[x * 3 + 2]),
        (COLOR_RGB, 16) => rgb_to_grey(row[x * 6], row[x * 6 + 2], row[x * 6 + 4]),

        // palette
        (COLOR_PALETTE, 8) => pal[row[x] as usize],
        (COLOR_PALETTE, bd) => {
            let idx = unpack_sub_byte_raw(row, x, bd);
            pal[idx as usize]
        }

        // greyscale + alpha
        (COLOR_GREY_ALPHA, 8) => blend_white(row[x * 2], row[x * 2 + 1]),
        (COLOR_GREY_ALPHA, 16) => blend_white(row[x * 4], row[x * 4 + 2]),

        // RGBA
        (COLOR_RGBA, 8) => {
            let g = rgb_to_grey(row[x * 4], row[x * 4 + 1], row[x * 4 + 2]);
            blend_white(g, row[x * 4 + 3])
        }
        (COLOR_RGBA, 16) => {
            let g = rgb_to_grey(row[x * 8], row[x * 8 + 2], row[x * 8 + 4]);
            blend_white(g, row[x * 8 + 6])
        }

        _ => 128, // unreachable for validated header
    }
}

// BT.601 luma from 8-bit RGB channels
#[inline]
fn rgb_to_grey(r: u8, g: u8, b: u8) -> u8 {
    ((r as u16 * 77 + g as u16 * 150 + b as u16 * 29) >> 8) as u8
}

// alpha-blend grey against white: out = grey*a/255 + 255*(255-a)/255
#[inline]
fn blend_white(grey: u8, alpha: u8) -> u8 {
    let g = grey as u16;
    let a = alpha as u16;
    ((g * a + 255 * (255 - a)) / 255) as u8
}

// unpack a sub-byte greyscale sample (1/2/4 bit) and scale to 0-255
#[inline]
fn unpack_sub_byte(row: &[u8], x: usize, bit_depth: u8) -> u8 {
    let raw = unpack_sub_byte_raw(row, x, bit_depth);
    let max = (1u16 << bit_depth) - 1;
    (raw as u16 * 255 / max) as u8
}

// unpack a sub-byte sample without rescaling (for palette index)
#[inline]
fn unpack_sub_byte_raw(row: &[u8], x: usize, bit_depth: u8) -> u8 {
    let bpp = bit_depth as usize;
    let ppb = 8 / bpp; // pixels per byte
    let byte_idx = x / ppb;
    let bit_offset = (ppb - 1 - x % ppb) * bpp;
    let mask = (1u8 << bpp) - 1;
    (row[byte_idx] >> bit_offset) & mask
}

// Floyd-Steinberg dithering

// dither one source row into 1-bit output; pick every scale-th pixel
fn dither_row(
    src_row: &[u8],
    hdr: &PngHeader,
    pal: &[u8; 256],
    scale: usize,
    out_w: usize,
    err_cur: &mut [i16],
    err_nxt: &mut [i16],
    out_row: &mut [u8],
) {
    for ox in 0..out_w {
        let sx = ox * scale;
        let grey = pixel_to_grey(src_row, sx, hdr, pal) as i16;
        // add accumulated error (offset by 1 for the left sentinel)
        let val = (grey + err_cur[ox + 1]).clamp(0, 255);
        // val < 128 -> black (bit set), else white (bit clear)
        let black = val < 128;
        let quantised = if black { 0i16 } else { 255 };
        let err = val - quantised;

        if black {
            out_row[ox / 8] |= 1 << (7 - (ox & 7));
        }

        // distribute error to neighbours (Floyd-Steinberg weights)
        err_cur[ox + 2] += err * 7 / 16; // right
        err_nxt[ox] += err * 3 / 16; // below-left
        err_nxt[ox + 1] += err * 5 / 16; // below
        err_nxt[ox + 2] += err / 16; // below-right
    }
}
