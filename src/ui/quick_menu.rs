// Quick-action overlay — summoned by Power (Menu action) from any app.
//
// Renders over the bottom portion of the current screen using a
// partial refresh.  Provides instant access to the most common
// actions without leaving the current app.
//
// Items:
//   Book Font     — cycle Small / Medium / Large
//   Refresh       — force a full GC display refresh to clear ghosting
//   Go Home       — dismiss and navigate to HomeApp
//
// Navigation while open:
//   Prev / Next          — move selection between rows
//   PrevJump / NextJump  — decrement / increment the selected value
//   Select               — activate the selected action row
//   Menu or Back         — dismiss overlay, sync values to settings

use core::fmt::Write as _;

use embedded_graphics::{pixelcolor::BinaryColor, prelude::*, primitives::PrimitiveStyle};

use super::bitmap_label::{BitmapDynLabel, BitmapLabel};
use super::widget::{Alignment, Region};
use crate::board::action::Action;
use crate::drivers::strip::StripBuffer;
use crate::fonts::font_data;

// ── Layout constants ──────────────────────────────────────────────

const OVERLAY_W: u16 = 400;
const OVERLAY_X: u16 = (480 - OVERLAY_W) / 2; // centered horizontally
const OVERLAY_BOTTOM: u16 = 790; // 10px from screen bottom
const ITEM_H: u16 = 40;
const ITEM_GAP: u16 = 4;
const ITEM_STRIDE: u16 = ITEM_H + ITEM_GAP;
const BORDER: u16 = 2;
const PAD_TOP: u16 = 10;
const PAD_BOTTOM: u16 = 8;
const LABEL_X: u16 = OVERLAY_X + 16;
const LABEL_W: u16 = 150;
const VALUE_X: u16 = LABEL_X + LABEL_W + 8;
const VALUE_W: u16 = OVERLAY_W - 16 - LABEL_W - 8 - 16;
const HELP_H: u16 = 20;

/// Items shown in the quick menu.
const NUM_ITEMS: usize = 3;

/// Indices into the item list.
const IDX_FONT_SIZE: usize = 0;
const IDX_REFRESH: usize = 1;
const IDX_HOME: usize = 2;

/// Height of the full overlay including border and padding.
const CONTENT_H: u16 = PAD_TOP + (ITEM_STRIDE * NUM_ITEMS as u16) + HELP_H + PAD_BOTTOM;
const OVERLAY_H: u16 = CONTENT_H + BORDER * 2;
const OVERLAY_Y: u16 = OVERLAY_BOTTOM - OVERLAY_H;

/// The region the overlay covers — used for dirty marking.
pub const OVERLAY_REGION: Region = Region::new(OVERLAY_X, OVERLAY_Y, OVERLAY_W, OVERLAY_H);

/// Snapshot of the settings values the overlay can modify.
#[derive(Clone, Copy)]
pub struct QuickMenuValues {
    pub book_font_size_idx: u8,
}

/// Result of quick menu interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickMenuResult {
    /// Overlay consumed the event, stay open.
    Consumed,
    /// User dismissed the overlay; values may have changed.
    Close,
    /// User chose "Refresh" — force a full GC display refresh.
    RefreshScreen,
    /// User chose "Go Home" — navigate to home screen.
    GoHome,
}

