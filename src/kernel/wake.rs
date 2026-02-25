use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// Wake source flags (set by ISR, cleared by main loop)
static WAKE_BUTTON: AtomicBool = AtomicBool::new(false);
static WAKE_DISPLAY: AtomicBool = AtomicBool::new(false);
static WAKE_TIMER: AtomicBool = AtomicBool::new(false);

// How many base ticks (10ms) each timer interrupt represents.
// Normally 1. When the timer is slowed to 100ms, set to 10
// so uptime_ticks() stays in consistent 10ms units.
static TICK_WEIGHT: AtomicU32 = AtomicU32::new(1);

// Uptime in base ticks (10ms each), regardless of actual timer period.
// Protected by critical section because riscv32imc lacks atomic RMW.
static UPTIME_TICKS: critical_section::Mutex<core::cell::Cell<u32>> =
    critical_section::Mutex::new(core::cell::Cell::new(0));

/// Which wake sources fired since the last check.
///
/// Each flag is independent — multiple sources can fire between
/// checks and none are lost. The main loop tests each flag it
/// cares about and dispatches accordingly.
#[derive(Debug, Clone, Copy)]
pub struct WakeFlags {
    pub button: bool,
    pub display: bool,
    pub timer: bool,
}

impl WakeFlags {
    /// True if any input-related source fired (button or timer).
    /// Timer wakes always poll input because the ADC-based buttons
    /// are sampled on the timer tick.
    #[inline]
    pub fn has_input(&self) -> bool {
        self.button || self.timer
    }
}

/// Atomically read and clear all pending wake flags.
/// Returns None if nothing fired.
fn take_wake_flags() -> Option<WakeFlags> {
    critical_section::with(|_| {
        let button = WAKE_BUTTON.load(Ordering::Relaxed);
        let display = WAKE_DISPLAY.load(Ordering::Relaxed);
        let timer = WAKE_TIMER.load(Ordering::Relaxed);

        if !button && !display && !timer {
            return None;
        }

        // Clear only the flags we observed
        if button {
            WAKE_BUTTON.store(false, Ordering::Relaxed);
        }
        if display {
            WAKE_DISPLAY.store(false, Ordering::Relaxed);
        }
        if timer {
            WAKE_TIMER.store(false, Ordering::Relaxed);
        }

        Some(WakeFlags {
            button,
            display,
            timer,
        })
    })
}

// power button was pressed.
#[inline]
pub fn signal_button() {
    WAKE_BUTTON.store(true, Ordering::Release);
}

// signal that the display finished refreshing.
#[inline]
pub fn signal_display() {
    WAKE_DISPLAY.store(true, Ordering::Release);
}

// signal a timer tick.
#[inline]
pub fn signal_timer() {
    WAKE_TIMER.store(true, Ordering::Release);
    let weight = TICK_WEIGHT.load(Ordering::Relaxed);
    critical_section::with(|cs| {
        let ticks = UPTIME_TICKS.borrow(cs);
        ticks.set(ticks.get().wrapping_add(weight));
    });
}

/// Set the tick weight — how many base ticks (10ms) each timer
/// interrupt represents. Called when the timer period changes.
pub fn set_tick_weight(weight: u32) {
    TICK_WEIGHT.store(weight, Ordering::Release);
}

/// Uptime in base ticks (10ms each) since boot.
/// Stays consistent regardless of actual timer period.
pub fn uptime_ticks() -> u32 {
    critical_section::with(|cs| UPTIME_TICKS.borrow(cs).get())
}

/// Uptime in seconds since boot.
pub fn uptime_secs() -> u32 {
    uptime_ticks() / 100
}

#[inline]
pub fn wait_for_interrupt() {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }

    // For testing on host
    #[cfg(not(target_arch = "riscv32"))]
    {
        // On host, just yield to simulate
        std::thread::yield_now();
    }
}

/// Block until a wake event occurs.
/// Used for deep sleep / idle patterns where the caller
/// wants to hand off control entirely until something happens.
pub fn sleep_until_wake() -> WakeFlags {
    loop {
        if let Some(flags) = take_wake_flags() {
            return flags;
        }

        wait_for_interrupt();
    }
}

/// Non-blocking wake check. Returns the pending wake flags
/// (consuming them) or None if nothing fired.
pub fn try_wake() -> Option<WakeFlags> {
    take_wake_flags()
}
