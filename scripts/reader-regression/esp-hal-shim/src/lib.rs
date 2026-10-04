// Host stand-in for esp-hal: only the type kernel/src/kernel/app.rs names in the
// AppLayer trait signature (esp_hal::delay::Delay). Nothing here is behaviour.
pub mod delay {
    #[derive(Clone, Copy, Default)]
    pub struct Delay;
}
