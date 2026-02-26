// System settings with persistent storage
//
// Only settings that correspond to real hardware knobs or kernel-level
// parameters live here.  Fields that have no physical target on the
// XTEink X4 (e.g. backlight — absent on e-paper; orientation — hardwired
// to Deg270 in the display driver) are intentionally excluded.
//
// Wiring status of each field:
//
//   sleep_timeout    — stored; kernel still uses a compile-time constant.
//                      Short path: read SystemSettings in the main loop
//                      and replace IDLE_THRESHOLD_POLLS with this value.
//
//   contrast         — SSD1677 VCOM register 0x2C.  Stored; not yet sent
//                      to the display driver.  Plumbing: add a
//                      `set_vcom(u8)` method to DisplayDriver and call it
//                      after loading settings.
//
//   ghost_clear_every — directly replaces FULL_REFRESH_INTERVAL in
//                       main.rs once the main loop reads this value.
//
//   font_size_idx    — reader font size selector.  Stored now; only takes
//                      effect once build.rs rasterises a second pixel size
//                      and ReaderApp consults this field on on_enter().
//
// Persistence: SystemSettings is a #[repr(C)] struct written as raw bytes
// to "settings.bin" in the SD card root.  The struct is 8 bytes; the pad
// field reserves room for future additions without a breaking change.
//
// I/O discipline: load and save are deferred entirely to on_work() so
// the render path is never blocked by SD card access.

use core::fmt::Write as _;

use embedded_graphics::mono_font::ascii::{FONT_8X13, FONT_10X20};

use crate::apps::{App, AppContext, Services, Transition};
use crate::board::button::Button as HwButton;
use crate::board::strip::StripBuffer;
use crate::drivers::input::Event;
use crate::ui::{Alignment, CONTENT_TOP, DynamicLabel, Label, Region, Widget};

// ── Layout ────────────────────────────────────────────────────────────────────
//
// Logical screen: 480 wide × 800 tall (Deg270 rotation).
// Status bar occupies y 0..CONTENT_TOP (18 px).

const TITLE_REGION: Region = Region::new(16, CONTENT_TOP + 4, 448, 28);

const ITEMS_TOP: u16 = CONTENT_TOP + 44;
const ROW_H: u16 = 40;
const ROW_GAP: u16 = 6;
const ROW_STRIDE: u16 = ROW_H + ROW_GAP;

// Left column: setting name.
const LABEL_X: u16 = 16;
const LABEL_W: u16 = 160;

// Right column: current value.
const COL_GAP: u16 = 8;
const VALUE_X: u16 = LABEL_X + LABEL_W + COL_GAP;
const VALUE_W: u16 = 296; // reaches to x = 480 − 8 = 472

const NUM_ITEMS: usize = 4;

// Help line sits below the last item row.
const HELP_Y: u16 = ITEMS_TOP + NUM_ITEMS as u16 * ROW_STRIDE + 14;
const HELP_REGION: Region = Region::new(8, HELP_Y, 464, 18);

// ── Persistent settings ───────────────────────────────────────────────────────

const SETTINGS_FILE: &str = "settings.bin";

/// Hardware-mapped settings persisted to the SD card as raw bytes.
///
/// `#[repr(C)]` guarantees a stable on-disk layout.  Never reorder or
/// remove fields; add new ones before `_pad` and shrink `_pad` by the
/// same number of bytes to keep `size_of::<SystemSettings>() == 8`.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SystemSettings {
    /// Minutes of inactivity before the device sleeps.  0 = never.
    /// Practical range: 0–120 in steps of 5.
    pub sleep_timeout: u16,

    /// SSD1677 VCOM level sent to register 0x2C.  Controls ink
    /// contrast; higher values produce darker rendering.
    /// Range: 0–255, default 150.
    pub contrast: u8,

    /// How many partial refreshes to allow before forcing a full
    /// hardware refresh to clear e-paper ghosting.
    /// Range: 5–50 in steps of 5, default 10.
    pub ghost_clear_every: u8,

    /// Reader body-font size index.
    /// 0 = Small (~16 px), 1 = Medium (~20 px), 2 = Large (~24 px).
    /// Falls back to 0 if the selected size is not compiled in.
    pub font_size_idx: u8,

    /// Reserved.  Always written as zero; ignored on read.
    /// Keeps the struct at 8 bytes for future field additions.
    _pad: [u8; 3],
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl SystemSettings {
    pub const fn defaults() -> Self {
        Self {
            sleep_timeout: 10,
            contrast: 150,
            ghost_clear_every: 10,
            font_size_idx: 0,
            _pad: [0u8; 3],
        }
    }

    /// Reinterpret `self` as a byte slice for writing to SD.
    pub fn to_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    /// Deserialise from raw bytes; returns defaults on short input.
    pub fn from_bytes(data: &[u8]) -> Self {
        let size = core::mem::size_of::<Self>();
        if data.len() >= size {
            let mut s = Self::defaults();
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr(), &mut s as *mut Self as *mut u8, size);
            }
            s
        } else {
            Self::defaults()
        }
    }
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct SettingsApp {
    settings: SystemSettings,
    selected: usize,
    edit_mode: bool,
    loaded: bool,
    save_needed: bool,
}

