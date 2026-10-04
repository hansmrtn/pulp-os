// Stand-in for crate::board on the host: the same names input.rs uses
// (InputHw, power_button_is_low, button::*), with the real button.rs.
use std::cell::Cell;
thread_local! {
    static INPUTS: Cell<(bool, u16, u16)> = const { Cell::new((false, 3300, 3300)) };
    static COUNT: Cell<(u64, u64)> = const { Cell::new((0, 0)) }; // (adc reads, power reads)
}
pub fn set_inputs(p: bool, a: u16, b: u16) { INPUTS.with(|i| i.set((p, a, b))); }
pub fn counters() -> (u64, u64) { COUNT.with(|c| c.get()) }

#[path = "@ROOT@/kernel/src/board/button.rs"]
pub mod button;

#[derive(Clone, Copy)]
pub enum Row { One, Two, Battery }
pub struct AdcPin(pub Row);
pub struct Adc;
impl Adc {
    pub fn read_oneshot(&mut self, pin: &mut AdcPin) -> nb::Result<u16, ()> {
        COUNT.with(|c| { let (a, p) = c.get(); c.set((a + 1, p)); });
        let (_, a, b) = INPUTS.with(|i| i.get());
        Ok(match pin.0 { Row::One => a, Row::Two => b, Row::Battery => 4000 })
    }
}
pub struct InputHw { pub adc: Adc, pub row1: AdcPin, pub row2: AdcPin, pub battery: AdcPin }
impl InputHw {
    pub fn new() -> Self {
        InputHw { adc: Adc, row1: AdcPin(Row::One), row2: AdcPin(Row::Two), battery: AdcPin(Row::Battery) }
    }
}
pub fn power_button_is_low() -> bool {
    COUNT.with(|c| { let (a, p) = c.get(); c.set((a, p + 1)); });
    INPUTS.with(|i| i.get().0)
}
