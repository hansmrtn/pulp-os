// settings app UI; configuration types live in kernel::config
//
// settings items (6 total, all fit on one screen at default font):
//   0: Sleep After    – power management
//   1: Ghost Clear    – e-paper refresh interval
//   2: Book Font      – reading font size
//   3: UI Font        – chrome font size
//   4: Reading Theme  – Compact / Default / Relaxed / Spacious
//   5: Swap Buttons   – swap Back/OK with Left/Right for left-handed use

use core::fmt::Write as _;

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::max_size_idx;
use crate::kernel::KernelHandle;
use crate::kernel::config::{
    self, GHOST_CLEAR_STEP, MAX_GHOST_CLEAR, MAX_SLEEP_TIMEOUT, MIN_GHOST_CLEAR,
    NUM_READING_THEMES, SLEEP_TIMEOUT_STEP, SystemSettings, WifiConfig, parse_settings_txt,
    reading_theme, write_settings_txt,
};
use crate::ui::{
    Alignment, BUTTON_BAR_H, BitmapLabel, CONTENT_TOP, FULL_CONTENT_W, LARGE_MARGIN, Region,
    SECTION_GAP, StackFmt, TITLE_Y, wrap_next, wrap_prev,
};

#[inline]
fn adjust_idx(v: &mut u8, up: bool, max: u8) {
    if up {
        if *v < max {
            *v += 1;
        }
    } else if *v > 0 {
        *v -= 1;
    }
}

// layout constants
const ROW_H: u16 = 40;
const ROW_GAP: u16 = 6;
const ROW_STRIDE: u16 = ROW_H + ROW_GAP;

const LABEL_X: u16 = LARGE_MARGIN;
const LABEL_W: u16 = 160;
const COL_GAP: u16 = 8;
const VALUE_X: u16 = LABEL_X + LABEL_W + COL_GAP;
const VALUE_W: u16 = FULL_CONTENT_W - LABEL_W - COL_GAP;

const NUM_ITEMS: usize = 6;
const HEADING_ITEMS_GAP: u16 = SECTION_GAP;

impl Default for SettingsApp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SettingsApp {
    settings: SystemSettings,
    wifi: WifiConfig,
    selected: usize,
    scroll: usize,
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
            scroll: 0,
            loaded: false,
            save_needed: false,
            ui_fonts: uf,
            items_top: TITLE_Y + uf.heading.line_height + HEADING_ITEMS_GAP,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.ui_fonts = fonts::UiFonts::for_size(idx);
        self.items_top = TITLE_Y + self.ui_fonts.heading.line_height + HEADING_ITEMS_GAP;
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

    // visible items: how many rows fit between items_top and BUTTON_BAR_H
    fn visible_items(&self) -> usize {
        let avail = SCREEN_H.saturating_sub(self.items_top + BUTTON_BAR_H);
        let count = (avail / ROW_STRIDE) as usize;
        count.clamp(1, NUM_ITEMS)
    }

    // item labels and values:

