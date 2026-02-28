// Quick-action overlay — summoned by Power (Menu) from any app
//
// Core actions (Refresh, Go Home) always present; apps inject up to
// MAX_APP_ACTIONS items above. Two kinds: Cycle (rotate options)
// and Trigger (fire on Select). Menu/Back dismisses.
//
// Rendering: plain inverted-text rows, no borders or separators.

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle};

use super::bitmap_label::BitmapDynLabel;
use super::widget::{Alignment, Region};
use crate::board::action::Action;
use crate::drivers::strip::StripBuffer;
use crate::fonts::font_data;

// ── Layout constants ──────────────────────────────────────────────

const OVERLAY_W: u16 = 400;
const OVERLAY_X: u16 = (480 - OVERLAY_W) / 2;
const OVERLAY_BOTTOM: u16 = 790; // 10px from screen bottom
const ITEM_H: u16 = 40;
const ITEM_GAP: u16 = 4;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;
const PAD_TOP: u16 = 10;
const PAD_BOTTOM: u16 = 8;
const LABEL_X: u16 = OVERLAY_X + 16;
const LABEL_W: u16 = 150;
const VALUE_X: u16 = LABEL_X + LABEL_W + 8;
const VALUE_W: u16 = OVERLAY_W - 16 - LABEL_W - 8 - 16;
const HELP_H: u16 = 20;

pub const MAX_APP_ACTIONS: usize = 6;

const NUM_CORE: usize = 2; // Refresh + Go Home
const MAX_ITEMS: usize = MAX_APP_ACTIONS + NUM_CORE;

// ── Public types ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum QuickActionKind {
    // rotates through named options; value is the current index
    Cycle {
        value: u8,
        options: &'static [&'static str],
    },
    // fires immediately on Select; display is the right-column label
    Trigger {
        display: &'static str,
    },
}

// app-provided quick action descriptor; id echoed in AppTrigger
#[derive(Debug, Clone, Copy)]
pub struct QuickAction {
    pub id: u8,
    pub label: &'static str,
    pub kind: QuickActionKind,
}

