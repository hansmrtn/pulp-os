// System settings with persistent storage.
// Text-based key=value format in _PULP/SETTINGS.TXT.
use core::fmt::Write as _;

use crate::apps::{App, AppContext, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::ui::{Alignment, BitmapLabel, CONTENT_TOP, Region, StackFmt, wrap_next, wrap_prev};

const ROW_H: u16 = 40;
const ROW_GAP: u16 = 6;
const ROW_STRIDE: u16 = ROW_H + ROW_GAP;

const LABEL_X: u16 = 16;
const LABEL_W: u16 = 160;
const COL_GAP: u16 = 8;
const VALUE_X: u16 = LABEL_X + LABEL_W + COL_GAP;
const VALUE_W: u16 = 296; // reaches x = 472

const NUM_ITEMS: usize = 4;
const HEADING_ITEMS_GAP: u16 = 8; // gap between heading bottom and first row

// persistent settings

const SETTINGS_FILE: &str = "SETTINGS.TXT";
#[derive(Clone, Copy)]
pub struct SystemSettings {
    pub sleep_timeout: u16,     // minutes idle before sleep; 0 = never
    pub ghost_clear_every: u8,  // partial refreshes before a forced full refresh
    pub book_font_size_idx: u8, // 0 = Small, 1 = Medium, 2 = Large
    pub ui_font_size_idx: u8,   // 0 = Small, 1 = Medium, 2 = Large
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
            ghost_clear_every: 10,
            book_font_size_idx: 1,
            ui_font_size_idx: 1,
        }
    }

    fn sanitize(&mut self) {
        self.sleep_timeout = self.sleep_timeout.min(120);
        self.ghost_clear_every = self.ghost_clear_every.clamp(1, 50);
        self.book_font_size_idx = self.book_font_size_idx.min(2);
        self.ui_font_size_idx = self.ui_font_size_idx.min(2);
    }
}

// wifi config (in settings.txt)
pub const WIFI_SSID_CAP: usize = 32;
pub const WIFI_PASS_CAP: usize = 63;

pub struct WifiConfig {
    ssid: [u8; WIFI_SSID_CAP],
    ssid_len: u8,
    pass: [u8; WIFI_PASS_CAP],
    pass_len: u8,
}

impl WifiConfig {
    pub const fn empty() -> Self {
        Self {
            ssid: [0u8; WIFI_SSID_CAP],
            ssid_len: 0,
            pass: [0u8; WIFI_PASS_CAP],
            pass_len: 0,
        }
    }

    pub fn ssid(&self) -> &str {
        core::str::from_utf8(&self.ssid[..self.ssid_len as usize]).unwrap_or("")
    }

    pub fn password(&self) -> &str {
        core::str::from_utf8(&self.pass[..self.pass_len as usize]).unwrap_or("")
    }

    pub fn has_credentials(&self) -> bool {
        self.ssid_len > 0
    }

    fn set_ssid(&mut self, val: &[u8]) {
        let n = val.len().min(WIFI_SSID_CAP);
        self.ssid[..n].copy_from_slice(&val[..n]);
        self.ssid_len = n as u8;
    }

    fn set_pass(&mut self, val: &[u8]) {
        let n = val.len().min(WIFI_PASS_CAP);
        self.pass[..n].copy_from_slice(&val[..n]);
        self.pass_len = n as u8;
    }
}

// Text format parser / writer
fn trim(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && matches!(s[start], b' ' | b'\t' | b'\r') {
        start += 1;
    }
    while end > start && matches!(s[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }
    &s[start..end]
}

fn parse_u16(s: &[u8]) -> Option<u16> {
    if s.is_empty() {
        return None;
    }
    let mut val: u16 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u16)?;
    }
    Some(val)
}

fn apply_setting(key: &[u8], val: &[u8], s: &mut SystemSettings, w: &mut WifiConfig) {
    match key {
        b"sleep_timeout" => {
            if let Some(v) = parse_u16(val) {
                s.sleep_timeout = v;
            }
        }
        b"ghost_clear" => {
            if let Some(v) = parse_u16(val) {
                s.ghost_clear_every = v as u8;
            }
        }
        b"book_font" => {
            if let Some(v) = parse_u16(val) {
                s.book_font_size_idx = v as u8;
            }
        }
        b"ui_font" => {
            if let Some(v) = parse_u16(val) {
                s.ui_font_size_idx = v as u8;
            }
        }
        b"wifi_ssid" => w.set_ssid(val),
        b"wifi_pass" => w.set_pass(val),
        _ => {} // unknown keys silently ignored for forward compat
    }
}

