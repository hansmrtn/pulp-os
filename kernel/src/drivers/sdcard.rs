// sd card over SPI: sync SdCard + async volume manager
//
// sync SdCard handles the SD protocol (CMD0, init, sector I/O)
// using embedded_hal SpiDevice + DelayNs traits
//
// BlockDeviceAdapter bridges sync BlockDevice to AsyncBlockDevice
// so AsyncVolumeManager can consume it
//
// poll_once drives file-I/O futures to completion in a single poll
// (SPI bus is blocking, so every .await resolves immediately)

use core::cell::RefCell;
use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use embedded_hal::delay::DelayNs;

use embedded_sdmmc::{
    AsyncBlockDevice, AsyncVolumeManager, Block, BlockCount, BlockDevice, BlockIdx, RawDirectory,
    RawVolume, SdCard, TimeSource, Timestamp, VolumeIdx,
};
use log::info;

use crate::board::SdSpiDevice;

// sync BlockDevice -> AsyncBlockDevice adapter
//
// sync SdCard uses RefCell internally, takes &self for BlockDevice
// methods; we delegate AsyncBlockDevice &mut self to the inner &self
// methods. All resolve immediately since SPI is DMA-blocking

pub(crate) struct BlockDeviceAdapter<D: BlockDevice>(D);

impl<D: BlockDevice> AsyncBlockDevice for BlockDeviceAdapter<D> {
    type Error = D::Error;

    async fn read(
        &mut self,
        blocks: &mut [Block],
        start_block_idx: BlockIdx,
    ) -> Result<(), Self::Error> {
        self.0.read(blocks, start_block_idx)
    }

    async fn write(
        &mut self,
        blocks: &[Block],
        start_block_idx: BlockIdx,
    ) -> Result<(), Self::Error> {
        self.0.write(blocks, start_block_idx)
    }

    async fn num_blocks(&mut self) -> Result<BlockCount, Self::Error> {
        self.0.num_blocks()
    }
}

// no RTC on this board

pub(crate) struct NullTimeSource;

impl TimeSource for NullTimeSource {
    fn get_timestamp(&self) -> Timestamp {
        Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

// type aliases

pub type SyncSdCard = SdCard<SdSpiDevice, esp_hal::delay::Delay>;
pub(crate) type SdBlockDev = BlockDeviceAdapter<SyncSdCard>;
pub(crate) type VolMgr = AsyncVolumeManager<SdBlockDev, NullTimeSource, 4, 4, 1>;

// persistent volume manager state, held behind RefCell for interior
// mutability (AsyncVolumeManager requires &mut self)

pub(crate) struct SdStorageInner {
    pub(crate) mgr: VolMgr,
    #[allow(dead_code)]
    pub(crate) vol: RawVolume,
    pub(crate) root: RawDirectory,
}

// holds a persistently-mounted AsyncVolumeManager with volume 0 and
// root directory kept open for the device lifetime; RefCell provides
// interior mutability so storage functions can take &SdStorage

pub struct SdStorage {
    inner: Option<RefCell<SdStorageInner>>,
}

impl SdStorage {
    pub fn empty() -> Self {
        Self { inner: None }
    }

    // init SD card at 400 kHz (SD spec init frequency)
    //
    // sync SdCard auto-initialises on first method call; we call
    // num_bytes() to force init and verify the card responds
    //
    // pub so Board::init can run this before other SPI peripherals
    // touch the bus - SD spec requires a clean 400 kHz bus for CMD0
    pub fn init_card(spi_device: SdSpiDevice) -> Option<SyncSdCard> {
        let sd = SdCard::new(spi_device, esp_hal::delay::Delay::new());

        for attempt in 1..=5 {
            match sd.num_bytes() {
                Ok(size) => {
                    info!("SD card: initialised (attempt {})", attempt);
                    info!("SD card: {} bytes ({} MB)", size, size / 1024 / 1024);
                    return Some(sd);
                }
                Err(e) => {
                    info!("SD card: init attempt {} failed: {:?}", attempt, e);
                    sd.mark_card_uninit();
                    esp_hal::delay::Delay::new().delay_ms(50);
                }
            }
        }

        info!("SD card: all init attempts failed");
        None
    }

    // mount FAT filesystem on an already-initialised SD card
    //
    // opens volume 0 (first MBR partition) and keeps the root
    // directory open for the device lifetime
    pub async fn mount(sd: SyncSdCard) -> Self {
        let adapter = BlockDeviceAdapter(sd);
        let mut mgr = AsyncVolumeManager::new(adapter, NullTimeSource);

        let vol = match mgr.open_raw_volume(VolumeIdx(0)).await {
            Ok(v) => v,
            Err(e) => {
                info!("SD card: open volume failed: {}", e);
                return Self { inner: None };
            }
        };

        let root = match mgr.open_root_dir(vol) {
            Ok(d) => d,
            Err(e) => {
                info!("SD card: open root dir failed: {}", e);
                let _ = mgr.close_volume(vol).await;
                return Self { inner: None };
            }
        };

        info!("SD card: filesystem mounted");
        Self {
            inner: Some(RefCell::new(SdStorageInner { mgr, vol, root })),
        }
    }

    #[inline]
    pub fn probe_ok(&self) -> bool {
        self.inner.is_some()
    }

    #[inline]
    pub(crate) fn borrow_inner(&self) -> Option<core::cell::RefMut<'_, SdStorageInner>> {
        self.inner.as_ref().map(|c| c.borrow_mut())
    }

    // flush pending writes and close fat handles; best-effort before halt.
    // after this call no further sd i/o is possible until mcu reset
    pub fn flush_and_close(&self) {
        if let Some(ref cell) = self.inner {
            let mut guard = cell.borrow_mut();
            let inner = &mut *guard;
            let _ = inner.mgr.close_dir(inner.root);
            poll_once(async {
                let _ = inner.mgr.close_volume(inner.vol).await;
            });
        }
    }
}

// drive a future to completion in exactly one poll
//
// correct because the SPI bus is blocking and the sync SdCard
// completes every operation before returning - no inner .await
// ever returns Pending
//
// only use for file-level operations (open, read, write, seek,
// iterate); mount runs inside the real Embassy executor

pub fn poll_once<T>(fut: impl Future<Output = T>) -> T {
    let waker: &Waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = pin!(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("poll_once: future pended -- SPI must be in Blocking mode"),
    }
}
