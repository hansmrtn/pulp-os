// OnePage C61 minimal boot image (task T2): esp-hal init, esp-rtos +
// embassy executor, heap, logging, app descriptor. Link-only milestone;
// no keys/sleep/PSRAM yet (T7-T11), and no radio (T3).
//
// T4: takes the board pins (single ownership, GPIO27 inside PeripheralPower)
// and runs the boot GPIO27 power-cycle that must precede any SD access. The
// state machine is pulp_board_logic::power, the code the host tests cover.
//
// T5: brings up SPI2 + DMA (EPD and SD handles, arbitrated), then SD init via
// the GPIO27 SdInitPermit, mount and storage-error status. A missing or bad
// card is logged as a recoverable storage error, never a panic.
//
// T6: after SD, runs the EPD path once: software-reset init (display_reset()
// contract, GPIO27 untouched) and one full refresh of the orientation card
// with bounded BUSY handling. A display failure is logged, never a panic.
//
// T7: creates the key driver (front ADC ladder GPIO4 + side keys GPIO2/6/9;
// 2.5 s startup grace from creation) and polls it every 10 ms in the main
// loop; events are logged with their semantic Action. Card detect is sampled
// every CD_SAMPLE_INTERVAL_MS (a multiple of the 10 ms tick).
//
// T8: heap sizes come from the memory budget (`pulp_board_logic::memory`), PSRAM
// is brought up on its own heap (never the global allocator), and one
// budget-limited allocation round is logged: an in-budget PSRAM block, a
// 1-byte-over-limit request that must be refused, a DMA buffer that must be
// internal. A missing or broken PSRAM only degrades to internal limits.
//
// T9: one shared ADC1 (`board_c61::adc`) is created with GPIO4 (keys) and GPIO5
// (battery) enabled in the same AdcConfig; keys and the battery monitor get
// handles to it. The battery is measured once at boot with the BSP charge-pause
// contract (GPIO10 low, 30 ms, 16 reads, GPIO10 high) and then every
// BATTERY_EVERY_TICKS; USB (GPIO11, polarity from the board config) is polled
// every USB_EVERY_TICKS and plug/unplug events are logged. A failed ADC read is
// logged as an error, never as 0 V. No UI use (T12), no sleep handling (T11).
//
// T11: the wake cause is read right after `esp_hal::init` (`sleep::wake_cause`,
// logged), the restore decision after SD init goes through `plan_boot(cause,
// decision)`, and the full deep-sleep sequence (`sleep::enter_deep_sleep`:
// save session -> arm GPIO2 wake -> begin_shutdown -> park EPD -> flush SD ->
// charge restore -> silence lines -> GPIO27 low -> sleep) is linked into this
// image. It is DISABLED by default: `DEMO_IDLE_SLEEP_SECS = 0`. The app layer
// that decides when to sleep is T12; this loop has no sleep key. To exercise it
// on a board set the constant to N > 0 and rebuild: after N idle seconds
// (no key event) the sequence runs once. `black_box` keeps the call from being
// optimised out while the constant is 0, so `nm` shows the sequence.
//
// Not verified on hardware: this proves the toolchain, dependency set,
// linker script and runner can produce a riscv32imac ELF for esp32c61.

#![no_std]
#![no_main]

extern crate alloc;

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info, warn};
use pulp_kernel::board_c61::PeripheralPower;
use pulp_kernel::board_c61::adc;
use pulp_kernel::board_c61::battery;
use pulp_kernel::board_c61::epd;
use pulp_kernel::board_c61::keys;
use pulp_kernel::board_c61::memory::{
    self, ExternalClass, INTERNAL_HEAP_MAIN_BYTES, INTERNAL_HEAP_RECLAIMED_BYTES, MemClass,
    PSRAM_CHAPTER_TEXT_BYTES,
};
use pulp_kernel::board_c61::power::{Gpio27Rail, HalDelay};
use pulp_kernel::board_c61::sd::{
    self, CD_SAMPLE_INTERVAL_MS, CardDetect, CardDetectPin, CardEvent, StorageHealth,
};
use pulp_kernel::board_c61::session::{self, BootDecision, SdSessionStore, SessionState};
use pulp_kernel::board_c61::sleep::{
    self, BootPlan, C61Lines, C61SleepEntry, StoreSaver, WakeCause, WakeKey,
};
use pulp_kernel::board_c61::spi::{self, SpiPins};
use pulp_kernel::board_c61::usb::{self, UsbPort};
use pulp_kernel::drivers::{sdcard::SdStorage, storage};
use pulp_kernel::take_c61_pins;

esp_bootloader_esp_idf::esp_app_desc!();

