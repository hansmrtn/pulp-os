// selectable row widget: inverted selection highlight for list items
//
// consolidates the repeated pattern of drawing a selected/unselected row
// with inverted colors. used by home, files, settings, reader TOC, and
// quick menu for consistent selection rendering.
// the widget draws the selection background and returns the foreground
// color to use for text, letting the caller handle the actual content.

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::drivers::strip::StripBuffer;
use crate::ui::Region;

#[inline]
pub fn draw_selection_if_visible(
    strip: &mut StripBuffer,
    region: Region,
    selected: bool,
) -> BinaryColor {
    if selected && region.intersects(strip.logical_window()) {
        region
            .to_rect()
            .into_styled(PrimitiveStyle::with_fill(BinaryColor::On))
            .draw(strip)
            .unwrap();
        BinaryColor::Off
    } else if selected {
        BinaryColor::Off
    } else {
        BinaryColor::On
    }
}
