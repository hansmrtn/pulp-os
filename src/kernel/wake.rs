// Wake flag signaling between ISRs and the main loop
//
// ISRs set atomic flags; the main loop consumes them via try_wake().
// Flags are independent so concurrent sources (button + timer + display)
// never swallow each other. All cleared atomically inside a critical
// section to prevent races on riscv32imc (no hardware atomic RMW).
//
// Uptime is tracked in 10ms base ticks regardless of actual timer
// period. When the timer slows to 100ms during idle, TICK_WEIGHT
// is set to 10 so the counter stays in consistent units.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static WAKE_BUTTON: AtomicBool = AtomicBool::new(false);
static WAKE_DISPLAY: AtomicBool = AtomicBool::new(false);
static WAKE_TIMER: AtomicBool = AtomicBool::new(false);

// how many 10ms base ticks each timer interrupt represents (1 or 10)
static TICK_WEIGHT: AtomicU32 = AtomicU32::new(1);

// critical section because riscv32imc has no atomic add
static UPTIME_TICKS: critical_section::Mutex<core::cell::Cell<u32>> =
    critical_section::Mutex::new(core::cell::Cell::new(0));

#[derive(Debug, Clone, Copy)]
pub struct WakeFlags {
    pub button: bool,
    pub display: bool,
    pub timer: bool,
}

impl WakeFlags {
    #[inline]
    pub fn has_input(&self) -> bool {
        self.button || self.timer
    }
}

fn take_wake_flags() -> Option<WakeFlags> {
    critical_section::with(|_| {
        let button = WAKE_BUTTON.load(Ordering::Relaxed);
        let display = WAKE_DISPLAY.load(Ordering::Relaxed);
        let timer = WAKE_TIMER.load(Ordering::Relaxed);

        if !button && !display && !timer {
            return None;
        }

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

#[inline]
pub fn signal_button() {
    WAKE_BUTTON.store(true, Ordering::Release);
}

#[inline]
pub fn signal_display() {
    WAKE_DISPLAY.store(true, Ordering::Release);
}

#[inline]
pub fn signal_timer() {
    WAKE_TIMER.store(true, Ordering::Release);
    let weight = TICK_WEIGHT.load(Ordering::Relaxed);
    critical_section::with(|cs| {
        let ticks = UPTIME_TICKS.borrow(cs);
        ticks.set(ticks.get().wrapping_add(weight));
    });
}

pub fn set_tick_weight(weight: u32) {
    TICK_WEIGHT.store(weight, Ordering::Release);
}

pub fn uptime_ticks() -> u32 {
    critical_section::with(|cs| UPTIME_TICKS.borrow(cs).get())
}

pub fn uptime_secs() -> u32 {
    uptime_ticks() / 100
}

#[inline]
pub fn wait_for_interrupt() {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        std::thread::yield_now();
    }
}

pub fn sleep_until_wake() -> WakeFlags {
    loop {
        if let Some(flags) = take_wake_flags() {
            return flags;
        }

        wait_for_interrupt();
    }
}

pub fn try_wake() -> Option<WakeFlags> {
    take_wake_flags()
}
