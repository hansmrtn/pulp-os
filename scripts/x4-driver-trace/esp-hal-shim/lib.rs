use std::cell::RefCell;
thread_local! { pub static LOG: RefCell<Vec<String>> = RefCell::new(Vec::new()); pub static NOW: RefCell<u64> = RefCell::new(0); }
pub fn take_log() -> Vec<String> { LOG.with(|l| std::mem::take(&mut *l.borrow_mut())) }
pub fn log(s: String) { LOG.with(|l| l.borrow_mut().push(s)); }
pub mod delay {
    pub struct Delay;
    impl Delay {
        pub fn new() -> Self { Delay }
        pub fn delay_millis(&mut self, ms: u32) { super::log(format!("DELAY {}", ms)); super::NOW.with(|n| *n.borrow_mut() += ms as u64); }
    }
}
pub mod time {
    #[derive(Copy, Clone, PartialEq, PartialOrd)]
    pub struct Instant(u64);
    pub struct Duration(u64);
    impl Duration { pub fn from_millis(ms: u64) -> Self { Duration(ms) } }
    impl Instant { pub fn now() -> Self { Instant(super::NOW.with(|n| { let mut n = n.borrow_mut(); *n += 1; *n })) } }
    impl core::ops::Add<Duration> for Instant { type Output = Instant; fn add(self, d: Duration) -> Instant { Instant(self.0 + d.0) } }
}
