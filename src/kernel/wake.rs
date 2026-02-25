use core::sync::atomic::{AtomicBool, Ordering};

// Wake source flags (set by ISR and  cleared by main loop)
static WAKE_BUTTON: AtomicBool = AtomicBool::new(false);
static WAKE_DISPLAY: AtomicBool = AtomicBool::new(false);
static WAKE_TIMER: AtomicBool = AtomicBool::new(false);

// who done woke us up from sleep
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    Button,
    Display,
    Timer,
    Multiple,
}

pub fn take_wake_reason() -> Option<WakeReason> {
    critical_section::with(|_| {
        let button = WAKE_BUTTON.load(Ordering::Relaxed);
        let display = WAKE_DISPLAY.load(Ordering::Relaxed);
        let timer = WAKE_TIMER.load(Ordering::Relaxed);

        // Clear flags we read
        if button {
            WAKE_BUTTON.store(false, Ordering::Relaxed);
        }
        if display {
            WAKE_DISPLAY.store(false, Ordering::Relaxed);
        }
        if timer {
            WAKE_TIMER.store(false, Ordering::Relaxed);
        }

        match (button, display, timer) {
            (false, false, false) => None,
            (true, false, false) => Some(WakeReason::Button),
            (false, true, false) => Some(WakeReason::Display),
            (false, false, true) => Some(WakeReason::Timer),
            _ => Some(WakeReason::Multiple),
        }
    })
}

// Check pending waits (without clearing)
pub fn has_pending_wake() -> bool {
    WAKE_BUTTON.load(Ordering::Acquire)
        || WAKE_DISPLAY.load(Ordering::Acquire)
        || WAKE_TIMER.load(Ordering::Acquire)
}

// Check individual wake sources (without clearing)
pub fn is_button_pending() -> bool {
    WAKE_BUTTON.load(Ordering::Acquire)
}

pub fn is_display_pending() -> bool {
    WAKE_DISPLAY.load(Ordering::Acquire)
}

pub fn is_timer_pending() -> bool {
    WAKE_TIMER.load(Ordering::Acquire)
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

pub fn sleep_until_wake() -> WakeReason {
    loop {
        if let Some(reason) = take_wake_reason() {
            return reason;
        }

        wait_for_interrupt();
    }
}

// Non-blocking wake check.
// If there's a pending wake reason, return it.
pub fn try_wake() -> Option<WakeReason> {
    take_wake_reason()
}

pub fn clear_all_flags() {
    WAKE_BUTTON.store(false, Ordering::Release);
    WAKE_DISPLAY.store(false, Ordering::Release);
    WAKE_TIMER.store(false, Ordering::Release);
}

pub fn pending_flags() -> (bool, bool, bool) {
    (
        WAKE_BUTTON.load(Ordering::Acquire),
        WAKE_DISPLAY.load(Ordering::Acquire),
        WAKE_TIMER.load(Ordering::Acquire),
    )
}
