// uptime helper backed by embassy's monotonic clock
pub fn uptime_secs() -> u32 {
    let ticks = embassy_time::Instant::now().as_ticks();
    (ticks / embassy_time::TICK_HZ) as u32
}
