// battery calibration for the XTEink X4.
// GPIO0 reads through 100K/100K divider (2:1); ADC 11dB attenuation
// gives 0..2500 mV --- multiply by 2 for actual cell voltage.

// voltage divider multiplier (100K/100K resistive divider).
pub const DIVIDER_MULT: u32 = 2;

// piecewise-linear li-ion discharge curve. sorted descending by mV.
pub const DISCHARGE_CURVE: &[(u32, u8)] = &[
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