/// Main-loop tick; keys are polled every tick (X4 uses 10 ms while active).
const KEY_TICK_MS: u64 = 10;
/// Card detect runs every this many ticks (CD_SAMPLE_INTERVAL_MS / tick).
const CD_EVERY_TICKS: u32 = CD_SAMPLE_INTERVAL_MS / KEY_TICK_MS as u32;
const _: () = assert!(CD_SAMPLE_INTERVAL_MS % KEY_TICK_MS as u32 == 0);
/// Battery measurement period of this bring-up loop: 30 s, like the X4
/// (BATTERY_INTERVAL_TICKS = 3000 x 10 ms).
const BATTERY_EVERY_TICKS: u32 = 3000;
/// USB poll every 20 ms (the debounce is `USB_DEBOUNCE_SAMPLES` samples).
const USB_EVERY_TICKS: u32 = 2;
/// Bring-up only: seconds without a key event after which the deep-sleep
/// sequence runs once. 0 = never (default, so bring-up is not interrupted).
const DEMO_IDLE_SLEEP_SECS: u32 = 0;

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    // R19/R20: why did we start? (empty = power-on / any non-deep-sleep reset)
    let wake = sleep::wake_cause();
    // Internal heap = the planned size (board-logic `INTERNAL_HEAP_*`): main RAM
    // part plus the part reclaimed from the 2nd stage bootloader (dram2).
    esp_alloc::heap_allocator!(size: INTERNAL_HEAP_MAIN_BYTES);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: INTERNAL_HEAP_RECLAIMED_BYTES);

    // T8: PSRAM (40 MHz) on its own heap; failure degrades, never panics.
    memory::init(peripherals.PSRAM);
    memory_demo();

    // R6: board init starts with the GPIO27 power-cycle, before any SD init.
    // Partial move: TIMG0 / FROM_CPU_INTR0 stay available below.
    let mut pins = take_c61_pins!(peripherals);
    match pins.power.power_cycle(&mut HalDelay::new()) {
        Ok(()) => info!("gpio27 power-cycled, state={:?}", pins.power.state()),
        Err(e) => error!("gpio27 power-cycle rejected: {:?}", e),
    }
    // `pins` stays alive for the whole run: it owns GPIO27 (rail must stay high).

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("pulp-os c61 boot: esp32c61 rv32imac, rtos + embassy up");
    info!("boot: wake cause {:?}", wake);

    // R12 + R13 + R16: ONE ADC1 for the front ladder (GPIO4) and the battery
    // (GPIO5); both pins are enabled in the same AdcConfig before Adc::new.
    // `adc::init` also checks esp-hal's channel table (BSP: CH2 / CH3).
    // R19: the deep-sleep wake needs its own handle to GPIO2 (the key driver
    // takes the original). SAFETY: both configure GPIO2 as the same pull-up
    // input, the wake handle is only used by the sleep sequence, and the key
    // driver is not polled after that sequence starts.
    let wake_gpio2 = unsafe { pins.key_wake.clone_unchecked() };
    let lpwr = peripherals.LPWR;
    let (front_ch, battery_ch) = adc::channels(&pins.front_adc, &pins.battery_adc);
    info!(
        "adc: GPIO4 -> ADC1_CH{} (BSP 2), GPIO5 -> ADC1_CH{} (BSP 3)",
        front_ch, battery_ch
    );
    let (mut key_input, mut battery_mon) =
        match adc::init(peripherals.ADC1, pins.front_adc, pins.battery_adc) {
            Ok(set) => {
                let k = keys::new(
                    set.adc,
                    set.front,
                    pins.key_wake,
                    pins.key_prev,
                    pins.key_next,
                );
                info!(
                    "keys: ladder GPIO4 + GPIO2/6/9 ready, startup grace {} ms",
                    keys::STARTUP_GRACE_US / 1000
                );
                let b = battery::new(pins.charge_enable, set.adc, set.battery);
                info!("battery: GPIO5 + charge control GPIO10 ready (charging enabled)");
                (Some(k), Some(b))
            }
            Err(e) => {
                error!("adc: init failed: {:?}", e);
                (None, None)
            }
        };
    report_battery(&mut battery_mon);

    // R17: USB detect (GPIO11); polarity comes from the board configuration.
    let mut usb_port = UsbPort::new(pins.usb_detect);
    info!(
        "usb: GPIO11 reads {} -> {} (polarity {:?}, UNVERIFIED on hardware)",
        if usb_port.level_high() { "high" } else { "low" },
        if usb_port.plugged() {
            "plugged"
        } else {
            "unplugged"
        },
        usb::USB_POLARITY
    );

    // R8: SPI2 + DMA on the BSP pins; EPD (T6 builds the driver) and SD get
    // arbitrated device handles on the one bus.
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
            park().await
        }
    };

    // R6 + R9: SD init needs the SdInitPermit minted after the power-cycle.
    let card_detect = CardDetectPin::new(pins.sd_card_detect);
    let mut cd = CardDetect::new(card_detect.state());
    info!("sd: card detect GPIO28 reads {:?}", card_detect.state());
    let mut up = sd::bring_up(&mut pins.power, card_detect.state(), spi_board.sd.clone()).await;
    info!("{}", up.health.status().message());
    // SD needs the 400 kHz probe clock; go to the 10 MHz operating clock after
    if let Err(e) = spi_board.control.speed_up() {
        error!("spi2 speed-up failed: {:?}", e);
    }

    report_storage(&up.storage, &mut up.health);

    // R18 + R20: save -> "restart" (restore decision again) demo on the SD session slots.
    session_demo(&pins.power, &up.storage, wake).await;

    // R10 + R11: EPD init (SW_RESET only) and one full refresh, BUSY bounded.
    let mut epd = epd::new(spi_board.epd, pins.epd_dc, pins.epd_busy);
    match pins.power.display_reset() {
        Ok(reset) => match epd.init(reset) {
            Ok(()) => match epd::full_refresh_test_pattern(&mut epd) {
                Ok(()) => info!("epd: full refresh done (480x800 portrait test card)"),
                Err(e) => error!("epd: full refresh failed: {}", e),
            },
            Err(e) => error!("epd: init failed: {}", epd::display_error(e)),
        },
        Err(e) => error!("epd: display reset refused: {:?}", e),
    }

    // R19 sequence parts, consumed by the (default-off) idle-sleep demo
    let mut sleep_parts = Some((
        C61Lines::new(spi_board.control.clone(), pins.mic_pdm_clk),
        WakeKey::new(wake_gpio2),
        C61SleepEntry::new(lpwr),
    ));
    let mut idle_ticks: u32 = 0;

    let mut ticks: u32 = 0;
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_millis(KEY_TICK_MS)).await;
        ticks = ticks.wrapping_add(1);

        // keys: 10 ms poll (debounce 15 ms needs a finer tick than card detect)
        if let Some(k) = key_input.as_mut()
            && let Some(ev) = k.poll()
        {
            idle_ticks = 0;
            info!("key: {:?} -> {:?}", ev, keys::map_event(ev, false));
        } else {
            idle_ticks = idle_ticks.saturating_add(1);
        }

        // bring-up deep-sleep demo; `black_box`: not folded away while 0
        let idle_limit = core::hint::black_box(DEMO_IDLE_SLEEP_SECS);
        if idle_limit != 0 && idle_ticks >= idle_limit.saturating_mul(1000 / KEY_TICK_MS as u32) {
            idle_ticks = 0;
            match sleep_parts.take() {
                Some((lines, wake_key, entry)) => {
                    let mut state = SessionState::home();
                    state.wake_count = 1;
                    let mut store = SdSessionStore::new(&up.storage);
                    let abort = sleep::enter_deep_sleep(
                        &mut pins.power,
                        StoreSaver {
                            store: &mut store,
                            state: &state,
                        },
                        &mut epd,
                        &up.storage,
                        &mut battery_mon,
                        lines,
                        wake_key,
                        entry,
                    );
                    // only reached when the sequence aborted (still awake)
                    error!("sleep demo aborted: {:?}", abort.reason);
                }
                None => warn!("sleep demo: already attempted, staying awake"),
            }
        }

        if ticks % USB_EVERY_TICKS == 0
            && let Some(ev) = usb_port.poll()
        {
            info!("usb: {:?}", ev);
        }
        if ticks % BATTERY_EVERY_TICKS == 0 {
            report_battery(&mut battery_mon);
        }

        if ticks % CD_EVERY_TICKS != 0 {
            continue;
        }
        // card detect: debounced insert/remove -> re-init. (A real UI owns this
        // poll; this loop only proves the flow compiles and links.)
        match cd.sample(card_detect.level_high()) {
            Some(CardEvent::Removed) => {
                up.health.on_card_event(CardEvent::Removed);
                up.storage = SdStorage::empty();
                let _ = pins.power.card_removed();
                warn!("sd: card removed");
                report_storage(&up.storage, &mut up.health);
            }
            Some(CardEvent::Inserted) => {
                if up.health.on_card_event(CardEvent::Inserted) {
                    up = sd::bring_up(&mut pins.power, card_detect.state(), spi_board.sd.clone())
                        .await;
                    info!("{}", up.health.status().message());
                    report_storage(&up.storage, &mut up.health);
                }
            }
            None => {}
        }
        if ticks % (250 * CD_EVERY_TICKS) == 0 {
            info!("pulp-os c61 boot: alive");
        }
    }
}

