// OnePage C61 pin map. Source of truth: ../bsp_onepage_c61/board_c61.c
// (#defines at lines 37-42, 58-66) and board_keys.c (lines 26-28).
//
// The typed ownership (one esp-hal GPIO singleton per field) lives in
// pulp-kernel `board_c61::pins`; this table is the HAL-free mirror so the
// "every pin has exactly one owner" rule can be checked on the host.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PinRole {
    SpiSck,
    SpiMosi,
    SpiMiso,
    EpdCs,
    EpdDc,
    EpdBusy,
    SdCs,
    SdCardDetect,
    // GPIO27 is ONE pin with two functions: EPD RST and the SD/MIC power
    // enable (high = powered). Exactly one owner: the power state machine.
    PeripheralPowerAndEpdReset,
    FrontAdcLadder,
    KeyWake,
    KeyPrev,
    KeyNext,
    BatteryAdc,
    ChargeEnable,
    UsbDetect,
    // reserved: PDM mic, not used by the reader; CLK is driven low before
    // GPIO27 is cut so it cannot back-power the rail (board_c61.c:344)
    MicPdmClk,
    MicPdmData,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PinAssignment {
    pub gpio: u8,
    pub role: PinRole,
}

pub const SPI_SCK: u8 = 22; // board_c61.c:38
pub const SPI_MOSI: u8 = 23; // board_c61.c:37
pub const SPI_MISO: u8 = 24; // board_c61.c:89,96
pub const EPD_CS: u8 = 25; // board_c61.c:39
pub const EPD_DC: u8 = 8; // board_c61.c:40
pub const PERIPHERAL_POWER_EPD_RST: u8 = 27; // board_c61.c:41 (PIN_RST)
pub const EPD_BUSY: u8 = 29; // board_c61.c:42
pub const SD_CS: u8 = 26; // board_c61.c:58
pub const SD_CARD_DETECT: u8 = 28; // board_c61.c:59
pub const FRONT_ADC_LADDER: u8 = 4; // board_keys.c:26 (ADC1)
pub const KEY_WAKE: u8 = 2; // board_c61.c:66, board_keys.c:28 (LP-capable)
pub const KEY_PREV: u8 = 6; // board_keys.c:28
pub const KEY_NEXT: u8 = 9; // board_keys.c:28
pub const BATTERY_ADC: u8 = 5; // board_c61.c:63 (ADC1)
pub const CHARGE_ENABLE: u8 = 10; // board_c61.c:60
pub const USB_DETECT: u8 = 11; // board_c61.c:61
pub const MIC_PDM_CLK: u8 = 7; // board_c61.c:64
pub const MIC_PDM_DATA: u8 = 3; // board_c61.c:65

pub const PIN_MAP: [PinAssignment; 18] = [
    PinAssignment {
        gpio: SPI_SCK,
        role: PinRole::SpiSck,
    },
    PinAssignment {
        gpio: SPI_MOSI,
        role: PinRole::SpiMosi,
    },
    PinAssignment {
        gpio: SPI_MISO,
        role: PinRole::SpiMiso,
    },
    PinAssignment {
        gpio: EPD_CS,
        role: PinRole::EpdCs,
    },
    PinAssignment {
        gpio: EPD_DC,
        role: PinRole::EpdDc,
    },
    PinAssignment {
        gpio: EPD_BUSY,
        role: PinRole::EpdBusy,
    },
    PinAssignment {
        gpio: SD_CS,
        role: PinRole::SdCs,
    },
    PinAssignment {
        gpio: SD_CARD_DETECT,
        role: PinRole::SdCardDetect,
    },
    PinAssignment {
        gpio: PERIPHERAL_POWER_EPD_RST,
        role: PinRole::PeripheralPowerAndEpdReset,
    },
    PinAssignment {
        gpio: FRONT_ADC_LADDER,
        role: PinRole::FrontAdcLadder,
    },
    PinAssignment {
        gpio: KEY_WAKE,
        role: PinRole::KeyWake,
    },
    PinAssignment {
        gpio: KEY_PREV,
        role: PinRole::KeyPrev,
    },
    PinAssignment {
        gpio: KEY_NEXT,
        role: PinRole::KeyNext,
    },
    PinAssignment {
        gpio: BATTERY_ADC,
        role: PinRole::BatteryAdc,
    },
    PinAssignment {
        gpio: CHARGE_ENABLE,
        role: PinRole::ChargeEnable,
    },
    PinAssignment {
        gpio: USB_DETECT,
        role: PinRole::UsbDetect,
    },
    PinAssignment {
        gpio: MIC_PDM_CLK,
        role: PinRole::MicPdmClk,
    },
    PinAssignment {
        gpio: MIC_PDM_DATA,
        role: PinRole::MicPdmData,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_gpio_has_exactly_one_owner() {
        for (i, a) in PIN_MAP.iter().enumerate() {
            for b in &PIN_MAP[i + 1..] {
                assert_ne!(
                    a.gpio, b.gpio,
                    "GPIO{} claimed twice: {:?} / {:?}",
                    a.gpio, a.role, b.role
                );
                assert_ne!(a.role, b.role, "role {:?} assigned twice", a.role);
            }
        }
    }

    #[test]
    fn r6_gpio27_is_a_single_shared_power_and_reset_pin() {
        let on27: usize = PIN_MAP.iter().filter(|p| p.gpio == 27).count();
        assert_eq!(on27, 1);
        assert_eq!(PERIPHERAL_POWER_EPD_RST, 27);
        assert!(
            PIN_MAP
                .iter()
                .any(|p| p.gpio == 27 && p.role == PinRole::PeripheralPowerAndEpdReset)
        );
    }

    #[test]
    fn pin_numbers_match_bsp() {
        assert_eq!((SPI_SCK, SPI_MOSI, SPI_MISO), (22, 23, 24));
        assert_eq!((EPD_CS, EPD_DC, EPD_BUSY), (25, 8, 29));
        assert_eq!((SD_CS, SD_CARD_DETECT), (26, 28));
        assert_eq!(FRONT_ADC_LADDER, 4);
        assert_eq!((KEY_WAKE, KEY_PREV, KEY_NEXT), (2, 6, 9));
        assert_eq!((BATTERY_ADC, CHARGE_ENABLE, USB_DETECT), (5, 10, 11));
    }

    #[test]
    fn pins_exist_on_esp32c61() {
        // esp32c61 exposes GPIO0..=GPIO29 (esp-metadata-generated esp32c61)
        assert!(PIN_MAP.iter().all(|p| p.gpio <= 29));
    }
}
