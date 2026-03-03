// battery voltage estimation --- generic over board calibration.
// board-specific divider ratio and discharge curve live in board::battery.

use crate::board::battery::{DISCHARGE_CURVE, DIVIDER_MULT};

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
