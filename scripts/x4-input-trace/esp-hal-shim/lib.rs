// Minimal esp-hal stand-in for host-compiling kernel/src/drivers/input.rs:
// only esp_hal::time::{Instant, Duration} (microsecond clock under harness
// control). Surface mirrors esp-hal 1.2: Instant::now, Instant - Instant ->
// Duration, Instant::duration_since_epoch, Duration::{from_millis, as_millis,
// as_micros}.
use std::cell::Cell;
thread_local! { pub static NOW_US: Cell<u64> = const { Cell::new(0) }; }
pub fn set_now_us(us: u64) { NOW_US.with(|n| n.set(us)); }
pub mod time {
    #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant(u64);
    #[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Duration(u64);
    impl Duration {
        pub fn from_millis(ms: u64) -> Self { Duration(ms * 1000) }
        pub fn as_millis(&self) -> u64 { self.0 / 1000 }
        pub fn as_micros(&self) -> u64 { self.0 }
    }
    impl Instant {
        pub fn now() -> Self { Instant(super::NOW_US.with(|n| n.get())) }
        pub fn duration_since_epoch(&self) -> Duration { Duration(self.0) }
    }
    impl core::ops::Sub for Instant {
        type Output = Duration;
        fn sub(self, o: Instant) -> Duration { Duration(self.0 - o.0) }
    }
}
