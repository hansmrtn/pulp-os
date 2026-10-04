// OnePage C61 full offline firmware (T12): hardware init, construct Kernel +
// AppManager, boot, run. Same shape as src/bin/main.rs (X4); the board bring-up
// order is the one proven in c61_boot.rs (T4-T11), which stays as the minimal
// bring-up image.
//
//   esp_hal::init -> wake cause -> heaps -> PSRAM (degrades, never panics)
//   -> GPIO27 power-cycle (R6, before any SD access) -> esp-rtos + embassy
//   -> ADC1 (keys + battery) -> USB detect -> SPI2/DMA -> SD init (R6, R9)
//   -> _PULP dir -> EPD driver -> Kernel + AppManager -> boot (session
//   restore, first full refresh) -> tasks -> run.
//
// Offline only: no radio, no Wi-Fi (`wifi` + this board is a compile_error!).
// Hardware behaviour is unverified, see the "not verified" lists in
// board_c61/*.rs and the T12 section of the baseline.

#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info, warn};

use pulp_kernel::board_c61::hw::{C61Hw, CardHw, SleepParts};
use pulp_kernel::board_c61::memory::{
    self, INTERNAL_HEAP_MAIN_BYTES, INTERNAL_HEAP_RECLAIMED_BYTES,
};
use pulp_kernel::board_c61::power::HalDelay;
use pulp_kernel::board_c61::sd::{self, CardDetect, CardDetectPin};
use pulp_kernel::board_c61::sleep::{self, C61Lines, C61SleepEntry, WakeKey};
use pulp_kernel::board_c61::spi::{self, SpiPins};
use pulp_kernel::board_c61::usb::UsbPort;
use pulp_kernel::board_c61::{adc, battery, epd, keys};
use pulp_kernel::drivers::storage;
use pulp_kernel::take_c61_pins;
use pulp_os::apps::Launcher;
use pulp_os::apps::files::FilesApp;
use pulp_os::apps::home::HomeApp;
use pulp_os::apps::manager::AppManager;
use pulp_os::apps::reader::ReaderApp;
use pulp_os::apps::settings::SettingsApp;
use pulp_os::apps::widgets::{ButtonFeedback, QuickMenu};
use pulp_os::board::action::ButtonMapper;
use pulp_os::drivers::strip::StripBuffer;
use pulp_os::kernel::BookmarkCache;
use pulp_os::kernel::Kernel;
use pulp_os::kernel::dir_cache::DirCache;
use pulp_os::kernel::tasks;
use pulp_os::kernel::work_queue;
use pulp_os::kernel::{BigBuf, BufClass};
use pulp_os::ui::paint_stack;
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

// heavy statics: kept out of the async future (same set as the X4)

static STRIP: ConstStaticCell<StripBuffer> = ConstStaticCell::new(StripBuffer::new());
static READER: ConstStaticCell<ReaderApp> = ConstStaticCell::new(ReaderApp::new());
static LAUNCHER: ConstStaticCell<Launcher> = ConstStaticCell::new(Launcher::new());
static QUICK_MENU: ConstStaticCell<QuickMenu> = ConstStaticCell::new(QuickMenu::new());
static BUMPS: ConstStaticCell<ButtonFeedback> = ConstStaticCell::new(ButtonFeedback::new());
static DIR_CACHE: ConstStaticCell<DirCache> = ConstStaticCell::new(DirCache::new());
static BM_CACHE: ConstStaticCell<BookmarkCache> = ConstStaticCell::new(BookmarkCache::new());

