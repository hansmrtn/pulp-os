//! Minimal baseline JPEG decoder producing 1-bit Floyd–Steinberg dithered bitmaps.
//!
//! Streams MCU-row-by-row via 4 KB chunked reads; peak RAM ≈ 30 KB.
//! Luminance (Y) channel only — chrominance is Huffman-decoded to
//! advance the bitstream, then discarded.
//!
//! Progressive JPEG (SOF2) is partially supported: first scan only
//! (DC + low-frequency AC).
//!
//! Output is packed 1-bit MSB-first, row-major — see [`DecodedImage`](crate::DecodedImage).

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::DecodedImage;

// JPEG marker bytes

const M_SOF0: u8 = 0xC0;
const M_SOF2: u8 = 0xC2;
const M_DHT: u8 = 0xC4;
const M_SOI: u8 = 0xD8;
const M_EOI: u8 = 0xD9;
const M_SOS: u8 = 0xDA;
const M_DQT: u8 = 0xDB;
const M_DRI: u8 = 0xDD;
const M_RST0: u8 = 0xD0;
const M_RST7: u8 = 0xD7;

// limits

const MAX_COMP: usize = 4;
const MAX_PIXELS: u32 = 2048 * 2048;

// header bytes to read for marker parsing; large APP/EXIF segments skipped by length
const HEADER_READ: usize = 32768;

// chunk size for streaming reads during MCU decode
const CHUNK_SIZE: usize = 4096;

// DEFLATE sliding-window size for streaming ZIP decompression
const DEFLATE_WINDOW: usize = 32768;

// zig-zag scan order

#[rustfmt::skip]
const ZZ: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

// IDCT constants (IJG ISLOW, CONST_BITS = 13)

const CB: i32 = 13;
const P1: i32 = 2;
const F0298: i32 = 2446;
const F0390: i32 = 3196;
const F0541: i32 = 4433;
const F0765: i32 = 6270;
const F0899: i32 = 7373;
const F1175: i32 = 9633;
const F1501: i32 = 12299;
const F1847: i32 = 15137;
const F1961: i32 = 16069;
const F2053: i32 = 16819;
const F2562: i32 = 20995;
const F3072: i32 = 25172;

// types

#[derive(Clone, Copy, Default)]
struct Component {
    id: u8,
    h_samp: u8,
    v_samp: u8,
    qt_idx: u8,
    dc_tbl: u8,
    ac_tbl: u8,
}

struct HuffTable {
    lut: [(u8, u8); 256],
    mincode: [i32; 17],
    maxcode: [i32; 17],
    valptr: [usize; 17],
    values: [u8; 256],
}

struct JpegState {
    width: u16,
    height: u16,
    num_comp: u8,
    comp: [Component; MAX_COMP],
    max_h: u8,
    max_v: u8,
    qt: [[u16; 64]; 4],
    qt_ok: [bool; 4],
    dc_huff: [HuffTable; 4],
    ac_huff: [HuffTable; 4],
    dc_ok: [bool; 4],
    ac_ok: [bool; 4],
    restart_interval: u16,
    // byte offset of entropy data (relative to start of JPEG data)
    scan_start: usize,
    scan_num_comp: u8,
    scan_order: [u8; MAX_COMP],
    progressive: bool,
    // first-scan spectral selection start (0 = DC)
    scan_ss: u8,
    // first-scan spectral selection end (0 = DC only, 63 = all AC)
    scan_se: u8,
    // first-scan successive approximation low bit (point transform)
    scan_al: u8,
}

impl JpegState {
    fn heap_new() -> Result<Box<Self>, &'static str> {
        let layout = core::alloc::Layout::new::<Self>();
        let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err("jpeg: OOM for decoder state");
        }
        let mut st = unsafe { Box::from_raw(ptr as *mut Self) };
        st.max_h = 1;
        st.max_v = 1;
        for ht in st.dc_huff.iter_mut().chain(st.ac_huff.iter_mut()) {
            ht.maxcode.fill(-1);
        }
        Ok(st)
    }
}

// byte source trait + implementations

// raw byte source for the JPEG bitstream
trait JpegRead {
    fn read_byte(&mut self) -> Result<u8, &'static str>;
    fn is_eof(&self) -> bool;
}

// reads from an in-memory slice
struct SliceReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> SliceReader<'a> {
    fn new(data: &'a [u8], start: usize) -> Self {
        Self { data, pos: start }
    }
}

impl JpegRead for SliceReader<'_> {
    #[inline]
    fn read_byte(&mut self) -> Result<u8, &'static str> {
        if self.pos >= self.data.len() {
            return Err("jpeg: unexpected end of data");
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn is_eof(&self) -> bool {
        self.pos >= self.data.len()
    }
}

