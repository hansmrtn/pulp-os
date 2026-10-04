// SSD1677 e-paper logic shared by the X4 (GDEQ0426T82) and the OnePage C61
// (EPD0426A02) boards: geometry, rotation, command sequences, bounded BUSY
// wait and the full-refresh driver `Epd` (R10, R11).
//
// Both boards drive the same 800x480 source x gate panel size. BSP evidence for
// the C61 (../bsp_onepage_c61/board_c61.c): EPD_W 800 / EPD_H 480 (:50-51,
// "480x800 is NOT drivable" natively), gate_scan_dir 0x02 (:211), mirror_y
// "panel gates reversed" (:216), MOUI_ROTATION_270 -> portrait 480x800 (:222).
// X4 has the same native size, scan direction 0x02 in DRIVER_OUTPUT_CONTROL,
// reversed gates (Y flipped, X inc / Y dec) and Deg270 as its portrait
// rotation, so rotation and strip layout are shared as-is (no new rotation).
//
// Layering: this module never touches hardware. It reaches the panel through
// three small traits (`EpdBus` command/data, `power::DelayMs`, `BusyPin`),
// implemented over esp-hal in the kernel (X4: drivers/ssd1677.rs, C61:
// board_c61/epd.rs) and over recording fakes in the host tests below. Note the
// absence of any reset-pin API: on the C61 RST is GPIO27, the SD/MIC power
// rail, so the driver can only reset the controller with the SW_RESET command
// (0x12); `Epd::init` additionally demands a `power::DisplayReset` token,
// which `PeripheralPower::display_reset()` only ever hands out as `Software`.
//
// X4 compatibility: the sequences below are the former private methods of the
// X4 driver, moved without reordering (the X4 adapters ignore bus errors, as
// the old code did with `let _ =`). `x4_*` tests lock the command order.

use crate::power::{DelayMs, DisplayReset};
use crate::strip::{STRIP_COUNT, StripCore};

/// Native panel size (source x gate). Portrait (Deg90/Deg270) is 480x800.
pub const WIDTH: u16 = 800;
pub const HEIGHT: u16 = 480;

/// BSP `bridge_wait_busy` gives up after 5000 ms (board_c61.c:147-150). The
/// BSP only logs "EPD busy timeout" and carries on; here it is a display
/// failure. Configurable per `Epd` (`set_busy_timeout_ms`).
pub const BUSY_TIMEOUT_MS: u32 = 5000;
/// BUSY poll interval: BSP polls with `vTaskDelay(1 ms)` (board_c61.c:148).
pub const BUSY_POLL_MS: u32 = 1;
/// BUSY is active high on the C61 (BSP waits while the level is 1, :147) and
/// on X4 (driver waits for low); electrical level only, the `BusyPin` adapter
/// maps it.
pub const BUSY_ACTIVE_HIGH: bool = true;

/// Delay after SW_RESET before the next command (X4 value, unchanged).
pub const SOFT_RESET_DELAY_MS: u32 = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Rotation {
    #[default]
    Deg0,
    Deg90,
    Deg180,
    Deg270,
}

impl Rotation {
    /// Logical (width, height): 480x800 portrait for Deg90/Deg270.
    pub const fn logical_size(self) -> (u16, u16) {
        match self {
            Rotation::Deg0 | Rotation::Deg180 => (WIDTH, HEIGHT),
            Rotation::Deg90 | Rotation::Deg270 => (HEIGHT, WIDTH),
        }
    }
}

pub mod cmd {
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

/// Why a display operation failed. All of these are reported, never panic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayError {
    /// BUSY stayed asserted past the configured limit (R11).
    BusyTimeout,
    /// The SPI/pin transfer itself failed.
    Bus,
    /// `init` has not completed (or a failure invalidated it): re-init first.
    NotInitialized,
}

impl DisplayError {
    pub const fn as_str(self) -> &'static str {
        match self {
            DisplayError::BusyTimeout => "display busy timeout",
            DisplayError::Bus => "display bus error",
            DisplayError::NotInitialized => "display not initialized",
        }
    }
}

/// Command/data channel to the controller. `command` is DC low + one byte,
/// `data` is DC high + payload; chip select belongs to the SPI device.
pub trait EpdBus {
    fn command(&mut self, cmd: u8) -> Result<(), DisplayError>;
    fn data(&mut self, data: &[u8]) -> Result<(), DisplayError>;
}

/// BUSY line plus a monotonic millisecond clock (esp-hal `Instant` in the
/// kernel). `is_busy` is the logical state (electrical high, see
/// `BUSY_ACTIVE_HIGH`).
pub trait BusyPin {
    fn is_busy(&mut self) -> bool;
    fn now_ms(&mut self) -> u64;
}

/// Wait until BUSY clears, at most `timeout_ms`, polling every
/// `BUSY_POLL_MS`. BUSY is sampled before the deadline check, so a release
/// observed at exactly `timeout_ms` still counts as success; otherwise the
/// result is `Err(BusyTimeout)`. Termination does not depend on the clock
/// alone: the poll count is capped too, so a stuck clock cannot hang this.
pub fn wait_busy_bounded<P: BusyPin + DelayMs>(
    p: &mut P,
    timeout_ms: u32,
) -> Result<(), DisplayError> {
    let start = p.now_ms();
    let max_polls = timeout_ms / BUSY_POLL_MS + 1;
    let mut polls = 0u32;
    loop {
        if !p.is_busy() {
            return Ok(());
        }
        if p.now_ms().saturating_sub(start) >= timeout_ms as u64 || polls >= max_polls {
            return Err(DisplayError::BusyTimeout);
        }
        polls += 1;
        p.delay_ms(BUSY_POLL_MS);
    }
}

/// Source of rendered strips for a full frame: strip `idx` (0..STRIP_COUNT,
/// 40 physical rows each) rendered with `rotation`. X4: begin_strip + the
/// caller's draw closure; C61 boot: a test pattern.
pub trait StripSource {
    fn render_strip(&mut self, rotation: Rotation, idx: u16) -> &[u8];
}

