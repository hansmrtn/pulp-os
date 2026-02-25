//! SD Card support for XTEink X4
//!
//! The SD card shares the SPI2 bus with the e-paper display.
//! Bus arbitration is handled at the board level using RefCellDevice.
use embedded_sdmmc::{SdCard, TimeSource, Timestamp, VolumeManager};
use log::info;

// Dummy time source for FAT timestamps (X4 has no RTC).
#[derive(Default, Clone, Copy)]
pub struct DummyTimeSource;

impl TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 55, // 2025
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

// sd card initialization frequency (Hz).
pub const SD_INIT_FREQ_HZ: u32 = 400_000;

// Normal operating frequency after init
// TODO: Put this somewhere else?
pub const SD_NORMAL_FREQ_HZ: u32 = 20_000_000;

// Wrapper that holds the SdCard + VolumeManager together.
pub struct SdStorage<SPI>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    pub volume_mgr: VolumeManager<SdCard<SPI, esp_hal::delay::Delay>, DummyTimeSource>,
}

impl<SPI> SdStorage<SPI>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    // Create SD storage, probing the card during construction.
    pub fn new(spi: SPI) -> Self {
        let sdcard = SdCard::new(spi, esp_hal::delay::Delay::new());

        // Probe card before handing ownership to VolumeManager.
        // This triggers the SD init sequence (CMD0, CMD8, ACMD41, etc).
        match sdcard.num_bytes() {
            Ok(bytes) => info!("SD card: {} bytes ({} MB)", bytes, bytes / 1024 / 1024),
            Err(e) => info!("SD card probe failed: {:?}", e),
        }

        let volume_mgr = VolumeManager::new(sdcard, DummyTimeSource);
        Self { volume_mgr }
    }
}
