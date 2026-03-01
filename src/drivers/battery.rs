// Li-ion battery voltage estimation.
// GPIO0 reads through 100K/100K divider (2:1); ADC 11dB attenuation gives
// 0..2500mV; multiply by 2 for actual cell voltage.
// Piecewise-linear LUT models the discharge curve.

const DIVIDER_MULT: u32 = 2;

// (millivolts, percentage); must be sorted descending by mV
const DISCHARGE_CURVE: &[(u32, u8)] = &[
    (4200, 100),
    (4060, 90),
    (3980, 80),
    (3920, 70),
    (3870, 60),
    (3830, 50),
    (3790, 40),
    (3750, 30),
    (3700, 20),
    (3600, 10),
    (3400, 5),
    (3000, 0),
];

pub fn adc_to_battery_mv(adc_mv: u16) -> u16 {
    (adc_mv as u32 * DIVIDER_MULT) as u16
}

pub fn battery_percentage(battery_mv: u16) -> u8 {
    let mv = battery_mv as u32;

    if mv >= DISCHARGE_CURVE[0].0 {
        return DISCHARGE_CURVE[0].1;
    }

    let last = DISCHARGE_CURVE.len() - 1;
    if mv <= DISCHARGE_CURVE[last].0 {
        return DISCHARGE_CURVE[last].1;
    }

    // interpolate between bracketing points
    let mut i = 0;
    while i + 1 < DISCHARGE_CURVE.len() {
        let (mv_hi, pct_hi) = DISCHARGE_CURVE[i];
        let (mv_lo, pct_lo) = DISCHARGE_CURVE[i + 1];
        if mv >= mv_lo {
            let span_mv = mv_hi - mv_lo;
            if span_mv == 0 {
                return pct_hi;
            }
            let span_pct = (pct_hi - pct_lo) as u32;
            let frac = mv - mv_lo;
            return (pct_lo as u32 + frac * span_pct / span_mv) as u8;
        }
        i += 1;
    }

    0
}
