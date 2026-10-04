// SD card bring-up for the OnePage C61 (R6 permit, R9 recoverable errors).
//
// All decisions (retry policy, failure classification, card-detect debounce
// and polarity, storage status) are in pulp_board_logic::sd, host-tested; this
// file maps them onto esp-hal GPIO28 and embedded-sdmmc.
//
// UI hand-off: a failed bring-up yields `SdStorage::empty()`. The existing
// storage layer then reports `ErrorKind::NoCard` from `storage::borrow` for
// every call (`probe_ok()` is false), which is the path the X4 Files app
// already renders (`FilesApp::load_failed(Error)`). `StorageHealth` adds the
// finer status/message ("SD: no card", ...) for the UI to show; wiring it into
// the app layer belongs to the C61 app-lifecycle task (T12). Runtime
// read/write errors from `drivers::storage` are classified by
// `observe_storage_result`.
//
// Not verified on hardware: SD init, card-detect polarity (BSP :419 only
// "assumes" low = inserted), 10 MHz operation.

use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Pull},
    peripherals::GPIO28,
};
use log::{info, warn};
use pulp_board_logic::{
    power::PeripheralPower,
    sd::{
        CardState, SdFault, SdInitReport, SdProbe, StorageStatus, card_state_from_level, init_sd,
    },
};

// re-exported so binaries do not need a direct dependency on the logic crate
pub use pulp_board_logic::sd::{CD_SAMPLE_INTERVAL_MS, CardDetect, CardEvent, StorageHealth};

use super::{
    power::{Gpio27Rail, HalDelay},
    spi::SdSpiDevice,
};
use crate::drivers::sdcard::{SdStorage, SyncSdCard};
use crate::error::{Error, ErrorKind};

/// GPIO28 card-detect switch, input with internal pull-up (board_c61.c:59,
/// :282-285). Polarity: `pulp_board_logic::sd::CD_PRESENT_WHEN_LOW`.
pub struct CardDetectPin(Input<'static>);

impl CardDetectPin {
    pub fn new(gpio28: GPIO28<'static>) -> Self {
        Self(Input::new(
            gpio28,
            InputConfig::default().with_pull(Pull::Up),
        ))
    }

    /// Raw level for `pulp_board_logic::sd::CardDetect::sample`.
    pub fn level_high(&self) -> bool {
        self.0.is_high()
    }

    pub fn state(&self) -> CardState {
        card_state_from_level(self.level_high())
    }
}

/// embedded-sdmmc `SdCard` seen through the HAL-free `SdProbe` trait. Init is
/// lazy in embedded-sdmmc, so `num_bytes()` forces and verifies it (same as the
/// X4 `SdStorage::init_card`).
struct CardProbe<'a>(&'a SyncSdCard);

impl SdProbe for CardProbe<'_> {
    fn probe(&mut self) -> Option<u64> {
        match self.0.num_bytes() {
            Ok(size) => Some(size),
            Err(e) => {
                info!("sd: probe failed: {:?}", e);
                None
            }
        }
    }

    fn mark_uninit(&mut self) {
        self.0.mark_card_uninit();
    }
}

/// SD init through the GPIO27 state machine: `init_sd` takes the
/// `SdInitPermit` (power-cycle must have run), retries, and reports back.
/// On failure no card handle is kept; call again after a card-detect insert
/// event with a clone of the same `SdSpiDevice`.
pub fn init(
    power: &mut PeripheralPower<Gpio27Rail>,
    card_detect: CardState,
    spi: SdSpiDevice,
) -> Result<(SyncSdCard, SdInitReport), SdFault> {
    let card = SyncSdCard::new(spi, Delay::new());
    let report = init_sd(
        power,
        card_detect,
        &mut CardProbe(&card),
        &mut HalDelay::new(),
    )?;
    info!(
        "sd: {} bytes ({} MB), attempt {}",
        report.bytes,
        report.bytes / 1024 / 1024,
        report.attempts
    );
    Ok((card, report))
}

pub struct StorageBringUp {
    /// Mounted volume, or `SdStorage::empty()` after any failure.
    pub storage: SdStorage,
    pub health: StorageHealth,
}

/// Init + mount, never panics: every failure becomes `StorageHealth` status
/// plus an empty `SdStorage`.
pub async fn bring_up(
    power: &mut PeripheralPower<Gpio27Rail>,
    card_detect: CardState,
    spi: SdSpiDevice,
) -> StorageBringUp {
    let mut health = StorageHealth::new();
    let (card, _report) = match init(power, card_detect, spi) {
        Ok(v) => v,
        Err(fault) => {
            warn!("sd: init failed: {:?}", fault);
            health.on_fault(fault);
            return StorageBringUp {
                storage: SdStorage::empty(),
                health,
            };
        }
    };
    let storage = SdStorage::mount(card).await;
    if storage.probe_ok() {
        health.on_success();
    } else {
        warn!("sd: card answered but volume mount failed");
        health.on_fault(SdFault::MountFailed);
    }
    StorageBringUp { storage, health }
}

/// Fault -> the kernel's existing storage `Error` (same kinds the Files app
/// and `storage::borrow` already use).
pub fn fault_to_error(fault: SdFault) -> Error {
    match fault {
        SdFault::NoCard => Error::new(ErrorKind::NoCard, "sd: no card"),
        SdFault::InitFailed => Error::new(ErrorKind::NoCard, "sd: init failed"),
        SdFault::PowerNotReady(_) => Error::new(ErrorKind::NoCard, "sd: power not ready"),
        SdFault::MountFailed => Error::new(ErrorKind::OpenVolume, "sd: mount failed"),
        SdFault::ReadFailed => Error::new(ErrorKind::ReadFailed, "sd: read"),
        SdFault::WriteFailed => Error::new(ErrorKind::WriteFailed, "sd: write"),
    }
}

/// Which card-level fault (if any) a storage `Error` represents. File-level
/// errors (not found, open file/dir) are not card faults.
pub fn fault_from_error(e: &Error) -> Option<SdFault> {
    match e.kind() {
        ErrorKind::NoCard => Some(SdFault::NoCard),
        ErrorKind::OpenVolume => Some(SdFault::MountFailed),
        ErrorKind::ReadFailed | ErrorKind::SeekFailed => Some(SdFault::ReadFailed),
        ErrorKind::WriteFailed | ErrorKind::DeleteFailed | ErrorKind::DirFull => {
            Some(SdFault::WriteFailed)
        }
        _ => None,
    }
}

/// Feed a `drivers::storage` result into the health tracker: card-level
/// failures set an error status, success clears a previous I/O error.
pub fn observe_storage_result<T>(health: &mut StorageHealth, r: &crate::Result<T>) {
    match r {
        Err(e) => {
            if let Some(f) = fault_from_error(e) {
                health.on_fault(f);
            }
        }
        Ok(_) => {
            if health.status() == StorageStatus::IoError {
                health.on_success();
            }
        }
    }
}