/// Window setup. Gates are wired in reverse: Y flipped, X increment / Y
/// decrement (data entry mode 0x01).
pub fn set_ram_area<B: EpdBus>(
    b: &mut B,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> Result<(), DisplayError> {
    let y_flipped = HEIGHT - y - h;

    b.command(cmd::DATA_ENTRY_MODE)?;
    b.data(&[0x01])?;

    b.command(cmd::SET_RAM_X_RANGE)?;
    b.data(&[
        (x & 0xFF) as u8,
        (x >> 8) as u8,
        ((x + w - 1) & 0xFF) as u8,
        ((x + w - 1) >> 8) as u8,
    ])?;

    b.command(cmd::SET_RAM_Y_RANGE)?;
    b.data(&[
        ((y_flipped + h - 1) & 0xFF) as u8,
        ((y_flipped + h - 1) >> 8) as u8,
        (y_flipped & 0xFF) as u8,
        (y_flipped >> 8) as u8,
    ])?;

    b.command(cmd::SET_RAM_X_COUNTER)?;
    b.data(&[(x & 0xFF) as u8, (x >> 8) as u8])?;

    b.command(cmd::SET_RAM_Y_COUNTER)?;
    b.data(&[
        ((y_flipped + h - 1) & 0xFF) as u8,
        ((y_flipped + h - 1) >> 8) as u8,
    ])
}

/// SW_RESET (0x12) and settle. The only way this driver resets the controller.
pub fn soft_reset<P: EpdBus + DelayMs>(p: &mut P) -> Result<(), DisplayError> {
    p.command(cmd::SW_RESET)?;
    p.delay_ms(SOFT_RESET_DELAY_MS);
    Ok(())
}

/// Everything after the reset: temperature sensor, booster, driver output
/// (480 gates, scan 0x02), border, full RAM window.
pub fn configure<B: EpdBus>(b: &mut B) -> Result<(), DisplayError> {
    b.command(cmd::TEMPERATURE_SENSOR)?;
    b.data(&[0x80])?;

    b.command(cmd::BOOSTER_SOFT_START)?;
    b.data(&[0xAE, 0xC7, 0xC3, 0xC0, 0x80])?;

    b.command(cmd::DRIVER_OUTPUT_CONTROL)?;
    b.data(&[((HEIGHT - 1) & 0xFF) as u8, ((HEIGHT - 1) >> 8) as u8, 0x02])?;

    b.command(cmd::BORDER_WAVEFORM)?;
    b.data(&[0x01])?;

    set_ram_area(b, 0, 0, WIDTH, HEIGHT)
}

/// X4 init body: software reset then configure, no BUSY wait (unchanged).
pub fn init_display<P: EpdBus + DelayMs>(p: &mut P) -> Result<(), DisplayError> {
    soft_reset(p)?;
    configure(p)
}

/// Full frame into RED then BW RAM, strip by strip, each RAM preceded by a
/// full-window setup. Same order the X4 driver always used.
pub fn write_full_frame<P: EpdBus + DelayMs, S: StripSource>(
    p: &mut P,
    rotation: Rotation,
    src: &mut S,
) -> Result<(), DisplayError> {
    p.delay_ms(1);

    for &ram_cmd in &[cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
        set_ram_area(p, 0, 0, WIDTH, HEIGHT)?;
        p.command(ram_cmd)?;
        p.delay_ms(1);

        for i in 0..STRIP_COUNT {
            let data = src.render_strip(rotation, i);
            p.data(data)?;
        }
    }
    Ok(())
}

/// Full-refresh update: waveform from OTP/LUT (0xF7), power down at the end.
pub fn start_full_update<B: EpdBus>(b: &mut B) -> Result<(), DisplayError> {
    b.command(cmd::DISPLAY_UPDATE_CONTROL_1)?;
    b.data(&[0x40, 0x00])?;

    b.command(cmd::DISPLAY_UPDATE_CONTROL_2)?;
    b.data(&[0xF7])?;

    b.command(cmd::MASTER_ACTIVATION)
}

/// DEEP_SLEEP (0x10) mode 1: RAM and image retained, ~3 uA. Only a hardware
/// reset wakes it again (the X4 comment says the same; on the C61 the reset is
/// the GPIO27 power-up, see `Epd::enter_deep_sleep`). The X4 driver ends its
/// own `enter_deep_sleep` with exactly these two writes.
pub fn deep_sleep<B: EpdBus>(b: &mut B) -> Result<(), DisplayError> {
    b.command(cmd::DEEP_SLEEP)?;
    b.data(&[0x01])
}

/// Logical region -> physical panel region for `rotation`.
pub fn transform_region(
    rotation: Rotation,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> (u16, u16, u16, u16) {
    match rotation {
        Rotation::Deg0 => (x, y, w, h),
        Rotation::Deg90 => (WIDTH - y - h, x, h, w),
        Rotation::Deg180 => (WIDTH - x - w, HEIGHT - y - h, w, h),
        Rotation::Deg270 => (y, HEIGHT - x - w, h, w),
    }
}

/// Logical region -> byte-aligned physical region plus edge masks (the RAM
/// window is byte-aligned in X; pixels outside the region are masked white).
pub fn align_partial_region(
    rotation: Rotation,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
) -> Option<RenderState> {
    let (tx, ty, tw, th) = transform_region(rotation, x, y, w, h);

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

/// `StripSource` that renders the pattern with a closure into a `StripCore`
/// (what both the X4 draw path and the C61 boot demo do per strip).
pub struct CoreStrips<'a, F: FnMut(&mut StripCore)> {
    pub core: &'a mut StripCore,
    pub draw: F,
}

impl<F: FnMut(&mut StripCore)> StripSource for CoreStrips<'_, F> {
    fn render_strip(&mut self, rotation: Rotation, idx: u16) -> &[u8] {
        self.core.begin_strip(rotation, idx);
        (self.draw)(self.core);
        self.core.data()
    }
}

/// Full-refresh-only SSD1677 driver (OnePage C61 first version; partial
/// refresh waveform tuning is out of scope). Every failure invalidates the
/// controller state, so the next call must go through `init` again, which
/// resets with SW_RESET (never the GPIO27 hardware path).
pub struct Epd<P> {
    port: P,
    rotation: Rotation,
    init_done: bool,
    initial_refresh: bool,
    busy_timeout_ms: u32,
}

impl<P: EpdBus + DelayMs + BusyPin> Epd<P> {
    /// Portrait Deg270 (480x800), like X4 and BSP `MOUI_ROTATION_270`.
    pub fn new(port: P) -> Self {
        Self {
            port,
            rotation: Rotation::Deg270,
            init_done: false,
            initial_refresh: true,
            busy_timeout_ms: BUSY_TIMEOUT_MS,
        }
    }

    pub fn set_busy_timeout_ms(&mut self, ms: u32) {
        self.busy_timeout_ms = ms;
    }

    pub fn busy_timeout_ms(&self) -> u32 {
        self.busy_timeout_ms
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn is_initialized(&self) -> bool {
        self.init_done
    }

    pub fn needs_initial_refresh(&self) -> bool {
        self.initial_refresh
    }

    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// Controller init. `reset` is the policy from
    /// `PeripheralPower::display_reset()`; `Software` is the only value, so
    /// the controller is reset with SW_RESET and GPIO27 is not touched. BUSY
    /// is waited (bounded) before the reset and after it, which the X4 init
    /// does not do (conservative; BSP init flow could not be read, see
    /// baseline.md).
    pub fn init(&mut self, reset: DisplayReset) -> Result<(), DisplayError> {
        match reset {
            DisplayReset::Software => {}
        }
        self.init_done = false;
        let r = self.init_inner();
        self.init_done = r.is_ok();
        r
    }

    fn init_inner(&mut self) -> Result<(), DisplayError> {
        wait_busy_bounded(&mut self.port, self.busy_timeout_ms)?;
        soft_reset(&mut self.port)?;
        wait_busy_bounded(&mut self.port, self.busy_timeout_ms)?;
        configure(&mut self.port)
    }

    /// Render all strips into both RAMs, run the full update and wait (bounded)
    /// for BUSY. On any error the controller is considered uninitialised.
    pub fn full_refresh<S: StripSource>(&mut self, src: &mut S) -> Result<(), DisplayError> {
        if !self.init_done {
            return Err(DisplayError::NotInitialized);
        }
        let r = self.full_refresh_inner(src);
        if r.is_err() {
            self.init_done = false;
        } else {
            self.initial_refresh = false;
        }
        r
    }

    /// Park the controller for deep sleep (sleep-entry step "EPD park", BSP
    /// `board_display_sleep` -> `be->sleep`, board_c61.c:339 / :237-241).
    ///
    /// Waits (bounded) for any running update to finish, so power is never cut
    /// in the middle of a refresh, then sends DEEP_SLEEP mode 1 (image kept).
    /// The deep-sleep command is attempted even when the BUSY wait timed out or
    /// failed (a stuck controller may ignore it, but nothing is lost by
    /// trying); the first error is returned. The controller is considered
    /// uninitialised afterwards: wake-up needs a hardware reset, which on the
    /// C61 only happens through the GPIO27 power-up of the next boot (the
    /// state machine forbids any other GPIO27 reset), so `init` must not be
    /// expected to work again in the same run.
    pub fn enter_deep_sleep(&mut self) -> Result<(), DisplayError> {
        let busy = wait_busy_bounded(&mut self.port, self.busy_timeout_ms);
        let sent = deep_sleep(&mut self.port);
        self.init_done = false;
        busy.and(sent)
    }

    fn full_refresh_inner<S: StripSource>(&mut self, src: &mut S) -> Result<(), DisplayError> {
        write_full_frame(&mut self.port, self.rotation, src)?;
        start_full_update(&mut self.port)?;
        wait_busy_bounded(&mut self.port, self.busy_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::power::{PeripheralPower, PowerError, RailPin};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::string::String;
    use std::vec;
    use std::vec::Vec;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Ev {
        Cmd(u8),
        Data(Vec<u8>),
        Delay(u32),
        Busy(bool),
    }

    /// Recording fake: wire trace + a millisecond clock that only advances
    /// through `delay_ms`. BUSY goes high on SW_RESET / MASTER_ACTIVATION for
    /// `busy_ms` (`None` = never clears).
    struct Fake {
        log: Vec<Ev>,
        now: u64,
        busy_until: u64,
        busy_ms: Option<u64>,
        freeze_clock: bool,
        fail_after_writes: Option<usize>,
        writes: usize,
        samples: u32,
        /// log length right after init (set by `render_frame`)
        mark: usize,
    }

    impl Fake {
        fn new(busy_ms: Option<u64>) -> Self {
            Self {
                log: Vec::new(),
                now: 0,
                busy_until: 0,
                busy_ms,
                freeze_clock: false,
                fail_after_writes: None,
                writes: 0,
                samples: 0,
                mark: 0,
            }
        }

        fn start_busy(&mut self) {
            self.busy_until = match self.busy_ms {
                Some(ms) => self.now + ms,
                None => u64::MAX,
            };
        }

        fn write_gate(&mut self) -> Result<(), DisplayError> {
            self.writes += 1;
            match self.fail_after_writes {
                Some(n) if self.writes > n => Err(DisplayError::Bus),
                _ => Ok(()),
            }
        }

        fn cmds(&self) -> Vec<u8> {
            self.log
                .iter()
                .filter_map(|e| if let Ev::Cmd(c) = e { Some(*c) } else { None })
                .collect()
        }
    }

    impl EpdBus for Fake {
        fn command(&mut self, c: u8) -> Result<(), DisplayError> {
            self.write_gate()?;
            self.log.push(Ev::Cmd(c));
            if c == cmd::SW_RESET || c == cmd::MASTER_ACTIVATION {
                self.start_busy();
            }
            Ok(())
        }
        fn data(&mut self, d: &[u8]) -> Result<(), DisplayError> {
            self.write_gate()?;
            self.log.push(Ev::Data(d.to_vec()));
            Ok(())
        }
    }

    impl DelayMs for Fake {
        fn delay_ms(&mut self, ms: u32) {
            self.log.push(Ev::Delay(ms));
            if !self.freeze_clock {
                self.now += ms as u64;
            }
        }
    }

    impl BusyPin for Fake {
        fn is_busy(&mut self) -> bool {
            self.samples += 1;
            let b = self.now < self.busy_until;
            self.log.push(Ev::Busy(b));
            b
        }
        fn now_ms(&mut self) -> u64 {
            self.now
        }
    }

    fn init_trace_expected() -> Vec<Ev> {
        vec![
            Ev::Cmd(0x12),
            Ev::Delay(10),
            Ev::Cmd(0x18),
            Ev::Data(vec![0x80]),
            Ev::Cmd(0x0C),
            Ev::Data(vec![0xAE, 0xC7, 0xC3, 0xC0, 0x80]),
            Ev::Cmd(0x01),
            Ev::Data(vec![0xDF, 0x01, 0x02]),
            Ev::Cmd(0x3C),
            Ev::Data(vec![0x01]),
            // full RAM window (0,0,800,480)
            Ev::Cmd(0x11),
            Ev::Data(vec![0x01]),
            Ev::Cmd(0x44),
            Ev::Data(vec![0x00, 0x00, 0x1F, 0x03]),
            Ev::Cmd(0x45),
            Ev::Data(vec![0xDF, 0x01, 0x00, 0x00]),
            Ev::Cmd(0x4E),
            Ev::Data(vec![0x00, 0x00]),
            Ev::Cmd(0x4F),
            Ev::Data(vec![0xDF, 0x01]),
        ]
    }

    // -- helpers for streams -------------------------------------------------

    /// Concatenate the payloads of the bulk (strip-sized) data writes that
    /// follow `ram_cmd` until the next command.
    fn ram_stream(log: &[Ev], ram_cmd: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut on = false;
        for e in log {
            match e {
                Ev::Cmd(c) => on = *c == ram_cmd,
                Ev::Data(d) if on => out.extend_from_slice(d),
                _ => {}
            }
        }
        out
    }

    fn render_frame(draw: impl FnMut(&mut StripCore)) -> (Fake, Result<(), DisplayError>) {
        let mut epd = Epd::new(Fake::new(Some(1600)));
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().mark = epd.port_mut().log.len();
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw,
        };
        let r = epd.full_refresh(&mut src);
        let Epd { port, .. } = epd;
        (port, r)
    }

    /// Byte offset / bit mask of physical pixel (px, py) in a full-frame RAM
    /// stream (row-major, 100 bytes per physical row, MSB = lowest x).
    fn stream_pos(px: u16, py: u16) -> (usize, u8) {
        (
            py as usize * PHYS_BYTES + (px as usize / 8),
            0x80u8 >> (px % 8),
        )
    }
    const PHYS_BYTES: usize = (WIDTH as usize) / 8;

    // -- command trace (R10) -------------------------------------------------

    #[test]
    fn x4_init_sequence_is_unchanged_golden() {
        // The exact order the X4 driver has always used; X4 now calls the
        // same `init_display`.
        let mut f = Fake::new(None);
        init_display(&mut f).unwrap();
        assert_eq!(f.log, init_trace_expected());
    }

    #[test]
    fn x4_partial_ram_window_commands_are_unchanged_golden() {
        // window (px=96, py=340, pw=88, ph=123): y_flipped = 480-340-123 = 17
        let mut f = Fake::new(None);
        set_ram_area(&mut f, 96, 340, 88, 123).unwrap();
        assert_eq!(
            f.log,
            vec![
                Ev::Cmd(0x11),
                Ev::Data(vec![0x01]),
                Ev::Cmd(0x44),
                Ev::Data(vec![96, 0, 96 + 88 - 1, 0]),
                Ev::Cmd(0x45),
                Ev::Data(vec![17 + 123 - 1, 0, 17, 0]),
                Ev::Cmd(0x4E),
                Ev::Data(vec![96, 0]),
                Ev::Cmd(0x4F),
                Ev::Data(vec![17 + 123 - 1, 0]),
            ]
        );
    }

    #[test]
    fn x4_full_update_sequence_is_unchanged_golden() {
        let mut f = Fake::new(None);
        start_full_update(&mut f).unwrap();
        assert_eq!(
            f.log,
            vec![
                Ev::Cmd(0x21),
                Ev::Data(vec![0x40, 0x00]),
                Ev::Cmd(0x22),
                Ev::Data(vec![0xF7]),
                Ev::Cmd(0x20),
            ]
        );
    }

    #[test]
    fn x4_deep_sleep_sequence_is_unchanged_golden() {
        // X4 `enter_deep_sleep` ends with DEEP_SLEEP + 0x01; it now calls this.
        let mut f = Fake::new(None);
        deep_sleep(&mut f).unwrap();
        assert_eq!(f.log, vec![Ev::Cmd(0x10), Ev::Data(vec![0x01])]);
    }

    #[test]
    fn r19_epd_park_waits_for_idle_then_sends_deep_sleep_mode_1() {
        let mut epd = Epd::new(Fake::new(Some(30)));
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().mark = epd.port_mut().log.len();
        let start = epd.port_mut().mark;
        epd.enter_deep_sleep().unwrap();
        let tail: Vec<Ev> = epd.port_mut().log[start..].to_vec();
        // BUSY sampled first (and once more per ms until idle), then 0x10 0x01,
        // nothing after it
        assert_eq!(tail.first(), Some(&Ev::Busy(false)));
        assert_eq!(
            &tail[tail.len() - 2..],
            &[Ev::Cmd(0x10), Ev::Data(vec![0x01])]
        );
        assert!(
            !epd.is_initialized(),
            "deep sleep needs a hardware reset to leave"
        );
    }

    #[test]
    fn r19_epd_park_never_cuts_in_on_a_running_update() {
        // a refresh just started: BUSY is high for 1600 ms; the park must wait
        // it out before DEEP_SLEEP (the clock only advances through delay_ms)
        let mut epd = Epd::new(Fake::new(Some(1600)));
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().command(cmd::MASTER_ACTIVATION).unwrap();
        let before = epd.port_mut().now;
        epd.enter_deep_sleep().unwrap();
        assert!(epd.port_mut().now - before >= 1600 - 1);
        let log = &epd.port_mut().log;
        let busy_high_last = log
            .iter()
            .rposition(|e| *e == Ev::Busy(true))
            .expect("saw BUSY high");
        let deep = log.iter().position(|e| *e == Ev::Cmd(0x10)).unwrap();
        assert!(busy_high_last < deep);
    }

    #[test]
    fn r19_epd_park_with_a_stuck_busy_still_tries_deep_sleep_and_reports() {
        let mut epd = Epd::new(Fake::new(None));
        epd.port_mut().start_busy(); // BUSY never clears
        let r = epd.enter_deep_sleep();
        assert_eq!(r, Err(DisplayError::BusyTimeout));
        assert_eq!(
            epd.port_mut().cmds(),
            vec![0x10],
            "command attempted anyway"
        );
        assert!(epd.port_mut().now <= BUSY_TIMEOUT_MS as u64 + 1, "bounded");
    }

    #[test]
    fn r19_epd_park_bus_failure_is_reported() {
        let mut epd = Epd::new(Fake::new(Some(1)));
        epd.port_mut().fail_after_writes = Some(0);
        assert_eq!(epd.enter_deep_sleep(), Err(DisplayError::Bus));
        assert!(!epd.is_initialized());
    }

    #[test]
    fn r19_epd_park_works_on_a_never_initialised_controller() {
        let mut epd = Epd::new(Fake::new(None));
        epd.enter_deep_sleep().unwrap();
        assert_eq!(epd.port_mut().cmds(), vec![0x10]);
    }

    #[test]
    fn r10_epd_init_trace_is_bounded_wait_soft_reset_wait_configure() {
        let mut epd = Epd::new(Fake::new(Some(12)));
        epd.init(DisplayReset::Software).unwrap();
        assert!(epd.is_initialized());
        let log = &epd.port_mut().log;
        // BUSY idle before the reset, then SW_RESET is the very first command
        assert_eq!(log[0], Ev::Busy(false));
        assert_eq!(log[1], Ev::Cmd(0x12));
        assert_eq!(log[2], Ev::Delay(10));
        // BUSY (12 ms after SW_RESET, 10 ms already elapsed) polled until clear
        let after_reset: Vec<_> = log[3..]
            .iter()
            .take_while(|e| !matches!(e, Ev::Cmd(_)))
            .cloned()
            .collect();
        assert_eq!(
            after_reset,
            vec![
                Ev::Busy(true),
                Ev::Delay(1),
                Ev::Busy(true),
                Ev::Delay(1),
                Ev::Busy(false)
            ]
        );
        // everything after matches the X4 configure sequence, nothing is
        // written between the reset wait and TEMPERATURE_SENSOR
        let cfg_start = 3 + after_reset.len();
        assert_eq!(log[cfg_start..], init_trace_expected()[2..]);
    }

    #[test]
    fn r10_full_refresh_command_trace() {
        let (f, r) = render_frame(|_| {});
        assert_eq!(r, Ok(()));
        let first_refresh_delay = f.mark;
        // compress bulk strip payloads into their length for readability
        let mut summary: Vec<String> = Vec::new();
        for e in &f.log[first_refresh_delay..] {
            summary.push(match e {
                Ev::Cmd(c) => std::format!("C{:02X}", c),
                Ev::Data(d) if d.len() > 16 => std::format!("S{}", d.len()),
                Ev::Data(d) => std::format!("D{:02X?}", d),
                Ev::Delay(ms) => std::format!("T{}", ms),
                Ev::Busy(b) => std::format!("B{}", *b as u8),
            });
        }
        let window: Vec<String> = [
            "C11",
            "D[01]",
            "C44",
            "D[00, 00, 1F, 03]",
            "C45",
            "D[DF, 01, 00, 00]",
            "C4E",
            "D[00, 00]",
            "C4F",
            "D[DF, 01]",
        ]
        .iter()
        .map(|s| std::string::String::from(*s))
        .collect();
        let mut expect: Vec<String> = vec!["T1".into()];
        for ram in ["C26", "C24"] {
            expect.extend(window.iter().cloned());
            expect.push(ram.into());
            expect.push("T1".into());
            for _ in 0..12 {
                expect.push("S4000".into());
            }
        }
        expect.extend(
            ["C21", "D[40, 00]", "C22", "D[F7]", "C20"]
                .iter()
                .map(|s| std::string::String::from(*s)),
        );
        // BUSY wait: 1600 ms, polled 1 ms apart, ends on the first clear sample
        let wait: Vec<String> = summary.split_off(expect.len());
        assert_eq!(summary, expect);
        assert_eq!(wait.first().map(|s| s.as_str()), Some("B1"));
        assert_eq!(wait.last().map(|s| s.as_str()), Some("B0"));
        assert_eq!(wait.iter().filter(|s| s.as_str() == "T1").count(), 1600);
    }

    #[test]
    fn r10_full_refresh_writes_red_then_bw_each_full_frame() {
        let (f, r) = render_frame(|_| {});
        assert_eq!(r, Ok(()));
        let cmds = f.cmds();
        let red = cmds.iter().position(|c| *c == cmd::WRITE_RAM_RED).unwrap();
        let bw = cmds.iter().position(|c| *c == cmd::WRITE_RAM_BW).unwrap();
        assert!(red < bw);
        let red_bytes = ram_stream(&f.log, cmd::WRITE_RAM_RED);
        let bw_bytes = ram_stream(&f.log, cmd::WRITE_RAM_BW);
        assert_eq!(red_bytes.len(), 100 * 480);
        assert_eq!(bw_bytes.len(), 100 * 480);
        assert_eq!(red_bytes, bw_bytes);
        // blank page = all white (1 bits) in both RAMs
        assert!(bw_bytes.iter().all(|b| *b == 0xFF));
        // activation is last, after both RAM writes
        let act = cmds
            .iter()
            .position(|c| *c == cmd::MASTER_ACTIVATION)
            .unwrap();
        assert!(act > bw);
        assert_eq!(act, cmds.len() - 1);
    }

    #[test]
    fn r10_full_refresh_before_init_is_refused_and_sends_nothing() {
        let mut epd = Epd::new(Fake::new(None));
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        assert_eq!(
            epd.full_refresh(&mut src),
            Err(DisplayError::NotInitialized)
        );
        assert!(epd.port_mut().log.is_empty());
        assert!(epd.needs_initial_refresh());
    }

    #[test]
    fn r10_successful_refresh_clears_initial_refresh_flag() {
        let mut epd = Epd::new(Fake::new(Some(5)));
        epd.init(DisplayReset::Software).unwrap();
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        assert!(epd.needs_initial_refresh());
        epd.full_refresh(&mut src).unwrap();
        assert!(!epd.needs_initial_refresh());
        assert_eq!(epd.rotation(), Rotation::Deg270);
    }

    // -- strip data (R10) ----------------------------------------------------

    #[test]
    fn r10_portrait_logical_size_is_480x800() {
        assert_eq!(Rotation::Deg270.logical_size(), (480, 800));
        assert_eq!(Rotation::Deg90.logical_size(), (480, 800));
        assert_eq!(Rotation::Deg0.logical_size(), (800, 480));
        assert_eq!(StripCore::new().logical_size(), (480, 800));
        assert_eq!(STRIP_COUNT, 12);
        assert_eq!(crate::strip::STRIP_BUF_SIZE, 4000);
    }

    #[test]
    fn r10_portrait_four_corner_pixels_land_at_known_stream_offsets() {
        // Deg270: physical = (ly, 479 - lx). Streams are physical rows 0..479.
        let corners: [((i32, i32), (u16, u16)); 4] = [
            ((0, 0), (0, 479)),     // logical top-left     -> phys x=0,   y=479
            ((479, 0), (0, 0)),     // logical top-right    -> phys x=0,   y=0
            ((0, 799), (799, 479)), // logical bottom-left  -> phys x=799, y=479
            ((479, 799), (799, 0)), // logical bottom-right -> phys x=799, y=0
        ];
        for &((lx, ly), (px, py)) in &corners {
            let (f, r) = render_frame(|s| s.draw_pixel_logical(lx, ly, true));
            assert_eq!(r, Ok(()));
            let (off, mask) = stream_pos(px, py);
            for ram in [cmd::WRITE_RAM_RED, cmd::WRITE_RAM_BW] {
                let bytes = ram_stream(&f.log, ram);
                for (i, b) in bytes.iter().enumerate() {
                    if i == off {
                        assert_eq!(*b, 0xFF & !mask, "corner ({lx},{ly}) ram {ram:#x}");
                    } else {
                        assert_eq!(*b, 0xFF, "corner ({lx},{ly}) stray byte at {i}");
                    }
                }
            }
        }
        // explicit numbers for the two ends of the stream
        assert_eq!(stream_pos(0, 479), (47900, 0x80)); // 0x7F in the stream
        assert_eq!(stream_pos(799, 0), (99, 0x01)); // 0xFE
    }

    #[test]
    fn r10_top_left_pixel_is_stream_byte_47900_value_7f() {
        let (f, _) = render_frame(|s| s.draw_pixel_logical(0, 0, true));
        let bw = ram_stream(&f.log, cmd::WRITE_RAM_BW);
        assert_eq!(bw[47900], 0x7F);
        assert_eq!(bw.iter().filter(|b| **b != 0xFF).count(), 1);
        // it sits in the last strip (11), row 39, byte 0
        assert_eq!(47900, 11 * 4000 + 39 * 100);
    }

    #[test]
    fn r10_single_logical_row_is_one_physical_column_with_bit_order() {
        // logical row y=3 (x 0..479) -> physical x=3, y = 479 - x: one column
        let (f, _) = render_frame(|s| {
            for x in 0..480 {
                s.draw_pixel_logical(x, 3, true);
            }
        });
        let bw = ram_stream(&f.log, cmd::WRITE_RAM_BW);
        for py in 0..480usize {
            for bx in 0..100usize {
                let b = bw[py * 100 + bx];
                if bx == 0 {
                    assert_eq!(b, 0xFF & !(0x80 >> 3), "row {py}"); // bit 4 cleared
                } else {
                    assert_eq!(b, 0xFF);
                }
            }
        }
    }

    #[test]
    fn r10_single_logical_column_is_one_physical_row() {
        // logical column x=100 (y 0..799) -> physical y = 479-100 = 379, x=y
        let (f, _) = render_frame(|s| {
            for y in 0..800 {
                s.draw_pixel_logical(100, y, true);
            }
        });
        let bw = ram_stream(&f.log, cmd::WRITE_RAM_BW);
        for (i, b) in bw.iter().enumerate() {
            if i / 100 == 379 {
                assert_eq!(*b, 0x00, "row 379 byte {}", i % 100);
            } else {
                assert_eq!(*b, 0xFF, "stray at {i}");
            }
        }
    }

    #[test]
    fn r10_every_rotation_maps_bijectively_onto_the_panel() {
        for rot in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let mut core = StripCore::new();
            core.begin_strip(rot, 0);
            let (lw, lh) = core.logical_size();
            let mut seen = vec![false; WIDTH as usize * HEIGHT as usize];
            for ly in 0..lh {
                for lx in 0..lw {
                    let (px, py) = core.to_physical(lx, ly);
                    assert!(
                        px < WIDTH && py < HEIGHT,
                        "{rot:?} ({lx},{ly}) -> ({px},{py})"
                    );
                    let i = py as usize * WIDTH as usize + px as usize;
                    assert!(!seen[i], "{rot:?} collision at ({px},{py})");
                    seen[i] = true;
                }
            }
            assert!(seen.iter().all(|s| *s), "{rot:?} does not cover the panel");
        }
    }

    #[test]
    fn r10_blit_fast_path_matches_pixel_path_for_portrait() {
        // blit_1bpp has a dedicated Deg270 path (fonts) and a generic path;
        // both must place identical bits, including clipping at the panel
        // edges and non-multiple-of-8 glyph widths.
        let bm: [u8; 16] = [
            0x81, 0xC3, 0xA5, 0x99, 0x99, 0xA5, 0xC3, 0x81, 0xF0, 0x0F, 0xAA, 0x55, 0xFF, 0x00,
            0x3C, 0xC3,
        ];
        let spots: [(i32, i32); 9] = [
            (0, 0),
            (3, 5),
            (470, 790),
            (-4, -3),
            (475, 795),
            (100, 397),
            (200, 41),
            (33, 120),
            (479, 799),
        ];
        for &(w, h, stride) in &[
            (8usize, 8usize, 2usize),
            (5, 7, 2),
            (13, 8, 2),
            (1, 1, 2),
            (8, 16, 1),
        ] {
            for &(gx, gy) in &spots {
                for black in [true, false] {
                    for strip in 0..STRIP_COUNT {
                        let mut a = StripCore::new();
                        a.begin_strip(Rotation::Deg270, strip);
                        let mut b = StripCore::new();
                        b.begin_strip(Rotation::Deg270, strip);
                        // start from a non-blank strip so `black = false` is visible
                        if !black {
                            a.data_mut().fill(0x00);
                            b.data_mut().fill(0x00);
                        }
                        a.blit_1bpp(&bm, 0, w, h, stride, gx, gy, black);
                        b.blit_1bpp_generic(&bm, 0, w, h, stride, gx, gy, black);
                        assert_eq!(
                            a.data(),
                            b.data(),
                            "w{w} h{h} s{stride} at ({gx},{gy}) black={black} strip {strip}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn r10_fill_rect_matches_pixel_path_in_every_rotation() {
        let rects: [(u16, u16, u16, u16); 5] = [
            (0, 0, 1, 1),
            (3, 5, 14, 9),
            (5, 100, 42, 300),
            (7, 7, 8, 8),
            (0, 0, 480, 3),
        ];
        for rot in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let (lw, lh) = rot.logical_size();
            for &(x, y, w, h) in &rects {
                let (x1, y1) = ((x + w).min(lw), (y + h).min(lh));
                if x >= x1 || y >= y1 {
                    continue;
                }
                for strip in 0..STRIP_COUNT {
                    let mut a = StripCore::new();
                    a.begin_strip(rot, strip);
                    let mut b = StripCore::new();
                    b.begin_strip(rot, strip);
                    a.fill_logical_rect(x, y, x1, y1, true);
                    for ly in y..y1 {
                        for lx in x..x1 {
                            b.draw_pixel_logical(lx as i32, ly as i32, true);
                        }
                    }
                    assert_eq!(
                        a.data(),
                        b.data(),
                        "{rot:?} rect {:?} strip {strip}",
                        (x, y, w, h)
                    );
                }
            }
        }
    }

    #[test]
    fn r10_bringup_pattern_marks_the_logical_corners_asymmetrically() {
        let (f, r) = render_frame(crate::strip::draw_bringup_pattern);
        assert_eq!(r, Ok(()));
        let bw = ram_stream(&f.log, cmd::WRITE_RAM_BW);
        let core = StripCore::new(); // Deg270 mapping
        let black = |lx: u16, ly: u16| {
            let (px, py) = core.to_physical(lx, ly);
            let (off, mask) = stream_pos(px, py);
            bw[off] & mask == 0
        };
        assert!(black(10, 10)); // 64x64 block, top-left
        assert!(black(63, 63) && !black(64, 64));
        assert!(black(470, 10)); // 24x24 block, top-right
        assert!(black(10, 790)); // 24x24 block, bottom-left
        assert!(!black(470, 790)); // nothing bottom-right
        assert!(black(2, 400) && black(477, 400) && black(240, 2) && black(240, 797)); // frame
        assert!(!black(240, 400)); // inside stays white
        assert!(!black(100, 100));
    }

    #[test]
    fn r10_offscreen_pixels_are_ignored() {
        let (f, _) = render_frame(|s| {
            for &(x, y) in &[(-1, 0), (0, -1), (480, 0), (0, 800), (i32::MAX, i32::MIN)] {
                s.draw_pixel_logical(x, y, true);
            }
        });
        assert!(
            ram_stream(&f.log, cmd::WRITE_RAM_BW)
                .iter()
                .all(|b| *b == 0xFF)
        );
    }

    #[test]
    fn r10_partial_regions_align_to_byte_boundaries_with_edge_masks() {
        // aligned: logical (0,0,480,800) is the whole panel
        let rs = align_partial_region(Rotation::Deg270, 0, 0, 480, 800).unwrap();
        assert_eq!(
            (rs.px, rs.py, rs.pw, rs.ph, rs.left_mask, rs.right_mask),
            (0, 0, 800, 480, 0, 0)
        );
        // width not a multiple of 8: logical (3,5,10,10) -> phys x=5,w=10 -> 8..: [0,16)
        let rs = align_partial_region(Rotation::Deg270, 3, 5, 10, 10).unwrap();
        assert_eq!((rs.px, rs.py, rs.pw, rs.ph), (0, 467, 16, 10));
        assert_eq!(rs.left_mask, 0b1111_1000); // x0..4 outside the region
        assert_eq!(rs.right_mask, 0b0000_0001); // x15 outside
        // one pixel at the far corner keeps the masks inside one byte
        let rs = align_partial_region(Rotation::Deg270, 479, 799, 1, 1).unwrap();
        assert_eq!((rs.px, rs.py, rs.pw, rs.ph), (792, 0, 8, 1));
        assert_eq!((rs.left_mask, rs.right_mask), (0b1111_1110, 0));
        let rs = align_partial_region(Rotation::Deg270, 0, 0, 1, 1).unwrap();
        assert_eq!((rs.px, rs.py, rs.pw, rs.ph), (0, 479, 8, 1));
        assert_eq!((rs.left_mask, rs.right_mask), (0, 0b0111_1111));
        // empty region
        assert!(align_partial_region(Rotation::Deg270, 100, 200, 0, 5).is_none());
        // other rotations stay inside the panel
        for rot in [Rotation::Deg0, Rotation::Deg90, Rotation::Deg180] {
            let (lw, lh) = rot.logical_size();
            let rs = align_partial_region(rot, lw - 1, lh - 1, 1, 1).unwrap();
            assert!(rs.px + rs.pw <= WIDTH && rs.py + rs.ph <= HEIGHT);
            assert_eq!(rs.px % 8, 0);
            assert_eq!(rs.pw % 8, 0);
        }
    }

    // -- BUSY (R11) ----------------------------------------------------------

    #[test]
    fn r11_default_limit_is_the_bsp_5000_ms() {
        assert_eq!(BUSY_TIMEOUT_MS, 5000);
        assert_eq!(BUSY_POLL_MS, 1);
        assert!(BUSY_ACTIVE_HIGH);
        assert_eq!(Epd::new(Fake::new(None)).busy_timeout_ms(), 5000);
    }

    fn arm_busy(f: &mut Fake) {
        f.start_busy();
    }

    #[test]
    fn r11_stuck_busy_times_out_within_the_limit_and_does_not_hang() {
        let mut f = Fake::new(None);
        arm_busy(&mut f);
        assert_eq!(
            wait_busy_bounded(&mut f, 5000),
            Err(DisplayError::BusyTimeout)
        );
        assert!(
            f.now >= 5000 && f.now <= 5000 + BUSY_POLL_MS as u64,
            "waited {} ms",
            f.now
        );
        assert!(f.samples <= 5002);
    }

    #[test]
    fn r11_busy_released_before_the_limit_succeeds() {
        let mut f = Fake::new(Some(1234));
        arm_busy(&mut f);
        assert_eq!(wait_busy_bounded(&mut f, 5000), Ok(()));
        assert_eq!(f.now, 1234);
        // not busy at all: single sample, no delay
        let mut g = Fake::new(Some(0));
        arm_busy(&mut g);
        assert_eq!(wait_busy_bounded(&mut g, 5000), Ok(()));
        assert_eq!(g.log, vec![Ev::Busy(false)]);
    }

    #[test]
    fn r11_boundary_release_at_limit_succeeds_one_ms_later_fails() {
        for (release, ok) in [(99, true), (100, true), (101, false), (102, false)] {
            let mut f = Fake::new(Some(release));
            arm_busy(&mut f);
            let r = wait_busy_bounded(&mut f, 100);
            assert_eq!(r.is_ok(), ok, "release at {release} ms, limit 100 ms");
            if !ok {
                assert_eq!(r, Err(DisplayError::BusyTimeout));
                assert!(f.now <= 101);
            }
        }
    }

    #[test]
    fn r11_zero_limit_samples_once_then_fails() {
        let mut f = Fake::new(None);
        arm_busy(&mut f);
        assert_eq!(wait_busy_bounded(&mut f, 0), Err(DisplayError::BusyTimeout));
        assert_eq!(f.samples, 1);
        assert_eq!(f.now, 0);
    }

    #[test]
    fn r11_a_stuck_clock_cannot_hang_the_wait() {
        let mut f = Fake::new(None);
        f.freeze_clock = true;
        arm_busy(&mut f);
        assert_eq!(
            wait_busy_bounded(&mut f, 50),
            Err(DisplayError::BusyTimeout)
        );
        assert!(f.samples <= 52, "polled {} times", f.samples);
    }

    #[test]
    fn r11_stuck_busy_after_update_is_a_display_failure_not_a_hang() {
        let mut epd = Epd::new(Fake::new(None));
        // init cannot complete with a stuck line after SW_RESET: use a fake
        // whose BUSY only sticks after MASTER_ACTIVATION.
        epd.port_mut().busy_ms = Some(0);
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().busy_ms = None;
        epd.set_busy_timeout_ms(300);
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        let t0 = epd.port_mut().now;
        assert_eq!(epd.full_refresh(&mut src), Err(DisplayError::BusyTimeout));
        let waited = epd.port_mut().now - t0;
        // three 1 ms settles in write_full_frame + the 300 ms limit (+1 poll)
        assert!(waited <= 3 + 300 + 1, "waited {waited} ms");
        // controller state is untrusted now
        assert!(!epd.is_initialized());
        assert!(epd.needs_initial_refresh());
        assert_eq!(
            epd.full_refresh(&mut src),
            Err(DisplayError::NotInitialized)
        );
        // recovery: software re-init works once BUSY behaves again
        epd.port_mut().busy_ms = Some(0);
        epd.port_mut().busy_until = 0;
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().busy_ms = Some(10);
        assert_eq!(epd.full_refresh(&mut src), Ok(()));
    }

    #[test]
    fn r11_busy_timeout_limit_is_configurable() {
        let mut epd = Epd::new(Fake::new(Some(0)));
        epd.init(DisplayReset::Software).unwrap();
        epd.set_busy_timeout_ms(50);
        epd.port_mut().busy_ms = Some(40);
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        assert_eq!(epd.full_refresh(&mut src), Ok(()));
        epd.init(DisplayReset::Software).unwrap();
        epd.port_mut().busy_ms = Some(60);
        assert_eq!(epd.full_refresh(&mut src), Err(DisplayError::BusyTimeout));
    }

    #[test]
    fn r11_init_with_a_stuck_busy_line_fails_before_any_configuration() {
        let mut epd = Epd::new(Fake::new(Some(0)));
        epd.port_mut().busy_until = u64::MAX; // BUSY already stuck before init
        epd.set_busy_timeout_ms(20);
        assert_eq!(
            epd.init(DisplayReset::Software),
            Err(DisplayError::BusyTimeout)
        );
        assert!(!epd.is_initialized());
        // nothing was sent: not even the SW_RESET
        assert!(epd.port_mut().cmds().is_empty());
    }

    #[test]
    fn r11_spi_failure_mid_frame_is_reported_and_stops_the_stream() {
        let mut epd = Epd::new(Fake::new(Some(0)));
        epd.init(DisplayReset::Software).unwrap();
        let writes_after_init = epd.port_mut().writes;
        epd.port_mut().fail_after_writes = Some(writes_after_init + 25);
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        assert_eq!(epd.full_refresh(&mut src), Err(DisplayError::Bus));
        assert_eq!(epd.port_mut().writes, writes_after_init + 26); // stopped at first failure
        assert!(!epd.is_initialized());
        assert!(!epd.port_mut().cmds().contains(&cmd::MASTER_ACTIVATION));
    }

    #[test]
    fn r11_errors_have_displayable_text() {
        assert_eq!(DisplayError::BusyTimeout.as_str(), "display busy timeout");
        assert_eq!(DisplayError::Bus.as_str(), "display bus error");
        assert_eq!(
            DisplayError::NotInitialized.as_str(),
            "display not initialized"
        );
    }

    // -- software reset / GPIO27 ---------------------------------------------

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum RailEv {
        Low,
        High,
    }
    struct FakeRail(Rc<RefCell<Vec<RailEv>>>);
    impl RailPin for FakeRail {
        fn set_high(&mut self) {
            self.0.borrow_mut().push(RailEv::High);
        }
        fn set_low(&mut self) {
            self.0.borrow_mut().push(RailEv::Low);
        }
    }
    struct NoDelay;
    impl DelayMs for NoDelay {
        fn delay_ms(&mut self, _ms: u32) {}
    }

    #[test]
    fn r10_display_init_after_sd_is_software_reset_and_never_touches_gpio27() {
        let rail_log = Rc::new(RefCell::new(Vec::new()));
        let mut power = PeripheralPower::new(FakeRail(rail_log.clone()));
        power.power_cycle(&mut NoDelay).unwrap();
        let permit = power.begin_sd_init().unwrap();
        power.finish_sd_init(permit, true);
        let before = rail_log.borrow().clone();
        assert_eq!(before, vec![RailEv::Low, RailEv::High]); // the boot power-cycle only

        // runtime display reset policy, then the whole init + refresh
        let reset = power.display_reset().unwrap();
        assert_eq!(reset, DisplayReset::Software);
        let mut epd = Epd::new(Fake::new(Some(3)));
        epd.init(reset).unwrap();
        let mut core = StripCore::new();
        let mut src = CoreStrips {
            core: &mut core,
            draw: |_: &mut StripCore| {},
        };
        epd.full_refresh(&mut src).unwrap();
        // re-init (error recovery) also stays software
        epd.init(power.display_reset().unwrap()).unwrap();

        assert_eq!(
            *rail_log.borrow(),
            before,
            "GPIO27 must not move after the boot power-cycle"
        );
        // the controller was reset exactly by SW_RESET (twice: two inits)
        let resets = epd
            .port_mut()
            .cmds()
            .iter()
            .filter(|c| **c == cmd::SW_RESET)
            .count();
        assert_eq!(resets, 2);
        // hardware reset stays forbidden in every state
        assert_eq!(
            power.hardware_reset_display(),
            Err(PowerError::HardwareResetForbidden)
        );
        assert_eq!(*rail_log.borrow(), before);
    }

    #[test]
    fn r10_display_reset_is_refused_before_the_rail_is_up() {
        let rail_log = Rc::new(RefCell::new(Vec::new()));
        let mut power = PeripheralPower::new(FakeRail(rail_log.clone()));
        assert!(power.display_reset().is_err());
        assert!(rail_log.borrow().is_empty());
    }
}