impl SettingsApp {
    pub const fn new() -> Self {
        Self {
            settings: SystemSettings::defaults(),
            selected: 0,
            edit_mode: false,
            loaded: false,
            save_needed: false,
        }
    }

    /// Returns the current (possibly still-default) settings.
    /// Callers should check `is_loaded()` if they need to know whether
    /// the SD card values have been read yet.
    pub fn system_settings(&self) -> &SystemSettings {
        &self.settings
    }

    /// True once the initial SD load has completed (or failed).
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    // ── Storage ───────────────────────────────────────────────────────────────

    fn load<SPI: embedded_hal::spi::SpiDevice>(&mut self, services: &mut Services<'_, SPI>) {
        // Buffer must be >= size_of::<SystemSettings>() = 8 bytes.
        let mut buf = [0u8; 32];
        match services.read_file_start(SETTINGS_FILE, &mut buf) {
            Ok((size, n)) if n > 0 => {
                self.settings = SystemSettings::from_bytes(&buf[..n.min(size as usize)]);
                log::info!("settings: loaded from {}", SETTINGS_FILE);
            }
            _ => {
                self.settings = SystemSettings::defaults();
                log::info!("settings: file absent or empty, using defaults");
            }
        }
        self.loaded = true;
    }

    fn save<SPI: embedded_hal::spi::SpiDevice>(&self, services: &Services<'_, SPI>) -> bool {
        match services.write_file(SETTINGS_FILE, self.settings.to_bytes()) {
            Ok(_) => {
                log::info!("settings: saved to {}", SETTINGS_FILE);
                true
            }
            Err(e) => {
                log::error!("settings: save failed: {}", e);
                false
            }
        }
    }

    // ── Item metadata ─────────────────────────────────────────────────────────

    fn item_label(i: usize) -> &'static str {
        match i {
            0 => "Sleep After",
            1 => "Contrast",
            2 => "Ghost Clear",
            3 => "Font Size",
            _ => "",
        }
    }

    /// Write a human-readable value for item `i` into `buf`.
    fn format_value<const N: usize>(&self, i: usize, buf: &mut DynamicLabel<N>) {
        buf.clear_text();
        match i {
            // "Never" | "N min"
            0 => {
                if self.settings.sleep_timeout == 0 {
                    let _ = write!(buf, "Never");
                } else {
                    let _ = write!(buf, "{} min", self.settings.sleep_timeout);
                }
            }
            // Raw VCOM byte 0–255
            1 => {
                let _ = write!(buf, "{}", self.settings.contrast);
            }
            // "Every N"
            2 => {
                let _ = write!(buf, "Every {}", self.settings.ghost_clear_every);
            }
            // "Small" | "Medium" | "Large"
            3 => {
                let s = match self.settings.font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            _ => {}
        }
    }

    // ── Value mutation ────────────────────────────────────────────────────────

    fn increment(&mut self) {
        match self.selected {
            // Sleep: 0 → 5 → 10 → … → 120, then hold at 120.
            0 => {
                self.settings.sleep_timeout = match self.settings.sleep_timeout {
                    0 => 5,
                    t if t >= 120 => 120,
                    t => t + 5,
                };
            }
            // Contrast: 0–255 in 16-step increments (16 discrete levels).
            1 => {
                self.settings.contrast = self.settings.contrast.saturating_add(16);
            }
            // Ghost clear: 5–50 in steps of 5.
            2 => {
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_add(5).min(50);
            }
            // Font size: 0 → 1 → 2, then hold.
            3 => {
                if self.settings.font_size_idx < 2 {
                    self.settings.font_size_idx += 1;
                }
            }
            _ => return,
        }
        self.save_needed = true;
    }

    fn decrement(&mut self) {
        match self.selected {
            // Sleep: 5 → 0, then hold at 0 (Never).
            0 => {
                self.settings.sleep_timeout = match self.settings.sleep_timeout {
                    0..=5 => 0,
                    t => t - 5,
                };
            }
            // Contrast: floor at 0.
            1 => {
                self.settings.contrast = self.settings.contrast.saturating_sub(16);
            }
            // Ghost clear: floor at 5.
            2 => {
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_sub(5).max(5);
            }
            // Font size: 2 → 1 → 0, then hold.
            3 => {
                if self.settings.font_size_idx > 0 {
                    self.settings.font_size_idx -= 1;
                }
            }
            _ => return,
        }
        self.save_needed = true;
    }

    // ── Region helpers ────────────────────────────────────────────────────────

    #[inline]
    fn label_region(i: usize) -> Region {
        Region::new(LABEL_X, ITEMS_TOP + i as u16 * ROW_STRIDE, LABEL_W, ROW_H)
    }

    #[inline]
    fn value_region(i: usize) -> Region {
        Region::new(VALUE_X, ITEMS_TOP + i as u16 * ROW_STRIDE, VALUE_W, ROW_H)
    }

    /// Bounding box that covers both columns of row `i`.
    /// Used for dirty-tracking on selection and mode changes.
    #[inline]
    fn row_region(i: usize) -> Region {
        Region::new(
            LABEL_X,
            ITEMS_TOP + i as u16 * ROW_STRIDE,
            LABEL_W + COL_GAP + VALUE_W,
            ROW_H,
        )
    }
}