// reads from SD via closure, buffering 4KB chunks
struct ChunkReader<F> {
    read_fn: F,
    offset: u32, // absolute offset of next byte to fetch
    end: u32,    // end-of-data offset (exclusive)
    buf: [u8; CHUNK_SIZE],
    pos: usize,
    len: usize,
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> ChunkReader<F> {
    fn new(read_fn: F, start: u32, end: u32) -> Self {
        Self {
            read_fn,
            offset: start,
            end,
            buf: [0u8; CHUNK_SIZE],
            pos: 0,
            len: 0,
        }
    }

    fn refill(&mut self) -> Result<(), &'static str> {
        if self.offset >= self.end {
            self.len = 0;
            return Ok(());
        }
        let want = CHUNK_SIZE.min((self.end - self.offset) as usize);
        let n = (self.read_fn)(self.offset, &mut self.buf[..want])?;
        if n == 0 {
            self.len = 0;
            return Ok(());
        }
        self.offset += n as u32;
        self.pos = 0;
        self.len = n;
        Ok(())
    }
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> JpegRead for ChunkReader<F> {
    fn read_byte(&mut self) -> Result<u8, &'static str> {
        if self.pos >= self.len {
            self.refill()?;
            if self.len == 0 {
                return Err("jpeg: unexpected end of data");
            }
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.len && self.offset >= self.end
    }
}

// streaming DEFLATE-from-SD reader; 4KB chunks in, one decompressed byte at a time out.
// peak heap: ~47KB (11KB decompressor + 32KB window + 4KB read buf).
struct DeflateReader<F> {
    read_fn: F,
    file_pos: u32,    // absolute offset of next compressed byte
    comp_left: usize, // compressed bytes remaining in ZIP entry
    rbuf: Vec<u8>,    // compressed-data read buffer
    in_avail: usize,  // valid bytes in rbuf
    decomp: Box<miniz_oxide::inflate::core::DecompressorOxide>, // ~11KB
    window: Vec<u8>,  // 32KB circular dictionary
    dict_pos: usize,  // write position in window (cumulative, mod DEFLATE_WINDOW)
    read_pos: usize,  // next byte to yield from window
    avail: usize,     // decompressed bytes available (dict_pos - read_pos)
    done: bool,       // true once miniz reports Done
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> DeflateReader<F> {
    fn new(read_fn: F, data_offset: u32, comp_size: u32) -> Result<Self, &'static str> {
        use miniz_oxide::inflate::core::DecompressorOxide;

        let decomp_ptr =
            unsafe { alloc::alloc::alloc_zeroed(core::alloc::Layout::new::<DecompressorOxide>()) };
        if decomp_ptr.is_null() {
            return Err("jpeg: OOM for DEFLATE decompressor");
        }
        let decomp = unsafe { Box::from_raw(decomp_ptr as *mut DecompressorOxide) };

        let mut window = Vec::new();
        window
            .try_reserve_exact(DEFLATE_WINDOW)
            .map_err(|_| "jpeg: OOM for DEFLATE window")?;
        window.resize(DEFLATE_WINDOW, 0);

        let mut rbuf = Vec::new();
        rbuf.try_reserve_exact(CHUNK_SIZE)
            .map_err(|_| "jpeg: OOM for DEFLATE read buffer")?;
        rbuf.resize(CHUNK_SIZE, 0);

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

    // decompress more data into the circular window
    fn pump(&mut self) -> Result<(), &'static str> {
        use miniz_oxide::inflate::TINFLStatus;
        use miniz_oxide::inflate::core::{decompress, inflate_flags};

        if self.done {
            return Ok(());
        }

        // top up read buffer from SD
        if self.in_avail < CHUNK_SIZE && self.comp_left > 0 {
            let space = CHUNK_SIZE - self.in_avail;
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
                Err(e) => return Err(e),
            }
        }

        let flags = if self.comp_left > 0 {
            inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            0
        };

        let write_pos = self.dict_pos & (DEFLATE_WINDOW - 1);
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
            _ => return Err("jpeg: DEFLATE decompression error"),
        }

        Ok(())
    }

    // read up to buf.len() decompressed bytes into buf; return count read
    fn read_bytes(&mut self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let mut total = 0usize;
        while total < buf.len() {
            if self.avail == 0 {
                if self.done {
                    break;
                }
                self.pump()?;
                if self.avail == 0 {
                    break;
                }
            }
            let rp = self.read_pos & (DEFLATE_WINDOW - 1);
            let contiguous = (DEFLATE_WINDOW - rp).min(self.avail);
            let n = contiguous.min(buf.len() - total);
            buf[total..total + n].copy_from_slice(&self.window[rp..rp + n]);
            self.read_pos += n;
            self.avail -= n;
            total += n;
        }
        Ok(total)
    }
}

impl<F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>> JpegRead for DeflateReader<F> {
    fn read_byte(&mut self) -> Result<u8, &'static str> {
        if self.avail == 0 {
            if self.done {
                return Err("jpeg: unexpected end of DEFLATE stream");
            }
            self.pump()?;
            if self.avail == 0 {
                return Err("jpeg: unexpected end of DEFLATE stream");
            }
        }
        let rp = self.read_pos & (DEFLATE_WINDOW - 1);
        let b = self.window[rp];
        self.read_pos += 1;
        self.avail -= 1;
        Ok(b)
    }

    fn is_eof(&self) -> bool {
        self.avail == 0 && self.done
    }
}

// BitReader: generic over byte source

struct BitReader<R> {
    source: R,
    buf: u32,
    avail: u8,
    marker: u8, // stashed marker byte (non-zero = encountered during next_byte)
}

