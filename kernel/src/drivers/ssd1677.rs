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
use pulp_board_logic::power::DelayMs;
// geometry, rotation, command sequences and region math are shared with the
// OnePage C61 (host-tested in pulp-board-logic); X4 keeps its own SPI/pin/delay
// plumbing here and ignores bus errors exactly as before
use pulp_board_logic::ssd1677::{self as shared, DisplayError, EpdBus, StripSource, cmd};
pub use pulp_board_logic::ssd1677::{HEIGHT, RenderState, Rotation, WIDTH};

use super::strip::StripBuffer;

pub const SPI_FREQ_MHZ: u32 = 20;

const POWER_OFF_TIME_MS: u32 = 200; // analog shutdown timeout

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
        let _ = shared::init_display(&mut WithDelay { bus: self, delay });
        self.init_done = true;
    }

    fn align_partial_region(&self, x: u16, y: u16, w: u16, h: u16) -> Option<RenderState> {
        shared::align_partial_region(self.rotation, x, y, w, h)
    }

    // gates wired in reverse; Y flipped, X inc / Y dec
    fn set_partial_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let _ = shared::set_ram_area(self, x, y, w, h);
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

        let rotation = self.rotation;
        let mut src = DrawSource { strip, draw };
        let _ = shared::write_full_frame(&mut WithDelay { bus: self, delay }, rotation, &mut src);
    }

    pub fn start_full_update(&mut self) {
        let _ = shared::start_full_update(self);
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

        let _ = shared::deep_sleep(self);
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
        let _ = shared::start_full_update(self);
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
        let (w, h) = self.rotation.logical_size();
        Size::new(w as u32, h as u32)
    }
}

// the shared sequences drive the panel through this; errors are dropped like
// the old `let _ = spi.write(..)` calls
impl<SPI, DC, RST, BUSY, E> EpdBus for DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    fn command(&mut self, c: u8) -> Result<(), DisplayError> {
        self.send_command(c);
        Ok(())
    }

    fn data(&mut self, data: &[u8]) -> Result<(), DisplayError> {
        self.send_data(data);
        Ok(())
    }
}

// bus + the caller's esp-hal delay, for the sequences that sleep
struct WithDelay<'a, B: EpdBus> {
    bus: &'a mut B,
    delay: &'a mut Delay,
}

impl<B: EpdBus> EpdBus for WithDelay<'_, B> {
    fn command(&mut self, c: u8) -> Result<(), DisplayError> {
        self.bus.command(c)
    }

    fn data(&mut self, data: &[u8]) -> Result<(), DisplayError> {
        self.bus.data(data)
    }
}

impl<B: EpdBus> DelayMs for WithDelay<'_, B> {
    fn delay_ms(&mut self, ms: u32) {
        self.delay.delay_millis(ms);
    }
}

// one strip per call: clear, run the caller's draw closure, hand out the bytes
struct DrawSource<'a, F> {
    strip: &'a mut StripBuffer,
    draw: &'a F,
}

impl<F: Fn(&mut StripBuffer)> StripSource for DrawSource<'_, F> {
    fn render_strip(&mut self, rotation: Rotation, idx: u16) -> &[u8] {
        self.strip.begin_strip(rotation, idx);
        (self.draw)(self.strip);
        self.strip.data()
    }
}
