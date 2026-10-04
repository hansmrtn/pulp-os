// Stimulus for the X4 InputDriver: scripted scenarios plus a deterministic
// pseudo-random walk over (power button, row1 mV, row2 mV, poll spacing,
// reset_hold_state). The trace records every returned event with the poll
// time and the cumulative ADC read / power-pin read counters, so it locks
// events, their timing, and which polls touched the hardware.
use crate::board::{self, InputHw};
use crate::drivers::input::InputDriver;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 { self.next() % n }
}

fn drive(drv: &mut InputDriver, out: &mut Vec<String>, t_us: &mut u64, steps: &[(u64, bool, u16, u16)]) {
    // (advance_us, power_low, mv1, mv2) -> one poll each
    for &(adv, p, a, b) in steps {
        *t_us += adv;
        esp_hal::set_now_us(*t_us);
        board::set_inputs(p, a, b);
        if let Some(ev) = drv.poll() {
            let (adc, pwr) = board::counters();
            out.push(format!("t={} {:?} adc={} pwr={}", *t_us, ev, adc, pwr));
        }
    }
}

pub fn run() -> Vec<String> {
    let mut out = Vec::new();
    let mut t: u64 = 5_000;
    esp_hal::set_now_us(t);
    let mut drv = InputDriver::new(InputHw::new());
    const REST: u16 = 3300;

    // 1. single press -> release with 10 ms polling (debounce, long press, repeat)
    out.push("# scenario 1: Left held 2.2 s at 10 ms polls".into());
    let mut s = vec![(10_000u64, false, REST, REST); 5];
    s.extend(vec![(10_000, false, 1113, REST); 220]);
    s.extend(vec![(10_000, false, REST, REST); 5]);
    drive(&mut drv, &mut out, &mut t, &s);

    // 2. bounce shorter than debounce does not produce events
    out.push("# scenario 2: sub-debounce bounces".into());
    let mut s = Vec::new();
    for _ in 0..20 { s.push((4_000, false, 1984, REST)); s.push((4_000, false, REST, REST)); }
    s.extend(vec![(10_000, false, REST, REST); 5]);
    drive(&mut drv, &mut out, &mut t, &s);

    // 3. exact boundary polls: 14/15/16 ms stable, 999/1000 ms hold, 149/150 repeat
    out.push("# scenario 3: boundary timings (1 ms resolution)".into());
    let mut s = vec![(1_000u64, false, REST, REST)];
    s.extend(vec![(1_000, false, 2556, REST); 15]);   // press accepted at 15th ms
    s.extend(vec![(1_000, false, 2556, REST); 1100]); // long press + repeats
    s.extend(vec![(1_000, false, REST, REST); 30]);
    drive(&mut drv, &mut out, &mut t, &s);

    // 4. power button priority and row2 fallthrough
    out.push("# scenario 4: power priority, row2, switching keys".into());
    let mut s = vec![];
    s.extend(vec![(10_000u64, false, REST, 1659); 30]);
    s.extend(vec![(10_000, true, 1984, 1659); 30]);
    s.extend(vec![(10_000, false, 1984, REST); 30]);
    s.extend(vec![(10_000, false, 3, REST); 30]);
    s.extend(vec![(10_000, false, REST, 3); 120]);
    s.extend(vec![(10_000, true, REST, REST); 150]);
    s.extend(vec![(10_000, false, REST, REST); 10]);
    drive(&mut drv, &mut out, &mut t, &s);

    // 5. reset_hold_state while held: no LongPress / Repeat until release
    out.push("# scenario 5: reset_hold_state".into());
    let mut s = vec![(10_000u64, false, 2556, REST); 20];
    drive(&mut drv, &mut out, &mut t, &s);
    drv.reset_hold_state();
    s = vec![(10_000, false, 2556, REST); 250];
    s.extend(vec![(10_000, false, REST, REST); 10]);
    s.extend(vec![(10_000, false, 2556, REST); 130]);
    s.extend(vec![(10_000, false, REST, REST); 10]);
    drive(&mut drv, &mut out, &mut t, &s);

    // 6. pseudo-random walk
    out.push("# scenario 6: pseudo-random walk".into());
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let row1 = [REST, REST, 3, 1113, 1984, 2556, 700, 4095, 1100, 1263, 1264, 2000];
    let row2 = [REST, REST, 3, 1659, 900, 1500, 1809, 1810];
    let mut p = false;
    let (mut a, mut b) = (REST, REST);
    for i in 0..300_000u32 {
        // hold the current inputs for a random stretch of polls
        // first half: fast churn; second half: long holds (LongPress / Repeat)
        let churn = if i < 150_000 { 6 } else { 150 };
        if rng.below(churn) == 0 { p = rng.below(8) == 0; }
        if rng.below(churn) == 0 { a = row1[rng.below(row1.len() as u64) as usize]; }
        if rng.below(churn) == 0 { b = row2[rng.below(row2.len() as u64) as usize]; }
        let adv = match rng.below(4) { 0 => 1 + rng.below(1_000), 1 => 1_000 + rng.below(20_000), _ => 10_000 };
        if rng.below(2000) == 0 { drv.reset_hold_state(); out.push(format!("t={} reset_hold_state", t)); }
        let step = [(adv, p, a, b)];
        drive(&mut drv, &mut out, &mut t, &step);
        if i % 50_000 == 0 {
            let (adc, pwr) = board::counters();
            out.push(format!("i={} t={} adc={} pwr={}", i, t, adc, pwr));
        }
    }
    let (adc, pwr) = board::counters();
    out.push(format!("final adc={} pwr={} events={}", adc, pwr, out.len()));
    out
}