/// One battery measurement with the charge-pause contract; failures are
/// logged as errors (never 0 V).
fn report_battery(mon: &mut Option<battery::C61Battery>) {
    let Some(m) = mon.as_mut() else { return };
    match m.measure() {
        Ok(r) => info!(
            "battery: pin {} mV -> cell {} mV, {}%",
            r.adc_mv,
            r.cell_mv,
            r.percent()
        ),
        Err(e) => error!("battery: sample failed: {:?} (charging resumed)", e),
    }
}

/// One budget-limited allocation round (compile/link proof; logs only).
fn memory_demo() {
    memory::log_report();

    match memory::alloc_external(ExternalClass::ChapterText, 64 * 1024, 16) {
        Ok(mut b) => {
            b.as_mut_slice()[0] = 0xA5;
            info!(
                "memory: chapter-text block {} B at {:#010x} in {:?}",
                b.len(),
                b.addr(),
                b.region()
            );
        }
        Err(e) => warn!("memory: PSRAM chapter block refused: {:?}", e),
    }
    // one byte over the class limit must be an error, not a panic or OOM
    match memory::alloc_external(ExternalClass::ChapterText, PSRAM_CHAPTER_TEXT_BYTES + 1, 16) {
        Ok(_) => error!("memory: over-limit request was accepted (budget bug)"),
        Err(e) => info!("memory: over-limit request refused as expected: {:?}", e),
    }
    // auto placement: PSRAM when ready, internal limits otherwise
    match memory::alloc(MemClass::ImageData, 32 * 1024, 16) {
        Ok(b) => info!("memory: image block {} B in {:?}", b.len(), b.region()),
        Err(e) => warn!("memory: image block refused: {:?}", e),
    }
    match memory::alloc_dma(256) {
        Ok(b) => info!(
            "memory: dma block at {:#010x} in {:?}",
            b.addr(),
            b.region()
        ),
        Err(e) => error!("memory: dma block refused: {:?}", e),
    }
    memory::log_report();
}