impl<R: JpegRead> BitReader<R> {
    fn new(source: R) -> Self {
        Self {
            source,
            buf: 0,
            avail: 0,
            marker: 0,
        }
    }

    // fetch next entropy-coded byte, handling JPEG byte stuffing
    fn next_byte(&mut self) -> Result<u8, &'static str> {
        if self.marker != 0 {
            return Ok(0);
        }
        let b = self.source.read_byte()?;
        if b != 0xFF {
            return Ok(b);
        }
        loop {
            if self.source.is_eof() {
                return Ok(0);
            }
            let next = self.source.read_byte()?;
            match next {
                0x00 => return Ok(0xFF),
                0xFF => continue,
                _ => {
                    self.marker = next;
                    return Ok(0);
                }
            }
        }
    }

    fn ensure(&mut self, n: u8) -> Result<(), &'static str> {
        while self.avail < n {
            let b = self.next_byte()?;
            self.buf |= (b as u32) << (24 - self.avail);
            self.avail += 8;
        }
        Ok(())
    }

    #[inline]
    fn peek(&mut self, n: u8) -> Result<u32, &'static str> {
        self.ensure(n)?;
        Ok(self.buf >> (32 - n as u32))
    }

    #[inline]
    fn drop_bits(&mut self, n: u8) {
        self.buf <<= n as u32;
        self.avail -= n;
    }

    #[inline]
    fn read_bits(&mut self, n: u8) -> Result<u32, &'static str> {
        if n == 0 {
            return Ok(0);
        }
        self.ensure(n)?;
        let val = self.buf >> (32 - n as u32);
        self.buf <<= n as u32;
        self.avail -= n;
        Ok(val)
    }

    // discard remaining bits, advance past the next restart marker
    fn consume_restart(&mut self) -> Result<(), &'static str> {
        self.buf = 0;
        self.avail = 0;

        // if next_byte already stashed a marker, check it now
        if self.marker != 0 {
            let m = self.marker;
            self.marker = 0;
            if m >= M_RST0 && m <= M_RST7 {
                return Ok(());
            }
            // non-RST marker; keep going
            return Ok(());
        }

        // scan forward for the restart marker
        loop {
            if self.source.is_eof() {
                return Ok(());
            }
            let b = self.source.read_byte()?;
            if b != 0xFF {
                continue;
            }
            loop {
                if self.source.is_eof() {
                    return Ok(());
                }
                let m = self.source.read_byte()?;
                match m {
                    0xFF => continue,
                    0x00 => break,
                    M_RST0..=M_RST7 => return Ok(()),
                    _ => return Ok(()),
                }
            }
        }
    }
}

// public API

// decode a baseline JPEG from an in-memory buffer
/// Decode a JPEG from an in-memory buffer to a 1-bit dithered bitmap.
///
/// The image is integer-downscaled so the result fits within
/// `max_w` × `max_h` pixels.
pub fn decode_jpeg_fit(data: &[u8], max_w: u16, max_h: u16) -> Result<DecodedImage, &'static str> {
    let st = parse_markers(data)?;

    validate_tables(&st)?;

    let reader = SliceReader::new(data, st.scan_start);
    decode_baseline(&st, BitReader::new(reader), max_w, max_h)
}

/// Decode a JPEG from a **stored** (uncompressed) ZIP entry by streaming
/// 4 KB chunks through `read_fn`.
///
/// `read_fn(offset, buf)` reads bytes at the given absolute offset and
/// returns the number of bytes actually read. Progressive JPEGs are
/// decoded using the first scan only.
pub fn decode_jpeg_streaming<F>(
    mut read_fn: F,
    data_offset: u32,
    data_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    // read the first portion of the JPEG for marker parsing
    let hdr_size = HEADER_READ.min(data_size as usize);
    let mut hdr = Vec::new();
    hdr.try_reserve_exact(hdr_size)
        .map_err(|_| "jpeg: OOM for header")?;
    hdr.resize(hdr_size, 0);
    let n = read_fn(data_offset, &mut hdr)?;
    hdr.truncate(n);

    let st = parse_markers(&hdr)?;

    validate_tables(&st)?;

    // free header; marker data is now in JpegState
    drop(hdr);

    let scan_abs = data_offset + st.scan_start as u32;
    let end_abs = data_offset + data_size;
    let reader = ChunkReader::new(read_fn, scan_abs, end_abs);

    decode_baseline(&st, BitReader::new(reader), max_w, max_h)
}

/// Backward-compatible alias for [`decode_jpeg_streaming`].
pub fn decode_jpeg_sd<F>(
    read_fn: F,
    data_offset: u32,
    data_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    decode_jpeg_streaming(read_fn, data_offset, data_size, max_w, max_h)
}

