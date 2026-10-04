// C61 session persistence adapter (R18, R20): `SessionStore` over the existing
// SD storage layer. Format, validation, slot policy, SD-active gating and all
// failure handling are in pulp_board_logic::session (host-tested, same code).
//
// Files: `_PULP/SESSA.BIN` and `_PULP/SESSB.BIN` (8.3 names, next to
// SETTINGS.TXT / BKMK.BIN). The `_PULP` directory is created at boot by
// `storage::ensure_pulp_dir_async` (as the X4 main does); if it is missing,
// reads report "not found" and saves report a write error, nothing panics.
//
// Error mapping is limited by `drivers::storage`: its file macros report every
// failed open as `OpenFile`/`OpenDir` (missing file and unreadable directory
// entry look the same), so those map to `NotFound`. Worst case a transient open
// failure of the newest slot makes a save pick that slot as its target; the
// other slot still holds a valid (one generation older) record.
//
// Not verified on hardware: any of these SD operations, truncate-then-write
// behaviour on power loss, FAT consistency, write latency and card wear.

use crate::drivers::{sdcard::SdStorage, storage};
use crate::error::ErrorKind;
use pulp_board_logic::session::{SessionStore, Slot, StoreError};

pub use pulp_board_logic::session::{
    BootDecision, NormalBootReason, READ_BUF_LEN, RECORD_LEN, Restored, SaveError, SaveReport,
    SessionState,
};

use super::power::Gpio27Rail;
use pulp_board_logic::power::PeripheralPower;

/// Maps a storage-layer error onto the store's recoverable error kinds.
pub fn map_error(kind: ErrorKind) -> StoreError {
    match kind {
        ErrorKind::NoCard => StoreError::NoCard,
        // the storage macros use these for "could not open" (incl. not there)
        ErrorKind::OpenFile | ErrorKind::OpenDir | ErrorKind::NotFound => StoreError::NotFound,
        _ => StoreError::Io,
    }
}

/// `SessionStore` over the mounted SD volume.
pub struct SdSessionStore<'a> {
    sd: &'a SdStorage,
}

impl<'a> SdSessionStore<'a> {
    pub fn new(sd: &'a SdStorage) -> Self {
        Self { sd }
    }
}

impl SessionStore for SdSessionStore<'_> {
    fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<usize, StoreError> {
        storage::read_chunk_in_pulp(self.sd, slot.file_name(), 0, buf)
            .map_err(|e| map_error(e.kind()))
    }

    fn write(&mut self, slot: Slot, data: &[u8]) -> Result<(), StoreError> {
        // a failed open of the directory/file on write is a write failure, not
        // "not found": there is nothing to look for
        storage::write_in_pulp(self.sd, slot.file_name(), data).map_err(|e| match e.kind() {
            ErrorKind::NoCard => StoreError::NoCard,
            _ => StoreError::Io,
        })
    }

    fn delete(&mut self, slot: Slot) -> Result<(), StoreError> {
        // delete of a missing file is not an error for the caller
        match storage::file_size_in_pulp(self.sd, slot.file_name()) {
            Ok(_) => {}
            Err(e) => {
                return match map_error(e.kind()) {
                    StoreError::NotFound => Ok(()),
                    other => Err(other),
                };
            }
        }
        storage::delete_in_pulp(self.sd, slot.file_name()).map_err(|e| match e.kind() {
            ErrorKind::NoCard => StoreError::NoCard,
            _ => StoreError::Io,
        })
    }
}

/// Save the session (R18). Refused unless `power` is `SdActive`: call it before
/// `begin_shutdown`, while the SD rail is still up. Failure is returned, not
/// handled: T11 decides whether to sleep anyway.
pub fn save_session(
    power: &PeripheralPower<Gpio27Rail>,
    sd: &SdStorage,
    state: &SessionState,
) -> Result<SaveReport, SaveError> {
    pulp_board_logic::session::save_session(power, &mut SdSessionStore::new(sd), state)
}

/// Boot decision (R20): call after SD init. `Restore` carries a validated
/// state; anything else means boot normally.
pub fn restore_session(power: &PeripheralPower<Gpio27Rail>, sd: &SdStorage) -> BootDecision {
    pulp_board_logic::session::restore_session(power, &mut SdSessionStore::new(sd))
}

/// Delete both slot files (for a one-shot restore policy chosen in T12).
pub fn clear_session(power: &PeripheralPower<Gpio27Rail>, sd: &SdStorage) -> Result<(), SaveError> {
    pulp_board_logic::session::clear_session(power, &mut SdSessionStore::new(sd))
}
