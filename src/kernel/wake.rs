// Uptime helper backed by Embassy's monotonic clock.
//
// The old manual WFI loop, wake-flag atomics, tick counting, and
// `block_on` executor have all been removed.  Embassy's executor
// handles WFI internally, and `embassy_time::Instant` provides a
// high-resolution monotonic clock driven by the hardware timer that
// was previously managed by hand (TIMG0 periodic ISR).
//
// The power-button GPIO ISR still needs to clear its interrupt flag
// (handled in board/mod.rs).  Any interrupt — including GPIO3 — exits
// WFI and lets the Embassy executor re-poll its task queue, so no
// explicit signalling is required.

/// Seconds since boot, derived from Embassy's monotonic clock.
///
/// Resolution depends on the Embassy time-driver tick rate (typically
/// 1 MHz on ESP32-C3 with TIMG0), but we only expose whole seconds
/// for the status bar.
pub fn uptime_secs() -> u32 {
    // Instant::now() is zero-cost on single-core (no critical section).
    let ticks = embassy_time::Instant::now().as_ticks();
    // TICK_HZ is a const set by the esp-hal embassy time driver
    // (1_000_000 on ESP32-C3).  Integer division is fine for seconds.
    (ticks / embassy_time::TICK_HZ) as u32
}
