// SSD1677 e-paper display driver (generic, board-independent).
// Tested on GDEQ0426T82 (800×480) via XTEink X4 board.
// No framebuffer in SRAM; pixels are streamed to the controller
// through a 4KB StripBuffer.
//
// Partial refresh sequence matches GxEPD2_426_GDEQ0426T82:
//   1. Write new content to BW RAM (0x24) only
//   2. DU refresh (Mode 2, 0xFC)
//   3. Write new content to BOTH RED (0x26) AND BW (0x24)
// This ensures both RAM planes are perfectly synced after every
// partial update, giving the DU waveform a correct baseline.

use embedded_graphics_core::geometry::{OriginDimensions, Size};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use esp_hal::delay::Delay;

use super::strip::{STRIP_COUNT, StripBuffer}; // sibling in drivers/

pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

pub const SPI_FREQ_MHZ: u32 = 20;

const POWER_OFF_TIME_MS: u32 = 200;
const FULL_REFRESH_TIME_MS: u32 = 1600;
const PARTIAL_REFRESH_TIME_MS: u32 = 600;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

// SSD1677 command table
#[allow(dead_code)]
mod cmd {
    pub const DRIVER_OUTPUT_CONTROL: u8 = 0x01;
    pub const BOOSTER_SOFT_START: u8 = 0x0C;
    pub const DEEP_SLEEP: u8 = 0x10;
    pub const DATA_ENTRY_MODE: u8 = 0x11;
    pub const SW_RESET: u8 = 0x12;
    pub const TEMPERATURE_SENSOR: u8 = 0x18;
    pub const WRITE_TEMP_REGISTER: u8 = 0x1A;
    pub const MASTER_ACTIVATION: u8 = 0x20;
    pub const DISPLAY_UPDATE_CONTROL_1: u8 = 0x21;
    pub const DISPLAY_UPDATE_CONTROL_2: u8 = 0x22;
    pub const WRITE_RAM_BW: u8 = 0x24; // current/new buffer
    pub const WRITE_RAM_RED: u8 = 0x26; // previous buffer (differential)
    pub const BORDER_WAVEFORM: u8 = 0x3C;
    pub const SET_RAM_X_RANGE: u8 = 0x44;
    pub const SET_RAM_Y_RANGE: u8 = 0x45;
    pub const SET_RAM_X_COUNTER: u8 = 0x4E;
    pub const SET_RAM_Y_COUNTER: u8 = 0x4F;
}

pub struct DisplayDriver<SPI, DC, RST, BUSY> {
    spi: SPI,
    dc: DC,
    rst: RST,
    busy: BUSY,
    rotation: Rotation,
    power_is_on: bool,
    init_done: bool,
    initial_refresh: bool,
}

