// Wake flag signaling between ISRs and the main loop
//
// ISRs set atomic flags; main loop consumes via try_wake().
// Independent flags prevent concurrent sources from swallowing
// each other. Critical section guards riscv32imc (no atomic RMW).
// Uptime tracked in 10ms base ticks; TICK_WEIGHT compensates
// when the timer slows to 100ms during idle.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

static WAKE_BUTTON: AtomicBool = AtomicBool::new(false);
static WAKE_DISPLAY: AtomicBool = AtomicBool::new(false);
static WAKE_TIMER: AtomicBool = AtomicBool::new(false);

// 10ms base ticks per timer interrupt (1 or 10)
static TICK_WEIGHT: AtomicU32 = AtomicU32::new(1);

// cs: riscv32imc has no atomic add
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

pub fn try_wake() -> Option<WakeFlags> {
    take_wake_flags()
}
