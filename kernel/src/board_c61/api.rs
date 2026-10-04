// Board-neutral surface of the OnePage C61 (T12).
//
// The kernel scheduler and the apps are written against `crate::board::*`
// (X4: kernel/src/board/). On the C61 `pulp_kernel::board` is this module, so
// the same paths resolve without touching the app sources:
//
//   board::{SCREEN_W, SCREEN_H}  logical portrait size (480 x 800)
//   board::Epd                   the full-refresh SSD1677 driver
//   board::button::Button        the physical key (C61 `Key`, no Power button)
//   board::action::ButtonMapper  key event -> semantic ActionEvent (swap setting)
//   board::layout::*             label slots of the front keys
//
// There is nothing new here that decides behaviour: the key -> Action table and
// the swap rule are pulp_board_logic::keys (host-tested since T7).

use pulp_board_logic::ssd1677::{HEIGHT, WIDTH};

// app-lifecycle decisions the apps layer needs (quick-menu mapping)
pub use pulp_board_logic::lifecycle;

pub use super::epd::Epd;
pub use crate::drivers::sdcard::{SdStorage, SyncSdCard};
pub use crate::drivers::strip::StripBuffer;

// logical screen size (portrait via 270-degree rotation of the 800x480 panel)
pub const SCREEN_W: u16 = HEIGHT; // 480
pub const SCREEN_H: u16 = WIDTH; // 800

pub mod button {
    /// Physical key: WAKE / PREV / NEXT (side) and BACK / LEFT / RIGHT / ENTER
    /// (front ladder). There is no Power key on the C61.
    pub use pulp_board_logic::keys::Key as Button;
}

pub mod action {
    use super::button::Button;
    use crate::drivers::input::Event;
    use pulp_board_logic::keys;

    pub use pulp_board_logic::action::{Action, ActionEvent};

    /// Key -> semantic action with the optional left-handed swap (front row
    /// Back<->Left, Enter<->Right; side keys and WAKE never swap). Same API as
    /// the X4 mapper so `AppManager` is shared.
    #[derive(Default)]
    pub struct ButtonMapper {
        swap_buttons: bool,
    }

    impl ButtonMapper {
        pub const fn new() -> Self {
            Self {
                swap_buttons: false,
            }
        }

        pub fn set_swap(&mut self, swap: bool) {
            self.swap_buttons = swap;
        }

        pub fn is_swapped(&self) -> bool {
            self.swap_buttons
        }

        pub fn map_button(&self, button: Button) -> Action {
            button.action(self.swap_buttons)
        }

        pub fn map_event(&self, event: Event) -> ActionEvent {
            keys::map_event(event, self.swap_buttons)
        }
    }
}

pub mod layout {
    // Bezel slots of the front keys (center-x on the bottom edge) and of the
    // side keys (center-y on the right edge) for the button label overlay. The
    // physical key order and the side-key positions on the OnePage bezel are
    // UNVERIFIED (the BSP only lists Back / Left / Right / Enter): the X4 slot
    // centres are reused, the spacing is equal anyway. T14 checks them on the
    // device; only the bottom row is drawn.
    pub const CX_BACK: u16 = 84;
    pub const CX_CONFIRM: u16 = 194;
    pub const CX_LEFT: u16 = 286;
    pub const CX_RIGHT: u16 = 396;
    pub const CY_VOL_UP: u16 = 364;
    pub const CY_VOL_DOWN: u16 = 484;
}
