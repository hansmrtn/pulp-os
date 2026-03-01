// Kernel: Embassy async runtime wrapper.
// wake:  uptime helper (embassy_time)
// tasks: spawned tasks (input polling, housekeeping, idle sleep)

pub mod tasks;
pub mod wake;

pub use wake::uptime_secs;
