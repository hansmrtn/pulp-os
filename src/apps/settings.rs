// System settings with persistent storage
//
// Only settings that correspond to real hardware knobs or kernel-level
// parameters live here.  Fields that have no physical target on the
// XTEink X4 (e.g. backlight — absent on e-paper; orientation — hardwired
// to Deg270 in the display driver) are intentionally excluded.
//
// Wiring status of each field:
//
//   sleep_timeout      — stored; kernel still uses a compile-time constant.
//                        Short path: read SystemSettings in the main loop
//                        and replace IDLE_THRESHOLD_POLLS with this value.
//
//   contrast           — SSD1677 VCOM register 0x2C.  Stored; not yet sent
//                        to the display driver.  Plumbing: add a
//                        `set_vcom(u8)` method to DisplayDriver and call it
//                        after loading settings.
//
//   ghost_clear_every  — directly replaces FULL_REFRESH_INTERVAL in
//                        main.rs once the main loop reads this value.
//
//   book_font_size_idx — reader body-font size selector (0=Small, 1=Medium,
//                        2=Large).  ReaderApp consults this on on_enter().
//
//   ui_font_size_idx   — shell / settings UI font size selector.
//                        Same index scale as book_font_size_idx.
//                        Fully wired: HomeApp / FilesApp / SettingsApp all
//                        store a body_font pointer updated via
//                        set_ui_font_size().  main.rs propagates the index
//                        to all three apps on every nav transition, before
//                        the lifecycle callback fires.
//
//   button_map         — selects a key-layout profile.
//                        0 = Default, 1 = Swapped (L/R swapped for left hand).
//
// Persistence: SystemSettings is a #[repr(C)] struct written as raw bytes
// to "settings.bin" in the SD card root.  The struct is 8 bytes; the pad
// field reserves room for future additions without a breaking change.
//
// I/O discipline: load and save are deferred entirely to on_work() so
// the render path is never blocked by SD card access.

use core::fmt::Write as _;

use crate::apps::{App, AppContext, Services, Transition};
use crate::board::button::Button as HwButton;
use crate::drivers::input::Event;
use crate::drivers::strip::StripBuffer;
use crate::fonts::bitmap::BitmapFont;
use crate::fonts::font_data;
use crate::ui::{Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, Region};

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

const NUM_ITEMS: usize = 6;

// Help line sits below the last item row.
const HELP_Y: u16 = ITEMS_TOP + NUM_ITEMS as u16 * ROW_STRIDE + 14;
const HELP_REGION: Region = Region::new(8, HELP_Y, 464, 18);

// ── Persistent settings ───────────────────────────────────────────────────────

const SETTINGS_FILE: &str = "settings.bin";

// Hardware-mapped settings persisted to the SD card as raw bytes.
//
// #[repr(C)] guarantees a stable on-disk layout. Never reorder or
// remove fields; add new ones before _pad and shrink _pad by the
// same number of bytes to keep size_of::<SystemSettings>() == 8.
//
// Layout (8 bytes total):
//   sleep_timeout      u16   bytes 0–1
//   contrast           u8    byte  2
//   ghost_clear_every  u8    byte  3
//   book_font_size_idx u8    byte  4
//   ui_font_size_idx   u8    byte  5
//   button_map         u8    byte  6
//   _pad               u8    byte  7
#[derive(Clone, Copy)]
#[repr(C)]
pub struct SystemSettings {
    pub sleep_timeout: u16,     // minutes of inactivity before sleep; 0 = never
    pub contrast: u8,           // SSD1677 VCOM register 0x2C; higher = darker
    pub ghost_clear_every: u8,  // partial refreshes before a forced full refresh
    pub book_font_size_idx: u8, // 0 = Small, 1 = Medium, 2 = Large
    pub ui_font_size_idx: u8,   // 0 = Small, 1 = Medium, 2 = Large
    pub button_map: u8,         // 0 = Default, 1 = Swapped (L/R)
    _pad: [u8; 1],              // reserved; keeps struct at 8 bytes
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
            book_font_size_idx: 0,
            ui_font_size_idx: 0,
            button_map: 0,
            _pad: [0u8; 1],
        }
    }

    // reinterpret self as a byte slice for writing to SD
    pub fn to_bytes(&self) -> &[u8] {
        unsafe {
            core::slice::from_raw_parts(
                self as *const Self as *const u8,
                core::mem::size_of::<Self>(),
            )
        }
    }

    // deserialise from raw bytes; returns defaults on short input
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
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
}

impl SettingsApp {
    pub fn new() -> Self {
        Self {
            settings: SystemSettings::defaults(),
            selected: 0,
            edit_mode: false,
            loaded: false,
            save_needed: false,
            body_font: &font_data::REGULAR_BODY_SMALL,
            heading_font: &font_data::REGULAR_HEADING,
        }
    }

