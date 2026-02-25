// "Adaptive" polling for power-efficient input handling
// NOTE: Instead of polling attempt to adjust based on activity:
// - active input: poll fast (10ms) for responsive debouncing
// - recently active: poll moderate (50ms)
// - idle: smoll poll (100ms) to save power

use core::fmt;

/// Base timer tick interval (ms)
pub const BASE_TICK_MS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PollRate {
    #[default]
    Fast,
    Normal,
    Slow,
}

impl PollRate {
    // How many base ticks between polls at this rate
    pub const fn divisor(self) -> u32 {
        match self {
            PollRate::Fast => 1,
            PollRate::Normal => 5,
            PollRate::Slow => 10,
        }
    }

    // Effective polling interval in milliseconds
    pub const fn interval_ms(self) -> u32 {
        self.divisor() * BASE_TICK_MS
    }
}

impl fmt::Display for PollRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PollRate::Fast => write!(f, "Fast({}ms)", self.interval_ms()),
            PollRate::Normal => write!(f, "Normal({}ms)", self.interval_ms()),
            PollRate::Slow => write!(f, "Slow({}ms)", self.interval_ms()),
        }
    }
}

// Thresholds for rate transitions 
mod thresholds {
    pub const FAST_TO_NORMAL: u32 = 20; // 20 × 10ms = 200ms
    pub const NORMAL_TO_SLOW: u32 = 20; // 20 × 50ms = 1000ms
}

// Tracks activity and adjusts polling rate accordingly(?)
pub struct AdaptivePoller {
    rate: PollRate,
    // since last poll
    tick_count: u32,
    // consecutive idle polls 
    idle_count: u32,
}

impl AdaptivePoller {
    pub const fn new() -> Self {
        Self {
            rate: PollRate::Fast, // default to blazingly fast
            tick_count: 0,
            idle_count: 0,
        }
    }

    pub fn tick(&mut self) -> bool {
        self.tick_count += 1;

        if self.tick_count >= self.rate.divisor() {
            self.tick_count = 0;
            true
        } else {
            false
        }
    }

    pub fn on_activity(&mut self) {
        self.rate = PollRate::Fast;
        self.idle_count = 0;
    }

    pub fn on_idle(&mut self) {
        self.idle_count += 1;

        match self.rate {
            PollRate::Fast => {
                if self.idle_count >= thresholds::FAST_TO_NORMAL {
                    self.rate = PollRate::Normal;
                    self.idle_count = 0;
                }
            }
            PollRate::Normal => {
                if self.idle_count >= thresholds::NORMAL_TO_SLOW {
                    self.rate = PollRate::Slow;
                    // Keep incrementing idle_count but rate won't change further
                }
            }
            PollRate::Slow => {
                // Already at slowest rate, nothing to do
            }
        }
    }

    pub fn rate(&self) -> PollRate {
        self.rate
    }

    pub fn interval_ms(&self) -> u32 {
        self.rate.interval_ms()
    }

    pub fn idle_count(&self) -> u32 {
        self.idle_count
    }

    // force a specific rate
    pub fn set_rate(&mut self, rate: PollRate) {
        self.rate = rate;
        self.idle_count = 0;
        self.tick_count = 0;
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for AdaptivePoller {
    fn default() -> Self {
        Self::new()
    }
}

