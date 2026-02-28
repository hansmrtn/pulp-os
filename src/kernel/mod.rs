// Kernel module — Embassy-based async runtime
//
// The hand-rolled cooperative scheduler and WFI/wake primitives have
// been replaced by Embassy's executor and timer driver.  This module
// now provides only a thin uptime helper (backed by embassy_time) and
// a no-op signal hook for the power-button GPIO ISR.

pub mod wake;

pub use wake::uptime_secs;
