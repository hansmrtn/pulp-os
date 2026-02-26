// Li-ion battery voltage estimation
//
// GPIO0 reads through a 100K/100K divider (2:1). ADC with 11dB
// attenuation gives 0..2500mV; multiply by 2 for actual cell voltage.
// Linear approximation: 4200mV = 100%, 3000mV = 0%.

const DIVIDER_MULT: u32 = 2;

const VBAT_FULL_MV: u32 = 4200;
const VBAT_EMPTY_MV: u32 = 3000;

pub fn adc_to_battery_mv(adc_mv: u16) -> u16 {
    (adc_mv as u32 * DIVIDER_MULT) as u16
}

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