static HOME: StaticCell<HomeApp> = StaticCell::new();
static FILES: StaticCell<FilesApp> = StaticCell::new();
static SETTINGS: StaticCell<SettingsApp> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    paint_stack();
    // R19/R20: why did we start? (empty = power-on / any non-deep-sleep reset)
    let wake = sleep::wake_cause();

    // internal heap = the planned size (board-logic INTERNAL_HEAP_*): main RAM
    // part plus the part reclaimed from the 2nd stage bootloader (dram2)
    esp_alloc::heap_allocator!(size: INTERNAL_HEAP_MAIN_BYTES);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: INTERNAL_HEAP_RECLAIMED_BYTES);

    // PSRAM (40 MHz) on its own heap, budget-checked; failure degrades to the
    // internal limits, the firmware still reads books
    memory::init(peripherals.PSRAM);
    memory::log_report();

    // R6: the GPIO27 power-cycle runs before any SD access. Partial move:
    // TIMG0 / FROM_CPU_INTR0 stay available below.
    let mut pins = take_c61_pins!(peripherals);
    match pins.power.power_cycle(&mut HalDelay::new()) {
        Ok(()) => info!("gpio27 power-cycled, state={:?}", pins.power.state()),
        Err(e) => error!("gpio27 power-cycle rejected: {:?}", e),
    }

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);
    info!(
        "pulp-os c61: esp32c61 rv32imac, rtos + embassy up, wake {:?}",
        wake
    );

    // GPIO2 doubles as the deep-sleep wake pad: the sleep sequence needs its own
    // handle (the key driver owns the original). SAFETY: both configure the pad
    // as the same pull-up input, the wake handle is only used by the sleep
    // sequence, and the key driver is not polled once that sequence runs.
    let wake_gpio2 = unsafe { pins.key_wake.clone_unchecked() };
    let lpwr = peripherals.LPWR;

    // R12 + R13 + R16: one ADC1 for the front ladder (GPIO4) and the battery
    // (GPIO5). Without it there are no keys at all: park.
    let (key_input, battery_mon) =
        match adc::init(peripherals.ADC1, pins.front_adc, pins.battery_adc) {
            Ok(set) => {
                let k = keys::new(
                    set.adc,
                    set.front,
                    pins.key_wake,
                    pins.key_prev,
                    pins.key_next,
                );
                let b = battery::new(pins.charge_enable, set.adc, set.battery);
                (k, b)
            }
            Err(e) => {
                error!("adc: init failed: {:?}", e);
                park("adc init failed").await
            }
        };
    let mut battery_mon = Some(battery_mon);

    // R17: USB detect (GPIO11), polarity from the board configuration
    let usb_port = UsbPort::new(pins.usb_detect);

    // R8: SPI2 + DMA; EPD and SD get arbitrated handles on the one bus. No bus
    // means no storage and no display: park.
    let spi_board = match spi::init(
        peripherals.SPI2,
        peripherals.DMA_CH0,
        SpiPins {
            sck: pins.spi_sck,
            mosi: pins.spi_mosi,
            miso: pins.spi_miso,
            epd_cs: pins.epd_cs,
            sd_cs: pins.sd_cs,
        },
    ) {
        Ok(b) => b,
        Err(e) => {
            error!("spi2 init failed: {:?}", e);
            park("spi2 init failed").await
        }
    };

    // R6 + R9: SD init needs the SdInitPermit minted after the power-cycle; a
    // missing or bad card is a recoverable storage error (NoCard path)
    let card_pin = CardDetectPin::new(pins.sd_card_detect);
    let card_detect = CardDetect::new(card_pin.state());
    let up = sd::bring_up(&mut pins.power, card_pin.state(), spi_board.sd.clone()).await;
    info!("{}", up.health.status().message());
    // SD init needs the 400 kHz probe clock; operate at 10 MHz afterwards
    if let Err(e) = spi_board.control.speed_up() {
        error!("spi2 speed-up failed: {:?}", e);
    }
    let sd = up.storage;
    let sd_ok = sd.probe_ok();
    if sd_ok {
        info!("sd: fat32 mounted");
        if let Err(e) = storage::ensure_pulp_dir_async(&sd).await {
            warn!("ensure_pulp_dir: {:?}", e);
        }
    }

    // display driver; the controller is initialised by Kernel::boot
    let epd = epd::new(spi_board.epd, pins.epd_dc, pins.epd_busy);

    // first battery reading with the BSP charge-pause contract
    let battery_mv = match battery_mon.as_mut().map(|m| m.measure()) {
        Some(Ok(r)) => {
            info!("battery: cell {} mV, {}%", r.cell_mv, r.percent());
            r.cell_mv
        }
        Some(Err(e)) => {
            warn!("battery: first sample failed: {:?}", e);
            0
        }
        None => 0,
    };

    let hw = C61Hw::new(
        pins.power,
        battery_mon,
        CardHw {
            pin: card_pin,
            detect: card_detect,
            health: up.health,
            spi: spi_board.sd.clone(),
            control: spi_board.control.clone(),
        },
        SleepParts {
            lines: C61Lines::new(spi_board.control.clone(), pins.mic_pdm_clk),
            wake: WakeKey::new(wake_gpio2),
            entry: C61SleepEntry::new(lpwr),
        },
        embassy_time::Instant::now().as_millis(),
    );

    let mut kernel = Kernel::new(
        sd,
        epd,
        STRIP.take(),
        DIR_CACHE.take(),
        BM_CACHE.take(),
        Delay::new(),
        sd_ok,
        battery_mv,
        hw,
    );

    let mut app_mgr = AppManager::new(
        LAUNCHER.take(),
        HOME.init(HomeApp::new()),
        FILES.init(FilesApp::new()),
        READER.take(),
        SETTINGS.init(SettingsApp::new()),
        QUICK_MENU.take(),
        BUMPS.take(),
        ButtonMapper::new(),
    );

    // no boot console on the C61: every refresh is a full (flashing) refresh,
    // so the console screen would only add one; progress is in the log
    kernel.boot(&mut app_mgr, wake).await;

    // register the image decoder so the kernel's worker task can decode
    // JPEG/PNG without depending on smol-epub directly
    work_queue::register_image_decoder(|data, is_jpeg, max_w, max_h| {
        let raw = if is_jpeg {
            smol_epub::jpeg::decode_jpeg_fit(data, max_w, max_h)
        } else {
            smol_epub::png::decode_png_fit(data, max_w, max_h)
        };
        raw.and_then(|img| {
            Ok(work_queue::DecodedImage {
                width: img.width,
                height: img.height,
                data: BigBuf::from_vec(img.data, BufClass::ImageData)
                    .map_err(|_| "image buffer over budget")?,
                stride: img.stride,
            })
        })
    });

    match tasks::input_task(key_input, usb_port) {
        Ok(t) => spawner.spawn(t),
        Err(_) => error!("spawn input_task failed"),
    }
    match tasks::housekeeping_task() {
        Ok(t) => spawner.spawn(t),
        Err(_) => error!("spawn housekeeping_task failed"),
    }
    match tasks::idle_timeout_task() {
        Ok(t) => spawner.spawn(t),
        Err(_) => error!("spawn idle_timeout_task failed"),
    }
    match work_queue::worker_task() {
        Ok(t) => spawner.spawn(t),
        Err(_) => error!("spawn worker_task failed"),
    }
    info!("kernel ready.");

    kernel.run(&mut app_mgr).await
}

// unrecoverable bring-up failure: log and idle instead of panicking
async fn park(why: &str) -> ! {
    loop {
        error!("pulp-os c61: parked ({})", why);
        embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
    }
}
