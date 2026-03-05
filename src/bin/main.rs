// hardware init, construct Kernel + AppManager, boot, run

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::ram;
use esp_hal::timer::timg::TimerGroup;
use log::info;

use pulp_os::apps::Launcher;
use pulp_os::apps::files::FilesApp;
use pulp_os::apps::home::HomeApp;
use pulp_os::apps::manager::AppManager;
use pulp_os::apps::reader::ReaderApp;
use pulp_os::apps::settings::SettingsApp;
use pulp_os::apps::widgets::{ButtonFeedback, QuickMenu};
use pulp_os::board::action::ButtonMapper;
use pulp_os::board::{Board, speed_up_spi};
use pulp_os::drivers::battery;
use pulp_os::drivers::input::InputDriver;
use pulp_os::drivers::sdcard::SdStorage;
use pulp_os::drivers::storage;
use pulp_os::drivers::strip::StripBuffer;
use pulp_os::kernel::BookmarkCache;
use pulp_os::kernel::BootConsole;
use pulp_os::kernel::Kernel;
use pulp_os::kernel::dir_cache::DirCache;
use pulp_os::kernel::tasks;
use pulp_os::kernel::work_queue;
use pulp_os::ui::paint_stack;
use static_cell::{ConstStaticCell, StaticCell};

esp_bootloader_esp_idf::esp_app_desc!();

// heavy statics: kept out of the async future to keep it ~200 B

static STRIP: ConstStaticCell<StripBuffer> = ConstStaticCell::new(StripBuffer::new());
static READER: ConstStaticCell<ReaderApp> = ConstStaticCell::new(ReaderApp::new());
static LAUNCHER: ConstStaticCell<Launcher> = ConstStaticCell::new(Launcher::new());
static QUICK_MENU: ConstStaticCell<QuickMenu> = ConstStaticCell::new(QuickMenu::new());
static BUMPS: ConstStaticCell<ButtonFeedback> = ConstStaticCell::new(ButtonFeedback::new());
static DIR_CACHE: ConstStaticCell<DirCache> = ConstStaticCell::new(DirCache::new());
static BM_CACHE: ConstStaticCell<BookmarkCache> = ConstStaticCell::new(BookmarkCache::new());
static CONSOLE: ConstStaticCell<BootConsole> = ConstStaticCell::new(BootConsole::new());

static HOME: StaticCell<HomeApp> = StaticCell::new();
static FILES: StaticCell<FilesApp> = StaticCell::new();
static SETTINGS: StaticCell<SettingsApp> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    paint_stack();
    // 108 KB main DRAM heap; leaves ~56 KB for stack
    esp_alloc::heap_allocator!(size: 110_592);
    // reclaim ~64 KB from 2nd-stage bootloader; net heap ~172 KB
    esp_alloc::heap_allocator!(#[ram(reclaimed)] size: 64_000);

    let console = CONSOLE.take();
    console.push("pulp-os 0.1.0");
    console.push("esp32c3 rv32imc 160mhz");
    console.push("heap: 172K (108K + 64K reclaimed)");

    info!("booting...");

    // Safety: TIMG0 and SW_INTERRUPT are cloned here and consumed by
    // esp_rtos::start. They are never used again after this point.
    // Board::init (which takes ownership of `peripherals`) does not
    // touch TIMG0 or SW_INTERRUPT, see the pin ownership table in
    // board/mod.rs for the full split.
    let timg0 = TimerGroup::new(unsafe { peripherals.TIMG0.clone_unchecked() });
    let sw_ints =
        SoftwareInterruptControl::new(unsafe { peripherals.SW_INTERRUPT.clone_unchecked() });
    esp_rtos::start(timg0.timer0, sw_ints.software_interrupt0);

    // Peripherals move into Board::init, which splits them across
    // init_input (ADC pins, GPIO3, IO_MUX) and init_spi_peripherals
    // (SPI2, DMA, display + SD GPIOs). each peripheral is used in
    // exactly one place, see the ownership table in board/mod.rs.
    let board = Board::init(peripherals);
    console.push("spi: dma ch0, 4096B tx+rx");

    let mut epd = board.display.epd;
    let mut delay = Delay::new();
    epd.init(&mut delay);
    console.push("epd: ssd1677 800x480 init");

    speed_up_spi();
    console.push("spi: 400kHz -> 20MHz");

    let sd = match board.storage.sd_card {
        Some(card) => {
            console.push("sd: card detected");
            SdStorage::mount(card).await
        }
        None => {
            console.push("sd: not found");
            SdStorage::empty()
        }
    };

    let sd_ok = sd.probe_ok();
    if sd_ok {
        console.push("sd: fat32 mounted");
        if let Err(e) = storage::ensure_pulp_dir_async(&sd).await {
            console.push("sd: pulp dir failed");
            log::warn!("ensure_pulp_dir: {:?}", e);
        }
    }

    let mut input = InputDriver::new(board.input);
    let battery_mv = battery::adc_to_battery_mv(input.read_battery_mv());

    let mut kernel = Kernel::new(
        sd,
        epd,
        STRIP.take(),
        DIR_CACHE.take(),
        BM_CACHE.take(),
        delay,
        sd_ok,
        battery_mv,
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

    console.push("kernel: constructed");
    kernel.show_boot_console(console).await;

    kernel.boot(&mut app_mgr).await;

    spawner.spawn(tasks::input_task(input)).expect("spawn input_task");
    spawner.spawn(tasks::housekeeping_task()).expect("spawn housekeeping_task");
    spawner.spawn(tasks::idle_timeout_task()).expect("spawn idle_timeout_task");
    spawner.spawn(work_queue::worker_task()).expect("spawn worker_task");
    info!("kernel ready.");

    kernel.run(&mut app_mgr).await
}