/// Decode a JPEG from a **DEFLATE-compressed** ZIP entry by streaming
/// reads through `read_fn`.
///
/// Both ZIP decompression and MCU decode are streamed concurrently,
/// so the full entry is never held in memory. Peak heap ≈ 79 KB.
pub fn decode_jpeg_deflate_streaming<F>(
    read_fn: F,
    data_offset: u32,
    comp_size: u32,
    uncomp_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    let mut deflate = DeflateReader::new(read_fn, data_offset, comp_size)?;

    // decompress enough for marker parsing
    let hdr_size = HEADER_READ.min(uncomp_size as usize);
    let mut hdr = Vec::new();
    hdr.try_reserve_exact(hdr_size)
        .map_err(|_| "jpeg: OOM for header")?;
    hdr.resize(hdr_size, 0);
    let n = deflate.read_bytes(&mut hdr)?;
    hdr.truncate(n);

    let st = parse_markers(&hdr)?;

    validate_tables(&st)?;

    // advance past header bytes already decompressed beyond scan_start;
    // if scan_start > n skip forward; if <= n rewind read cursor
    let scan_start = st.scan_start;
    if scan_start > n {
        // rare: headers larger than HEADER_READ; skip forward
        let skip = scan_start - n;
        let mut trash = [0u8; 256];
        let mut left = skip;
        while left > 0 {
            let chunk = left.min(trash.len());
            let got = deflate.read_bytes(&mut trash[..chunk])?;
            if got == 0 {
                return Err("jpeg: truncated DEFLATE before scan data");
            }
            left -= got;
        }
    } else {
        // bytes from scan_start..n already in window; rewind read cursor
        let rewind = n - scan_start;
        deflate.read_pos -= rewind;
        deflate.avail += rewind;
    }

    // free header; marker data is in JpegState
    drop(hdr);

    decode_baseline(&st, BitReader::new(deflate), max_w, max_h)
}

/// Backward-compatible alias for [`decode_jpeg_deflate_streaming`].
pub fn decode_jpeg_deflate_sd<F>(
    read_fn: F,
    data_offset: u32,
    comp_size: u32,
    uncomp_size: u32,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str>
where
    F: FnMut(u32, &mut [u8]) -> Result<usize, &'static str>,
{
    decode_jpeg_deflate_streaming(read_fn, data_offset, comp_size, uncomp_size, max_w, max_h)
}

// baseline decode core (generic over byte source)

fn validate_tables(st: &JpegState) -> Result<(), &'static str> {
    for sci in 0..st.scan_num_comp as usize {
        let ci = st.scan_order[sci] as usize;
        let c = &st.comp[ci];
        if !st.qt_ok[c.qt_idx as usize] {
            return Err("jpeg: missing quant table");
        }
        if !st.dc_ok[c.dc_tbl as usize] {
            return Err("jpeg: missing DC Huffman table");
        }
        if st.scan_se > 0 && !st.ac_ok[c.ac_tbl as usize] {
            return Err("jpeg: missing AC Huffman table");
        }
    }
    Ok(())
}

fn decode_baseline<R: JpegRead>(
    st: &JpegState,
    mut reader: BitReader<R>,
    max_w: u16,
    max_h: u16,
) -> Result<DecodedImage, &'static str> {
    let w = st.width as usize;
    let h = st.height as usize;
    if w == 0 || h == 0 {
        return Err("jpeg: zero dimensions");
    }
    if (w as u32).saturating_mul(h as u32) > MAX_PIXELS {
        return Err("jpeg: exceeds pixel limit");
    }

    let scale = {
        let sw = (w + max_w as usize - 1) / max_w as usize;
        let sh = (h + max_h as usize - 1) / max_h as usize;
        sw.max(sh).max(1)
    };
    let out_w = (w / scale).max(1);
    let out_h = (h / scale).max(1);
    let out_stride = (out_w + 7) / 8;

    let mcu_w = st.max_h as usize * 8;
    let mcu_h = st.max_v as usize * 8;
    let mcus_x = (w + mcu_w - 1) / mcu_w;
    let mcus_y = (h + mcu_h - 1) / mcu_h;
    let row_w = mcus_x * mcu_w;

    if st.progressive {
        log::warn!(
            "jpeg: progressive {}x{} -> {}x{} (scale {}, first scan Ss={} Se={} Al={})",
            w,
            h,
            out_w,
            out_h,
            scale,
            st.scan_ss,
            st.scan_se,
            st.scan_al
        );
    } else {
        log::info!(
            "jpeg: baseline {}x{} → {}x{} (scale {})",
            w,
            h,
            out_w,
            out_h,
            scale
        );
    }

    // allocate buffers

    let mut y_row = vec![128u8; row_w * mcu_h];
    let mut output = Vec::new();
    output
        .try_reserve_exact(out_stride * out_h)
        .map_err(|_| "jpeg: OOM for output")?;
    output.resize(out_stride * out_h, 0u8);
    let mut err_cur = vec![0i16; out_w + 2];
    let mut err_nxt = vec![0i16; out_w + 2];

    let mut dc_pred = [0i32; MAX_COMP];
    let mut block = [0i32; 64];
    let mut pix = [0u8; 64];
    let mut mcu_cnt: u32 = 0;
    let total_mcus = (mcus_x * mcus_y) as u32;
    let mut out_y: usize = 0;

    // MCU decode loop

    for mcu_row in 0..mcus_y {
        y_row.fill(128);

        for mcu_col in 0..mcus_x {
            for sci in 0..st.scan_num_comp as usize {
                let ci = st.scan_order[sci] as usize;
                let c = &st.comp[ci];
                let is_y = ci == 0;

                for bv in 0..c.v_samp as usize {
                    for bh in 0..c.h_samp as usize {
                        if is_y {
                            decode_block(
                                &mut reader,
                                &st.dc_huff[c.dc_tbl as usize],
                                &st.ac_huff[c.ac_tbl as usize],
                                &mut dc_pred[ci],
                                &st.qt[c.qt_idx as usize],
                                &mut block,
                                st.scan_se as usize,
                                st.scan_al,
                            )?;
                            idct(&block, &mut pix);
                            let bx = mcu_col * mcu_w + bh * 8;
                            let by = bv * 8;
                            for r in 0..8 {
                                let dst = (by + r) * row_w + bx;
                                y_row[dst..dst + 8].copy_from_slice(&pix[r * 8..r * 8 + 8]);
                            }
                        } else {
                            skip_block(
                                &mut reader,
                                &st.dc_huff[c.dc_tbl as usize],
                                &st.ac_huff[c.ac_tbl as usize],
                                &mut dc_pred[ci],
                                st.scan_se as usize,
                            )?;
                        }
                    }
                }
            }

            mcu_cnt += 1;

            if st.restart_interval > 0
                && mcu_cnt % st.restart_interval as u32 == 0
                && mcu_cnt < total_mcus
            {
                reader.consume_restart()?;
                dc_pred.fill(0);
            }
        }

        // dither this MCU row
        for py in 0..mcu_h {
            let src_y = mcu_row * mcu_h + py;
            if src_y >= h || out_y >= out_h {
                break;
            }
            if src_y % scale != 0 {
                continue;
            }
            let row_off = py * row_w;
            let out_row = &mut output[out_y * out_stride..(out_y + 1) * out_stride];
            dither_row_grey(
                &y_row[row_off..],
                scale,
                out_w,
                &mut err_cur,
                &mut err_nxt,
                out_row,
            );
            out_y += 1;
            core::mem::swap(&mut err_cur, &mut err_nxt);
            err_nxt.fill(0);
        }
    }

    Ok(DecodedImage {
        width: out_w as u16,
        height: out_y as u16,
        data: output,
        stride: out_stride,
    })
}

