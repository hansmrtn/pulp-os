// SSD1677 e-paper driver (board-independent).
// Tested on GDEQ0426T82 (800x480). No framebuffer; pixels streamed
// through a 4KB StripBuffer. Partial refresh per GxEPD2 sequence:
// BW-only write -> DU update -> sync both planes.
// Async variants replace the blocking busy-wait with .await (WFI between polls).

use embedded_graphics_core::geometry::{OriginDimensions, Size};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use esp_hal::delay::Delay;

use super::strip::{STRIP_BUF_SIZE, STRIP_COUNT, StripBuffer};

pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

pub const SPI_FREQ_MHZ: u32 = 20;

const POWER_OFF_TIME_MS: u32 = 200;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

// SSD1677 commands
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

// region params threaded between split-phase partial refresh steps
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

// blocking API

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

    // strip helpers

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

    // sync RED+BW with one draw per chunk (2N vs 3N SPI writes); DMA-backed
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

            let data_len = strip.data().len();
            let mut replay = [0xFFu8; STRIP_BUF_SIZE];
            replay[..data_len].copy_from_slice(strip.data());

            for &ram_cmd in &[cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
                self.set_partial_ram_area(px, y, pw, rows);
                self.send_command(ram_cmd);
                self.send_data(&replay[..data_len]);
            }

            y += rows;
        }
    }

    // display init (matches GxEPD2 _InitDisplay)

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

    // coordinate helpers

    fn transform_region(&self, x: u16, y: u16, w: u16, h: u16) -> (u16, u16, u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (x, y, w, h),
            Rotation::Deg90 => (WIDTH - y - h, x, h, w),
            Rotation::Deg180 => (WIDTH - x - w, HEIGHT - y - h, w, h),
            Rotation::Deg270 => (y, HEIGHT - x - w, h, w),
        }
    }

    // gates wired in reverse; Y flipped per GxEPD2
    fn set_partial_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) {
        let y_flipped = HEIGHT - y - h;

        // X increment, Y decrement; compensates gate reversal
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

    // low-level SPI / busy

    // WFI between polls; BUSY falling-edge IRQ wakes; timer backstop
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

    // split-phase partial refresh:
    // phase1_bw -> start_du -> (busy wait / input) -> phase3_sync -> power_off_async

    // write new content to BW RAM; returns None on degenerate region or initial_refresh set
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

        self.write_region_strips(
            strip,
            px,
            py,
            pw,
            ph,
            cmd::WRITE_RAM_BW,
            draw,
            left_mask,
            right_mask,
        );

        Some(RenderState {
            px,
            py,
            pw,
            ph,
            left_mask,
            right_mask,
        })
    }

    // kick DU waveform (non-blocking); caller polls is_busy
    pub fn partial_start_du(&mut self, rs: &RenderState) {
        self.set_partial_ram_area(rs.px, rs.py, rs.pw, rs.ph);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x00, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xFC]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.power_is_on = true;
    }

    // true while controller is busy
    #[inline]
    pub fn is_busy(&mut self) -> bool {
        self.busy.is_high().unwrap_or(false)
    }

    // sync both RAM planes after DU completes; call when is_busy is false
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

    // true until first full refresh has been performed
    pub fn needs_initial_refresh(&self) -> bool {
        self.initial_refresh
    }

    // split-phase full refresh:
    // write_full_frame -> start_full_update -> (busy wait) -> finish_full_update

    // write full frame to RED and BW RAM; does not kick GC waveform
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

    // kick GC waveform (non-blocking); BUSY high ~1.6s; poll is_busy then call finish_full_update
    pub fn start_full_update(&mut self) {
        // bypass RED=0, BW normal
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        // mode 1: GC full waveform
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
    }

    // finalise state after GC waveform completes
    pub fn finish_full_update(&mut self) {
        self.power_is_on = false;
        self.initial_refresh = false;
    }

    // deep-sleep mode 1: image retained, ~3uA; requires hw reset to wake
    pub fn enter_deep_sleep(&mut self) {
        // power off before sleeping
        if self.power_is_on {
            self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
            self.send_data(&[0x83]);
            self.send_command(cmd::MASTER_ACTIVATION);
            self.wait_busy(POWER_OFF_TIME_MS);
            self.power_is_on = false;
        }

        // mode 1: RAM retained, <3uA
        self.send_command(cmd::DEEP_SLEEP);
        self.send_data(&[0x01]);
        self.init_done = false;
    }
}

// async API: BUSY-wait replaced with .await; SPI writes remain synchronous

impl<SPI, DC, RST, BUSY, E> DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin + embedded_hal_async::digital::Wait,
{
    // expose busy pin for external async wait
    pub fn busy_pin(&mut self) -> &mut BUSY {
        &mut self.busy
    }

    async fn wait_busy_async(&mut self) {
        let _ = self.busy.wait_for_low().await;
    }

    // async write_full_frame; SPI writes still blocking (DMA)
    pub async fn write_full_frame_async<F>(
        &mut self,
        strip: &mut StripBuffer,
        delay: &mut Delay,
        draw: &F,
    ) where
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

    // full GC refresh in one await (~1.6s); prefer split-phase in event loops
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

    // async power-off (~200ms); awaits BUSY low
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
        // bypass RED=0, BW normal
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        // GC full waveform
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
