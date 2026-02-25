//! Minimal kernel for pulp-os
//!
//! Provides:
//! - Job scheduler with priority queues
//! - Adaptive polling for power efficiency
//! - Sleep/wake primitives

pub mod poll;
pub mod scheduler;
pub mod wake;

pub use scheduler::{Job, Priority, PushError, Scheduler};
pub use poll::{PollRate, AdaptivePoller, BASE_TICK_MS};
pub use wake::{WakeReason, sleep_until_wake, signal_button, signal_display, signal_timer};