// marker parsing (operates on &[u8] header buffer)

fn parse_markers(data: &[u8]) -> Result<Box<JpegState>, &'static str> {
    if data.len() < 2 || data[0] != 0xFF || data[1] != M_SOI {
        return Err("jpeg: invalid signature");
    }
    let mut st = JpegState::heap_new()?;
    let mut pos = 2usize;
    let len = data.len();

    loop {
        while pos < len && data[pos] != 0xFF {
            pos += 1;
        }
        while pos < len && data[pos] == 0xFF {
            pos += 1;
        }
        if pos >= len {
            return Err("jpeg: truncated");
        }
        let marker = data[pos];
        pos += 1;

        match marker {
            0x00 | M_RST0..=M_RST7 => continue,

            M_SOF0 => parse_sof(data, &mut pos, &mut st, false)?,
            M_SOF2 => parse_sof(data, &mut pos, &mut st, true)?,
            0xC1 | 0xC3 | 0xC5..=0xCB | 0xCD..=0xCF => {
                return Err("jpeg: unsupported SOF variant");
            }
            M_DHT => parse_dht(data, &mut pos, &mut st)?,
            M_DQT => parse_dqt(data, &mut pos, &mut st)?,
            M_DRI => parse_dri(data, &mut pos, &mut st)?,
            M_SOS => {
                parse_sos(data, &mut pos, &mut st)?;
                st.scan_start = pos;
                return Ok(st);
            }
            M_EOI => return Err("jpeg: EOI before SOS"),
            _ => {
                if pos + 2 > len {
                    return Err("jpeg: truncated marker");
                }
                let seg = be_u16(data, pos) as usize;
                if seg < 2 || pos + seg > len {
                    return Err("jpeg: bad marker length");
                }
                pos += seg;
            }
        }
    }
}

fn parse_sof(
    data: &[u8],
    pos: &mut usize,
    st: &mut JpegState,
    progressive: bool,
) -> Result<(), &'static str> {
    if *pos + 2 > data.len() {
        return Err("jpeg: SOF truncated");
    }
    let seg = be_u16(data, *pos) as usize;
    *pos += 2;
    if *pos + seg - 2 > data.len() {
        return Err("jpeg: SOF truncated");
    }
    let p = *pos;
    if data[p] != 8 {
        return Err("jpeg: only 8-bit precision");
    }
    st.height = be_u16(data, p + 1);
    st.width = be_u16(data, p + 3);
    st.num_comp = data[p + 5];
    st.progressive = progressive;
    if st.num_comp == 0 || st.num_comp as usize > MAX_COMP {
        return Err("jpeg: bad component count");
    }
    if p + 6 + st.num_comp as usize * 3 > data.len() {
        return Err("jpeg: SOF truncated");
    }
    let mut off = p + 6;
    st.max_h = 1;
    st.max_v = 1;
    for i in 0..st.num_comp as usize {
        st.comp[i].id = data[off];
        let samp = data[off + 1];
        st.comp[i].h_samp = samp >> 4;
        st.comp[i].v_samp = samp & 0x0F;
        st.comp[i].qt_idx = data[off + 2];
        if st.comp[i].h_samp == 0 || st.comp[i].v_samp == 0 {
            return Err("jpeg: zero sampling factor");
        }
        st.max_h = st.max_h.max(st.comp[i].h_samp);
        st.max_v = st.max_v.max(st.comp[i].v_samp);
        off += 3;
    }
    *pos += seg - 2;
    Ok(())
}

