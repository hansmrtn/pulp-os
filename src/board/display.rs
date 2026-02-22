//! Display hardware constants for XTEink X4
//!
//! The X4 uses an 800x480 e-paper display driven by an SSD1677 controller.

pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

/// Framebuffer size in bytes (1 bit per pixel, packed).
pub const FRAMEBUFFER_SIZE: usize = (WIDTH as usize * HEIGHT as usize) / 8;

/// The SSD1677 typically supports up to 20MHz, but 10MHz seems fine for now
pub const SPI_FREQ_MHZ: u32 = 10;