impl<SPI, DC, RST, BUSY, E> DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    pub fn new(spi: SPI, dc: DC, rst: RST, busy: BUSY) -> Self {
        Self {
            spi,
            dc,
            rst,
            busy,
            rotation: Rotation::Deg270,
            power_is_on: false,
            init_done: false,
            initial_refresh: true,
        }
    }

    pub fn reset(&mut self, delay: &mut Delay) {
        let _ = self.rst.set_high();
        delay.delay_millis(20);
        let _ = self.rst.set_low();
        delay.delay_millis(2);
        let _ = self.rst.set_high();
        delay.delay_millis(20);
    }

    pub fn init(&mut self, delay: &mut Delay) {
        self.reset(delay);
        self.init_display(delay);
    }

    // ── Full refresh ────────────────────────────────────────────
    //
    // Matches GxEPD2 clearScreen / writeImageForFullRefresh:
    //   Write identical content to RED (0x26) then BW (0x24),
    //   followed by a GC (Mode 1) update.

    pub fn render_full<F>(&mut self, strip: &mut StripBuffer, delay: &mut Delay, draw: F)
    where
        F: Fn(&mut StripBuffer),
    {
        if !self.init_done {
            self.init_display(delay);
        }

        delay.delay_millis(1);

        for &ram_cmd in &[cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
            self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
            self.send_command(ram_cmd);
            delay.delay_millis(1);

            for i in 0..STRIP_COUNT {
                strip.begin_strip(self.rotation, i);
                draw(strip);
                self.send_data(strip.data());
            }
        }

        self.update_full();
        self.initial_refresh = false;
    }

    // ── Partial refresh (DU waveform) ───────────────────────────
    //
    // Matches GxEPD2 drawImage:
    //   1. writeImage        → BW (0x24) only
    //   2. refresh (partial) → DU update (0xFC)
    //   3. writeImageAgain   → RED (0x26) then BW (0x24)
    //
    // Step 3 ensures both planes are perfectly synced so the next
    // DU refresh has the correct "old frame" in RED RAM.

    pub fn render_partial<F>(
        &mut self,
        strip: &mut StripBuffer,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        delay: &mut Delay,
        draw: F,
    ) where
        F: Fn(&mut StripBuffer),
    {
        if self.initial_refresh {
            return self.render_full(strip, delay, draw);
        }

        if !self.init_done {
            self.init_display(delay);
        }

        let (tx, ty, tw, th) = self.transform_region(x, y, w, h);

        // Align to 8-pixel byte boundaries for SSD1677 RAM addressing
        let px = (tx & !7).min(WIDTH);
        let py = ty.min(HEIGHT);
        let pw = ((tw + (tx & 7) + 7) & !7).min(WIDTH - px);
        let ph = th.min(HEIGHT - py);

        if pw == 0 || ph == 0 {
            return;
        }

        // Alignment padding masks.
        //
        // px may start up to 7 pixels before tx, and px+pw may
        // extend up to 7 pixels past tx+tw.  The draw closure
        // renders all visible widgets, so widgets just outside the
        // logical dirty region can bleed pixels into those padding
        // columns (their physical coordinates pass the strip-window
        // clip check).
        //
        // Force padding columns to white (0xFF = no ink) in every
        // RAM write so the DU waveform never alters the display
        // outside the intended dirty region.
        let lp = (tx - px) as u32;
        let rp = ((px + pw) - (tx + tw)) as u32;
        let left_mask: u8 = if lp > 0 { !((1u8 << (8 - lp)) - 1) } else { 0 };
        let right_mask: u8 = if rp > 0 { (1u8 << rp) - 1 } else { 0 };

        // Step 1: write new content to BW RAM only (the "new frame").
        self.write_region_strips(
            strip,
            px,
            py,
            pw,
            ph,
            cmd::WRITE_RAM_BW,
            &draw,
            left_mask,
            right_mask,
        );

        // Step 2: DU partial update.
        // set_partial_ram_area was called inside write_region_strips
        // with the full (px,py,pw,ph) region, so the update window
        // is correct.  Call it again explicitly to be safe (matches
        // GxEPD2's refresh() calling _setPartialRamArea before
        // _Update_Part).
        self.set_partial_ram_area(px, py, pw, ph);
        self.update_partial();

        // Step 3: sync both planes (matches GxEPD2 writeImageAgain).
        // Write RED first, then BW — both get the new content.
        for &ram_cmd in &[cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
            self.write_region_strips(strip, px, py, pw, ph, ram_cmd, &draw, left_mask, right_mask);
        }

        self.power_off();
    }

    pub fn power_off(&mut self) {
        if self.power_is_on {
            self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
            self.send_data(&[0x83]);
            self.send_command(cmd::MASTER_ACTIVATION);
            self.wait_busy(POWER_OFF_TIME_MS);
            self.power_is_on = false;
        }
    }

    // ── Strip data helpers ──────────────────────────────────────

    // send strip data, masking alignment-padding columns to white (0xFF) when needed;
    // fast path skips masking and sends the raw buffer in a single SPI transaction
    fn send_strip_data(&mut self, data: &[u8], row_bytes: usize, left_mask: u8, right_mask: u8) {
        let needs_mask = left_mask != 0 || right_mask != 0;

        if !needs_mask {
            self.send_data(data);
            return;
        }

        // Row-by-row: force padding columns to white (0xFF) so
        // they don't alter the display outside the logical dirty
        // region.  row_bytes is at most WIDTH/8 = 100.
        let mut tmp = [0xFFu8; 100];
        for row in data.chunks(row_bytes) {
            let n = row.len();
            tmp[..n].copy_from_slice(row);
            tmp[0] |= left_mask;
            if n > 0 {
                tmp[n - 1] |= right_mask;
            }
            self.send_data(&tmp[..n]);
        }
    }

    fn write_region_strips<F>(
        &mut self,
        strip: &mut StripBuffer,
        px: u16,
        py: u16,
        pw: u16,
        ph: u16,
        ram_cmd: u8,
        draw: &F,
        left_mask: u8,
        right_mask: u8,
    ) where
        F: Fn(&mut StripBuffer),
    {
        let max_rows = StripBuffer::max_rows_for_width(pw);
        let row_bytes = (pw / 8) as usize;

        self.set_partial_ram_area(px, py, pw, ph);
        self.send_command(ram_cmd);

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);
            self.send_strip_data(strip.data(), row_bytes, left_mask, right_mask);
            y += rows;
        }
    }

    // ── Display init ────────────────────────────────────────────
    // Matches GxEPD2 _InitDisplay exactly.

    fn init_display(&mut self, delay: &mut Delay) {
        self.send_command(cmd::SW_RESET);
        delay.delay_millis(10);

        self.send_command(cmd::TEMPERATURE_SENSOR);
        self.send_data(&[0x80]);

        self.send_command(cmd::BOOSTER_SOFT_START);
        self.send_data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x80]);

        self.send_command(cmd::DRIVER_OUTPUT_CONTROL);
        self.send_data(&[((HEIGHT - 1) & 0xFF) as u8, ((HEIGHT - 1) >> 8) as u8, 0x02]);

        self.send_command(cmd::BORDER_WAVEFORM);
        self.send_data(&[0x01]);

        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);

        self.init_done = true;
    }

    // ── Coordinate helpers ──────────────────────────────────────

    fn transform_region(&self, x: u16, y: u16, w: u16, h: u16) -> (u16, u16, u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (x, y, w, h),
            Rotation::Deg90 => (WIDTH - y - h, x, h, w),
            Rotation::Deg180 => (WIDTH - x - w, HEIGHT - y - h, w, h),
            Rotation::Deg270 => (y, HEIGHT - x - w, h, w),
        }
    }

    // Gates are wired in reverse on this panel; Y must be flipped.
    // Matches GxEPD2 _setPartialRamArea exactly.
    fn set_partial_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let y_flipped = HEIGHT - y - h;

        // X increment, Y decrement (compensates gate reversal)
        self.send_command(cmd::DATA_ENTRY_MODE);
        self.send_data(&[0x01]);

        self.send_command(cmd::SET_RAM_X_RANGE);
        self.send_data(&[
            (x & 0xFF) as u8,
            (x >> 8) as u8,
            ((x + w - 1) & 0xFF) as u8,
            ((x + w - 1) >> 8) as u8,
        ]);

        self.send_command(cmd::SET_RAM_Y_RANGE);
        self.send_data(&[
            ((y_flipped + h - 1) & 0xFF) as u8,
            ((y_flipped + h - 1) >> 8) as u8,
            (y_flipped & 0xFF) as u8,
            (y_flipped >> 8) as u8,
        ]);

        self.send_command(cmd::SET_RAM_X_COUNTER);
        self.send_data(&[(x & 0xFF) as u8, (x >> 8) as u8]);

        self.send_command(cmd::SET_RAM_Y_COUNTER);
        self.send_data(&[
            ((y_flipped + h - 1) & 0xFF) as u8,
            ((y_flipped + h - 1) >> 8) as u8,
        ]);
    }

    // ── Update sequences ────────────────────────────────────────
    // Match GxEPD2 _Update_Full / _Update_Part exactly.

    fn update_full(&mut self) {
        // 0x21: bypass RED as 0 (A[7:4]=0100), BW normal
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        // 0xF7 = Display Mode 1 (GC full waveform)
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(FULL_REFRESH_TIME_MS);

        self.power_is_on = false;
    }

    fn update_partial(&mut self) {
        // 0x21: RED normal, BW normal
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x00, 0x00]);

        // 0xFC = Display Mode 2 (DU partial waveform)
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xFC]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(PARTIAL_REFRESH_TIME_MS);

        self.power_is_on = true;
    }

    // ── Low-level SPI / busy ────────────────────────────────────

    // Sleeps via WFI between polls. BUSY falling edge interrupt
    // wakes immediately; 10ms timer tick is the backstop.
    fn wait_busy(&mut self, timeout_ms: u32) {
        use esp_hal::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        loop {
            if self.busy.is_low().unwrap_or(true) {
                return;
            }
            if Instant::now() >= deadline {
                return;
            }
            #[cfg(target_arch = "riscv32")]
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    fn send_command(&mut self, cmd: u8) {
        let _ = self.dc.set_low();
        let _ = self.spi.write(&[cmd]);
        let _ = self.dc.set_high();
    }

    fn send_data(&mut self, data: &[u8]) {
        let _ = self.dc.set_high();
        let _ = self.spi.write(data);
    }
}

impl<SPI, DC, RST, BUSY, E> OriginDimensions for DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    fn size(&self) -> Size {
        match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => Size::new(WIDTH as u32, HEIGHT as u32),
            Rotation::Deg90 | Rotation::Deg270 => Size::new(HEIGHT as u32, WIDTH as u32),
        }
    }
}
