//! GPIO |     Function    |      Notes
//! -----+-----------------+----------------------------------
//!  0   | ADC - Battery   | Battery voltage sense (if wired)
//!  1   | ADC1 - Button 2 | Resistance ladder: Right/Left/Confirm/Back
//!  2   | ADC2 - Button 1 | Resistance ladder: Volume Up/Down
//!  3   | Digital - Power | Active LOW, internal pullup
//!  4   | EPD DC          | Data/Command select
//!  5   | EPD RST         | Reset (active low)
//!  6   | EPD BUSY        | Busy signal from display
//!  7   | SPI2 MISO       | SD card data in (display is write-only)
//!  8   | SPI2 SCK        | Shared SPI clock
//! 10   | SPI2 MOSI       | Shared SPI data out
//! 12   | SD CS           | SD card chip select (flash pin, DIO mode frees it)
//! 21   | EPD CS          | Display chip select

// ----- E-Paper Display -----
pub const EPD_CS: u8 = 21;
pub const EPD_DC: u8 = 4;
pub const EPD_RST: u8 = 5;
pub const EPD_BUSY: u8 = 6;

// ----- SPI Bus (shared) -----
pub const SPI_SCK: u8 = 8;
pub const SPI_MOSI: u8 = 10;
pub const SPI_MISO: u8 = 7; // SD card only; display ignores MISO

// ----- SD Card -----
pub const SD_CS: u8 = 12; // GPIO12 — flash SPIHD pin, free in DIO mode

// ----- Battery ADC -----
pub const BAT_ADC: u8 = 0; // GPIO0 - Battery voltage (via divider)

// ----- Buttons (ADC) -----
pub const BTN_ROW1_ADC: u8 = 1; // GPIO1 - Right/Left/Confirm/Back
pub const BTN_ROW2_ADC: u8 = 2; // GPIO2 - Vol Up/Down

// ----- Power Button -----
pub const BTN_POWER: u8 = 3; // Digital, active LOW
