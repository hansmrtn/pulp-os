//! Battery monitoring for XTEink X4
//!
//! GPIO0 reads battery voltage through an on-board voltage divider (1:1, 100K/100K).
//! ADC with 11dB attenuation reads 0-2500mV; multiply by 2 for actual battery voltage.
//! Li-ion cell: 4200mV = 100%, 3000mV = 0%.

/// Voltage divider ratio (100K/100K = 2:1)
const DIVIDER_MULT: u32 = 2;

/// Li-ion voltage bounds in millivolts
const VBAT_FULL_MV: u32 = 4200;
const VBAT_EMPTY_MV: u32 = 3000;

/// Convert ADC millivolts (post-calibration) to actual battery millivolts.
pub fn adc_to_battery_mv(adc_mv: u16) -> u16 {
    (adc_mv as u32 * DIVIDER_MULT) as u16
}

/// Battery voltage to charge percentage (0-100), linear approximation.
pub fn battery_percentage(battery_mv: u16) -> u8 {
    let mv = battery_mv as u32;
    if mv >= VBAT_FULL_MV {
        100
    } else if mv <= VBAT_EMPTY_MV {
        0
    } else {
        ((mv - VBAT_EMPTY_MV) * 100 / (VBAT_FULL_MV - VBAT_EMPTY_MV)) as u8
    }
}
