// Cooperative scheduler and wake/sleep primitives
// Single core, no preemption. WFI idles the CPU between events.
//
// block_on: minimal single-threaded executor for async hardware waits
// (display BUSY, GPIO edges). Runs one future to completion, WFI
// between polls. Not a general-purpose executor.

pub mod scheduler;
pub mod wake;

pub use scheduler::{Job, Scheduler};
pub use wake::block_on;
