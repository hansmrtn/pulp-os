//! SSD1677 E-Paper Display Driver for XTEink X4
//!
//! Based on GxEPD2_426_GDEQ0426T82.cpp by Jean-Marc Zingg
//! <https://github.com/ZinggJM/GxEPD2>
use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
};
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use esp_hal::delay::Delay;

// Display dimensions (physical)
pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;
pub const FRAMEBUFFER_SIZE: usize = (WIDTH as usize * HEIGHT as usize) / 8;

// SPI frequency
pub const SPI_FREQ_MHZ: u32 = 20;

// Timing constants from GxEPD2
#[allow(dead_code)]
const POWER_ON_TIME_MS: u32 = 100;
const POWER_OFF_TIME_MS: u32 = 200;
const FULL_REFRESH_TIME_MS: u32 = 1600;
const PARTIAL_REFRESH_TIME_MS: u32 = 600;

/// Display rotation
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Rotation {
    /// Landscape, 800x480, no rotation
    #[default]
    Deg0,
    /// Portrait, 480x800, rotated 90° clockwise
    Deg90,
    /// Landscape, 800x480, upside down
    Deg180,
    /// Portrait, 480x800, rotated 270° clockwise  
    Deg270,
}

// SSD1677 Commands (matching GxEPD2 exactly)
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
    pub const WRITE_RAM_BW: u8 = 0x24; // Current/New buffer
    pub const WRITE_RAM_RED: u8 = 0x26; // Previous buffer (for differential)
    pub const BORDER_WAVEFORM: u8 = 0x3C;
    pub const SET_RAM_X_RANGE: u8 = 0x44;
    pub const SET_RAM_Y_RANGE: u8 = 0x45;
    pub const SET_RAM_X_COUNTER: u8 = 0x4E;
    pub const SET_RAM_Y_COUNTER: u8 = 0x4F;
}

/// Display driver for SSD1677-based e-paper (GDEQ0426T82)
pub struct DisplayDriver<SPI, DC, RST, BUSY> {
    spi: SPI,
    dc: DC,
    rst: RST,
    busy: BUSY,
    framebuffer: [u8; FRAMEBUFFER_SIZE],
    rotation: Rotation,
    power_is_on: bool,
    init_done: bool,
    initial_refresh: bool,
    initial_write: bool,
}