fn parse_dqt(data: &[u8], pos: &mut usize, st: &mut JpegState) -> Result<(), &'static str> {
    if *pos + 2 > data.len() {
        return Err("jpeg: DQT truncated");
    }
    let seg = be_u16(data, *pos) as usize;
    let end = *pos + seg;
    *pos += 2;
    if end > data.len() {
        return Err("jpeg: DQT truncated");
    }
    while *pos < end {
        let info = data[*pos];
        *pos += 1;
        let prec = info >> 4;
        let id = (info & 0x0F) as usize;
        if id >= 4 {
            return Err("jpeg: DQT id out of range");
        }
        if prec == 0 {
            if *pos + 64 > end {
                return Err("jpeg: DQT truncated");
            }
            for i in 0..64 {
                st.qt[id][i] = data[*pos] as u16;
                *pos += 1;
            }
        } else {
            if *pos + 128 > end {
                return Err("jpeg: DQT truncated");
            }
            for i in 0..64 {
                st.qt[id][i] = be_u16(data, *pos);
                *pos += 2;
            }
        }
        st.qt_ok[id] = true;
    }
    Ok(())
}

fn parse_dht(data: &[u8], pos: &mut usize, st: &mut JpegState) -> Result<(), &'static str> {
    if *pos + 2 > data.len() {
        return Err("jpeg: DHT truncated");
    }
    let seg = be_u16(data, *pos) as usize;
    let end = *pos + seg;
    *pos += 2;
    if end > data.len() {
        return Err("jpeg: DHT truncated");
    }
    while *pos < end {
        if *pos + 17 > end {
            return Err("jpeg: DHT truncated");
        }
        let info = data[*pos];
        *pos += 1;
        let class = info >> 4;
        let id = (info & 0x0F) as usize;
        if id >= 4 {
            return Err("jpeg: DHT id out of range");
        }
        let mut bits = [0u8; 16];
        bits.copy_from_slice(&data[*pos..*pos + 16]);
        *pos += 16;
        let total: usize = bits.iter().map(|&b| b as usize).sum();
        if total > 256 || *pos + total > end {
            return Err("jpeg: DHT value overflow");
        }
        let vals = &data[*pos..*pos + total];
        *pos += total;
        if class == 0 {
            build_huff_table(&mut st.dc_huff[id], &bits, vals);
            st.dc_ok[id] = true;
        } else {
            build_huff_table(&mut st.ac_huff[id], &bits, vals);
            st.ac_ok[id] = true;
        }
    }
    Ok(())
}

fn parse_dri(data: &[u8], pos: &mut usize, st: &mut JpegState) -> Result<(), &'static str> {
    if *pos + 4 > data.len() {
        return Err("jpeg: DRI truncated");
    }
    *pos += 2;
    st.restart_interval = be_u16(data, *pos);
    *pos += 2;
    Ok(())
}

fn parse_sos(data: &[u8], pos: &mut usize, st: &mut JpegState) -> Result<(), &'static str> {
    if *pos + 2 > data.len() {
        return Err("jpeg: SOS truncated");
    }
    let seg = be_u16(data, *pos) as usize;
    if *pos + seg > data.len() {
        return Err("jpeg: SOS truncated");
    }
    *pos += 2;
    st.scan_num_comp = data[*pos];
    *pos += 1;
    if st.scan_num_comp == 0 || st.scan_num_comp > st.num_comp {
        return Err("jpeg: bad SOS component count");
    }
    for sci in 0..st.scan_num_comp as usize {
        let cs = data[*pos];
        let td_ta = data[*pos + 1];
        *pos += 2;
        let mut found = false;
        for j in 0..st.num_comp as usize {
            if st.comp[j].id == cs {
                st.comp[j].dc_tbl = td_ta >> 4;
                st.comp[j].ac_tbl = td_ta & 0x0F;
                st.scan_order[sci] = j as u8;
                found = true;
                break;
            }
        }
        if !found {
            return Err("jpeg: SOS references unknown component");
        }
    }
    st.scan_ss = data[*pos];
    st.scan_se = data[*pos + 1];
    let ah_al = data[*pos + 2];
    st.scan_al = ah_al & 0x0F;
    *pos += 3;
    Ok(())
}

// Huffman table construction

