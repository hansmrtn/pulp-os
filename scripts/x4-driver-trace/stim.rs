// shared stimulus: records the exact wire trace of the X4 DisplayDriver
use crate::drivers::ssd1677::{DisplayDriver, Rotation};
use crate::drivers::strip::StripBuffer;
use embedded_graphics_core::{pixelcolor::BinaryColor, prelude::*, primitives::Rectangle};
use embedded_hal::digital::{ErrorType as DErr, InputPin, OutputPin};
use embedded_hal::spi::{ErrorType as SErr, Operation, SpiDevice};
use std::cell::RefCell;
use std::rc::Rc;

fn fnv(d: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in d { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    h
}
type Dc = Rc<RefCell<bool>>; // true = high
struct Spi(Dc);
impl SErr for Spi { type Error = core::convert::Infallible; }
impl SpiDevice for Spi {
    fn transaction(&mut self, ops: &mut [Operation<'_, u8>]) -> Result<(), Self::Error> {
        for op in ops { if let Operation::Write(d) = op {
            if !*self.0.borrow() { esp_hal::log(format!("CMD {:02X} (len {})", d[0], d.len())); }
            else { esp_hal::log(format!("DATA len={} fnv={:016x}{}", d.len(), fnv(d), if d.len() <= 8 { format!(" {:02X?}", d) } else { String::new() })); }
        } }
        Ok(())
    }
}
struct Out(Dc, &'static str);
impl DErr for Out { type Error = core::convert::Infallible; }
impl OutputPin for Out {
    fn set_low(&mut self) -> Result<(), Self::Error> { *self.0.borrow_mut() = false; if self.1 == "RST" { esp_hal::log("RST low".into()); } Ok(()) }
    fn set_high(&mut self) -> Result<(), Self::Error> { *self.0.borrow_mut() = true; if self.1 == "RST" { esp_hal::log("RST high".into()); } Ok(()) }
}
struct Busy;
impl DErr for Busy { type Error = core::convert::Infallible; }
impl InputPin for Busy {
    fn is_high(&mut self) -> Result<bool, Self::Error> { Ok(false) }
    fn is_low(&mut self) -> Result<bool, Self::Error> { Ok(true) }
}

pub fn draw_pattern(s: &mut StripBuffer) {
    let _ = s.fill_solid(&Rectangle::new(Point::new(0, 0), Size::new(480, 3)), BinaryColor::On);
    let _ = s.fill_solid(&Rectangle::new(Point::new(5, 100), Size::new(37, 200)), BinaryColor::On);
    let _ = s.fill_solid(&Rectangle::new(Point::new(300, 700), Size::new(180, 100)), BinaryColor::On);
    let _ = s.fill_solid(&Rectangle::new(Point::new(10, 400), Size::new(100, 20)), BinaryColor::Off);
    let px = [(0, 0), (479, 0), (0, 799), (479, 799), (13, 17), (250, 401), (100, 399)];
    let _ = s.draw_iter(px.iter().map(|&(x, y)| Pixel(Point::new(x, y), BinaryColor::On)));
    let cols = (0..40).map(|i| if i % 3 == 0 { BinaryColor::On } else { BinaryColor::Off });
    let _ = s.fill_contiguous(&Rectangle::new(Point::new(7, 55), Size::new(8, 5)), cols);
    // glyph-like bitmaps through blit_1bpp (fast 270 path), incl. clipping at edges
    let bm: [u8; 16] = [0x81, 0xC3, 0xA5, 0x99, 0x99, 0xA5, 0xC3, 0x81, 0xF0, 0x0F, 0xAA, 0x55, 0xFF, 0x00, 0x3C, 0xC3];
    for &(gx, gy) in &[(0i32, 0i32), (3, 5), (470, 790), (-4, -3), (475, 795), (100, 397), (200, 41), (33, 120)] {
        s.blit_1bpp(&bm, 0, 8, 16, 1, gx, gy, true);
        s.blit_1bpp(&bm, 0, 6, 8, 1, gx + 9, gy + 2, false);
    }
    let w = s.logical_window();
    esp_hal::log(format!("LW {:?}", (w.x, w.y, w.w, w.h)));
}

pub fn run() -> Vec<String> {
    let _ = esp_hal::take_log();
    let mut strip = StripBuffer::new();
    let mut delay = esp_hal::delay::Delay::new();
    let dc: Dc = Rc::new(RefCell::new(true));
    let rst: Dc = Rc::new(RefCell::new(true));
    let mut drv = DisplayDriver::new(Spi(dc.clone()), Out(dc.clone(), "DC"), Out(rst.clone(), "RST"), Busy);
    let draw = |s: &mut StripBuffer| draw_pattern(s);

    esp_hal::log("== init".into());
    drv.init(&mut delay);
    esp_hal::log("== full frame".into());
    drv.write_full_frame(&mut strip, &mut delay, &draw);
    drv.start_full_update();
    drv.finish_full_update();
    esp_hal::log(format!("initial_refresh_needed={}", drv.needs_initial_refresh()));
    for (name, regions) in [("partial", &[(0u16, 0u16, 480u16, 800u16), (3, 5, 10, 10), (17, 100, 123, 77), (470, 790, 10, 10), (0, 0, 1, 1), (479, 799, 1, 1), (100, 200, 0, 5)][..])] {
        for &(x, y, w, h) in regions {
            esp_hal::log(format!("== {} region {:?}", name, (x, y, w, h)));
            if let Some(rs) = drv.partial_phase1_bw(&mut strip, x, y, w, h, &mut delay, &draw) {
                esp_hal::log(format!("RS {:?}", rs));
                drv.partial_start_du(&rs);
                let _ = drv.is_busy();
                drv.partial_phase3_sync(&mut strip, &rs, &draw);
            }
            if let Some(rs) = drv.partial_phase1_bw_inv_red(&mut strip, x, y, w, h, &mut delay, &draw) {
                esp_hal::log(format!("RS-inv {:?}", rs));
                drv.partial_start_du(&rs);
            }
        }
    }
    esp_hal::log("== deep sleep".into());
    drv.enter_deep_sleep();
    drv.enter_deep_sleep();
    // re-init path after deep sleep
    drv.write_full_frame(&mut strip, &mut delay, &draw);
    let _ = Rotation::Deg0;
    let _ = <DisplayDriver<Spi, Out, Out, Busy> as OriginDimensions>::size(&drv);
    esp_hal::take_log()
}
