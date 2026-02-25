//! Minimal kernel for pulp-os
//!
//! Provides:
//! - Job scheduler with priority queues
//! - Adaptive polling for power efficiency
//! - Sleep/wake primitives

pub mod poll;
pub mod scheduler;
pub mod wake;

pub use poll::{AdaptivePoller, BASE_TICK_MS, PollRate};
pub use scheduler::{Job, Priority, PushError, Scheduler};
pub use wake::{WakeReason, signal_button, signal_display, signal_timer, sleep_until_wake};