impl<SPI, DC, RST, BUSY, E> DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    /// Create a new display driver
    pub fn new(spi: SPI, dc: DC, rst: RST, busy: BUSY) -> Self {
        Self {
            spi,
            dc,
            rst,
            busy,
            framebuffer: [0xFF; FRAMEBUFFER_SIZE], // White
            rotation: Rotation::Deg270,
            power_is_on: false,
            init_done: false,
            initial_refresh: true,
            initial_write: true,
        }
    }

    /// Set display rotation
    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    /// Get current rotation
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// Get logical display size (accounts for rotation)
    pub fn size(&self) -> Size {
        match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => Size::new(WIDTH as u32, HEIGHT as u32),
            Rotation::Deg90 | Rotation::Deg270 => Size::new(HEIGHT as u32, WIDTH as u32),
        }
    }

    /// Hardware reset
    pub fn reset(&mut self, delay: &mut Delay) {
        let _ = self.rst.set_high();
        delay.delay_millis(20);
        let _ = self.rst.set_low();
        delay.delay_millis(2);
        let _ = self.rst.set_high();
        delay.delay_millis(20);
    }

    /// Initialize the display
    pub fn init(&mut self, delay: &mut Delay) {
        self.reset(delay);
        self.init_display(delay);
    }

    /// Clear the entire screen to white
    pub fn clear(&mut self, delay: &mut Delay) {
        self.framebuffer.fill(0xFF);
        self.clear_screen(delay);
    }

    /// Fill framebuffer only (no display update)
    pub fn fill_white(&mut self) {
        self.framebuffer.fill(0xFF);
    }

    /// Fill framebuffer with black (no display update)  
    pub fn fill_black(&mut self) {
        self.framebuffer.fill(0x00);
    }

    /// Set a pixel in the framebuffer using LOGICAL coordinates
    /// Coordinates are transformed based on rotation
    pub fn set_pixel(&mut self, x: u16, y: u16, black: bool) {
        // Get logical dimensions for bounds check
        let (log_w, log_h) = match self.rotation {
            Rotation::Deg0 | Rotation::Deg180 => (WIDTH, HEIGHT),
            Rotation::Deg90 | Rotation::Deg270 => (HEIGHT, WIDTH),
        };

        if x >= log_w || y >= log_h {
            return;
        }

        // Transform logical → physical coordinates
        let (px, py) = match self.rotation {
            Rotation::Deg0 => (x, y),
            Rotation::Deg90 => (WIDTH - 1 - y, x),
            Rotation::Deg180 => (WIDTH - 1 - x, HEIGHT - 1 - y),
            Rotation::Deg270 => (y, HEIGHT - 1 - x),
        };

        let idx = (px as usize / 8) + (py as usize * (WIDTH as usize / 8));
        let bit = 7 - (px % 8);
        if black {
            self.framebuffer[idx] &= !(1 << bit);
        } else {
            self.framebuffer[idx] |= 1 << bit;
        }
    }

    /// Fill framebuffer with color (true = black, false = white)
    pub fn fill(&mut self, black: bool) {
        self.framebuffer.fill(if black { 0x00 } else { 0xFF });
    }

    /// Full screen refresh (use after initial setup or to clear ghosting)
    pub fn refresh_full(&mut self, delay: &mut Delay) {
        if !self.init_done {
            self.init_display(delay);
        }

        // Small delay to ensure display is ready
        delay.delay_millis(1);

        // Write to both buffers for full refresh
        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_full_buffer(cmd::WRITE_RAM_RED); // Previous

        delay.delay_millis(1); // Yield between large transfers

        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_full_buffer(cmd::WRITE_RAM_BW); // Current

        self.update_full(delay);
        self.initial_refresh = false;
        self.initial_write = false;
    }

    /// Partial screen refresh
    /// Takes LOGICAL coordinates
    pub fn refresh_partial(&mut self, x: u16, y: u16, w: u16, h: u16, delay: &mut Delay) {
        // Initial refresh must be full
        if self.initial_refresh {
            return self.refresh_full(delay);
        }

        if !self.init_done {
            self.init_display(delay);
        }

        // Transform logical region to physical region
        let (px, py, pw, ph) = self.transform_region(x, y, w, h);

        // Ensure x and w are multiples of 8 (byte boundary requirement)
        let px_aligned = px & !7;
        let extra = px - px_aligned;
        let mut pw = pw + extra;
        if pw % 8 > 0 {
            pw += 8 - (pw % 8);
        }

        // Clamp to screen bounds
        let px = px_aligned.min(WIDTH);
        let py = py.min(HEIGHT);
        let pw = pw.min(WIDTH - px);
        let ph = ph.min(HEIGHT - py);

        if pw == 0 || ph == 0 {
            return;
        }

        // Step 1: Write to current buffer (0x24) only
        self.write_image_partial_physical(cmd::WRITE_RAM_BW, px, py, pw, ph);

        // Step 2: Partial refresh
        self.set_partial_ram_area(px, py, pw, ph);
        self.update_partial(delay);

        // Step 3: Sync buffers - write to BOTH previous (0x26) AND current (0x24)
        // This is writeImageAgain() in GxEPD2
        self.write_image_partial_physical(cmd::WRITE_RAM_RED, px, py, pw, ph);
        self.write_image_partial_physical(cmd::WRITE_RAM_BW, px, py, pw, ph);

        // Step 4: Power off display controller to save power.
        // E-paper retains image without power. Leaving it on draws ~15mA idle.
        self.power_off(delay);
    }

    /// Transform logical region to physical region based on rotation
    fn transform_region(&self, x: u16, y: u16, w: u16, h: u16) -> (u16, u16, u16, u16) {
        match self.rotation {
            Rotation::Deg0 => (x, y, w, h),
            Rotation::Deg90 => {
                // Logical (x,y,w,h) in 480x800 → Physical in 800x480
                // Logical top-left (x,y) → Physical (WIDTH-1-y, x)
                // But we need the physical top-left of the region
                (WIDTH - y - h, x, h, w)
            }
            Rotation::Deg180 => (WIDTH - x - w, HEIGHT - y - h, w, h),
            Rotation::Deg270 => (y, HEIGHT - x - w, h, w),
        }
    }

    /// Refresh a rectangular window (convenience method)
    pub fn refresh_window(&mut self, x: u16, y: u16, w: u16, h: u16, delay: &mut Delay) {
        self.refresh_partial(x, y, w, h, delay);
    }

    /// Power off the display (reduces power consumption, prevents fading)
    pub fn power_off(&mut self, delay: &mut Delay) {
        if self.power_is_on {
            self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
            self.send_data(&[0x83]);
            self.send_command(cmd::MASTER_ACTIVATION);
            self.wait_busy(delay, POWER_OFF_TIME_MS);
            self.power_is_on = false;
        }
    }

    /// Put display into deep sleep (minimum power, needs reset to wake)
    pub fn hibernate(&mut self, delay: &mut Delay) {
        self.power_off(delay);
        self.send_command(cmd::DEEP_SLEEP);
        self.send_data(&[0x01]);
        self.init_done = false;
    }

    // ========== Private methods ==========

    fn init_display(&mut self, delay: &mut Delay) {
        // Software reset
        self.send_command(cmd::SW_RESET);
        delay.delay_millis(10);

        // Temperature sensor: internal
        self.send_command(cmd::TEMPERATURE_SENSOR);
        self.send_data(&[0x80]);

        // Booster soft start
        self.send_command(cmd::BOOSTER_SOFT_START);
        self.send_data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x80]);

        // Driver output control
        self.send_command(cmd::DRIVER_OUTPUT_CONTROL);
        self.send_data(&[
            ((HEIGHT - 1) & 0xFF) as u8, // A[7:0]
            ((HEIGHT - 1) >> 8) as u8,   // A[9:8]
            0x02,                        // SM = interlaced
        ]);

        // Border waveform
        self.send_command(cmd::BORDER_WAVEFORM);
        self.send_data(&[0x01]);

        // Set initial RAM area
        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);

        self.init_done = true;
    }

    fn set_partial_ram_area(&mut self, x: u16, y: u16, w: u16, h: u16) {
        // Gates are reversed on this display - flip Y
        let y_flipped = HEIGHT - y - h;

        // Data entry mode: X increase, Y decrease (for gate reversal)
        self.send_command(cmd::DATA_ENTRY_MODE);
        self.send_data(&[0x01]);

        // Set RAM X address start/end
        self.send_command(cmd::SET_RAM_X_RANGE);
        self.send_data(&[
            (x & 0xFF) as u8,
            (x >> 8) as u8,
            ((x + w - 1) & 0xFF) as u8,
            ((x + w - 1) >> 8) as u8,
        ]);

        // Set RAM Y address start/end (reversed)
        self.send_command(cmd::SET_RAM_Y_RANGE);
        self.send_data(&[
            ((y_flipped + h - 1) & 0xFF) as u8,
            ((y_flipped + h - 1) >> 8) as u8,
            (y_flipped & 0xFF) as u8,
            (y_flipped >> 8) as u8,
        ]);

        // Set RAM X counter
        self.send_command(cmd::SET_RAM_X_COUNTER);
        self.send_data(&[(x & 0xFF) as u8, (x >> 8) as u8]);

        // Set RAM Y counter
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

        // Write white to both buffers
        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_screen_buffer(cmd::WRITE_RAM_RED, 0xFF);
        self.set_partial_ram_area(0, 0, WIDTH, HEIGHT);
        self.write_screen_buffer(cmd::WRITE_RAM_BW, 0xFF);

        self.update_full(delay);
        self.initial_write = false;
        self.initial_refresh = false;
    }

    fn write_screen_buffer(&mut self, command: u8, value: u8) {
        self.send_command(command);
        // Write in chunks to avoid watchdog issues
        let total = (WIDTH as u32 * HEIGHT as u32 / 8) as usize;
        let chunk_size = 256;
        let chunk = [value; 256];
        for i in (0..total).step_by(chunk_size) {
            let len = (total - i).min(chunk_size);
            self.send_data(&chunk[..len]);
        }
    }

    fn write_full_buffer(&mut self, command: u8) {
        self.send_command(command);
        // Write in normal row order - gate reversal is handled by RAM address setup
        // (Y-decrease mode in _setPartialRamArea), NOT by reversing rows here
        let bytes_per_row = (WIDTH / 8) as usize;

        // Use a temporary buffer to avoid borrow checker issues
        let mut row_buf = [0u8; 256];

        for row in 0..HEIGHT as usize {
            let start = row * bytes_per_row;
            // Write row in chunks to avoid issues
            for chunk_start in (0..bytes_per_row).step_by(256) {
                let chunk_end = (chunk_start + 256).min(bytes_per_row);
                let chunk_len = chunk_end - chunk_start;

                // Copy to temp buffer
                row_buf[..chunk_len]
                    .copy_from_slice(&self.framebuffer[start + chunk_start..start + chunk_end]);
                self.send_data(&row_buf[..chunk_len]);
            }
        }
    }

    fn write_image_partial_physical(&mut self, command: u8, x: u16, y: u16, w: u16, h: u16) {
        self.set_partial_ram_area(x, y, w, h);
        self.send_command(command);

        let bytes_per_row = (WIDTH / 8) as usize;
        let window_bytes = (w / 8) as usize;
        let x_byte = (x / 8) as usize;

        // Use a temporary buffer to avoid borrow checker issues
        // Max window width is 800/8 = 100 bytes per row
        let mut row_buf = [0u8; 100];

        // Write rows in NORMAL order (row 0, 1, 2, ...)
        // Gate reversal is handled by the RAM address setup, not here!
        // x, y, w, h are PHYSICAL coordinates
        for row in 0..h as usize {
            let src_row = y as usize + row;
            let src_start = src_row * bytes_per_row + x_byte;

            // Copy to temp buffer
            row_buf[..window_bytes]
                .copy_from_slice(&self.framebuffer[src_start..src_start + window_bytes]);
            self.send_data(&row_buf[..window_bytes]);
        }
    }

    fn update_full(&mut self, delay: &mut Delay) {
        // Display Update Control 1: bypass RED as 0
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x40, 0x00]);

        // Use standard full refresh (0xF7) - more reliable than fast mode (0xD7)
        // Fast mode requires temperature register which may not work on all panels
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xF7]);

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(delay, FULL_REFRESH_TIME_MS);

        self.power_is_on = false;
    }

    fn update_partial(&mut self, delay: &mut Delay) {
        // Display Update Control 1: RED normal
        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_1);
        self.send_data(&[0x00, 0x00]);

        self.send_command(cmd::DISPLAY_UPDATE_CONTROL_2);
        self.send_data(&[0xFC]); // Partial refresh (uses OTP LUT)

        self.send_command(cmd::MASTER_ACTIVATION);
        self.wait_busy(delay, PARTIAL_REFRESH_TIME_MS);

        self.power_is_on = true;
    }

    fn wait_busy(&mut self, delay: &mut Delay, timeout_ms: u32) {
        let mut elapsed = 0u32;
        while elapsed < timeout_ms {
            if self.busy.is_low().unwrap_or(true) {
                return;
            }
            delay.delay_millis(1);
            elapsed += 1;
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

// embedded-graphics integration

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

impl<SPI, DC, RST, BUSY, E> DrawTarget for DisplayDriver<SPI, DC, RST, BUSY>
where
    SPI: SpiDevice<Error = E>,
    DC: OutputPin,
    RST: OutputPin,
    BUSY: InputPin,
{
    type Color = BinaryColor;
    type Error = core::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let size = self.size();
        let log_w = size.width as i32;
        let log_h = size.height as i32;

        for Pixel(coord, color) in pixels {
            // Bounds check against LOGICAL dimensions
            if coord.x >= 0 && coord.x < log_w && coord.y >= 0 && coord.y < log_h {
                self.set_pixel(
                    coord.x as u16,
                    coord.y as u16,
                    color == BinaryColor::On, // On = black (foreground), Off = white (background)
                );
            }
        }
        Ok(())
    }
}
