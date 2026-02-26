// Cooperative scheduler and wake/sleep primitives
// Single core, no preemption. WFI idles the CPU between events.

pub mod scheduler;
pub mod wake;

pub use scheduler::{Job, Scheduler};