impl QuickAction {
    pub const fn cycle(
        id: u8,
        label: &'static str,
        value: u8,
        options: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            label,
            kind: QuickActionKind::Cycle { value, options },
        }
    }

    pub const fn trigger(id: u8, label: &'static str, display: &'static str) -> Self {
        Self {
            id,
            label,
            kind: QuickActionKind::Trigger { display },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickMenuResult {
    Consumed,
    Close,
    RefreshScreen,
    GoHome,
    AppTrigger(u8),
}

// ── Internal item representation ──────────────────────────────────

#[derive(Clone, Copy)]
enum MenuItemKind {
    AppCycle {
        id: u8,
        value: u8,
        options: &'static [&'static str],
    },
    AppTrigger {
        id: u8,
        display: &'static str,
    },
    CoreRefresh,
    CoreHome,
}

#[derive(Clone, Copy)]
struct MenuItem {
    label: &'static str,
    kind: MenuItemKind,
}

impl MenuItem {
    const EMPTY: Self = Self {
        label: "",
        kind: MenuItemKind::CoreRefresh,
    };
}

// ── QuickMenu ─────────────────────────────────────────────────────

pub struct QuickMenu {
    pub open: bool,
    items: [MenuItem; MAX_ITEMS],
    count: usize,
    app_count: usize,
    selected: usize,
    pub dirty: bool,
    overlay_region: Region,
}

impl Default for QuickMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickMenu {
    pub const fn new() -> Self {
        Self {
            open: false,
            items: [MenuItem::EMPTY; MAX_ITEMS],
            count: 0,
            app_count: 0,
            selected: 0,
            dirty: false,
            overlay_region: Region::new(0, 0, 0, 0),
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────

    // open overlay with app-provided items; core items appended automatically
    pub fn show(&mut self, app_actions: &[QuickAction]) {
        let n_app = app_actions.len().min(MAX_APP_ACTIONS);
        self.app_count = n_app;

        for (i, a) in app_actions.iter().enumerate().take(n_app) {
            self.items[i] = MenuItem {
                label: a.label,
                kind: match a.kind {
                    QuickActionKind::Cycle { value, options } => MenuItemKind::AppCycle {
                        id: a.id,
                        value,
                        options,
                    },
                    QuickActionKind::Trigger { display } => {
                        MenuItemKind::AppTrigger { id: a.id, display }
                    }
                },
            };
        }

        self.items[n_app] = MenuItem {
            label: "Refresh",
            kind: MenuItemKind::CoreRefresh,
        };
        self.items[n_app + 1] = MenuItem {
            label: "Go Home",
            kind: MenuItemKind::CoreHome,
        };

        self.count = n_app + NUM_CORE;
        self.selected = 0;
        self.open = true;
        self.dirty = true;
        self.overlay_region = Self::compute_region(self.count);
    }

    pub fn hide(&mut self) {
        self.open = false;
        self.dirty = true;
    }

    pub fn region(&self) -> Region {
        self.overlay_region
    }

    pub fn app_cycle_value(&self, id: u8) -> Option<u8> {
        for i in 0..self.app_count {
            if let MenuItemKind::AppCycle {
                id: item_id, value, ..
            } = self.items[i].kind
                && item_id == id
            {
                return Some(value);
            }
        }
        None
    }

    // ── Input handling ────────────────────────────────────────────

    pub fn on_action(&mut self, action: Action) -> QuickMenuResult {
        match action {
            Action::Menu | Action::Back => {
                self.hide();
                QuickMenuResult::Close
            }

            Action::Next => {
                if self.selected + 1 < self.count {
                    self.selected += 1;
                    self.dirty = true;
                }
                QuickMenuResult::Consumed
            }

            Action::Prev => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.dirty = true;
                }
                QuickMenuResult::Consumed
            }

            Action::NextJump => {
                self.adjust_selected(1);
                QuickMenuResult::Consumed
            }

            Action::PrevJump => {
                self.adjust_selected(-1);
                QuickMenuResult::Consumed
            }

            Action::Select => self.activate_selected(),
        }
    }

    // no-op on Trigger / core items
    fn adjust_selected(&mut self, delta: i8) {
        let item = &mut self.items[self.selected];
        if let MenuItemKind::AppCycle {
            ref mut value,
            options,
            ..
        } = item.kind
        {
            let max = options.len().saturating_sub(1) as u8;
            if delta > 0 && *value < max {
                *value += 1;
                self.dirty = true;
            } else if delta < 0 && *value > 0 {
                *value -= 1;
                self.dirty = true;
            }
        }
    }

    // Cycle: advance value; Trigger/core: fire and close
    fn activate_selected(&mut self) -> QuickMenuResult {
        match &mut self.items[self.selected].kind {
            MenuItemKind::AppCycle { value, options, .. } => {
                let len = options.len() as u8;
                if len > 0 {
                    *value = (*value + 1) % len;
                    self.dirty = true;
                }
                QuickMenuResult::Consumed
            }
            MenuItemKind::AppTrigger { id, .. } => {
                let id = *id;
                self.hide();
                QuickMenuResult::AppTrigger(id)
            }
            MenuItemKind::CoreRefresh => {
                self.hide();
                QuickMenuResult::RefreshScreen
            }
            MenuItemKind::CoreHome => {
                self.hide();
                QuickMenuResult::GoHome
            }
        }
    }

    // ── Layout helpers ────────────────────────────────────────────

    fn compute_region(total_items: usize) -> Region {
        let content_h = PAD_TOP + (ITEM_STRIDE * total_items as u16) + HELP_H + PAD_BOTTOM;
        let y = OVERLAY_BOTTOM - content_h;
        Region::new(OVERLAY_X, y, OVERLAY_W, content_h)
    }

    fn item_y(&self, i: usize) -> u16 {
        self.overlay_region.y + PAD_TOP + i as u16 * ITEM_STRIDE
    }

    fn item_label_region(&self, i: usize) -> Region {
        Region::new(LABEL_X, self.item_y(i), LABEL_W, ITEM_H)
    }

    fn item_value_region(&self, i: usize) -> Region {
        Region::new(VALUE_X, self.item_y(i), VALUE_W, ITEM_H)
    }

    fn help_region(&self) -> Region {
        let last = self.count.saturating_sub(1);
        let below_last = self.item_y(last) + ITEM_STRIDE + 2;
        Region::new(OVERLAY_X + 12, below_last, OVERLAY_W - 24, HELP_H)
    }

    fn format_value(&self, i: usize, buf: &mut BitmapDynLabel<20>) {
        buf.clear_text();
        match &self.items[i].kind {
            MenuItemKind::AppCycle { value, options, .. } => {
                let idx = *value as usize;
                let text = if idx < options.len() {
                    options[idx]
                } else {
                    "?"
                };
                let _ = core::fmt::Write::write_str(buf, text);
            }
            MenuItemKind::AppTrigger { display, .. } => {
                let _ = core::fmt::Write::write_str(buf, display);
            }
            MenuItemKind::CoreRefresh => {
                let _ = core::fmt::Write::write_str(buf, "Clear ghost");
            }
            MenuItemKind::CoreHome => {
                let _ = core::fmt::Write::write_str(buf, ">>>");
            }
        }
    }

    // ── Drawing ───────────────────────────────────────────────────

    pub fn draw(&self, strip: &mut StripBuffer) {
        if !self.open {
            return;
        }

        let font = &font_data::REGULAR_BODY_SMALL;

        // clear the overlay background
        let outer = self.overlay_region;
        if outer.intersects(strip.logical_window()) {
            outer
                .to_rect()
                .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                .draw(strip)
                .unwrap();
        }

        let mut val_buf = BitmapDynLabel::<20>::new(Region::new(0, 0, 1, 1), font);

        for i in 0..self.count {
            let selected = i == self.selected;

            let label_region = self.item_label_region(i);
            let value_region = self.item_value_region(i);

            // Selected row: inverted (white text on black)
            if selected {
                let row_region = Region::new(OVERLAY_X, self.item_y(i), OVERLAY_W, ITEM_H);
                if row_region.intersects(strip.logical_window()) {
                    row_region
                        .to_rect()
                        .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
                        .draw(strip)
                        .unwrap();
                }
            }

            // Draw label text
            if label_region.intersects(strip.logical_window()) {
                let fg = if selected {
                    BinaryColor::Off
                } else {
                    BinaryColor::On
                };
                let text = self.items[i].label;
                if !text.is_empty() {
                    let text_w = font.measure_str(text) as u32;
                    let text_h = font.line_height as u32;
                    let top_left =
                        Alignment::CenterLeft.position(label_region, Size::new(text_w, text_h));
                    let baseline = top_left.y + font.ascent as i32;
                    font.draw_str_fg(strip, text, fg, top_left.x, baseline);
                }
            }

            // Draw value text
            if value_region.intersects(strip.logical_window()) {
                self.format_value(i, &mut val_buf);
                let vtext = val_buf.text();
                if !vtext.is_empty() {
                    let fg = if selected {
                        BinaryColor::Off
                    } else {
                        BinaryColor::On
                    };
                    let text_w = font.measure_str(vtext) as u32;
                    let text_h = font.line_height as u32;
                    let top_left =
                        Alignment::Center.position(value_region, Size::new(text_w, text_h));
                    let baseline = top_left.y + font.ascent as i32;
                    font.draw_str_fg(strip, vtext, fg, top_left.x, baseline);
                }
            }
        }

        // Help text at the bottom
        let help = match &self.items[self.selected].kind {
            MenuItemKind::AppCycle { .. } => "Up/Down: move  Jump: adjust  Sel: cycle  Menu: close",
            _ => "Up/Down: move  Sel: activate  Menu: close",
        };

        let help_region = self.help_region();
        if help_region.intersects(strip.logical_window()) {
            let text_w = font.measure_str(help) as u32;
            let text_h = font.line_height as u32;
            let top_left = Alignment::Center.position(help_region, Size::new(text_w, text_h));
            let baseline = top_left.y + font.ascent as i32;
            font.draw_str_fg(strip, help, BinaryColor::On, top_left.x, baseline);
        }
    }
}
