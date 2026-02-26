// SD card over SPI with FAT volume manager
// No RTC on board; timestamps are fixed to 2025-01-01.

use embedded_sdmmc::{SdCard, TimeSource, Timestamp, VolumeManager};
use log::info;

#[derive(Default, Clone, Copy)]
pub struct DummyTimeSource;

impl TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 55,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

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
    pub fn new(spi: SPI) -> Self {
        let sdcard = SdCard::new(spi, esp_hal::delay::Delay::new());

        match sdcard.num_bytes() {
            Ok(bytes) => info!("SD card: {} bytes ({} MB)", bytes, bytes / 1024 / 1024),
            Err(e) => info!("SD card probe failed: {:?}", e),
        }

        let volume_mgr = VolumeManager::new(sdcard, DummyTimeSource);
        Self { volume_mgr }
    }
}