impl App for SettingsApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        self.selected = 0;
        self.edit_mode = false;
        self.save_needed = false;
        ctx.request_screen_redraw();
    }

    fn on_event(&mut self, event: Event, ctx: &mut AppContext) -> Transition {
        match event {
            // Back: leave edit mode first; second Back pops to Home.
            Event::Press(HwButton::Back) => {
                if self.edit_mode {
                    self.edit_mode = false;
                    ctx.mark_dirty(Self::row_region(self.selected));
                    return Transition::None;
                }
                Transition::Pop
            }

            // Right / VolDown: move selection down, or increment value.
            Event::Press(HwButton::Right | HwButton::VolDown) => {
                if self.edit_mode {
                    self.increment();
                    ctx.mark_dirty(Self::value_region(self.selected));
                } else {
                    let old = self.selected;
                    self.selected = (self.selected + 1).min(NUM_ITEMS - 1);
                    if self.selected != old {
                        ctx.mark_dirty(Self::row_region(old));
                        ctx.mark_dirty(Self::row_region(self.selected));
                    }
                }
                Transition::None
            }

            // Left / VolUp: move selection up, or decrement value.
            Event::Press(HwButton::Left | HwButton::VolUp) => {
                if self.edit_mode {
                    self.decrement();
                    ctx.mark_dirty(Self::value_region(self.selected));
                } else {
                    let old = self.selected;
                    self.selected = self.selected.saturating_sub(1);
                    if self.selected != old {
                        ctx.mark_dirty(Self::row_region(old));
                        ctx.mark_dirty(Self::row_region(self.selected));
                    }
                }
                Transition::None
            }

            // Confirm: toggle edit mode for the selected item.
            Event::Press(HwButton::Confirm) => {
                self.edit_mode = !self.edit_mode;
                ctx.mark_dirty(Self::row_region(self.selected));
                Transition::None
            }

            // Auto-repeat while holding Right / VolDown in edit mode.
            Event::Repeat(HwButton::Right | HwButton::VolDown) if self.edit_mode => {
                self.increment();
                ctx.mark_dirty(Self::value_region(self.selected));
                Transition::None
            }

            // Auto-repeat while holding Left / VolUp in edit mode.
            Event::Repeat(HwButton::Left | HwButton::VolUp) if self.edit_mode => {
                self.decrement();
                ctx.mark_dirty(Self::value_region(self.selected));
                Transition::None
            }

            _ => Transition::None,
        }
    }

    /// Keep the kernel from rendering while the initial SD load or a
    /// pending save is in flight (render-ownership invariant).
    fn needs_work(&self) -> bool {
        !self.loaded || self.save_needed
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        services: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        if !self.loaded {
            self.load(services);
            // Full repaint so the loaded values replace the "Loading..." text.
            ctx.request_screen_redraw();
            return;
        }

        if self.save_needed && self.save(services) {
            self.save_needed = false;
        }

        ctx.request_screen_redraw();
    }

    fn draw(&self, strip: &mut StripBuffer) {
        Label::new(TITLE_REGION, "Settings", &FONT_10X20)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if !self.loaded {
            let r = Region::new(LABEL_X, ITEMS_TOP, 200, ROW_H);
            Label::new(r, "Loading...", &FONT_8X13)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        // Shared formatting buffer; 20 chars covers all value strings.
        let mut val_buf = DynamicLabel::<20>::new(Region::new(0, 0, 1, 1), &FONT_8X13);

        for i in 0..NUM_ITEMS {
            let selected = i == self.selected;
            let editing = selected && self.edit_mode;

            // Label column — inverted background when this row is selected.
            Label::new(Self::label_region(i), Self::item_label(i), &FONT_8X13)
                .alignment(Alignment::CenterLeft)
                .inverted(selected)
                .draw(strip)
                .unwrap();

            // Value column — additionally inverted while in edit mode to
            // signal that Left / Right will adjust this value.
            self.format_value(i, &mut val_buf);
            Label::new(Self::value_region(i), val_buf.text(), &FONT_8X13)
                .alignment(Alignment::Center)
                .inverted(editing)
                .draw(strip)
                .unwrap();
        }

        // Context-sensitive help line.
        let help = if self.edit_mode {
            "L / R: adjust    Confirm / Back: done"
        } else {
            "L / R: select    Confirm: edit    Back: exit"
        };
        Label::new(HELP_REGION, help, &FONT_8X13)
            .alignment(Alignment::Center)
            .draw(strip)
            .unwrap();
    }
}
