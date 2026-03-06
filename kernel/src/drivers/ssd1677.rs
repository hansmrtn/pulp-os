// SSD1677 e-paper driver (board-independent)
// tested on GDEQ0426T82 (800x480), no framebuffer, strip-streamed
//
// partial refresh (3-phase):
//   phase1_bw    -- write new content to BW RAM
//   start_du     -- kick DU waveform; caller polls input while BUSY
//   phase3_sync  -- sync RED+BW; skipped on rapid nav (red_stale)
//
// when phase3 is skipped, phase1_bw_inv_red writes RED=!BW so DU
// drives every pixel to the correct BW target without a full GC

use embedded_graphics_core::geometry::{OriginDimensions, Size};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use esp_hal::delay::Delay;

use super::strip::{STRIP_COUNT, StripBuffer};

pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

pub const SPI_FREQ_MHZ: u32 = 20;

const POWER_OFF_TIME_MS: u32 = 200; // analog shutdown timeout

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

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
    pub const WRITE_RAM_BW: u8 = 0x24;
    pub const WRITE_RAM_RED: u8 = 0x26;
    pub const BORDER_WAVEFORM: u8 = 0x3C;
    pub const SET_RAM_X_RANGE: u8 = 0x44;
    pub const SET_RAM_Y_RANGE: u8 = 0x45;
    pub const SET_RAM_X_COUNTER: u8 = 0x4E;
    pub const SET_RAM_Y_COUNTER: u8 = 0x4F;
}

#[derive(Clone, Copy, Debug)]
pub struct RenderState {
    pub px: u16,
    pub py: u16,
    pub pw: u16,
    pub ph: u16,
    pub left_mask: u8,
    pub right_mask: u8,
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

    #[allow(clippy::too_many_arguments)]
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
        let needs_mask = left_mask != 0 || right_mask != 0;

