// Kernel module — Embassy-based async runtime
//
// The hand-rolled cooperative scheduler and WFI/wake primitives have
// been replaced by Embassy's executor and timer driver.  This module
// provides:
//
//   • `wake`  — thin uptime helper (backed by embassy_time)
//   • `tasks` — spawned Embassy tasks (input polling, housekeeping)

pub mod tasks;
pub mod wake;

pub use wake::uptime_secs;
