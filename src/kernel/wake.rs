// Uptime helper backed by Embassy's monotonic clock.

// seconds since boot
pub fn uptime_secs() -> u32 {
    let ticks = embassy_time::Instant::now().as_ticks();
    // TICK_HZ = 1_000_000 on ESP32-C3; integer division is fine for seconds
    (ticks / embassy_time::TICK_HZ) as u32
}
