// Persistent status bar at top of screen
// Shows battery, uptime, heap, stack, and SD card state.

use core::fmt::Write;

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;
use embedded_graphics::text::Text;

use super::widget::Region;

pub const BAR_HEIGHT: u16 = 18;

pub const CONTENT_TOP: u16 = BAR_HEIGHT;

pub const BAR_REGION: Region = Region::new(0, 0, 480, BAR_HEIGHT);

pub struct SystemStatus {
    pub uptime_secs: u32,
    pub battery_mv: u16,
    pub battery_pct: u8,
    pub heap_used: usize,
    pub heap_total: usize,
    pub stack_free: usize,
    pub sd_ok: bool,
}

pub struct StatusBar {
    buf: [u8; 80],
    len: usize,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    pub const fn new() -> Self {
        Self {
            buf: [0u8; 80],
            len: 0,
        }
    }

    pub fn update(&mut self, s: &SystemStatus) {
        self.len = 0;

        let secs = s.uptime_secs % 60;
        let mins = (s.uptime_secs / 60) % 60;
        let hrs = s.uptime_secs / 3600;

        let mut w = BufWriter {
            buf: &mut self.buf,
            pos: 0,
        };

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

        if hrs > 0 {
            let _ = write!(w, "  {}:{:02}:{:02}", hrs, mins, secs);
        } else {
            let _ = write!(w, "  {:02}:{:02}", mins, secs);
        }

        if s.heap_total > 0 {
            let _ = write!(w, "  H:{}/{}K", s.heap_used / 1024, s.heap_total / 1024);
        }

        if s.stack_free > 0 {
            let _ = write!(w, "  S:{}K", s.stack_free / 1024);
        }

        let _ = write!(w, "  SD:{}", if s.sd_ok { "OK" } else { "--" });

        self.len = w.pos;
    }

    fn text(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    pub fn draw<D>(&self, display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        BAR_REGION
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(display)?;

        let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
        Text::new(self.text(), Point::new(4, 14), style).draw(display)?;

        Ok(())
    }

    pub fn region(&self) -> Region {
        BAR_REGION
    }
}

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
    const DRAM_BASE: usize = 0x3FC8_0000;
    sp.saturating_sub(DRAM_BASE)
}

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
