// Kernel: Embassy async runtime wrapper.
// wake:  uptime helper (embassy_time)
// tasks: spawned tasks (input polling, housekeeping, idle sleep)
// work_queue: background task for CPU-heavy book/image caching

pub mod tasks;
pub mod wake;
pub mod work_queue;

pub use wake::uptime_secs;
