// RTC FAST memory session persistence
//
// stores session state in RTC FAST memory (8KB on ESP32-C3) which
// survives deep sleep but is zeroed on power-on reset. this enables
// instant wake restoration without SD card I/O.
//
// the session struct is placed in .rtc_fast.persistent via link_section;
// esp-hal's linker scripts ensure this memory is only zeroed on power-on,
// not on deep sleep wake.

use core::sync::atomic::{AtomicU32, Ordering};

// magic value to validate RTC session data: "PLPS" (PuLP Session)
const RTC_SESSION_MAGIC: u32 = 0x504C5053;

// max navigation stack depth (must match app::MAX_STACK_DEPTH)
pub const MAX_NAV_STACK: usize = 4;

// max filename length for reader state
pub const MAX_FILENAME_LEN: usize = 32;

// RTC-persistent session state
// stored in RTC FAST memory; survives deep sleep
// all fields use fixed-size types for stable memory layout
#[derive(Clone, Copy)]
#[repr(C, align(4))]
pub struct RtcSession {
    // header (16 bytes)
    magic: u32,      // must equal RTC_SESSION_MAGIC for valid data
    wake_count: u32, // incremented each successful wake
    flags: u32,      // reserved
    _header_pad: u32,

    // navigation stack (8 bytes)
    pub nav_depth: u8,                  // stack depth (1-4)
    pub nav_stack: [u8; MAX_NAV_STACK], // app ids: Home=0, Files=1, Reader=2, Settings=3, Upload=4
    _nav_pad: [u8; 3],

    // reader state (48 bytes)
    pub reader_filename: [u8; MAX_FILENAME_LEN],
    pub reader_filename_len: u8,
    pub reader_is_epub: u8,
    pub reader_chapter: u16,
    pub reader_page: u16,
    pub reader_byte_offset: u32,
    pub reader_font_size: u8,
    _reader_pad: [u8; 5],

    // files state (8 bytes)
    pub files_scroll: u16,
    pub files_selected: u8,
    pub files_total: u16,
    _files_pad: [u8; 3],

    // home state (8 bytes)
    pub home_state: u8, // 0=Menu, 1=ShowBookmarks
    pub home_selected: u8,
    pub home_bm_selected: u8,
    pub home_bm_scroll: u8,
    _home_pad: [u8; 4],

    // settings cache (16 bytes) - avoid SD reads on wake
    pub settings_sleep_timeout: u16,
    pub settings_ghost_clear: u8,
    pub settings_book_font: u8,
    pub settings_ui_font: u8,
    pub settings_valid: u8,
    _settings_pad: [u8; 10],

    // reserved (24 bytes)
    _reserved: [u8; 24],
}

// Compile-time size check: ensure struct fits comfortably in RTC FAST (8KB)
// and doesn't grow unexpectedly
const _: () = assert!(core::mem::size_of::<RtcSession>() <= 256);

impl RtcSession {
    // create zeroed session (invalid until populated)
    pub const fn zeroed() -> Self {
        Self {
            magic: 0,
            wake_count: 0,
            flags: 0,
            _header_pad: 0,

            nav_depth: 0,
            nav_stack: [0; MAX_NAV_STACK],
            _nav_pad: [0; 3],

            reader_filename: [0; MAX_FILENAME_LEN],
            reader_filename_len: 0,
            reader_is_epub: 0,
            reader_chapter: 0,
            reader_page: 0,
            reader_byte_offset: 0,
            reader_font_size: 0,
            _reader_pad: [0; 5],

            files_scroll: 0,
            files_selected: 0,
            files_total: 0,
            _files_pad: [0; 3],

            home_state: 0,
            home_selected: 0,
            home_bm_selected: 0,
            home_bm_scroll: 0,
            _home_pad: [0; 4],

            settings_sleep_timeout: 0,
            settings_ghost_clear: 0,
            settings_book_font: 0,
            settings_ui_font: 0,
            settings_valid: 0,
            _settings_pad: [0; 10],

            _reserved: [0; 24],
        }
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.magic == RTC_SESSION_MAGIC
    }

    #[inline]
    pub fn mark_valid(&mut self) {
        self.magic = RTC_SESSION_MAGIC;
    }

    pub fn clear(&mut self) {
        self.magic = 0;
    }

    pub fn increment_wake_count(&mut self) {
        self.wake_count = self.wake_count.wrapping_add(1);
    }

    #[inline]
    pub fn wake_count(&self) -> u32 {
        self.wake_count
    }
}

// RTC FAST persistent storage
//
// This static is placed in .rtc_fast.persistent section which:
// - Survives deep sleep (RTC domain stays powered)
// - Is zeroed only on power-on reset (not deep sleep wake)
// - Requires RTC FAST memory to remain powered during sleep
//
// Safety: Access is through save()/load() which use volatile operations
// and are only called from single-threaded boot/sleep contexts.
#[unsafe(link_section = ".rtc_fast.persistent")]
static mut RTC_SESSION: RtcSession = RtcSession::zeroed();

// Atomic flag to track if we've detected a valid session this boot
// (prevents re-reading stale data after initial restore)
static SESSION_CONSUMED: AtomicU32 = AtomicU32::new(0);

// check if RTC session data is valid and available for restore
// returns true only once per boot (subsequent calls return false)
// must be called from main thread during boot, before async tasks
pub fn is_valid_session() -> bool {
    // Only allow one restore per boot
    if SESSION_CONSUMED.load(Ordering::Relaxed) != 0 {
        return false;
    }

    // Safety: single-threaded boot context, volatile read via raw pointer
    let valid = unsafe {
        let ptr = core::ptr::addr_of!(RTC_SESSION);
        core::ptr::read_volatile(core::ptr::addr_of!((*ptr).magic))
    } == RTC_SESSION_MAGIC;

    if valid {
        SESSION_CONSUMED.store(1, Ordering::Relaxed);
    }

    valid
}

// load session data (caller should check is_valid_session() first)
// must be called from main thread during boot
pub fn load() -> RtcSession {
    // Safety: single-threaded context, volatile read via raw pointer
    unsafe {
        let ptr = core::ptr::addr_of!(RTC_SESSION);
        core::ptr::read_volatile(ptr)
    }
}

// save session data before entering deep sleep
// must be called from main thread before sleep, after tasks stopped
pub fn save(session: &RtcSession) {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(RTC_SESSION);
        // Copy all fields
        core::ptr::write_volatile(ptr, *session);
        // Ensure magic is set (caller may have forgotten)
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*ptr).magic), RTC_SESSION_MAGIC);
    }
}

// clear RTC session data
pub fn clear() {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(RTC_SESSION);
        core::ptr::write_volatile(core::ptr::addr_of_mut!((*ptr).magic), 0);
    }
}

// get wake count for debugging (doesn't consume session)
pub fn wake_count() -> u32 {
    unsafe {
        let ptr = core::ptr::addr_of!(RTC_SESSION);
        core::ptr::read_volatile(core::ptr::addr_of!((*ptr).wake_count))
    }
}
