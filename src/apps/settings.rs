// settings app UI; configuration types live in kernel::config
use core::fmt::Write as _;

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::max_size_idx;
use crate::kernel::KernelHandle;
use crate::kernel::config::{
    self, SystemSettings, WifiConfig, parse_settings_txt, write_settings_txt,
};
use crate::ui::{Alignment, BitmapLabel, CONTENT_TOP, Region, StackFmt, wrap_next, wrap_prev};

const ROW_H: u16 = 40;
const ROW_GAP: u16 = 6;
const ROW_STRIDE: u16 = ROW_H + ROW_GAP;

const LABEL_X: u16 = 16;
const LABEL_W: u16 = 160;
const COL_GAP: u16 = 8;
const VALUE_X: u16 = LABEL_X + LABEL_W + COL_GAP;
const VALUE_W: u16 = 296;

const NUM_ITEMS: usize = 4;
const HEADING_ITEMS_GAP: u16 = 8;

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
    ui_fonts: fonts::UiFonts,
    items_top: u16,
}

impl SettingsApp {
    pub fn new() -> Self {
        let uf = fonts::UiFonts::for_size(0);
        Self {
            settings: SystemSettings::defaults(),
            wifi: WifiConfig::empty(),
            selected: 0,
            loaded: false,
            save_needed: false,
            ui_fonts: uf,
            items_top: CONTENT_TOP + 4 + uf.heading.line_height + HEADING_ITEMS_GAP,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.ui_fonts = fonts::UiFonts::for_size(idx);
        self.items_top = CONTENT_TOP + 4 + self.ui_fonts.heading.line_height + HEADING_ITEMS_GAP;
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

    pub fn load_eager(&mut self, k: &mut KernelHandle<'_>) {
        self.load(k);
        self.set_ui_font_size(self.settings.ui_font_size_idx);
    }

    fn load(&mut self, k: &mut KernelHandle<'_>) {
        let mut buf = [0u8; 512];

        self.settings = SystemSettings::defaults();
        self.wifi = WifiConfig::empty();

        match k.read_app_data_start(config::SETTINGS_FILE, &mut buf) {
            Ok((_size, n)) if n > 0 => {
                parse_settings_txt(&buf[..n], &mut self.settings, &mut self.wifi);
                self.settings.sanitize();
                log::info!("settings: loaded from {}", config::SETTINGS_FILE);
            }
            _ => {
                log::info!("settings: no file found, using defaults");
            }
        }

        self.loaded = true;
    }

    fn save(&self, k: &mut KernelHandle<'_>) -> bool {
        let mut buf = [0u8; 512];
        let len = write_settings_txt(&self.settings, &self.wifi, &mut buf);
        match k.write_app_data(config::SETTINGS_FILE, &buf[..len]) {
            Ok(_) => {
                log::info!("settings: saved to {}", config::SETTINGS_FILE);
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
                let _ = write!(
                    buf,
                    "{}",
                    fonts::font_size_name(self.settings.book_font_size_idx)
                );
            }
            3 => {
                let _ = write!(
                    buf,
                    "{}",
                    fonts::font_size_name(self.settings.ui_font_size_idx)
                );
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
                    self.settings.ghost_clear_every.saturating_add(5).min(100);
            }
            2 => {
                if self.settings.book_font_size_idx < max_size_idx() {
                    self.settings.book_font_size_idx += 1;
                }
            }
            3 => {
                if self.settings.ui_font_size_idx < max_size_idx() {
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
                    self.settings.ghost_clear_every.saturating_sub(5).max(5);
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

impl App<AppId> for SettingsApp {
    fn on_enter(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        self.selected = 0;
        self.save_needed = false;
        ctx.mark_dirty(Region::new(
            0,
            CONTENT_TOP,
            SCREEN_W,
            SCREEN_H - CONTENT_TOP,
        ));
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

    async fn background(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        if !self.loaded {
            self.load(k);
            ctx.request_full_redraw();
            return;
        }

        if self.save_needed {
            if self.save(k) {
                self.save_needed = false;
            }
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let title_region = Region::new(16, CONTENT_TOP + 4, 448, self.ui_fonts.heading.line_height);
        BitmapLabel::new(title_region, "Settings", self.ui_fonts.heading)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if !self.loaded {
            let r = Region::new(LABEL_X, self.items_top, 200, ROW_H);
            BitmapLabel::new(r, "Loading...", self.ui_fonts.body)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        let mut val_buf = StackFmt::<20>::new();

        for i in 0..NUM_ITEMS {
            let selected = i == self.selected;

            BitmapLabel::new(
                self.label_region(i),
                Self::item_label(i),
                self.ui_fonts.body,
            )
            .alignment(Alignment::CenterLeft)
            .inverted(selected)
            .draw(strip)
            .unwrap();

            self.format_value(i, &mut val_buf);
            BitmapLabel::new(self.value_region(i), val_buf.as_str(), self.ui_fonts.body)
                .alignment(Alignment::Center)
                .inverted(selected)
                .draw(strip)
                .unwrap();
        }
    }
}
