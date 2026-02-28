// Wake flag signaling between ISRs and the main loop, plus a
// minimal single-threaded executor (`block_on`) that WFIs between
// polls so the CPU sleeps while awaiting async hardware operations.
//
// ISRs set atomic flags; main loop consumes via try_wake().
// Independent flags prevent concurrent sources from swallowing
// each other. Critical section guards riscv32imc (no atomic RMW).
//
// Uptime tracked in 10ms base ticks; TICK_WEIGHT compensates
// when the timer slows to 100ms during idle.
//
// `block_on(future)` is the bridge between the synchronous main
// loop and async esp-hal operations (e.g. display BUSY pin await,
// GPIO edge waits). It polls the future, doing WFI between polls
// so the CPU truly sleeps instead of spinning.

use core::future::Future;
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use core::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

static WAKE_BUTTON: AtomicBool = AtomicBool::new(false);
static WAKE_TIMER: AtomicBool = AtomicBool::new(false);

// 10ms base ticks per timer interrupt (1 or 10)
static TICK_WEIGHT: AtomicU32 = AtomicU32::new(1);

// cs: riscv32imc has no atomic add
static UPTIME_TICKS: critical_section::Mutex<core::cell::Cell<u32>> =
    critical_section::Mutex::new(core::cell::Cell::new(0));

// ── Wake flags ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct WakeFlags {
    pub button: bool,
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
        let timer = WAKE_TIMER.load(Ordering::Relaxed);

        if !button && !timer {
            return None;
        }

        if button {
            WAKE_BUTTON.store(false, Ordering::Relaxed);
        }
        if timer {
            WAKE_TIMER.store(false, Ordering::Relaxed);
        }

        Some(WakeFlags { button, timer })
    })
}

// ── ISR signal functions ────────────────────────────────────────────

#[inline]
pub fn signal_button() {
    WAKE_BUTTON.store(true, Ordering::Release);
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

// ── Tick weight / uptime ────────────────────────────────────────────

pub fn set_tick_weight(weight: u32) {
    TICK_WEIGHT.store(weight, Ordering::Release);
}

pub fn uptime_ticks() -> u32 {
    critical_section::with(|cs| UPTIME_TICKS.borrow(cs).get())
}

pub fn uptime_secs() -> u32 {
    uptime_ticks() / 100
}

// ── WFI primitive ───────────────────────────────────────────────────

/// Put the CPU to sleep until the next interrupt fires.
///
/// On riscv32 this is a single `wfi` instruction. Any enabled
/// interrupt (timer, GPIO, etc.) will wake the core, at which point
/// pending ISRs run and execution resumes after the `wfi`.
#[inline]
pub fn wait_for_interrupt() {
    #[cfg(target_arch = "riscv32")]
    unsafe {
        core::arch::asm!("wfi", options(nomem, nostack));
    }

    #[cfg(not(target_arch = "riscv32"))]
    {
        // Desktop testing fallback
        std::thread::yield_now();
    }
}

// ── Main loop wake consumer ─────────────────────────────────────────

/// Atomically drain all pending wake flags set by ISRs.
///
/// Returns `None` if no ISR has signaled since the last call.
pub fn try_wake() -> Option<WakeFlags> {
    take_wake_flags()
}

// ── Minimal single-threaded executor ────────────────────────────────
//
// `block_on` polls a future to completion, sleeping the CPU between
// polls via `wfi`. Because we're single-threaded and all wake sources
// are hardware interrupts, we don't need a "real" waker — any
// interrupt (timer, GPIO edge, display BUSY) will exit WFI and let
// us re-poll.
//
// This is *not* a general-purpose executor. It runs exactly one
// future at a time, synchronously, and is intended for short async
// sequences like display refresh waits where we'd otherwise
// spin-poll.

/// A no-op waker. On single-core riscv32imc, the hardware interrupt
/// that completes the awaited operation also exits WFI, so we never
/// need the waker to explicitly schedule anything.
fn noop_raw_waker() -> RawWaker {
    fn no_op(_: *const ()) {}
    fn clone(data: *const ()) -> RawWaker {
        RawWaker::new(data, &VTABLE)
    }
    const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, no_op, no_op, no_op);
    RawWaker::new(core::ptr::null(), &VTABLE)
}

/// Run a future to completion, sleeping the CPU between polls.
///
/// # Usage
///
/// ```ignore
/// use pulp_os::kernel::wake::block_on;
///
/// // Await the display BUSY pin going low (async GPIO wait).
/// block_on(epd.render_full_async(&mut strip, &mut delay, |s| {
///     statusbar.draw(s).unwrap();
///     app.draw(s);
/// }));
/// ```
///
/// While the future is pending, the CPU executes WFI between polls,
/// achieving the same power savings as the ISR + flag approach but
/// with a cleaner async API for hardware waits.
///
/// # Cancellation
///
/// The future runs to completion — there is no cancellation mechanism.
/// Do not pass futures that might never resolve.
pub fn block_on<F: Future>(mut future: F) -> F::Output {
    // SAFETY: we never move `future` after pinning it here, and this
    // function owns it until completion.
    let mut future = unsafe { Pin::new_unchecked(&mut future) };

    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);

    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(val) => return val,
            Poll::Pending => wait_for_interrupt(),
        }
    }
}
