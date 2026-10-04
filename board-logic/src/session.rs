// C61 session persistence on SD (R18, R20), HAL-free.
//
// The X4 keeps its session in RTC FAST memory (`kernel/src/kernel/rtc_session.rs`,
// `#[link_section = ".rtc_fast.persistent"]`). esp-hal 1.2 has no such section
// for the ESP32-C61 (proposal: "C61 無現有 HAL `.rtc_fast.persistent` 配置"), so
// the C61 keeps the same reading-position semantics in a file on the SD card.
// This module is the format, the validation and the failure policy; the kernel
// (`board_c61::session`) only maps `SessionStore` onto `drivers::storage`.
//
// Field mapping against the X4 `RtcSession` (names kept, see baseline.md T10):
//   X4 header magic "PLPS" (0x504C5053)  -> same constant, now little-endian u32
//   X4 wake_count                        -> `wake_count` (caller owned)
//   X4 nav_depth / nav_stack (<= 4)      -> same, validated (ids 0..=4, Home at bottom)
//   X4 reader_* (name <= 32, epub flag, chapter, page, byte offset, font size)
//                                        -> same fields
//   X4 files_* / home_*                  -> same fields
//   X4 settings cache (sleep timeout, fonts, ...)  -> NOT stored: it only existed
//        to skip SD reads on wake; on C61 the session is already an SD read and
//        SETTINGS.TXT stays the single source of truth.
//   X4 `_pad`/`_reserved`                -> 3 reserved bytes that must be zero.
//
// File format (80 bytes, little-endian, fixed size; CRC-32/IEEE over bytes 0..76):
//   0  u32 magic 0x504C5053     4 u16 version (1)     6 u16 flags (0)
//   8  u32 seq                 12 u32 wake_count
//  16  u8  nav_depth (1..=4)   17 [u8;4] nav_stack     21 [u8;32] reader_filename
//  53  u8  reader_filename_len 54 u8 reader_is_epub    55 u16 reader_chapter
//  57  u16 reader_page         59 u32 reader_byte_offset  63 u8 reader_font_size
//  64  u16 files_scroll        66 u8 files_selected    67 u16 files_total
//  69  u8  home_state          70 u8 home_selected     71 u8 home_bm_selected
//  72  u8  home_bm_scroll      73 [u8;3] reserved = 0  76 u32 crc32
//
// Atomicity: embedded-sdmmc has no rename, and `write` truncates the file
// before writing, so a power cut leaves a short or half-written file. Two slot
// files (SESSA.BIN / SESSB.BIN) are therefore used: a save never touches the
// slot holding the newest valid record, it overwrites the other one with
// `seq + 1`. A torn write can only damage the slot that was being replaced; the
// CRC rejects it and the previous record is still restored. After writing, the
// slot is read back and decoded, so a write that silently went nowhere is
// reported as an error instead of being trusted.

use crate::power::{PeripheralPower, PowerError, RailPin, RailState};
use core::marker::PhantomData;

/// "PLPS" (PuLP Session), same value as the X4 `RTC_SESSION_MAGIC`.
pub const MAGIC: u32 = 0x504C_5053;
pub const VERSION: u16 = 1;
pub const RECORD_LEN: usize = 80;
/// One byte more than a record: reading this much tells "exactly 80" from
/// "longer than 80" without reading the whole file.
pub const READ_BUF_LEN: usize = RECORD_LEN + 1;
const CRC_OFFSET: usize = RECORD_LEN - 4;

/// X4 `MAX_NAV_STACK` / `app::MAX_STACK_DEPTH`.
pub const MAX_NAV_STACK: usize = 4;
/// X4 `MAX_FILENAME_LEN`.
pub const MAX_FILENAME_LEN: usize = 32;
/// App ids as in the X4 session: Home=0, Files=1, Reader=2, Settings=3, Upload=4.
pub const APP_HOME: u8 = 0;
pub const APP_READER: u8 = 2;
pub const APP_ID_MAX: u8 = 4;
/// `fonts::FONT_SIZE_NAMES` has 5 entries (XSmall..XLarge).
pub const FONT_SIZE_MAX: u8 = 4;
/// Home screen state: 0 = Menu, 1 = ShowBookmarks.
pub const HOME_STATE_MAX: u8 = 1;

/// Slot file names (8.3), in `_PULP/` next to SETTINGS.TXT / BKMK.BIN.
pub const SLOT_A_FILE: &str = "SESSA.BIN";
pub const SLOT_B_FILE: &str = "SESSB.BIN";

// ---------------------------------------------------------------------------
// CRC-32/IEEE (poly 0xEDB88320 reflected, init/xorout 0xFFFFFFFF)
// ---------------------------------------------------------------------------

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// state + record
// ---------------------------------------------------------------------------

/// The reading position and navigation state (X4 `RtcSession` payload fields).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SessionState {
    pub wake_count: u32,
    pub nav_depth: u8,
    pub nav_stack: [u8; MAX_NAV_STACK],
    pub reader_filename: [u8; MAX_FILENAME_LEN],
    pub reader_filename_len: u8,
    pub reader_is_epub: bool,
    pub reader_chapter: u16,
    pub reader_page: u16,
    pub reader_byte_offset: u32,
    pub reader_font_size: u8,
    pub files_scroll: u16,
    pub files_selected: u8,
    pub files_total: u16,
    pub home_state: u8,
    pub home_selected: u8,
    pub home_bm_selected: u8,
    pub home_bm_scroll: u8,
}

impl SessionState {
    /// Home menu only, no book: always valid.
    pub const fn home() -> Self {
        Self {
            wake_count: 0,
            nav_depth: 1,
            nav_stack: [APP_HOME; MAX_NAV_STACK],
            reader_filename: [0; MAX_FILENAME_LEN],
            reader_filename_len: 0,
            reader_is_epub: false,
            reader_chapter: 0,
            reader_page: 0,
            reader_byte_offset: 0,
            reader_font_size: 0,
            files_scroll: 0,
            files_selected: 0,
            files_total: 0,
            home_state: 0,
            home_selected: 0,
            home_bm_selected: 0,
            home_bm_scroll: 0,
        }
    }

    /// Set the reader file name (must fit `MAX_FILENAME_LEN`); the rest of the
    /// name buffer is zeroed.
    pub fn set_reader_filename(&mut self, name: &[u8]) -> Result<(), FieldError> {
        if name.len() > MAX_FILENAME_LEN {
            return Err(FieldError::FilenameLen);
        }
        self.reader_filename = [0; MAX_FILENAME_LEN];
        self.reader_filename[..name.len()].copy_from_slice(name);
        self.reader_filename_len = name.len() as u8;
        Ok(())
    }

    pub fn reader_name(&self) -> &[u8] {
        let n = (self.reader_filename_len as usize).min(MAX_FILENAME_LEN);
        &self.reader_filename[..n]
    }