pub struct QuickMenu {
    pub open: bool,
    selected: usize,
    pub values: QuickMenuValues,
    pub dirty: bool,
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
            selected: 0,
            values: QuickMenuValues {
                book_font_size_idx: 0,
            },
            dirty: false,
        }
    }

    /// Populate with current settings values and mark visible.
    pub fn show(&mut self, vals: QuickMenuValues) {
        self.values = vals;
        self.selected = 0;
        self.open = true;
        self.dirty = true;
    }

    pub fn hide(&mut self) {
        self.open = false;
        self.dirty = true;
    }

    /// The screen region occupied by the overlay (for dirty marking).
    pub fn region(&self) -> Region {
        OVERLAY_REGION
    }

    fn item_y(i: usize) -> u16 {
        OVERLAY_Y + BORDER + PAD_TOP + i as u16 * ITEM_STRIDE
    }

    fn item_label_region(i: usize) -> Region {
        Region::new(LABEL_X, Self::item_y(i), LABEL_W, ITEM_H)
    }

    fn item_value_region(i: usize) -> Region {
        Region::new(VALUE_X, Self::item_y(i), VALUE_W, ITEM_H)
    }

    fn help_region() -> Region {
        Region::new(
            OVERLAY_X + 12,
            OVERLAY_Y + BORDER + PAD_TOP + NUM_ITEMS as u16 * ITEM_STRIDE + 2,
            OVERLAY_W - 24,
            HELP_H,
        )
    }

    fn item_label(i: usize) -> &'static str {
        match i {
            IDX_FONT_SIZE => "Book Font",
            IDX_REFRESH => "Refresh",
            IDX_HOME => "Go Home",
            _ => "",
        }
    }

    fn format_value(&self, i: usize, buf: &mut BitmapDynLabel<16>) {
        buf.clear_text();
        match i {
            IDX_FONT_SIZE => {
                let s = match self.values.book_font_size_idx {
                    1 => "Medium",
                    2 => "Large",
                    _ => "Small",
                };
                let _ = write!(buf, "{}", s);
            }
            IDX_REFRESH => {
                let _ = write!(buf, "Clear ghost");
            }
            IDX_HOME => {
                let _ = write!(buf, ">>>");
            }
            _ => {}
        }
    }

    fn increment_font(&mut self) {
        if self.values.book_font_size_idx < 2 {
            self.values.book_font_size_idx += 1;
            self.dirty = true;
        }
    }

    fn decrement_font(&mut self) {
        if self.values.book_font_size_idx > 0 {
            self.values.book_font_size_idx -= 1;
            self.dirty = true;
        }
    }

    /// Handle an action while the overlay is open.
    /// Returns what the main loop should do.
    pub fn on_action(&mut self, action: Action) -> QuickMenuResult {
        match action {
            Action::Menu | Action::Back => {
                self.hide();
                QuickMenuResult::Close
            }
            Action::Next => {
                if self.selected + 1 < NUM_ITEMS {
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
                if self.selected == IDX_FONT_SIZE {
                    self.increment_font();
                }
                QuickMenuResult::Consumed
            }
            Action::PrevJump => {
                if self.selected == IDX_FONT_SIZE {
                    self.decrement_font();
                }
                QuickMenuResult::Consumed
            }
            Action::Select => match self.selected {
                IDX_REFRESH => {
                    self.hide();
                    QuickMenuResult::RefreshScreen
                }
                IDX_HOME => {
                    self.hide();
                    QuickMenuResult::GoHome
                }
                IDX_FONT_SIZE => {
                    // Cycle forward on Select for convenience
                    self.values.book_font_size_idx = (self.values.book_font_size_idx + 1) % 3;
                    self.dirty = true;
                    QuickMenuResult::Consumed
                }
                _ => QuickMenuResult::Consumed,
            },
        }
    }

    /// Draw the overlay into a strip buffer.  Should be called during
    /// any render pass (full or partial) that intersects OVERLAY_REGION
    /// while the menu is open.  Widgets outside the strip window are
    /// clipped automatically by StripBuffer.
    pub fn draw(&self, strip: &mut StripBuffer) {
        if !self.open {
            return;
        }

        // Always use the small body font for the overlay to keep it compact.
        let font = &font_data::REGULAR_BODY_SMALL;

        let outer = OVERLAY_REGION.to_rect();

        // White fill
        outer
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
            .draw(strip)
            .unwrap();
        // Black border
        outer
            .into_styled(PrimitiveStyle::with_stroke(BinaryColor::On, BORDER as u32))
            .draw(strip)
            .unwrap();

        let mut val_buf = BitmapDynLabel::<16>::new(Region::new(0, 0, 1, 1), font);

        for i in 0..NUM_ITEMS {
            let selected = i == self.selected;

            BitmapLabel::new(Self::item_label_region(i), Self::item_label(i), font)
                .alignment(Alignment::CenterLeft)
                .inverted(selected)
                .draw(strip)
                .unwrap();

            self.format_value(i, &mut val_buf);
            BitmapLabel::new(Self::item_value_region(i), val_buf.text(), font)
                .alignment(Alignment::Center)
                .inverted(selected)
                .draw(strip)
                .unwrap();
        }

        BitmapLabel::new(
            Self::help_region(),
            "Prev/Next  Jump: adjust  Sel: act  Menu: close",
            font,
        )
        .alignment(Alignment::Center)
        .draw(strip)
        .unwrap();
    }
}