fn build_huff_table(table: &mut HuffTable, bits: &[u8; 16], vals: &[u8]) {
    let total: usize = bits.iter().map(|&b| b as usize).sum();
    table.values[..total].copy_from_slice(&vals[..total]);
    table.lut.fill((0, 0));
    table.maxcode.fill(-1);

    let mut code: u32 = 0;
    let mut si: usize = 0;

    for bl in 1..=16usize {
        let cnt = bits[bl - 1] as usize;
        if cnt > 0 {
            table.valptr[bl] = si;
            table.mincode[bl] = code as i32;
            for _ in 0..cnt {
                if bl <= 8 {
                    let prefix = (code << (8 - bl)) as usize;
                    let fill = 1usize << (8 - bl);
                    for k in 0..fill {
                        if prefix + k < 256 {
                            table.lut[prefix + k] = (vals[si], bl as u8);
                        }
                    }
                }
                si += 1;
                code += 1;
            }
            table.maxcode[bl] = (code - 1) as i32;
        }
        code <<= 1;
    }
}

// Huffman decode

fn huff_decode<R: JpegRead>(r: &mut BitReader<R>, t: &HuffTable) -> Result<u8, &'static str> {
    let peek8 = r.peek(8)? as usize;
    let (sym, nb) = t.lut[peek8];
    if nb > 0 {
        r.drop_bits(nb);
        return Ok(sym);
    }
    let peek16 = r.peek(16)? as i32;
    for bl in 9..=16u8 {
        let code = peek16 >> (16 - bl);
        if t.maxcode[bl as usize] >= 0 && code <= t.maxcode[bl as usize] {
            r.drop_bits(bl);
            let idx = t.valptr[bl as usize] as i32 + code - t.mincode[bl as usize];
            return Ok(t.values[idx as usize]);
        }
    }
    Err("jpeg: invalid Huffman code")
}

#[inline]
fn extend(bits: u32, size: u8) -> i32 {
    let half = 1u32 << (size as u32 - 1);
    if bits < half {
        bits as i32 - ((1u32 << size as u32) as i32 - 1)
    } else {
        bits as i32
    }
}

// block decode (Y) / skip (non-Y)

fn decode_block<R: JpegRead>(
    r: &mut BitReader<R>,
    dc_ht: &HuffTable,
    ac_ht: &HuffTable,
    dc_pred: &mut i32,
    qt: &[u16; 64],
    blk: &mut [i32; 64],
    se: usize,
    al: u8,
) -> Result<(), &'static str> {
    blk.fill(0);

    let dc_size = huff_decode(r, dc_ht)?;
    if dc_size > 0 {
        if dc_size > 11 {
            return Err("jpeg: DC size > 11");
        }
        let bits = r.read_bits(dc_size)?;
        *dc_pred += extend(bits, dc_size);
    }
    blk[0] = ((*dc_pred) << al).wrapping_mul(qt[0] as i32);

    if se > 0 {
        let mut k: usize = 1;
        while k <= se {
            let sym = huff_decode(r, ac_ht)?;
            let run = (sym >> 4) as usize;
            let size = sym & 0x0F;
            if size == 0 {
                if run == 15 {
                    k += 16;
                } else {
                    break;
                }
            } else {
                k += run;
                if k > se {
                    return Err("jpeg: AC index overflow");
                }
                let bits = r.read_bits(size)?;
                let val = extend(bits, size);
                blk[ZZ[k]] = (val << al).wrapping_mul(qt[k] as i32);
                k += 1;
            }
        }
    }
    Ok(())
}

fn skip_block<R: JpegRead>(
    r: &mut BitReader<R>,
    dc_ht: &HuffTable,
    ac_ht: &HuffTable,
    dc_pred: &mut i32,
    se: usize,
) -> Result<(), &'static str> {
    let dc_size = huff_decode(r, dc_ht)?;
    if dc_size > 0 {
        let bits = r.read_bits(dc_size)?;
        *dc_pred += extend(bits, dc_size);
    }
    if se > 0 {
        let mut k: usize = 1;
        while k <= se {
            let sym = huff_decode(r, ac_ht)?;
            let run = (sym >> 4) as usize;
            let size = sym & 0x0F;
            if size == 0 {
                if run == 15 {
                    k += 16;
                } else {
                    break;
                }
            } else {
                k += run + 1;
                let _ = r.read_bits(size)?;
            }
        }
    }
    Ok(())
}

// integer IDCT (IJG ISLOW, two-pass row + col)