    /// Value rules shared by `encode` (never write garbage) and `decode`
    /// (never accept garbage). Each rule corresponds to an X4 `apply_session`
    /// assumption or an array bound.
    pub fn validate(&self) -> Result<(), FieldError> {
        let depth = self.nav_depth as usize;
        if depth == 0 || depth > MAX_NAV_STACK {
            return Err(FieldError::NavDepth);
        }
        for (i, &id) in self.nav_stack.iter().enumerate() {
            if i < depth {
                if id > APP_ID_MAX {
                    return Err(FieldError::NavAppId);
                }
            } else if id != 0 {
                return Err(FieldError::NavStackTail);
            }
        }
        if self.nav_stack[0] != APP_HOME {
            return Err(FieldError::NavBottomNotHome);
        }
        let len = self.reader_filename_len as usize;
        if len > MAX_FILENAME_LEN {
            return Err(FieldError::FilenameLen);
        }
        for (i, &b) in self.reader_filename.iter().enumerate() {
            if i < len {
                // printable ASCII, no path separators or drive/wildcard chars
                if !(0x21..=0x7E).contains(&b) || matches!(b, b'/' | b'\\' | b':' | b'*' | b'?') {
                    return Err(FieldError::FilenameChar);
                }
            } else if b != 0 {
                return Err(FieldError::FilenameTail);
            }
        }
        if self.nav_stack[..depth].contains(&APP_READER) && len == 0 {
            return Err(FieldError::ReaderWithoutFile);
        }
        if self.reader_font_size > FONT_SIZE_MAX {
            return Err(FieldError::FontSize);
        }
        if self.home_state > HOME_STATE_MAX {
            return Err(FieldError::HomeState);
        }
        Ok(())
    }
}

/// A decoded file: sequence number plus state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub seq: u32,
    pub state: SessionState,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldError {
    NavDepth,
    NavAppId,
    NavStackTail,
    NavBottomNotHome,
    FilenameLen,
    FilenameChar,
    FilenameTail,
    ReaderWithoutFile,
    FontSize,
    HomeState,
    /// `reader_is_epub` byte other than 0/1.
    EpubFlag,
    /// Header `flags` or the reserved bytes are not zero.
    ReservedNotZero,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    TooShort {
        len: usize,
    },
    /// More than `RECORD_LEN` bytes (trailing data, or a different file).
    TooLong {
        len: usize,
    },
    BadMagic,
    BadVersion(u16),
    BadCrc,
    Field(FieldError),
}

fn put16(b: &mut [u8; RECORD_LEN], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn put32(b: &mut [u8; RECORD_LEN], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn get16(b: &[u8; RECORD_LEN], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn get32(b: &[u8; RECORD_LEN], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Serialise `state` as record number `seq`. Refuses a state that `decode`
/// would reject, so an invalid state can never reach the card.
pub fn encode(state: &SessionState, seq: u32) -> Result<[u8; RECORD_LEN], FieldError> {
    state.validate()?;
    let mut b = [0u8; RECORD_LEN];
    put32(&mut b, 0, MAGIC);
    put16(&mut b, 4, VERSION);
    // 6..8 flags = 0
    put32(&mut b, 8, seq);
    put32(&mut b, 12, state.wake_count);
    b[16] = state.nav_depth;
    b[17..21].copy_from_slice(&state.nav_stack);
    b[21..53].copy_from_slice(&state.reader_filename);
    b[53] = state.reader_filename_len;
    b[54] = state.reader_is_epub as u8;
    put16(&mut b, 55, state.reader_chapter);
    put16(&mut b, 57, state.reader_page);
    put32(&mut b, 59, state.reader_byte_offset);
    b[63] = state.reader_font_size;
    put16(&mut b, 64, state.files_scroll);
    b[66] = state.files_selected;
    put16(&mut b, 67, state.files_total);
    b[69] = state.home_state;
    b[70] = state.home_selected;
    b[71] = state.home_bm_selected;
    b[72] = state.home_bm_scroll;
    // 73..76 reserved = 0
    let crc = crc32(&b[..CRC_OFFSET]);
    put32(&mut b, CRC_OFFSET, crc);
    Ok(b)
}

/// Parse and fully validate a file image. Total over all inputs: never panics,
/// every malformed input maps to a `DecodeError`.
pub fn decode(bytes: &[u8]) -> Result<Record, DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::Empty);
    }
    if bytes.len() < RECORD_LEN {
        return Err(DecodeError::TooShort { len: bytes.len() });
    }
    let Ok(b) = <&[u8; RECORD_LEN]>::try_from(bytes) else {
        return Err(DecodeError::TooLong { len: bytes.len() });
    };
    if get32(b, 0) != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    let version = get16(b, 4);
    if version != VERSION {
        return Err(DecodeError::BadVersion(version));
    }
    if get32(b, CRC_OFFSET) != crc32(&b[..CRC_OFFSET]) {
        return Err(DecodeError::BadCrc);
    }
    // CRC passed: the remaining checks reject well-formed-but-impossible values
    // (a buggy writer, a different firmware, or a deliberate edit).
    if get16(b, 6) != 0 || b[73..76] != [0, 0, 0] {
        return Err(DecodeError::Field(FieldError::ReservedNotZero));
    }
    if b[54] > 1 {
        return Err(DecodeError::Field(FieldError::EpubFlag));
    }
    let mut state = SessionState {
        wake_count: get32(b, 12),
        nav_depth: b[16],
        nav_stack: [b[17], b[18], b[19], b[20]],
        reader_filename: [0; MAX_FILENAME_LEN],
        reader_filename_len: b[53],
        reader_is_epub: b[54] == 1,
        reader_chapter: get16(b, 55),
        reader_page: get16(b, 57),
        reader_byte_offset: get32(b, 59),
        reader_font_size: b[63],
        files_scroll: get16(b, 64),
        files_selected: b[66],
        files_total: get16(b, 67),
        home_state: b[69],
        home_selected: b[70],
        home_bm_selected: b[71],
        home_bm_scroll: b[72],
    };
    state.reader_filename.copy_from_slice(&b[21..53]);
    state.validate().map_err(DecodeError::Field)?;
    Ok(Record {
        seq: get32(b, 8),
        state,
    })
}

// ---------------------------------------------------------------------------
// store abstraction + slot policy
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    A,
    B,
}

impl Slot {
    pub const fn other(self) -> Slot {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }

    pub const fn file_name(self) -> &'static str {
        match self {
            Slot::A => SLOT_A_FILE,
            Slot::B => SLOT_B_FILE,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The slot file (or its directory) does not exist.
    NotFound,
    NoCard,
    /// Any other read, write or delete failure.
    Io,
}

/// Byte storage for the two slot files. The kernel implements it over
/// `drivers::storage`; host tests use an in-memory fake with fault injection.
pub trait SessionStore {
    /// Read from the start of the slot file, up to `buf.len()` bytes. Returns
    /// the number of bytes read (0 for an empty file).
    fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<usize, StoreError>;
    /// Replace the slot file's content with `data`.
    fn write(&mut self, slot: Slot, data: &[u8]) -> Result<(), StoreError>;
    /// Remove the slot file; a missing file is `Ok`.
    fn delete(&mut self, slot: Slot) -> Result<(), StoreError>;
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SlotStatus {
    Valid(Record),
    /// No file.
    Absent,
    /// File present but not a valid record.
    Invalid(DecodeError),
    /// The store itself failed; the slot's content is unknown.
    Unreadable(StoreError),
}

fn read_slot<S: SessionStore>(store: &mut S, slot: Slot) -> SlotStatus {
    let mut buf = [0u8; READ_BUF_LEN];
    match store.read(slot, &mut buf) {
        Err(StoreError::NotFound) => SlotStatus::Absent,
        Err(e) => SlotStatus::Unreadable(e),
        Ok(n) => {
            // a misbehaving store must not make us index out of range
            let n = n.min(buf.len());
            match decode(&buf[..n]) {
                Ok(r) => SlotStatus::Valid(r),
                Err(e) => SlotStatus::Invalid(e),
            }
        }
    }
}

/// Serial-number comparison (RFC 1982 style): `a` is newer than `b` when it is
/// ahead by less than half the sequence space, so a wrapped counter still
/// orders correctly.
pub const fn seq_newer(a: u32, b: u32) -> bool {
    a != b && (a.wrapping_sub(b) as i32) > 0
}

fn newest(a: &SlotStatus, b: &SlotStatus) -> Option<(Slot, Record)> {
    match (a, b) {
        (SlotStatus::Valid(ra), SlotStatus::Valid(rb)) => {
            if seq_newer(rb.seq, ra.seq) {
                Some((Slot::B, *rb))
            } else {
                Some((Slot::A, *ra))
            }
        }
        (SlotStatus::Valid(ra), _) => Some((Slot::A, *ra)),
        (_, SlotStatus::Valid(rb)) => Some((Slot::B, *rb)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// R18 ordering constraint: SD must be active
// ---------------------------------------------------------------------------

/// Proof that the SD card is initialised and its rail is still on. Borrows the
/// power state machine, so `begin_shutdown` / `cut_peripheral_power` (which need
/// `&mut`) cannot run while a save or restore is in progress.
#[derive(Debug)]
pub struct SdActiveProof<'a>(PhantomData<&'a ()>);

impl<P: RailPin> PeripheralPower<P> {
    /// `Ok` only in `RailState::SdActive`: after SD init, before shutdown.
    /// A removed card (`card_removed`) leaves `SdActive`, so it is refused too.
    pub fn sd_active(&self) -> Result<SdActiveProof<'_>, PowerError> {
        if self.state() != RailState::SdActive {
            return Err(PowerError::InvalidState(self.state()));
        }
        Ok(SdActiveProof(PhantomData))
    }
}

// ---------------------------------------------------------------------------
// save / restore / clear
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SaveReport {
    pub slot: Slot,
    pub seq: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// SD is not active (not power-cycled / not initialised / card removed /
    /// shutdown already begun). Nothing was read or written.
    SdNotActive(RailState),
    /// The state itself is not representable. Nothing was written.
    InvalidState(FieldError),
    /// A slot could not be read, so the next sequence number is unknown.
    /// Nothing was written.
    Read(StoreError),
    /// The write failed. The previous newest record is untouched and still
    /// restorable.
    Write(StoreError),
    /// The write reported success but the read-back did not decode to the
    /// record just written.
    Verify,
}

/// Persist `state` (R18). Call before `begin_shutdown`, while the SD rail is up.
/// The caller decides what a failure means for going to sleep (T11).
pub fn save_session<P: RailPin, S: SessionStore>(
    power: &PeripheralPower<P>,
    store: &mut S,
    state: &SessionState,
) -> Result<SaveReport, SaveError> {
    let proof = power
        .sd_active()
        .map_err(|_| SaveError::SdNotActive(power.state()))?;
    save_with_proof(&proof, store, state)
}

fn save_with_proof<S: SessionStore>(
    _proof: &SdActiveProof<'_>,
    store: &mut S,
    state: &SessionState,
) -> Result<SaveReport, SaveError> {
    // validate first: a bad state must not cause any card access
    state.validate().map_err(SaveError::InvalidState)?;

    let a = read_slot(store, Slot::A);
    let b = read_slot(store, Slot::B);
    for s in [&a, &b] {
        if let SlotStatus::Unreadable(e) = s {
            return Err(SaveError::Read(*e));
        }
    }
    let (target, seq) = match newest(&a, &b) {
        // never overwrite the newest valid record; replace the other slot
        Some((slot, rec)) => (slot.other(), rec.seq.wrapping_add(1)),
        None => (Slot::A, 1),
    };
    let bytes = encode(state, seq).map_err(SaveError::InvalidState)?;
    store.write(target, &bytes).map_err(SaveError::Write)?;
    match read_slot(store, target) {
        SlotStatus::Valid(r) if r.seq == seq && r.state == *state => {
            Ok(SaveReport { slot: target, seq })
        }
        _ => Err(SaveError::Verify),
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    pub slot: Slot,
    pub seq: u32,
    pub state: SessionState,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum NormalBootReason {
    /// Neither slot file exists (first boot, or cleared).
    NoSession,
    /// No valid record; the error is why the first bad slot was rejected.
    Corrupt(DecodeError),
    /// No valid record and a slot could not be read.
    StorageUnavailable(StoreError),
    /// SD not active, so nothing was read.
    SdNotActive(RailState),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BootDecision {
    Restore(Restored),
    NormalBoot(NormalBootReason),
}

/// Boot decision (R20): restore the newest valid record, otherwise boot
/// normally. Never panics and never fails the boot; a bad or unreadable slot
/// only costs the restored position. Bad slot files are left in place: the next
/// save overwrites an invalid slot before it touches a valid one.
pub fn restore_session<P: RailPin, S: SessionStore>(
    power: &PeripheralPower<P>,
    store: &mut S,
) -> BootDecision {
    match power.sd_active() {
        Ok(proof) => restore_with_proof(&proof, store),
        Err(_) => BootDecision::NormalBoot(NormalBootReason::SdNotActive(power.state())),
    }
}

fn restore_with_proof<S: SessionStore>(_proof: &SdActiveProof<'_>, store: &mut S) -> BootDecision {
    let a = read_slot(store, Slot::A);
    let b = read_slot(store, Slot::B);
    // If the other slot is unreadable it might hold a newer record; the valid
    // one is still the best known position, so it wins.
    if let Some((slot, rec)) = newest(&a, &b) {
        return BootDecision::Restore(Restored {
            slot,
            seq: rec.seq,
            state: rec.state,
        });
    }
    let mut corrupt = None;
    for s in [&a, &b] {
        match s {
            SlotStatus::Unreadable(e) => {
                return BootDecision::NormalBoot(NormalBootReason::StorageUnavailable(*e));
            }
            SlotStatus::Invalid(e) if corrupt.is_none() => corrupt = Some(*e),
            _ => {}
        }
    }
    match corrupt {
        Some(e) => BootDecision::NormalBoot(NormalBootReason::Corrupt(e)),
        None => BootDecision::NormalBoot(NormalBootReason::NoSession),
    }
}

/// Delete both slot files (e.g. one-shot restore, T12's choice). Both deletes
/// are attempted; the first real error (not "missing") is returned. A failure
/// between the two deletes can leave the older record behind.
pub fn clear_session<P: RailPin, S: SessionStore>(
    power: &PeripheralPower<P>,
    store: &mut S,
) -> Result<(), SaveError> {
    if power.sd_active().is_err() {
        return Err(SaveError::SdNotActive(power.state()));
    }
    let ra = store.delete(Slot::A);
    let rb = store.delete(Slot::B);
    match (ra, rb) {
        (Err(e), _) | (_, Err(e)) if e != StoreError::NotFound => Err(SaveError::Write(e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::power::DelayMs;
    use std::vec;
    use std::vec::Vec;

    // ---- fixtures ---------------------------------------------------------

    fn reading_state() -> SessionState {
        let mut s = SessionState::home();
        s.wake_count = 7;
        s.nav_depth = 2;
        s.nav_stack = [APP_HOME, APP_READER, 0, 0];
        s.set_reader_filename(b"BOOK.EPU").unwrap();
        s.reader_is_epub = true;
        s.reader_chapter = 12;
        s.reader_page = 345;
        s.reader_byte_offset = 0x0012_3456;
        s.reader_font_size = 3;
        s.files_scroll = 20;
        s.files_selected = 4;
        s.files_total = 99;
        s.home_state = 1;
        s.home_selected = 2;
        s.home_bm_selected = 3;
        s.home_bm_scroll = 1;
        s
    }

    fn other_state() -> SessionState {
        let mut s = reading_state();
        s.reader_page = 346;
        s.reader_byte_offset = 0x0012_4000;
        s
    }

    struct NoDelay;
    impl DelayMs for NoDelay {
        fn delay_ms(&mut self, _ms: u32) {}
    }
    struct Pin;
    impl RailPin for Pin {
        fn set_high(&mut self) {}
        fn set_low(&mut self) {}
    }

    fn power_in(state: RailState) -> PeripheralPower<Pin> {
        let mut p = PeripheralPower::new(Pin);
        let mut d = NoDelay;
        match state {
            RailState::Unpowered => {}
            RailState::PowerCycled => p.power_cycle(&mut d).unwrap(),
            RailState::SdInitializing => {
                p.power_cycle(&mut d).unwrap();
                let permit = p.begin_sd_init().unwrap();
                // keep the permit outstanding by leaking it; state stays SdInitializing
                core::mem::forget(permit);
            }
            RailState::SdActive => {
                p.power_cycle(&mut d).unwrap();
                let permit = p.begin_sd_init().unwrap();
                p.finish_sd_init(permit, true);
            }
            RailState::ShuttingDown => {
                p.power_cycle(&mut d).unwrap();
                p.begin_shutdown().unwrap();
            }
            RailState::PoweredOff => {
                p.power_cycle(&mut d).unwrap();
                p.begin_shutdown().unwrap();
                p.cut_peripheral_power().unwrap();
            }
        }
        assert_eq!(p.state(), state);
        p
    }

    fn active() -> PeripheralPower<Pin> {
        power_in(RailState::SdActive)
    }

    /// What a `write` call does under fault injection.
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum WriteFault {
        None,
        /// Fail before touching the slot (the slot is left as it was).
        Fail,
        /// Truncate the slot, write the first n bytes, then fail (power cut).
        Torn(usize),
        /// Truncate the slot to empty and fail (cut right after truncate).
        TruncateOnly,
        /// Report success without storing anything.
        Silent,
    }

    #[derive(Default)]
    struct FakeStore {
        slots: [Option<Vec<u8>>; 2],
        /// fault for the n-th (0-based) write call; `None` = no fault
        write_faults: Vec<(usize, WriteFault)>,
        writes: usize,
        read_error: Option<(Option<Slot>, StoreError)>,
        read_calls: usize,
        delete_error: Option<StoreError>,
        log: Vec<(&'static str, Slot)>,
    }

    fn idx(s: Slot) -> usize {
        match s {
            Slot::A => 0,
            Slot::B => 1,
        }
    }

    impl FakeStore {
        fn new() -> Self {
            Self::default()
        }
        fn with(a: Option<Vec<u8>>, b: Option<Vec<u8>>) -> Self {
            Self {
                slots: [a, b],
                ..Self::default()
            }
        }
    }

    impl SessionStore for FakeStore {
        fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<usize, StoreError> {
            self.read_calls += 1;
            self.log.push(("read", slot));
            if let Some((only, e)) = self.read_error
                && only.is_none_or(|s| s == slot)
            {
                return Err(e);
            }
            match &self.slots[idx(slot)] {
                None => Err(StoreError::NotFound),
                Some(data) => {
                    let n = data.len().min(buf.len());
                    buf[..n].copy_from_slice(&data[..n]);
                    Ok(n)
                }
            }
        }
        fn write(&mut self, slot: Slot, data: &[u8]) -> Result<(), StoreError> {
            self.log.push(("write", slot));
            let n = self.writes;
            self.writes += 1;
            let fault = self
                .write_faults
                .iter()
                .find(|(i, _)| *i == n)
                .map(|(_, f)| *f)
                .unwrap_or(WriteFault::None);
            match fault {
                WriteFault::None => {
                    self.slots[idx(slot)] = Some(data.to_vec());
                    Ok(())
                }
                WriteFault::Fail => Err(StoreError::Io),
                WriteFault::Torn(k) => {
                    self.slots[idx(slot)] = Some(data[..k.min(data.len())].to_vec());
                    Err(StoreError::Io)
                }
                WriteFault::TruncateOnly => {
                    self.slots[idx(slot)] = Some(Vec::new());
                    Err(StoreError::Io)
                }
                WriteFault::Silent => Ok(()),
            }
        }
        fn delete(&mut self, slot: Slot) -> Result<(), StoreError> {
            self.log.push(("delete", slot));
            if let Some(e) = self.delete_error {
                return Err(e);
            }
            self.slots[idx(slot)] = None;
            Ok(())
        }
    }

    fn valid_bytes() -> [u8; RECORD_LEN] {
        encode(&reading_state(), 5).unwrap()
    }

    /// recompute the CRC after a manual edit, to reach the field checks
    fn refix_crc(b: &mut [u8; RECORD_LEN]) {
        let crc = crc32(&b[..CRC_OFFSET]);
        b[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
    }

    fn restored(power: &PeripheralPower<Pin>, store: &mut FakeStore) -> Option<Restored> {
        match restore_session(power, store) {
            BootDecision::Restore(r) => Some(r),
            BootDecision::NormalBoot(_) => None,
        }
    }

    // ---- crc --------------------------------------------------------------

    #[test]
    fn crc32_matches_the_standard_check_values() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(&[0]), 0xD202_EF8D);
    }

    // ---- format constants -------------------------------------------------

    #[test]
    fn r18_format_constants_match_the_x4_session_semantics() {
        assert_eq!(MAGIC, 0x504C_5053, "X4 RTC_SESSION_MAGIC");
        assert_eq!(MAX_NAV_STACK, 4, "X4 MAX_NAV_STACK");
        assert_eq!(MAX_FILENAME_LEN, 32, "X4 MAX_FILENAME_LEN");
        assert_eq!(RECORD_LEN, 80);
        assert_eq!(READ_BUF_LEN, 81);
        assert_eq!(SLOT_A_FILE.len(), 9);
        assert_eq!(SLOT_B_FILE.len(), 9);
        for f in [SLOT_A_FILE, SLOT_B_FILE] {
            let (base, ext) = f.split_once('.').unwrap();
            assert!(base.len() <= 8 && ext.len() <= 3, "8.3 name: {f}");
            assert!(f.bytes().all(|c| c.is_ascii_uppercase() || c == b'.'));
        }
        assert_ne!(SLOT_A_FILE, SLOT_B_FILE);
    }

    #[test]
    fn r18_layout_is_the_documented_little_endian_layout() {
        let b = valid_bytes();
        assert_eq!(&b[0..4], &[0x53, 0x50, 0x4C, 0x50]);
        assert_eq!(&b[4..6], &1u16.to_le_bytes());
        assert_eq!(&b[8..12], &5u32.to_le_bytes());
        assert_eq!(b[16], 2);
        assert_eq!(&b[17..21], &[0, 2, 0, 0]);
        assert_eq!(&b[21..29], b"BOOK.EPU");
        assert_eq!(b[53], 8);
        assert_eq!(b[54], 1);
        assert_eq!(&b[55..57], &12u16.to_le_bytes());
        assert_eq!(&b[59..63], &0x0012_3456u32.to_le_bytes());
        assert_eq!(&b[73..76], &[0, 0, 0]);
        assert_eq!(&b[76..80], &crc32(&b[..76]).to_le_bytes());
    }

    // ---- round trip (R20) -------------------------------------------------

    #[test]
    fn r20_round_trip_restores_the_same_position() {
        let s = reading_state();
        let rec = decode(&encode(&s, 42).unwrap()).unwrap();
        assert_eq!(rec, Record { seq: 42, state: s });
    }

    #[test]
    fn r20_round_trip_home_only_state() {
        let s = SessionState::home();
        assert_eq!(decode(&encode(&s, 1).unwrap()).unwrap().state, s);
    }

    #[test]
    fn r20_round_trip_field_boundary_values() {
        let mut s = reading_state();
        s.wake_count = u32::MAX;
        s.reader_chapter = u16::MAX;
        s.reader_page = u16::MAX;
        s.reader_byte_offset = u32::MAX;
        s.reader_font_size = FONT_SIZE_MAX;
        s.files_scroll = u16::MAX;
        s.files_selected = u8::MAX;
        s.files_total = u16::MAX;
        s.home_state = HOME_STATE_MAX;
        s.home_selected = u8::MAX;
        s.home_bm_selected = u8::MAX;
        s.home_bm_scroll = u8::MAX;
        for seq in [0, 1, u32::MAX - 1, u32::MAX] {
            assert_eq!(decode(&encode(&s, seq).unwrap()).unwrap().seq, seq);
        }
        assert_eq!(decode(&encode(&s, 9).unwrap()).unwrap().state, s);
        // zero everywhere
        let mut z = SessionState::home();
        z.wake_count = 0;
        assert_eq!(decode(&encode(&z, 0).unwrap()).unwrap().state, z);
    }

    #[test]
    fn r20_round_trip_every_nav_depth_and_every_app_id() {
        for depth in 1..=MAX_NAV_STACK {
            for top in 0..=APP_ID_MAX {
                let mut s = SessionState::home();
                s.nav_depth = depth as u8;
                s.nav_stack = [0; MAX_NAV_STACK];
                for i in 1..depth {
                    s.nav_stack[i] = top;
                }
                // a Reader on the stack needs a file name
                s.set_reader_filename(b"A.TXT").unwrap();
                assert_eq!(decode(&encode(&s, 1).unwrap()).unwrap().state, s);
            }
        }
    }

    #[test]
    fn r20_round_trip_every_file_name_length() {
        for len in 0..=MAX_FILENAME_LEN {
            let mut s = SessionState::home();
            let name: Vec<u8> = (0..len).map(|i| b'A' + (i % 26) as u8).collect();
            s.set_reader_filename(&name).unwrap();
            let got = decode(&encode(&s, 1).unwrap()).unwrap().state;
            assert_eq!(got.reader_name(), &name[..]);
        }
        let mut s = SessionState::home();
        assert_eq!(
            s.set_reader_filename(&[b'A'; MAX_FILENAME_LEN + 1]),
            Err(FieldError::FilenameLen)
        );
    }

    #[test]
    fn r20_encode_refuses_an_invalid_state() {
        let mut s = reading_state();
        s.nav_depth = 0;
        assert_eq!(encode(&s, 1), Err(FieldError::NavDepth));
        let mut s = reading_state();
        s.reader_font_size = FONT_SIZE_MAX + 1;
        assert_eq!(encode(&s, 1), Err(FieldError::FontSize));
    }

    // ---- corruption matrix (R20: invalid -> normal boot) ------------------

    #[test]
    fn r20_decode_empty_file() {
        assert_eq!(decode(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn r20_decode_truncated_to_every_length_is_rejected() {
        let b = valid_bytes();
        for len in 1..RECORD_LEN {
            assert_eq!(
                decode(&b[..len]),
                Err(DecodeError::TooShort { len }),
                "len {len}"
            );
        }
    }

    #[test]
    fn r20_decode_trailing_data_is_rejected() {
        let mut v = valid_bytes().to_vec();
        v.push(0);
        assert_eq!(decode(&v), Err(DecodeError::TooLong { len: 81 }));
        v.extend_from_slice(&[0xAB; 100]);
        assert_eq!(decode(&v), Err(DecodeError::TooLong { len: 181 }));
        // a record followed by a second record (e.g. a bad append) is also rejected
        let mut two = valid_bytes().to_vec();
        two.extend_from_slice(&valid_bytes());
        assert!(matches!(decode(&two), Err(DecodeError::TooLong { .. })));
    }

    #[test]
    fn r20_decode_bad_magic_with_a_valid_crc() {
        let mut b = valid_bytes();
        b[0] ^= 0x01;
        refix_crc(&mut b);
        assert_eq!(decode(&b), Err(DecodeError::BadMagic));
    }

    #[test]
    fn r20_decode_wrong_version_with_a_valid_crc() {
        for v in [0u16, 2, 0x0100, u16::MAX] {
            let mut b = valid_bytes();
            b[4..6].copy_from_slice(&v.to_le_bytes());
            refix_crc(&mut b);
            assert_eq!(decode(&b), Err(DecodeError::BadVersion(v)), "version {v}");
        }
    }

    #[test]
    fn r20_decode_crc_mismatch_is_rejected() {
        let mut b = valid_bytes();
        b[CRC_OFFSET] ^= 0xFF;
        assert_eq!(decode(&b), Err(DecodeError::BadCrc));
        // payload changed, CRC not refreshed
        let mut b = valid_bytes();
        b[56] ^= 0x10;
        assert_eq!(decode(&b), Err(DecodeError::BadCrc));
    }

    #[test]
    fn r20_every_single_bit_flip_in_the_record_is_rejected() {
        let good = valid_bytes();
        for i in 0..RECORD_LEN {
            for bit in 0..8 {
                let mut b = good;
                b[i] ^= 1 << bit;
                assert!(decode(&b).is_err(), "byte {i} bit {bit} flip was accepted");
            }
            let mut b = good;
            b[i] ^= 0xFF;
            assert!(decode(&b).is_err(), "byte {i} inversion was accepted");
            let mut b = good;
            b[i] = 0;
            if b != good {
                assert!(decode(&b).is_err(), "byte {i} zeroed was accepted");
            }
        }
    }

    #[test]
    fn r20_all_zero_and_all_ones_files_are_rejected() {
        assert!(decode(&[0u8; RECORD_LEN]).is_err());
        assert!(decode(&[0xFFu8; RECORD_LEN]).is_err());
        assert!(decode(&[0xFFu8; READ_BUF_LEN]).is_err());
    }

    #[test]
    fn r20_with_the_crc_repaired_a_flip_is_rejected_or_canonical() {
        // The CRC cannot help here (it is recomputed), so this checks the field
        // validation: whatever decode still accepts must be canonical, i.e.
        // re-encoding gives back exactly the same bytes (no accepted garbage in
        // reserved bytes, name tail or flags).
        let good = valid_bytes();
        let mut rejected = 0;
        for i in 0..CRC_OFFSET {
            for bit in 0..8 {
                let mut b = good;
                b[i] ^= 1 << bit;
                refix_crc(&mut b);
                match decode(&b) {
                    Err(_) => rejected += 1,
                    Ok(rec) => {
                        assert_eq!(
                            encode(&rec.state, rec.seq).unwrap(),
                            b,
                            "byte {i} bit {bit}: accepted a non-canonical record"
                        );
                    }
                }
            }
        }
        assert!(rejected > 100, "field validation rejected only {rejected}");
    }

    #[test]
    fn r20_illegal_field_values_with_a_valid_crc_are_rejected() {
        fn edit(f: impl FnOnce(&mut [u8; RECORD_LEN])) -> Result<Record, DecodeError> {
            let mut b = valid_bytes();
            f(&mut b);
            refix_crc(&mut b);
            decode(&b)
        }
        use DecodeError::Field as F;
        assert_eq!(edit(|b| b[16] = 0), Err(F(FieldError::NavDepth)));
        assert_eq!(edit(|b| b[16] = 5), Err(F(FieldError::NavDepth)));
        assert_eq!(edit(|b| b[16] = 255), Err(F(FieldError::NavDepth)));
        assert_eq!(edit(|b| b[18] = 5), Err(F(FieldError::NavAppId)));
        assert_eq!(edit(|b| b[19] = 1), Err(F(FieldError::NavStackTail)));
        assert_eq!(edit(|b| b[17] = 1), Err(F(FieldError::NavBottomNotHome)));
        assert_eq!(edit(|b| b[53] = 33), Err(F(FieldError::FilenameLen)));
        assert_eq!(edit(|b| b[53] = 255), Err(F(FieldError::FilenameLen)));
        assert_eq!(edit(|b| b[21] = 0), Err(F(FieldError::FilenameChar)));
        assert_eq!(edit(|b| b[21] = b'/'), Err(F(FieldError::FilenameChar)));
        assert_eq!(edit(|b| b[22] = 0x1F), Err(F(FieldError::FilenameChar)));
        assert_eq!(edit(|b| b[22] = 0x7F), Err(F(FieldError::FilenameChar)));
        assert_eq!(edit(|b| b[22] = 0xC3), Err(F(FieldError::FilenameChar)));
        assert_eq!(edit(|b| b[40] = b'X'), Err(F(FieldError::FilenameTail)));
        assert_eq!(edit(|b| b[54] = 2), Err(F(FieldError::EpubFlag)));
        assert_eq!(edit(|b| b[63] = 5), Err(F(FieldError::FontSize)));
        assert_eq!(edit(|b| b[69] = 2), Err(F(FieldError::HomeState)));
        assert_eq!(edit(|b| b[6] = 1), Err(F(FieldError::ReservedNotZero)));
        assert_eq!(edit(|b| b[75] = 1), Err(F(FieldError::ReservedNotZero)));
        // reader on the stack but no file name
        assert_eq!(
            edit(|b| {
                b[53] = 0;
                b[21..53].fill(0);
            }),
            Err(F(FieldError::ReaderWithoutFile))
        );
    }

    #[test]
    fn r20_decode_never_panics_on_pseudo_random_input() {
        // xorshift fuzz over lengths 0..=200, plus mutated valid records
        let mut x: u32 = 0x1234_5678;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x
        };
        for _ in 0..4000 {
            let len = (next() % 201) as usize;
            let v: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            let _ = decode(&v);
        }
        for _ in 0..4000 {
            let mut b = valid_bytes();
            for _ in 0..(1 + next() % 4) {
                b[(next() as usize) % RECORD_LEN] = next() as u8;
            }
            let _ = decode(&b);
        }
    }

    // ---- boot decision over a store (R20) ---------------------------------

    #[test]
    fn r20_valid_session_is_restored_to_the_same_position() {
        let p = active();
        let mut st = FakeStore::new();
        let rep = save_session(&p, &mut st, &reading_state()).unwrap();
        assert_eq!(
            rep,
            SaveReport {
                slot: Slot::A,
                seq: 1
            }
        );
        let r = restored(&p, &mut st).expect("valid session must restore");
        assert_eq!(r.state, reading_state());
        assert_eq!((r.slot, r.seq), (Slot::A, 1));
    }

    #[test]
    fn r20_no_session_files_is_a_normal_boot() {
        let p = active();
        assert_eq!(
            restore_session(&p, &mut FakeStore::new()),
            BootDecision::NormalBoot(NormalBootReason::NoSession)
        );
    }

    #[test]
    fn r20_every_corruption_of_the_only_slot_is_a_normal_boot() {
        let p = active();
        let good = valid_bytes();
        // empty, every truncation, every byte flip, trailing garbage
        let mut cases: Vec<Vec<u8>> = vec![vec![]];
        for len in 1..RECORD_LEN {
            cases.push(good[..len].to_vec());
        }
        for i in 0..RECORD_LEN {
            let mut b = good;
            b[i] ^= 0x80;
            cases.push(b.to_vec());
        }
        let mut longer = good.to_vec();
        longer.extend_from_slice(b"garbage");
        cases.push(longer);
        for (n, case) in cases.into_iter().enumerate() {
            for slot in [Slot::A, Slot::B] {
                let mut st = match slot {
                    Slot::A => FakeStore::with(Some(case.clone()), None),
                    Slot::B => FakeStore::with(None, Some(case.clone())),
                };
                match restore_session(&p, &mut st) {
                    BootDecision::NormalBoot(NormalBootReason::Corrupt(_)) => {}
                    other => panic!("case {n} slot {slot:?}: expected Corrupt, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn r20_a_corrupt_slot_does_not_hide_a_valid_older_one() {
        let p = active();
        let mut older = valid_bytes();
        let mut newer = encode(&other_state(), 6).unwrap();
        newer[30] ^= 0xFF; // newer slot damaged
        let st_a = Some(older.to_vec());
        let mut st = FakeStore::with(st_a, Some(newer.to_vec()));
        let r = restored(&p, &mut st).unwrap();
        assert_eq!((r.slot, r.seq), (Slot::A, 5));
        assert_eq!(r.state, reading_state());
        older[0] ^= 1; // and the other way round
        let mut st = FakeStore::with(
            Some(older.to_vec()),
            Some(encode(&other_state(), 6).unwrap().to_vec()),
        );
        let r = restored(&p, &mut st).unwrap();
        assert_eq!((r.slot, r.seq), (Slot::B, 6));
    }

    #[test]
    fn r20_newest_valid_sequence_wins_in_both_slot_orders() {
        let p = active();
        let low = encode(&reading_state(), 5).unwrap().to_vec();
        let high = encode(&other_state(), 6).unwrap().to_vec();
        let mut st = FakeStore::with(Some(low.clone()), Some(high.clone()));
        assert_eq!(restored(&p, &mut st).unwrap().seq, 6);
        let mut st = FakeStore::with(Some(high), Some(low));
        let r = restored(&p, &mut st).unwrap();
        assert_eq!((r.slot, r.seq, r.state), (Slot::A, 6, other_state()));
    }

    #[test]
    fn r20_equal_sequence_in_both_slots_is_resolved_deterministically_to_a() {
        // cannot happen through `save_session`; guards a hand-edited card
        let p = active();
        let a = encode(&reading_state(), 9).unwrap().to_vec();
        let b = encode(&other_state(), 9).unwrap().to_vec();
        let mut st = FakeStore::with(Some(a), Some(b));
        let r = restored(&p, &mut st).unwrap();
        assert_eq!((r.slot, r.state), (Slot::A, reading_state()));
    }

    #[test]
    fn r20_sequence_wraparound_still_picks_the_newer_record() {
        let p = active();
        let before = encode(&reading_state(), u32::MAX).unwrap().to_vec();
        let after = encode(&other_state(), 0).unwrap().to_vec();
        let mut st = FakeStore::with(Some(before.clone()), Some(after.clone()));
        assert_eq!(restored(&p, &mut st).unwrap().state, other_state());
        let mut st = FakeStore::with(Some(after), Some(before));
        assert_eq!(restored(&p, &mut st).unwrap().state, other_state());
        assert!(seq_newer(0, u32::MAX));
        assert!(!seq_newer(u32::MAX, 0));
        assert!(!seq_newer(7, 7));
    }

    #[test]
    fn r20_a_saved_wraparound_sequence_continues_correctly() {
        let p = active();
        let mut st = FakeStore::with(
            Some(encode(&reading_state(), u32::MAX).unwrap().to_vec()),
            None,
        );
        let rep = save_session(&p, &mut st, &other_state()).unwrap();
        assert_eq!((rep.slot, rep.seq), (Slot::B, 0));
        assert_eq!(restored(&p, &mut st).unwrap().state, other_state());
    }

    #[test]
    fn r9_r20_sd_errors_are_a_normal_boot_not_a_panic() {
        let p = active();
        for e in [StoreError::NoCard, StoreError::Io] {
            let mut st = FakeStore::new();
            st.read_error = Some((None, e));
            assert_eq!(
                restore_session(&p, &mut st),
                BootDecision::NormalBoot(NormalBootReason::StorageUnavailable(e))
            );
        }
    }

    #[test]
    fn r20_restore_is_refused_without_an_active_sd() {
        for s in [
            RailState::Unpowered,
            RailState::PowerCycled,
            RailState::SdInitializing,
            RailState::ShuttingDown,
            RailState::PoweredOff,
        ] {
            let p = power_in(s);
            let mut st = FakeStore::with(Some(valid_bytes().to_vec()), None);
            assert_eq!(
                restore_session(&p, &mut st),
                BootDecision::NormalBoot(NormalBootReason::SdNotActive(s))
            );
            assert_eq!(st.read_calls, 0, "no card access in {s:?}");
        }
    }

    #[test]
    fn r20_an_unreadable_slot_next_to_a_valid_one_still_restores() {
        let p = active();
        let mut st = FakeStore::with(Some(valid_bytes().to_vec()), None);
        st.read_error = Some((Some(Slot::B), StoreError::Io));
        assert_eq!(restored(&p, &mut st).unwrap().slot, Slot::A);
    }

    // ---- save: slots, sequence, atomicity (R18) ---------------------------

    #[test]
    fn r18_saves_alternate_slots_with_increasing_sequence() {
        let p = active();
        let mut st = FakeStore::new();
        let mut expect = Slot::A;
        for n in 1..=10u32 {
            let mut s = reading_state();
            s.reader_page = n as u16;
            let rep = save_session(&p, &mut st, &s).unwrap();
            assert_eq!(
                rep,
                SaveReport {
                    slot: expect,
                    seq: n
                }
            );
            expect = expect.other();
            let r = restored(&p, &mut st).unwrap();
            assert_eq!((r.seq, r.state.reader_page), (n, n as u16));
        }
    }

    #[test]
    fn r18_a_save_never_overwrites_the_newest_valid_record() {
        let p = active();
        let mut st = FakeStore::new();
        save_session(&p, &mut st, &reading_state()).unwrap();
        st.log.clear();
        let rep = save_session(&p, &mut st, &other_state()).unwrap();
        let writes: Vec<_> = st.log.iter().filter(|e| e.0 == "write").collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].1, rep.slot);
        assert_eq!(rep.slot, Slot::B, "slot A held the newest record");
    }

    #[test]
    fn r18_first_save_after_corruption_replaces_the_bad_slot_first() {
        let p = active();
        // A is garbage, B absent: write A (the invalid one), seq starts at 1
        let mut st = FakeStore::with(Some(vec![1, 2, 3]), None);
        let rep = save_session(&p, &mut st, &reading_state()).unwrap();
        assert_eq!((rep.slot, rep.seq), (Slot::A, 1));
        // B valid, A garbage: the garbage slot is the target, B stays
        let mut st = FakeStore::with(Some(vec![9; 80]), Some(valid_bytes().to_vec()));
        let rep = save_session(&p, &mut st, &other_state()).unwrap();
        assert_eq!((rep.slot, rep.seq), (Slot::A, 6));
        assert_eq!(restored(&p, &mut st).unwrap().state, other_state());
    }

    #[test]
    fn r18_write_failure_keeps_the_previous_session_restorable() {
        let p = active();
        for fault in [
            WriteFault::Fail,
            WriteFault::TruncateOnly,
            WriteFault::Silent,
        ] {
            let mut st = FakeStore::new();
            save_session(&p, &mut st, &reading_state()).unwrap();
            st.write_faults = vec![(1, fault)];
            let err = save_session(&p, &mut st, &other_state()).unwrap_err();
            match fault {
                WriteFault::Silent => assert_eq!(err, SaveError::Verify),
                _ => assert_eq!(err, SaveError::Write(StoreError::Io)),
            }
            let r = restored(&p, &mut st).expect("previous session must survive");
            assert_eq!(r.state, reading_state(), "fault {fault:?}");
            assert_eq!(r.seq, 1);
        }
    }

    #[test]
    fn r18_power_cut_at_every_byte_of_the_write_never_yields_a_wrong_session() {
        let p = active();
        // old record exists in A; the new save targets B and is torn after k bytes
        for k in 0..=RECORD_LEN {
            let mut st = FakeStore::new();
            save_session(&p, &mut st, &reading_state()).unwrap();
            st.write_faults = vec![(1, WriteFault::Torn(k))];
            let _ = save_session(&p, &mut st, &other_state());
            let r = restored(&p, &mut st).expect("some session must be restorable");
            if k == RECORD_LEN {
                // the whole record landed before the "cut": new one is valid
                assert_eq!(r.state, other_state());
            } else {
                assert_eq!(r.state, reading_state(), "torn at {k} bytes");
                assert_eq!(r.seq, 1);
            }
        }
    }

    #[test]
    fn r18_power_cut_during_the_very_first_save_is_a_normal_boot() {
        let p = active();
        for k in 0..RECORD_LEN {
            let mut st = FakeStore::new();
            st.write_faults = vec![(0, WriteFault::Torn(k))];
            assert!(save_session(&p, &mut st, &reading_state()).is_err());
            match restore_session(&p, &mut st) {
                BootDecision::NormalBoot(_) => {}
                other => panic!("torn first save at {k} bytes restored {other:?}"),
            }
        }
    }

    #[test]
    fn r18_repeated_torn_writes_never_corrupt_the_good_slot() {
        let p = active();
        let mut st = FakeStore::new();
        save_session(&p, &mut st, &reading_state()).unwrap();
        // five failures in a row all land on B; A is never touched
        for n in 1..=5 {
            st.write_faults = vec![(st.writes, WriteFault::Torn(n * 7))];
            assert!(save_session(&p, &mut st, &other_state()).is_err());
            assert_eq!(restored(&p, &mut st).unwrap().state, reading_state());
        }
        // and a later successful save works
        save_session(&p, &mut st, &other_state()).unwrap();
        assert_eq!(restored(&p, &mut st).unwrap().state, other_state());
    }

    #[test]
    fn r18_read_failure_before_save_aborts_without_writing() {
        let p = active();
        for (slot, e) in [
            (None, StoreError::NoCard),
            (None, StoreError::Io),
            (Some(Slot::A), StoreError::Io),
            (Some(Slot::B), StoreError::Io),
        ] {
            let mut st = FakeStore::new();
            st.read_error = Some((slot, e));
            assert_eq!(
                save_session(&p, &mut st, &reading_state()),
                Err(SaveError::Read(e))
            );
            assert_eq!(st.writes, 0);
        }
    }

    #[test]
    fn r18_no_card_on_write_is_reported_and_storage_stays_usable() {
        let p = active();
        let mut st = FakeStore::new();
        // write fails (card pulled): error reported, a later save works
        st.write_faults = vec![(0, WriteFault::Fail)];
        assert_eq!(
            save_session(&p, &mut st, &reading_state()),
            Err(SaveError::Write(StoreError::Io))
        );
        assert!(save_session(&p, &mut st, &reading_state()).is_ok());
    }

    #[test]
    fn r18_an_invalid_state_is_refused_before_any_card_access() {
        let p = active();
        let mut st = FakeStore::new();
        let mut s = reading_state();
        s.nav_depth = 9;
        assert_eq!(
            save_session(&p, &mut st, &s),
            Err(SaveError::InvalidState(FieldError::NavDepth))
        );
        assert_eq!(st.read_calls, 0);
        assert_eq!(st.writes, 0);
    }

    #[test]
    fn r18_verify_catches_a_read_back_that_differs() {
        // a store that stores a different byte than written
        struct Lying(FakeStore);
        impl SessionStore for Lying {
            fn read(&mut self, s: Slot, b: &mut [u8]) -> Result<usize, StoreError> {
                self.0.read(s, b)
            }
            fn write(&mut self, s: Slot, d: &[u8]) -> Result<(), StoreError> {
                let mut v = d.to_vec();
                v[60] ^= 1;
                self.0.write(s, &v)
            }
            fn delete(&mut self, s: Slot) -> Result<(), StoreError> {
                self.0.delete(s)
            }
        }
        let p = active();
        let mut st = Lying(FakeStore::new());
        assert_eq!(
            save_session(&p, &mut st, &reading_state()),
            Err(SaveError::Verify)
        );
    }

    #[test]
    fn r18_a_misbehaving_store_reporting_too_many_bytes_does_not_panic() {
        struct Liar;
        impl SessionStore for Liar {
            fn read(&mut self, _s: Slot, _b: &mut [u8]) -> Result<usize, StoreError> {
                Ok(10_000)
            }
            fn write(&mut self, _s: Slot, _d: &[u8]) -> Result<(), StoreError> {
                Ok(())
            }
            fn delete(&mut self, _s: Slot) -> Result<(), StoreError> {
                Ok(())
            }
        }
        let p = active();
        assert!(matches!(
            restore_session(&p, &mut Liar),
            BootDecision::NormalBoot(NormalBootReason::Corrupt(_))
        ));
    }

    // ---- R18 ordering constraint: SD active -------------------------------

    #[test]
    fn r18_save_is_refused_unless_the_sd_is_active() {
        for s in [
            RailState::Unpowered,
            RailState::PowerCycled,
            RailState::SdInitializing,
            RailState::ShuttingDown,
            RailState::PoweredOff,
        ] {
            let p = power_in(s);
            let mut st = FakeStore::new();
            assert_eq!(
                save_session(&p, &mut st, &reading_state()),
                Err(SaveError::SdNotActive(s)),
                "state {s:?}"
            );
            assert_eq!(st.writes, 0, "no write in {s:?}");
            assert_eq!(st.read_calls, 0, "no read in {s:?}");
        }
        assert!(save_session(&active(), &mut FakeStore::new(), &reading_state()).is_ok());
    }

    #[test]
    fn r18_save_before_shutdown_works_and_after_shutdown_begins_is_refused() {
        let mut p = active();
        let mut st = FakeStore::new();
        // the T11 order: save while SdActive, then shutdown
        save_session(&p, &mut st, &reading_state()).unwrap();
        p.begin_shutdown().unwrap();
        assert_eq!(
            save_session(&p, &mut st, &other_state()),
            Err(SaveError::SdNotActive(RailState::ShuttingDown))
        );
        p.cut_peripheral_power().unwrap();
        assert_eq!(
            save_session(&p, &mut st, &other_state()),
            Err(SaveError::SdNotActive(RailState::PoweredOff))
        );
        // the earlier save is what the next boot sees
        let next_boot = active();
        assert_eq!(
            restored(&next_boot, &mut st).unwrap().state,
            reading_state()
        );
    }

    #[test]
    fn r18_card_removal_blocks_saving_until_the_card_is_back() {
        let mut p = active();
        p.card_removed().unwrap();
        assert_eq!(p.state(), RailState::PowerCycled);
        assert_eq!(
            save_session(&p, &mut FakeStore::new(), &reading_state()),
            Err(SaveError::SdNotActive(RailState::PowerCycled))
        );
    }

    #[test]
    fn r18_sd_active_proof_is_only_minted_in_sd_active() {
        assert!(power_in(RailState::SdActive).sd_active().is_ok());
        for s in [
            RailState::Unpowered,
            RailState::PowerCycled,
            RailState::SdInitializing,
            RailState::ShuttingDown,
            RailState::PoweredOff,
        ] {
            assert_eq!(
                power_in(s).sd_active().unwrap_err(),
                PowerError::InvalidState(s)
            );
        }
    }

    // ---- clear ------------------------------------------------------------

    #[test]
    fn r20_clear_removes_both_slots_and_the_next_boot_is_normal() {
        let p = active();
        let mut st = FakeStore::new();
        save_session(&p, &mut st, &reading_state()).unwrap();
        save_session(&p, &mut st, &other_state()).unwrap();
        clear_session(&p, &mut st).unwrap();
        assert_eq!(
            restore_session(&p, &mut st),
            BootDecision::NormalBoot(NormalBootReason::NoSession)
        );
        // clearing again (files missing) is fine
        clear_session(&p, &mut st).unwrap();
    }

    #[test]
    fn r20_clear_reports_a_delete_failure_and_needs_the_sd() {
        let p = active();
        let mut st = FakeStore::with(Some(valid_bytes().to_vec()), None);
        st.delete_error = Some(StoreError::Io);
        assert_eq!(
            clear_session(&p, &mut st),
            Err(SaveError::Write(StoreError::Io))
        );
        assert_eq!(
            clear_session(&power_in(RailState::PoweredOff), &mut st),
            Err(SaveError::SdNotActive(RailState::PoweredOff))
        );
    }
}