    /// Called by main.rs whenever ui_font_size_idx changes.
    /// The heading font is always the fixed 24 px cut; only body text scales.
    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = match idx {
            1 => &font_data::REGULAR_BODY_MEDIUM,
            2 => &font_data::REGULAR_BODY_LARGE,
            _ => &font_data::REGULAR_BODY_SMALL,
        };
    }

    pub fn system_settings(&self) -> &SystemSettings {
        &self.settings
    }

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
            3 => "Book Font",
            4 => "UI Font",
            5 => "Button Map",
            _ => "",
        }
    }

    fn format_value<const N: usize>(&self, i: usize, buf: &mut BitmapDynLabel<N>) {
        buf.clear_text();
        match i {
            0 => {
                if self.settings.sleep_timeout == 0 {
                    let _ = write!(buf, "Never");
                } else {
                    let _ = write!(buf, "{} min", self.settings.sleep_timeout);
                }
            }
            1 => {
                let _ = write!(buf, "{}", self.settings.contrast);
            }
            2 => {
                let _ = write!(buf, "Every {}", self.settings.ghost_clear_every);
            }
            3 => {
                let s = match self.settings.book_font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            4 => {
                let s = match self.settings.ui_font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            5 => {
                let s = match self.settings.button_map {
                    1 => "Swapped",
                    _ => "Default",
                };
                let _ = write!(buf, "{}", s);
            }
            _ => {}
        }
    }

    // ── Value mutation ────────────────────────────────────────────────────────

    fn increment(&mut self) {
        match self.selected {
            0 => {
                self.settings.sleep_timeout = match self.settings.sleep_timeout {
                    0 => 5,
                    t if t >= 120 => 120,
                    t => t + 5,
                };
            }
            1 => {
                self.settings.contrast = self.settings.contrast.saturating_add(16);
            }
            2 => {
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_add(5).min(50);
            }
            3 => {
                if self.settings.book_font_size_idx < 2 {
                    self.settings.book_font_size_idx += 1;
                }
            }
            4 => {
                if self.settings.ui_font_size_idx < 2 {
                    self.settings.ui_font_size_idx += 1;
                }
            }
            5 => {
                self.settings.button_map = (self.settings.button_map + 1).min(1);
            }
            _ => return,
        }
        self.save_needed = true;
    }

    fn decrement(&mut self) {
        match self.selected {
            0 => {
                self.settings.sleep_timeout = match self.settings.sleep_timeout {
                    0..=5 => 0,
                    t => t - 5,
                };
            }
            1 => {
                self.settings.contrast = self.settings.contrast.saturating_sub(16);
            }
            2 => {
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_sub(5).max(5);
            }
            3 => {
                if self.settings.book_font_size_idx > 0 {
                    self.settings.book_font_size_idx -= 1;
                }
            }
            4 => {
                if self.settings.ui_font_size_idx > 0 {
                    self.settings.ui_font_size_idx -= 1;
                }
            }
            5 => {
                if self.settings.button_map > 0 {
                    self.settings.button_map -= 1;
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
            Event::Press(HwButton::Back) => {
                if self.edit_mode {
                    self.edit_mode = false;
                    ctx.mark_dirty(Self::row_region(self.selected));
                    return Transition::None;
                }
                Transition::Pop
            }

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

            Event::Press(HwButton::Confirm) => {
                self.edit_mode = !self.edit_mode;
                ctx.mark_dirty(Self::row_region(self.selected));
                Transition::None
            }

            Event::Repeat(HwButton::Right | HwButton::VolDown) if self.edit_mode => {
                self.increment();
                ctx.mark_dirty(Self::value_region(self.selected));
                Transition::None
            }

            Event::Repeat(HwButton::Left | HwButton::VolUp) if self.edit_mode => {
                self.decrement();
                ctx.mark_dirty(Self::value_region(self.selected));
                Transition::None
            }

            _ => Transition::None,
        }
    }

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
            ctx.request_screen_redraw();
            return;
        }

        if self.save_needed && self.save(services) {
            self.save_needed = false;
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        BitmapLabel::new(TITLE_REGION, "Settings", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if !self.loaded {
            let r = Region::new(LABEL_X, ITEMS_TOP, 200, ROW_H);
            BitmapLabel::new(r, "Loading...", self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        let mut val_buf = BitmapDynLabel::<20>::new(Region::new(0, 0, 1, 1), self.body_font);

        for i in 0..NUM_ITEMS {
            let selected = i == self.selected;
            let editing = selected && self.edit_mode;

            BitmapLabel::new(Self::label_region(i), Self::item_label(i), self.body_font)
                .alignment(Alignment::CenterLeft)
                .inverted(selected)
                .draw(strip)
                .unwrap();

            self.format_value(i, &mut val_buf);
            BitmapLabel::new(Self::value_region(i), val_buf.text(), self.body_font)
                .alignment(Alignment::Center)
                .inverted(editing)
                .draw(strip)
                .unwrap();
        }

        let help = if self.edit_mode {
            "L / R: adjust    Confirm / Back: done"
        } else {
            "L / R: select    Confirm: edit    Back: exit"
        };
        BitmapLabel::new(HELP_REGION, help, self.body_font)
            .alignment(Alignment::Center)
            .draw(strip)
            .unwrap();
    }
}