/// T10 compile/link demo of the session path: restore decision at boot, and
/// (only if there is no valid session yet) save a harmless Home-only state and
/// decide again, as a reboot would. SD write/rename behaviour is NOT verified on
/// hardware; every failure is logged and boot continues (never a panic).
async fn session_demo(power: &PeripheralPower<Gpio27Rail>, storage: &SdStorage, wake: WakeCause) {
    if storage.probe_ok()
        && let Err(e) = storage::ensure_pulp_dir_async(storage).await
    {
        warn!("session: _PULP dir: {}", e);
    }
    let first = session::restore_session(power, storage);
    // R20: decision = f(wake cause, session); valid session restores for any cause
    let plan = sleep::plan_boot(wake, first);
    match plan {
        BootPlan::Restore { cause, restored } => info!(
            "boot plan: RESTORE position (cause {:?}, slot {:?} seq {}, reader chapter {} offset {})",
            cause,
            restored.slot,
            restored.seq,
            restored.state.reader_chapter,
            restored.state.reader_byte_offset
        ),
        BootPlan::Normal { cause, reason } => {
            info!("boot plan: normal boot (cause {:?}, {:?})", cause, reason)
        }
    }
    log_decision("boot", &first);
    if plan.is_restore() {
        return;
    }
    let mut state = SessionState::home();
    state.wake_count = 1;
    match session::save_session(power, storage, &state) {
        Ok(r) => info!("session: saved to slot {:?} seq {}", r.slot, r.seq),
        Err(e) => warn!("session: save failed: {:?} (continuing)", e),
    }
    log_decision(
        "after save (simulated restart)",
        &session::restore_session(power, storage),
    );
}

fn log_decision(when: &str, d: &BootDecision) {
    match d {
        BootDecision::Restore(r) => info!(
            "session[{}]: restore slot {:?} seq {} nav_depth {}",
            when, r.slot, r.seq, r.state.nav_depth
        ),
        BootDecision::NormalBoot(why) => info!("session[{}]: normal boot ({:?})", when, why),
    }
}

/// List the root directory so the storage error path is exercised: with no
/// card this returns `Error(NoCard)` (the error the Files app displays)
/// instead of panicking.
fn report_storage(storage: &SdStorage, health: &mut StorageHealth) {
    let mut entries = [storage::DirEntry::EMPTY; 8];
    let r = storage::list_root_files(storage, &mut entries);
    sd::observe_storage_result(health, &r);
    match r {
        Ok(n) => info!(
            "storage: {} root entries ({})",
            n,
            health.status().message()
        ),
        Err(e) => warn!("storage error: {} ({})", e, health.status().message()),
    }
}

/// Unrecoverable bring-up failure (no bus = no storage and no display): log
/// and idle instead of panicking.
async fn park() -> ! {
    loop {
        embassy_time::Timer::after(embassy_time::Duration::from_secs(5)).await;
        error!("pulp-os c61 boot: parked (spi2 init failed)");
    }
}