        self.set_partial_ram_area(px, py, pw, ph);
        self.send_command(ram_cmd);

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);

            if needs_mask && row_bytes > 0 {
                for row in strip.data_mut().chunks_mut(row_bytes) {
                    row[0] |= left_mask;
                    row[row.len() - 1] |= right_mask;
                }
            }
            self.send_data(strip.data());
            y += rows;
        }
    }

    // write BW RAM with content, RED RAM with inverted content
    #[allow(clippy::too_many_arguments)]
    fn write_region_strips_bw_inv_red<F>(
        &mut self,
        strip: &mut StripBuffer,
        px: u16,
        py: u16,
        pw: u16,
        ph: u16,
        draw: &F,
        left_mask: u8,
        right_mask: u8,
    ) where
        F: Fn(&mut StripBuffer),
    {
        let max_rows = StripBuffer::max_rows_for_width(pw);
        let row_bytes = (pw / 8) as usize;
        let needs_mask = left_mask != 0 || right_mask != 0;

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);

            if needs_mask && row_bytes > 0 {
                for row in strip.data_mut().chunks_mut(row_bytes) {
                    row[0] |= left_mask;
                    row[row.len() - 1] |= right_mask;
                }
            }

            self.set_partial_ram_area(px, y, pw, rows);
            self.send_command(cmd::WRITE_RAM_BW);
            self.send_data(strip.data());

            self.set_partial_ram_area(px, y, pw, rows);
            self.send_command(cmd::WRITE_RAM_RED);
            self.send_data_inverted(strip.data(), left_mask, right_mask, row_bytes);

            y += rows;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_region_strips_dual<F>(
        &mut self,
        strip: &mut StripBuffer,
        px: u16,
        py: u16,
        pw: u16,
        ph: u16,
        draw: &F,
        left_mask: u8,
        right_mask: u8,
    ) where
        F: Fn(&mut StripBuffer),
    {
        let max_rows = StripBuffer::max_rows_for_width(pw);
        let row_bytes = (pw / 8) as usize;
        let needs_mask = left_mask != 0 || right_mask != 0;

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);

            if needs_mask && row_bytes > 0 {
                for row in strip.data_mut().chunks_mut(row_bytes) {
                    row[0] |= left_mask;
                    row[row.len() - 1] |= right_mask;
                }
            }

            // send the same rendered strip to both RAMs directly;
            // no replay copy needed since send_data only reads the buffer
            for &ram_cmd in &[cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
                self.set_partial_ram_area(px, y, pw, rows);
                self.send_command(ram_cmd);
                self.send_data(strip.data());
            }

            y += rows;
        }
    }

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

    fn transform_region(&self, x: u16, y: u16, w: u16, h: u16) -> (u16, u16, u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (x, y, w, h),
            Rotation::Deg90 => (WIDTH - y - h, x, h, w),
            Rotation::Deg180 => (WIDTH - x - w, HEIGHT - y - h, w, h),
            Rotation::Deg270 => (y, HEIGHT - x - w, h, w),
        }
    }

    fn align_partial_region(&self, x: u16, y: u16, w: u16, h: u16) -> Option<RenderState> {
        let (tx, ty, tw, th) = self.transform_region(x, y, w, h);

        let px = (tx & !7).min(WIDTH);
        let py = ty.min(HEIGHT);
        let pw = ((tw + (tx & 7) + 7) & !7).min(WIDTH - px);
        let ph = th.min(HEIGHT - py);

        if pw == 0 || ph == 0 {
            return None;
        }

        let lp = (tx - px) as u32;
        let rp = ((px + pw) - (tx + tw)) as u32;
        let left_mask: u8 = if lp > 0 { !((1u8 << (8 - lp)) - 1) } else { 0 };
        let right_mask: u8 = if rp > 0 { (1u8 << rp) - 1 } else { 0 };

        Some(RenderState {
            px,
            py,
            pw,
            ph,
            left_mask,
            right_mask,
        })
    }

    // gates wired in reverse; Y flipped, X inc / Y dec
    fn set_partial_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let y_flipped = HEIGHT - y - h;

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

    // send data with each byte inverted, re-applying edge masks
    // uses a small batch buffer to amortize SPI call overhead
    fn send_data_inverted(&mut self, data: &[u8], left_mask: u8, right_mask: u8, row_bytes: usize) {
        const BATCH_SIZE: usize = 64;
        let mut batch = [0u8; BATCH_SIZE];

        let _ = self.dc.set_high();

        let mut offset = 0;
        while offset < data.len() {
            let chunk_len = (data.len() - offset).min(BATCH_SIZE);
            for i in 0..chunk_len {
                let byte_in_row = if row_bytes > 0 {
                    (offset + i) % row_bytes
                } else {
                    0
                };
                let mut inverted = !data[offset + i];

                // Re-apply edge masks (inversion flipped them)
                if byte_in_row == 0 {
                    inverted |= left_mask;
                }
                if row_bytes > 0 && byte_in_row == row_bytes - 1 {
                    inverted |= right_mask;
                }
                batch[i] = inverted;
            }
            let _ = self.spi.write(&batch[..chunk_len]);
            offset += chunk_len;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn partial_phase1_bw<F>(
        &mut self,
        strip: &mut StripBuffer,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        delay: &mut Delay,
        draw: &F,
    ) -> Option<RenderState>
    where
        F: Fn(&mut StripBuffer),
    {
        if self.initial_refresh {
            return None;
        }
        if !self.init_done {
            self.init_display(delay);
        }

        let rs = self.align_partial_region(x, y, w, h)?;
        self.write_region_strips(
            strip,
            rs.px,
            rs.py,
            rs.pw,
            rs.ph,
            cmd::WRITE_RAM_BW,
            draw,
            rs.left_mask,
            rs.right_mask,
        );
        Some(rs)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn partial_phase1_bw_inv_red<F>(
        &mut self,
        strip: &mut StripBuffer,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        delay: &mut Delay,
        draw: &F,
    ) -> Option<RenderState>
    where
        F: Fn(&mut StripBuffer),
    {
        if self.initial_refresh {
            return None;
        }
        if !self.init_done {
            self.init_display(delay);
        }

        let rs = self.align_partial_region(x, y, w, h)?;
        self.write_region_strips_bw_inv_red(
            strip,
            rs.px,
            rs.py,
            rs.pw,
            rs.ph,
            draw,
            rs.left_mask,
            rs.right_mask,
        );
        Some(rs)
    }

    pub fn partial_start_du(&mut self, rs: &RenderState) {
        self.set_partial_ram_area(rs.px, rs.py, rs.pw, rs.ph);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x00, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xFC]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.power_is_on = true;
    }

    #[inline]
    pub fn is_busy(&mut self) -> bool {
        self.busy.is_high().unwrap_or(false)
    }

    pub fn partial_phase3_sync<F>(&mut self, strip: &mut StripBuffer, rs: &RenderState, draw: &F)
    where
        F: Fn(&mut StripBuffer),
    {
        self.write_region_strips_dual(
            strip,
            rs.px,
            rs.py,
            rs.pw,
            rs.ph,
            draw,
            rs.left_mask,
            rs.right_mask,
        );
    }

    pub fn needs_initial_refresh(&self) -> bool {
        self.initial_refresh
    }

    pub fn write_full_frame<F>(&mut self, strip: &mut StripBuffer, delay: &mut Delay, draw: &F)
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
    }

    pub fn start_full_update(&mut self) {
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
    }

    pub fn finish_full_update(&mut self) {
        self.power_is_on = false;
        self.initial_refresh = false;
    }

    // mode 1: image retained, ~3 uA; requires hw reset to wake
    pub fn enter_deep_sleep(&mut self) {
        if self.power_is_on {
            self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
            self.send_data(&[0x83]);
            self.send_command(cmd::MASTER_ACTIVATION);
            self.wait_busy(POWER_OFF_TIME_MS);
            self.power_is_on = false;
        }

        self.send_command(cmd::DEEP_SLEEP);
        self.send_data(&[0x01]);
        self.init_done = false;
    }
}