fn parse_settings_txt(data: &[u8], settings: &mut SystemSettings, wifi: &mut WifiConfig) {
    for line in data.split(|&b| b == b'\n') {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        if let Some(eq) = line.iter().position(|&b| b == b'=') {
            let key = trim(&line[..eq]);
            let val = trim(&line[eq + 1..]);
            apply_setting(key, val, settings, wifi);
        }
    }
}

// tiny cursor writer for building the text representation
struct TxtWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> TxtWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn put(&mut self, data: &[u8]) {
        let n = data.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
        self.pos += n;
    }

    fn put_u16(&mut self, val: u16) {
        if val == 0 {
            self.put(b"0");
            return;
        }
        let mut digits = [0u8; 5];
        let mut i = 5;
        let mut v = val;
        while v > 0 {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        self.put(&digits[i..5]);
    }

    fn kv_num(&mut self, key: &[u8], val: u16) {
        self.put(key);
        self.put(b"=");
        self.put_u16(val);
        self.put(b"\n");
    }

    fn kv_str(&mut self, key: &[u8], val: &[u8]) {
        self.put(key);
        self.put(b"=");
        self.put(val);
        self.put(b"\n");
    }

    fn len(&self) -> usize {
        self.pos
    }
}

fn write_settings_txt(s: &SystemSettings, w: &WifiConfig, buf: &mut [u8]) -> usize {
    let mut wr = TxtWriter::new(buf);
    wr.put(b"# pulp-os settings\n");
    wr.put(b"# lines starting with # are ignored\n\n");
    wr.kv_num(b"sleep_timeout", s.sleep_timeout);
    wr.kv_num(b"ghost_clear", s.ghost_clear_every as u16);
    wr.kv_num(b"book_font", s.book_font_size_idx as u16);
    wr.kv_num(b"ui_font", s.ui_font_size_idx as u16);
    wr.put(b"\n# wifi credentials for upload mode\n");
    wr.kv_str(b"wifi_ssid", &w.ssid[..w.ssid_len as usize]);
    wr.kv_str(b"wifi_pass", &w.pass[..w.pass_len as usize]);
    wr.len()
}

// SettingsApp
impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SettingsApp {
    settings: SystemSettings,
    wifi: WifiConfig,
    selected: usize,
    loaded: bool,
    save_needed: bool,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
    items_top: u16,
}

