//! Status bar — persistent system info strip.
//!
//! Drawn by main.rs on every render, outside of app control.
//! Shows battery, uptime, heap, stack, and SD status.

use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;
use embedded_graphics::text::Text;

use super::widget::Region;

/// Height of the status bar in pixels.
pub const BAR_HEIGHT: u16 = 18;

/// Y coordinate where app content should start (below the status bar).
pub const CONTENT_TOP: u16 = BAR_HEIGHT;

/// Full-width status bar region (top of screen, 480px wide in landscape).
pub const BAR_REGION: Region = Region::new(0, 0, 480, BAR_HEIGHT);

/// System snapshot passed to the status bar each frame.
pub struct SystemStatus {
    /// Uptime in seconds since boot.
    pub uptime_secs: u32,
    /// Battery voltage in mV (0 = not available).
    pub battery_mv: u16,
    /// Battery charge percentage (0-100).
    pub battery_pct: u8,
    /// Heap bytes currently allocated.
    pub heap_used: usize,
    /// Heap total bytes.
    pub heap_total: usize,
    /// Approximate free stack in bytes.
    pub stack_free: usize,
    /// Whether SD card is present.
    pub sd_ok: bool,
}

pub struct StatusBar {
    buf: [u8; 80],
    len: usize,
}

impl StatusBar {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; 80],
            len: 0,
        }
    }

    /// Update the status bar text from a system snapshot.
    pub fn update(&mut self, s: &SystemStatus) {
        self.len = 0;

        let secs = s.uptime_secs % 60;
        let mins = (s.uptime_secs / 60) % 60;
        let hrs = s.uptime_secs / 3600;

        let mut w = BufWriter {
            buf: &mut self.buf,
            pos: 0,
        };

        // Battery
        if s.battery_mv > 0 {
            let _ = write!(
                w,
                "BAT {}% {}.{}V",
                s.battery_pct,
                s.battery_mv / 1000,
                (s.battery_mv % 1000) / 100
            );
        } else {
            let _ = write!(w, "BAT --");
        }

        // Uptime
        if hrs > 0 {
            let _ = write!(w, "  {}:{:02}:{:02}", hrs, mins, secs);
        } else {
            let _ = write!(w, "  {:02}:{:02}", mins, secs);
        }

        // Heap
        if s.heap_total > 0 {
            let _ = write!(w, "  H:{}/{}K", s.heap_used / 1024, s.heap_total / 1024);
        }

        // Stack free
        if s.stack_free > 0 {
            let _ = write!(w, "  S:{}K", s.stack_free / 1024);
        }

        // SD
        let _ = write!(w, "  SD:{}", if s.sd_ok { "OK" } else { "--" });

        self.len = w.pos;
    }

    fn text(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// Draw the status bar.
    pub fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        // Dark background
        BAR_REGION
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)?;

        // White text
        let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
        Text::new(self.text(), Point::new(4, 14), style).draw(display)?;

        Ok(())
    }

    pub fn region(&self) -> Region {
        BAR_REGION
    }
}

/// Read approximate free stack space.
///
/// ESP32-C3 stack grows downward from top of DRAM.
/// Returns distance from current SP to DRAM base — a rough
/// measure of how much SRAM headroom remains below the stack.
pub fn free_stack_bytes() -> usize {
    let sp: usize;
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    #[cfg(not(target_arch = "riscv32"))]
    {
        sp = 0;
    }

    // ESP32-C3 DRAM: 0x3FC80000..0x3FCE0000 (400KB)
    // SP sits near the top; distance to base ≈ free headroom.
    const DRAM_BASE: usize = 0x3FC8_0000;
    if sp > DRAM_BASE { sp - DRAM_BASE } else { 0 }
}

/// Tiny no-alloc write helper.
struct BufWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> Write for BufWriter<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let avail = self.buf.len() - self.pos;
        let n = bytes.len().min(avail);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}