impl<SPI, DC, RST, BUSY, E> DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin + embedded_hal_async::digital::Wait,
{
    pub fn busy_pin(&mut self) -> &mut BUSY {
        &mut self.busy
    }

    async fn wait_busy_async(&mut self) {
        let _ = self.busy.wait_for_low().await;
    }

    pub async fn write_full_frame_async<F>(
        &mut self,
        strip: &mut StripBuffer,
        delay: &mut Delay,
        draw: &F,
    ) where
        F: Fn(&mut StripBuffer),
    {
        self.write_full_frame(strip, delay, draw);
    }

    pub async fn partial_refresh_async<F>(
        &mut self,
        strip: &mut StripBuffer,
        delay: &mut Delay,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
        draw: &F,
    ) where
        F: Fn(&mut StripBuffer),
    {
        if self.initial_refresh {
            self.full_refresh_async(strip, delay, draw).await;
            return;
        }
        if !self.init_done {
            self.init_display(delay);
        }

        let rs = match self.align_partial_region(x, y, w, h) {
            Some(rs) => rs,
            None => return,
        };

        self.write_region_strips(
            strip,
            rs.px,
            rs.py,
            rs.pw,
            rs.ph,
            cmd::WRITE_RAM_BW,
            draw,
            rs.left_mask,
            rs.right_mask,
        );

        self.partial_start_du(&rs);
        self.wait_busy_async().await;

        self.write_region_strips_dual(
            strip,
            rs.px,
            rs.py,
            rs.pw,
            rs.ph,
            draw,
            rs.left_mask,
            rs.right_mask,
        );

        self.power_off_async().await;
    }

    pub async fn full_refresh_async<F>(
        &mut self,
        strip: &mut StripBuffer,
        delay: &mut Delay,
        draw: &F,
    ) where
        F: Fn(&mut StripBuffer),
    {
        self.write_full_frame_async(strip, delay, draw).await;
        self.update_full_async().await;
        self.initial_refresh = false;
    }

    pub async fn power_off_async(&mut self) {
        if self.power_is_on {
            self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
            self.send_data(&[0x83]);
            self.send_command(cmd::MASTER_ACTIVATION);
            self.wait_busy_async().await;
            self.power_is_on = false;
        }
    }

    async fn update_full_async(&mut self) {
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy_async().await;

        self.power_is_on = false;
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