fn idct(block: &[i32; 64], out: &mut [u8; 64]) {
    let mut ws = [0i32; 64];

    for row in 0..8 {
        let b = row * 8;
        let (d0, d1, d2, d3) = (block[b], block[b + 1], block[b + 2], block[b + 3]);
        let (d4, d5, d6, d7) = (block[b + 4], block[b + 5], block[b + 6], block[b + 7]);

        if d1 == 0 && d2 == 0 && d3 == 0 && d4 == 0 && d5 == 0 && d6 == 0 && d7 == 0 {
            let dc = d0 << P1;
            ws[b..b + 8].fill(dc);
            continue;
        }

        let z1 = (d2 + d6).wrapping_mul(F0541);
        let tmp2 = z1 + d6.wrapping_mul(-F1847);
        let tmp3 = z1 + d2.wrapping_mul(F0765);
        let tmp0 = (d0 + d4) << CB;
        let tmp1 = (d0 - d4) << CB;
        let (t10, t13) = (tmp0 + tmp3, tmp0 - tmp3);
        let (t11, t12) = (tmp1 + tmp2, tmp1 - tmp2);

        let (zz1, zz2, zz3, zz4) = (d7 + d1, d5 + d3, d7 + d3, d5 + d1);
        let z5 = (zz3 + zz4).wrapping_mul(F1175);
        let mut o0 = d7.wrapping_mul(F0298);
        let mut o1 = d5.wrapping_mul(F2053);
        let mut o2 = d3.wrapping_mul(F3072);
        let mut o3 = d1.wrapping_mul(F1501);
        let (s1, s2) = (zz1.wrapping_mul(-F0899), zz2.wrapping_mul(-F2562));
        let s3 = zz3.wrapping_mul(-F1961) + z5;
        let s4 = zz4.wrapping_mul(-F0390) + z5;
        o0 += s1 + s3;
        o1 += s2 + s4;
        o2 += s2 + s3;
        o3 += s1 + s4;

        let sh = CB - P1;
        ws[b] = descale(t10 + o3, sh);
        ws[b + 7] = descale(t10 - o3, sh);
        ws[b + 1] = descale(t11 + o2, sh);
        ws[b + 6] = descale(t11 - o2, sh);
        ws[b + 2] = descale(t12 + o1, sh);
        ws[b + 5] = descale(t12 - o1, sh);
        ws[b + 3] = descale(t13 + o0, sh);
        ws[b + 4] = descale(t13 - o0, sh);
    }

    for col in 0..8 {
        let (d0, d1, d2, d3) = (ws[col], ws[col + 8], ws[col + 16], ws[col + 24]);
        let (d4, d5, d6, d7) = (ws[col + 32], ws[col + 40], ws[col + 48], ws[col + 56]);

        if d1 == 0 && d2 == 0 && d3 == 0 && d4 == 0 && d5 == 0 && d6 == 0 && d7 == 0 {
            let v = clamp(descale(d0, P1 + 3) + 128);
            out[col] = v;
            out[col + 8] = v;
            out[col + 16] = v;
            out[col + 24] = v;
            out[col + 32] = v;
            out[col + 40] = v;
            out[col + 48] = v;
            out[col + 56] = v;
            continue;
        }

        let z1 = (d2 + d6).wrapping_mul(F0541);
        let tmp2 = z1 + d6.wrapping_mul(-F1847);
        let tmp3 = z1 + d2.wrapping_mul(F0765);
        let tmp0 = (d0 + d4) << CB;
        let tmp1 = (d0 - d4) << CB;
        let (t10, t13) = (tmp0 + tmp3, tmp0 - tmp3);
        let (t11, t12) = (tmp1 + tmp2, tmp1 - tmp2);

        let (zz1, zz2, zz3, zz4) = (d7 + d1, d5 + d3, d7 + d3, d5 + d1);
        let z5 = (zz3 + zz4).wrapping_mul(F1175);
        let mut o0 = d7.wrapping_mul(F0298);
        let mut o1 = d5.wrapping_mul(F2053);
        let mut o2 = d3.wrapping_mul(F3072);
        let mut o3 = d1.wrapping_mul(F1501);
        let (s1, s2) = (zz1.wrapping_mul(-F0899), zz2.wrapping_mul(-F2562));
        let s3 = zz3.wrapping_mul(-F1961) + z5;
        let s4 = zz4.wrapping_mul(-F0390) + z5;
        o0 += s1 + s3;
        o1 += s2 + s4;
        o2 += s2 + s3;
        o3 += s1 + s4;

        let sh = CB + P1 + 3;
        out[col] = clamp(descale(t10 + o3, sh) + 128);
        out[col + 56] = clamp(descale(t10 - o3, sh) + 128);
        out[col + 8] = clamp(descale(t11 + o2, sh) + 128);
        out[col + 48] = clamp(descale(t11 - o2, sh) + 128);
        out[col + 16] = clamp(descale(t12 + o1, sh) + 128);
        out[col + 40] = clamp(descale(t12 - o1, sh) + 128);
        out[col + 24] = clamp(descale(t13 + o0, sh) + 128);
        out[col + 32] = clamp(descale(t13 - o0, sh) + 128);
    }
}

// Floyd-Steinberg dithering

// dither one row of Y pixels from the MCU row buffer inline
#[inline]
fn dither_row_grey(
    row: &[u8],
    scale: usize,
    out_w: usize,
    err_cur: &mut [i16],
    err_nxt: &mut [i16],
    out_row: &mut [u8],
) {
    for ox in 0..out_w {
        let sx = ox * scale;
        let g = row[sx] as i16;
        let val = (g + err_cur[ox + 1]).clamp(0, 255);
        let black = val < 128;
        let q = if black { 0i16 } else { 255 };
        let e = val - q;
        if black {
            out_row[ox / 8] |= 1 << (7 - (ox & 7));
        }
        err_cur[ox + 2] += e * 7 / 16;
        err_nxt[ox] += e * 3 / 16;
        err_nxt[ox + 1] += e * 5 / 16;
        err_nxt[ox + 2] += e / 16;
    }
}

// helpers

#[inline]
fn descale(x: i32, n: i32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

#[inline]
fn clamp(x: i32) -> u8 {
    x.clamp(0, 255) as u8
}

#[inline]
fn be_u16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
