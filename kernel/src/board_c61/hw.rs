// What the kernel owns on the OnePage C61 besides the display and the SD volume
// (T12). The X4 kernel gets these through globals in `board/` (SPI bus mutex,
// SD CS clone, power-button static); on the C61 they are plain fields of
// `Kernel`, handed over once by the firmware entry (`src/bin/main_c61.rs`), so
// there is exactly one owner of each piece and no `static mut`.
//
//   power    GPIO27 state machine: display re-init token, session gate, sleep
//   battery  GPIO5 monitor with the charge-pause contract (T9). Owned here and
//            measured from the housekeeping poll (`Kernel::poll_housekeeping`),
//            not from a task, because the sleep sequence needs `&mut` access
//            too (charge restore) and one measurement blocks ~30 ms + 16
//            conversions: a task would need a lock held across the block
//   card     GPIO28 detect + debounce + health + the SD SPI handle for re-init
//   sleep    the three hardware parts of the T11 sequence
//   display  recovery bookkeeping of a failed full refresh

use pulp_board_logic::lifecycle::{BATTERY_INTERVAL_MS, DisplayHealth, Periodic};
use pulp_board_logic::power::PeripheralPower;

use super::battery::C61Battery;
use super::power::Gpio27Rail;
use super::sd::{CardDetect, CardDetectPin, StorageHealth};
use super::sleep::{C61Lines, C61SleepEntry, WakeKey};
use super::spi::{SdSpiDevice, SpiControl};

/// Card detect: raw pin, debouncer, health tracker, and the SPI handle a
/// re-inserted card is initialised with.
pub struct CardHw {
    pub pin: CardDetectPin,
    pub detect: CardDetect,
    pub health: StorageHealth,
    pub spi: SdSpiDevice,
    /// Bus clock control: a card inserted at runtime is initialised at the
    /// 400 kHz probe clock again.
    pub control: SpiControl,
}

/// The parts the T11 sequence consumes by `&mut` (kept so an aborted sleep
/// leaves the device fully usable and a later idle timeout can try again).
pub struct SleepParts {
    pub lines: C61Lines,
    pub wake: WakeKey,
    pub entry: C61SleepEntry,
}

pub struct C61Hw {
    pub power: PeripheralPower<Gpio27Rail>,
    pub battery: Option<C61Battery>,
    pub card: CardHw,
    pub sleep: SleepParts,
    pub display: DisplayHealth,
    /// `wake_count` of the restored session (0 on a normal boot); the next
    /// saved session carries this + 1.
    pub wake_count: u32,
    pub battery_due: Periodic,
    pub card_due: Periodic,
}

impl C61Hw {
    /// `now_ms`: monotonic milliseconds at construction (the first battery
    /// measurement was already taken at boot by the caller).
    pub fn new(
        power: PeripheralPower<Gpio27Rail>,
        battery: Option<C61Battery>,
        card: CardHw,
        sleep: SleepParts,
        now_ms: u64,
    ) -> Self {
        Self {
            power,
            battery,
            card,
            sleep,
            display: DisplayHealth::new(),
            wake_count: 0,
            battery_due: Periodic::new(BATTERY_INTERVAL_MS, now_ms),
            card_due: Periodic::new(super::sd::CD_SAMPLE_INTERVAL_MS as u64, now_ms),
        }
    }
}
