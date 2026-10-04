// battery calibration for the XTEink X4
// GPIO0 reads through 100K/100K divider (2:1); ADC 11dB attenuation
// gives 0..2500 mV; multiply by 2 for actual cell voltage

// voltage divider multiplier (100K/100K resistive divider)
pub const DIVIDER_MULT: u32 = 2;

// piecewise-linear li-ion discharge curve, sorted descending by mV; the table
// lives in pulp-board-logic (shared with the OnePage C61, T9) and is unchanged
pub use pulp_board_logic::battery::LIPO_DISCHARGE_CURVE as DISCHARGE_CURVE;
