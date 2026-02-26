// GPIO pin assignments for XTEink X4 (ESP32-C3)
//
// GPIO  Function         Notes
//  0    ADC battery      voltage divider, 100K/100K
//  1    ADC row1         resistance ladder: Right/Left/Confirm/Back
//  2    ADC row2         resistance ladder: VolUp/VolDown
//  3    power button     active low, internal pullup
//  4    EPD DC           data/command select
//  5    EPD RST          active low
//  6    EPD BUSY         high while controller is working
//  7    SPI MISO         SD card only, display is write only
//  8    SPI SCK          shared clock
// 10    SPI MOSI         shared data out
// 12    SD CS            flash pin SPIHD, free in DIO mode
// 21    EPD CS           display chip select

#![allow(dead_code)]

pub const EPD_CS: u8 = 21;
pub const EPD_DC: u8 = 4;
pub const EPD_RST: u8 = 5;
pub const EPD_BUSY: u8 = 6;

pub const SPI_SCK: u8 = 8;
pub const SPI_MOSI: u8 = 10;
pub const SPI_MISO: u8 = 7;

pub const SD_CS: u8 = 12;

pub const BAT_ADC: u8 = 0;

pub const BTN_ROW1_ADC: u8 = 1;
pub const BTN_ROW2_ADC: u8 = 2;

pub const BTN_POWER: u8 = 3;
