// SSD1677 e-paper driver for GDEQ0426T82 (800x480).
// No framebuffer in SRAM; pixels are streamed to the controller
// through a 4KB StripBuffer. Both BW and RED RAM planes must be
// written for differential waveform updates.

use embedded_graphics_core::geometry::{OriginDimensions, Size};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use esp_hal::delay::Delay;

use super::strip::{STRIP_COUNT, StripBuffer};

pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

pub const SPI_FREQ_MHZ: u32 = 20;

#[allow(dead_code)]
const POWER_ON_TIME_MS: u32 = 100;
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
mod cmd {
    pub const DRIVER_OUTPUT_CONTROL: u8 = 0x01;
    pub const BOOSTER_SOFT_START: u8 = 0x0C;
    pub const DEEP_SLEEP: u8 = 0x10;
    pub const DATA_ENTRY_MODE: u8 = 0x11;
    pub const SW_RESET: u8 = 0x12;
    pub const TEMPERATURE_SENSOR: u8 = 0x18;
    #[allow(dead_code)]
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

    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn size(&self) -> Size {
        match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => Size::new(WIDTH as u32, HEIGHT as u32),
            Rotation::Deg90 | Rotation::Deg270 => Size::new(HEIGHT as u32, WIDTH as u32),
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

    pub fn clear(&mut self, delay: &mut Delay) {
        self.clear_screen(delay);
    }

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

        let (px, py, pw, ph) = self.transform_region(x, y, w, h);

        let px_aligned = px & !7;
        let extra = px - px_aligned;
        let mut pw = pw + extra;
        if pw % 8 > 0 {
            pw += 8 - (pw % 8);
        }

        let px = px_aligned.min(WIDTH);
        let py = py.min(HEIGHT);
        let pw = pw.min(WIDTH - px);
        let ph = ph.min(HEIGHT - py);

        if pw == 0 || ph == 0 {
            return;
        }

        self.write_region_strips(strip, px, py, pw, ph, cmd::WRITE_RAM_BW, &draw);

        self.update_partial();

        self.write_region_both_planes(strip, px, py, pw, ph, &draw);

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

    pub fn hibernate(&mut self) {
        self.power_off();
        self.send_command(cmd::DEEP_SLEEP);
        self.send_data(&[0x01]);
        self.init_done = false;
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
    ) where
        F: Fn(&mut StripBuffer),
    {
        let max_rows = StripBuffer::max_rows_for_width(pw);

        self.set_partial_ram_area(px, py, pw, ph);
        self.send_command(ram_cmd);

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);
            self.send_data(strip.data());
            y += rows;
        }
    }

    // Post-update copy: draw once per strip band, send to both RAM planes.
    // Saves redundant draw calls compared to two separate write_region_strips.
    fn write_region_both_planes<F>(
        &mut self,
        strip: &mut StripBuffer,
        px: u16,
        py: u16,
        pw: u16,
        ph: u16,
        draw: &F,
    ) where
        F: Fn(&mut StripBuffer),
    {
        let max_rows = StripBuffer::max_rows_for_width(pw);

        let mut y = py;
        while y < py + ph {
            let rows = max_rows.min(py + ph - y);
            strip.begin_window(self.rotation, px, y, pw, rows);
            draw(strip);

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

    // Gates are wired in reverse on this panel; Y must be flipped.
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

    fn clear_screen(&mut self, delay: &mut Delay) {
        if !self.init_done {
            self.init_display(delay);
        }

        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_screen_buffer(cmd::WRITE_RAM_RED, 0xFF);
        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_screen_buffer(cmd::WRITE_RAM_BW, 0xFF);

        self.update_full();
        self.initial_refresh = false;
    }

    fn write_screen_buffer(&mut self, command: u8, value: u8) {
        self.send_command(command);
        let total = (WIDTH as u32 * HEIGHT as u32 / 8) as usize;
        let chunk_size = 256;
        let chunk = [value; 256];
        for i in (0..total).step_by(chunk_size) {
            let len = (total - i).min(chunk_size);
            self.send_data(&chunk[..len]);
        }
    }

    fn update_full(&mut self) {
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(FULL_REFRESH_TIME_MS);

        self.power_is_on = false;
    }

    fn update_partial(&mut self) {
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x00, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xFC]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(PARTIAL_REFRESH_TIME_MS);

        self.power_is_on = true;
    }

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