impl SettingsApp {
    pub fn new() -> Self {
        let hf = fonts::heading_font(0);
        Self {
            settings: SystemSettings::defaults(),
            wifi: WifiConfig::empty(),
            selected: 0,
            loaded: false,
            save_needed: false,
            body_font: fonts::body_font(0),
            heading_font: hf,
            items_top: CONTENT_TOP + 4 + hf.line_height + HEADING_ITEMS_GAP,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = fonts::body_font(idx);
        self.heading_font = fonts::heading_font(idx);
        self.items_top = CONTENT_TOP + 4 + self.heading_font.line_height + HEADING_ITEMS_GAP;
    }

    pub fn system_settings(&self) -> &SystemSettings {
        &self.settings
    }

    pub fn system_settings_mut(&mut self) -> &mut SystemSettings {
        &mut self.settings
    }

    pub fn wifi_config(&self) -> &WifiConfig {
        &self.wifi
    }

    pub fn mark_save_needed(&mut self) {
        self.save_needed = true;
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    // load settings from SD and apply font indices immediately;
    // called once at boot so saved preferences are in effect from the first frame
    pub fn load_eager<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        services: &mut Services<'_, SPI>,
    ) {
        self.load(services);
        self.set_ui_font_size(self.settings.ui_font_size_idx);
    }

    fn load<SPI: embedded_hal::spi::SpiDevice>(&mut self, services: &mut Services<'_, SPI>) {
        let mut buf = [0u8; 512];

        self.settings = SystemSettings::defaults();
        self.wifi = WifiConfig::empty();

        match services.read_pulp_start(SETTINGS_FILE, &mut buf) {
            Ok((_size, n)) if n > 0 => {
                parse_settings_txt(&buf[..n], &mut self.settings, &mut self.wifi);
                self.settings.sanitize();
                log::info!("settings: loaded from {}", SETTINGS_FILE);
            }
            _ => {
                log::info!("settings: no file found, using defaults");
            }
        }

        self.loaded = true;
    }

    fn save<SPI: embedded_hal::spi::SpiDevice>(&self, services: &Services<'_, SPI>) -> bool {
        let mut buf = [0u8; 512];
        let len = write_settings_txt(&self.settings, &self.wifi, &mut buf);
        match services.write_pulp(SETTINGS_FILE, &buf[..len]) {
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

    fn item_label(i: usize) -> &'static str {
        match i {
            0 => "Sleep After",
            1 => "Ghost Clear",
            2 => "Book Font",
            3 => "UI Font",
            _ => "",
        }
    }

    fn format_value(&self, i: usize, buf: &mut StackFmt<20>) {
        buf.clear();
        match i {
            0 => {
                if self.settings.sleep_timeout == 0 {
                    let _ = write!(buf, "Never");
                } else {
                    let _ = write!(buf, "{} min", self.settings.sleep_timeout);
                }
            }
            1 => {
                let _ = write!(buf, "Every {}", self.settings.ghost_clear_every);
            }
            2 => {
                let s = match self.settings.book_font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            3 => {
                let s = match self.settings.ui_font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            _ => {}
        }
    }

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
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_add(5).min(50);
            }
            2 => {
                if self.settings.book_font_size_idx < 2 {
                    self.settings.book_font_size_idx += 1;
                }
            }
            3 => {
                if self.settings.ui_font_size_idx < 2 {
                    self.settings.ui_font_size_idx += 1;
                }
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
                self.settings.ghost_clear_every =
                    self.settings.ghost_clear_every.saturating_sub(5).max(1);
            }
            2 => {
                if self.settings.book_font_size_idx > 0 {
                    self.settings.book_font_size_idx -= 1;
                }
            }
            3 => {
                if self.settings.ui_font_size_idx > 0 {
                    self.settings.ui_font_size_idx -= 1;
                }
            }
            _ => return,
        }
        self.save_needed = true;
    }

    #[inline]
    fn label_region(&self, i: usize) -> Region {
        Region::new(
            LABEL_X,
            self.items_top + i as u16 * ROW_STRIDE,
            LABEL_W,
            ROW_H,
        )
    }

    #[inline]
    fn value_region(&self, i: usize) -> Region {
        Region::new(
            VALUE_X,
            self.items_top + i as u16 * ROW_STRIDE,
            VALUE_W,
            ROW_H,
        )
    }

    #[inline]
    fn row_region(&self, i: usize) -> Region {
        Region::new(
            LABEL_X,
            self.items_top + i as u16 * ROW_STRIDE,
            LABEL_W + COL_GAP + VALUE_W,
            ROW_H,
        )
    }
}

impl App for SettingsApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
        self.selected = 0;
        self.save_needed = false;
        ctx.mark_dirty(Region::new(0, CONTENT_TOP, 480, 800 - CONTENT_TOP));
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            ActionEvent::Press(Action::Next) => {
                let old = self.selected;
                self.selected = wrap_next(self.selected, NUM_ITEMS);
                if self.selected != old {
                    ctx.mark_dirty(self.row_region(old));
                    ctx.mark_dirty(self.row_region(self.selected));
                }
                Transition::None
            }

            ActionEvent::Press(Action::Prev) => {
                let old = self.selected;
                self.selected = wrap_prev(self.selected, NUM_ITEMS);
                if self.selected != old {
                    ctx.mark_dirty(self.row_region(old));
                    ctx.mark_dirty(self.row_region(self.selected));
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) | ActionEvent::Repeat(Action::NextJump) => {
                self.increment();
                ctx.mark_dirty(self.value_region(self.selected));
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) | ActionEvent::Repeat(Action::PrevJump) => {
                self.decrement();
                ctx.mark_dirty(self.value_region(self.selected));
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
            ctx.request_full_redraw();
            return;
        }

        if self.save_needed && self.save(services) {
            self.save_needed = false;
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let title_region = Region::new(16, CONTENT_TOP + 4, 448, self.heading_font.line_height);
        BitmapLabel::new(title_region, "Settings", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if !self.loaded {
            let r = Region::new(LABEL_X, self.items_top, 200, ROW_H);
            BitmapLabel::new(r, "Loading...", self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        let mut val_buf = StackFmt::<20>::new();

        for i in 0..NUM_ITEMS {
            let selected = i == self.selected;

            BitmapLabel::new(self.label_region(i), Self::item_label(i), self.body_font)
                .alignment(Alignment::CenterLeft)
                .inverted(selected)
                .draw(strip)
                .unwrap();

            self.format_value(i, &mut val_buf);
            BitmapLabel::new(self.value_region(i), val_buf.as_str(), self.body_font)
                .alignment(Alignment::Center)
                .inverted(selected)
                .draw(strip)
                .unwrap();
        }
    }
}
