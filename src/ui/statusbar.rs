// Debug status bar; zero height in release builds.

#[cfg(debug_assertions)]
use core::fmt::Write;

#[cfg(debug_assertions)]
use embedded_graphics::mono_font::MonoTextStyle;
#[cfg(debug_assertions)]
use embedded_graphics::mono_font::ascii::FONT_6X13;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;

use crate::board::SCREEN_W;
#[cfg(debug_assertions)]
use embedded_graphics::primitives::PrimitiveStyle;
#[cfg(debug_assertions)]
use embedded_graphics::text::Text;

#[cfg(debug_assertions)]
use super::stack_fmt::BorrowedFmt;
use super::widget::Region;

#[cfg(debug_assertions)]
pub const BAR_HEIGHT: u16 = 18;
#[cfg(not(debug_assertions))]
pub const BAR_HEIGHT: u16 = 4;

pub const CONTENT_TOP: u16 = BAR_HEIGHT;

pub const BAR_REGION: Region = Region::new(0, 0, SCREEN_W, BAR_HEIGHT);

pub struct SystemStatus {
    pub uptime_secs: u32,
    pub battery_mv: u16,
    pub battery_pct: u8,
    pub heap_used: usize,
    pub heap_peak: usize,
    pub heap_total: usize,
    pub stack_free: usize,
    pub stack_hwm: usize,
    pub sd_ok: bool,
}

pub struct StatusBar {
    #[cfg(debug_assertions)]
    buf: [u8; 112],
    #[cfg(debug_assertions)]
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
            #[cfg(debug_assertions)]
            buf: [0u8; 112],
            #[cfg(debug_assertions)]
            len: 0,
        }
    }

    pub fn update(&mut self, _s: &SystemStatus) {
        #[cfg(debug_assertions)]
        {
            let s = _s;
            self.len = 0;

            let secs = s.uptime_secs % 60;
            let mins = (s.uptime_secs / 60) % 60;
            let hrs = s.uptime_secs / 3600;

            let mut w = BorrowedFmt::new(&mut self.buf);

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
                let _ = write!(
                    w,
                    "  H:{}/{}/{}K",
                    s.heap_used / 1024,
                    s.heap_peak / 1024,
                    s.heap_total / 1024
                );
            }

            if s.stack_free > 0 {
                let _ = write!(w, "  S:{}K/{}K", s.stack_free / 1024, s.stack_hwm / 1024);
            }

            let _ = write!(w, "  SD:{}", if s.sd_ok { "OK" } else { "--" });

            self.len = w.len();
        }
    }

    #[cfg(debug_assertions)]
    fn text(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    pub fn draw<D>(&self, _display: &mut D) -> Result<(), D::Error>
    where
        D: DrawTarget<Color = BinaryColor>,
    {
        #[cfg(debug_assertions)]
        {
            let display = _display;
            BAR_REGION
                .to_rect()
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                .draw(display)?;

            let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::Off);
            Text::new(self.text(), Point::new(4, 14), style).draw(display)?;
        }

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

    #[cfg(target_arch = "riscv32")]
    {
        unsafe extern "C" {
            static _stack_end_cpu0: u8;
        }
        let stack_bottom = (&raw const _stack_end_cpu0) as usize;
        sp.saturating_sub(stack_bottom)
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        sp
    }
}

const STACK_PAINT_WORD: u32 = 0xDEAD_BEEF;

pub fn paint_stack() {
    #[cfg(target_arch = "riscv32")]
    {
        let sp: usize;
        unsafe {
            core::arch::asm!("mv {}, sp", out(reg) sp);
        }

        unsafe extern "C" {
            static _stack_end_cpu0: u8;
        }
        let bottom = (&raw const _stack_end_cpu0) as usize;

        let guard_skip = 256; // skip esp-hal stack guard word
        let paint_bottom = bottom + guard_skip;

        let paint_top = sp.saturating_sub(256); // leave 256B below SP for our frame + ISR

        if paint_top <= paint_bottom {
            return;
        }

        let start = (paint_bottom + 3) & !3;

        let mut addr = start;
        while addr + 4 <= paint_top {
            unsafe {
                core::ptr::write_volatile(addr as *mut u32, STACK_PAINT_WORD);
            }
            addr += 4;
        }
    }
}

pub fn stack_high_water_mark() -> usize {
    #[cfg(target_arch = "riscv32")]
    {
        unsafe extern "C" {
            static _stack_end_cpu0: u8;
            static _stack_start_cpu0: u8;
        }
        let bottom = (&raw const _stack_end_cpu0) as usize;
        let top = (&raw const _stack_start_cpu0) as usize;

        let guard_skip = 256; // same guard region as paint_stack
        let scan_bottom = bottom + guard_skip;

        let start = (scan_bottom + 3) & !3;

        let mut addr = start;
        while addr + 4 <= top {
            let val = unsafe { core::ptr::read_volatile(addr as *const u32) };
            if val != STACK_PAINT_WORD {
                break;
            }
            addr += 4;
        }

        top.saturating_sub(addr)
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        0
    }
}