    fn item_label(i: usize) -> &'static str {
        match i {
            0 => "Sleep After",
            1 => "Ghost Clear",
            2 => "Book Font",
            3 => "UI Font",
            4 => "Theme",
            5 => "Swap Buttons",
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
            4 => {
                let theme = reading_theme(self.settings.reading_theme);
                let _ = write!(buf, "{}", theme.name);
            }
            5 => {
                let _ = write!(
                    buf,
                    "{}",
                    if self.settings.swap_buttons {
                        "Yes"
                    } else {
                        "No"
                    }
                );
            }
            _ => {}
        }
    }

    fn adjust(&mut self, up: bool) {
        match self.selected {
            0 => {
                self.settings.sleep_timeout = if up {
                    match self.settings.sleep_timeout {
                        0 => SLEEP_TIMEOUT_STEP,
                        t if t >= MAX_SLEEP_TIMEOUT => MAX_SLEEP_TIMEOUT,
                        t => t + SLEEP_TIMEOUT_STEP,
                    }
                } else {
                    match self.settings.sleep_timeout {
                        t if t <= SLEEP_TIMEOUT_STEP => 0,
                        t => t - SLEEP_TIMEOUT_STEP,
                    }
                };
            }
            1 => {
                self.settings.ghost_clear_every = if up {
                    self.settings
                        .ghost_clear_every
                        .saturating_add(GHOST_CLEAR_STEP)
                        .min(MAX_GHOST_CLEAR)
                } else {
                    self.settings
                        .ghost_clear_every
                        .saturating_sub(GHOST_CLEAR_STEP)
                        .max(MIN_GHOST_CLEAR)
                };
            }
            2 => adjust_idx(&mut self.settings.book_font_size_idx, up, max_size_idx()),
            3 => adjust_idx(&mut self.settings.ui_font_size_idx, up, max_size_idx()),
            4 => adjust_idx(&mut self.settings.reading_theme, up, NUM_READING_THEMES - 1),
            5 => {
                self.settings.swap_buttons = !self.settings.swap_buttons;
            }
            _ => return,
        }
        self.save_needed = true;
    }

    // scroll management:

    fn scroll_into_view(&mut self) {
        let vis = self.visible_items();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + vis {
            self.scroll = self.selected + 1 - vis;
        }
    }

    // row region helpers (visible_idx = position on screen, 0 = first visible):

    #[inline]
    fn label_region(&self, visible_idx: usize) -> Region {
        Region::new(
            LABEL_X,
            self.items_top + visible_idx as u16 * ROW_STRIDE,
            LABEL_W,
            ROW_H,
        )
    }

    #[inline]
    fn value_region(&self, visible_idx: usize) -> Region {
        Region::new(
            VALUE_X,
            self.items_top + visible_idx as u16 * ROW_STRIDE,
            VALUE_W,
            ROW_H,
        )
    }

    #[inline]
    fn row_region(&self, visible_idx: usize) -> Region {
        Region::new(
            LABEL_X,
            self.items_top + visible_idx as u16 * ROW_STRIDE,
            LABEL_W + COL_GAP + VALUE_W,
            ROW_H,
        )
    }

    fn list_region(&self) -> Region {
        let vis = self.visible_items();
        Region::new(
            LABEL_X,
            self.items_top,
            LABEL_W + COL_GAP + VALUE_W,
            vis as u16 * ROW_STRIDE,
        )
    }
}

impl App<AppId> for SettingsApp {
    fn on_enter(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        self.selected = 0;
        self.scroll = 0;
        self.save_needed = false;
        ctx.mark_dirty(Region::new(
            0,
            CONTENT_TOP,
            SCREEN_W,
            SCREEN_H - CONTENT_TOP,
        ));
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        let vis = self.visible_items();

        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            ActionEvent::Press(Action::Next) | ActionEvent::Press(Action::Prev) => {
                let forward = matches!(event, ActionEvent::Press(Action::Next));
                let old_selected = self.selected;
                let old_scroll = self.scroll;
                self.selected = if forward {
                    wrap_next(self.selected, NUM_ITEMS)
                } else {
                    wrap_prev(self.selected, NUM_ITEMS)
                };
                // on wrap-around, snap scroll to the target end
                if forward && self.selected < old_selected {
                    self.scroll = 0;
                } else if !forward && self.selected > old_selected {
                    self.scroll = NUM_ITEMS.saturating_sub(vis);
                } else {
                    self.scroll_into_view();
                }
                if self.scroll != old_scroll {
                    ctx.mark_dirty(self.list_region());
                } else if self.selected != old_selected {
                    ctx.mark_dirty(self.row_region(old_selected - old_scroll));
                    ctx.mark_dirty(self.row_region(self.selected - self.scroll));
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) | ActionEvent::Repeat(Action::NextJump) => {
                self.adjust(true);
                ctx.mark_dirty(self.value_region(self.selected - self.scroll));
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) | ActionEvent::Repeat(Action::PrevJump) => {
                self.adjust(false);
                ctx.mark_dirty(self.value_region(self.selected - self.scroll));
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

        if self.save_needed && self.save(k) {
            self.save_needed = false;
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        // heading
        let title_region = Region::new(
            LARGE_MARGIN,
            TITLE_Y,
            FULL_CONTENT_W,
            self.ui_fonts.heading.line_height,
        );
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

        // draw visible settings rows
        let vis = self.visible_items();
        let visible_count = vis.min(NUM_ITEMS - self.scroll);
        let mut val_buf = StackFmt::<20>::new();

        for vi in 0..visible_count {
            let item_idx = self.scroll + vi;
            let selected = item_idx == self.selected;

            BitmapLabel::new(
                self.label_region(vi),
                Self::item_label(item_idx),
                self.ui_fonts.body,
            )
            .alignment(Alignment::CenterLeft)
            .inverted(selected)
            .draw(strip)
            .unwrap();

            self.format_value(item_idx, &mut val_buf);
            BitmapLabel::new(self.value_region(vi), val_buf.as_str(), self.ui_fonts.body)
                .alignment(Alignment::Center)
                .inverted(selected)
                .draw(strip)
                .unwrap();
        }
    }
}
