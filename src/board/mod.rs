//! XTEink X4 Board Support Package (BSP)
//!
//! ## SPI Bus Sharing
//!
//! The e-paper display and SD card share SPI2 (SCK=GPIO8, MOSI=GPIO10).
//! SD also uses MISO=GPIO7 (display is write-only, ignores MISO).
//! Bus arbitration uses `RefCellDevice` from embedded-hal-bus — safe
//! because we're single-threaded bare-metal and ISRs don't touch SPI.

pub mod button;
pub mod display;
pub mod pins;
pub mod raw_gpio;
pub mod sdcard;
pub mod strip;

pub use button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};
pub use display::{DisplayDriver, HEIGHT, SPI_FREQ_MHZ, WIDTH};
pub use sdcard::SdStorage;
pub use strip::StripBuffer;

use core::cell::RefCell;

use critical_section::Mutex;
use embedded_hal_bus::spi::RefCellDevice;
use esp_hal::{
    Blocking,
    analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
    delay::Delay,
    gpio::{Event, Input, InputConfig, Io, Level, Output, OutputConfig, Pull},
    peripherals::{ADC1, GPIO0, GPIO1, GPIO2, Peripherals},
    spi,
    time::Rate,
};
use log::info;
use static_cell::StaticCell;

use crate::kernel::wake;

// Type Aliases
pub type SpiBus = spi::master::Spi<'static, Blocking>;
pub type SharedSpiDevice = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
pub type SdSpiDevice = RefCellDevice<'static, SpiBus, raw_gpio::RawOutputPin, Delay>;
pub type Epd = DisplayDriver<SharedSpiDevice, Output<'static>, Output<'static>, Input<'static>>;

// Static SPI bus — shared between display and SD card.
static SPI_BUS: StaticCell<RefCell<SpiBus>> = StaticCell::new();

// ── Power button interrupt ─────────────────────────────────────
//
// The power button (GPIO3, active low) is driven by a hardware GPIO
// interrupt instead of timer-based polling. On a falling edge the ISR
// sets the WAKE_BUTTON flag so the CPU wakes from WFI immediately,
// eliminating the 0–10 ms timer latency for this button.
//
// The `Input` lives in a static so the ISR can clear the interrupt
// and the InputDriver can still read the pin level for debouncing.

static POWER_BTN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));

/// GPIO interrupt handler — shared by all GPIO pins.
///
/// Handles two interrupt sources:
/// - **Power button (GPIO3)**: Pin is in a static, checked via esp-hal API.
/// - **Display BUSY (GPIO6)**: Pin is owned by DisplayDriver, so the ISR
///   checks and clears via raw register access. Falling edge signals that
///   a display refresh has completed.
#[esp_hal::handler]
fn gpio_handler() {
    // Power button (GPIO3) — pin in static, use esp-hal API
    critical_section::with(|cs| {
        if let Some(btn) = POWER_BTN.borrow_ref_mut(cs).as_mut() {
            if btn.is_interrupt_set() {
                btn.clear_interrupt();
                wake::signal_button();
            }
        }
    });

    // Display BUSY (GPIO6) — pin owned by DisplayDriver, check/clear
    // via raw register access. Falling edge = refresh complete.
    //
    // ESP32-C3 GPIO registers:
    //   STATUS  (0x6000_4044): read pending interrupt bits
    //   STATUS_W1TC (0x6000_404C): write-1-to-clear pending bits
    const GPIO_STATUS: *const u32 = 0x6000_4044 as *const u32;
    const GPIO_STATUS_W1TC: *mut u32 = 0x6000_404C as *mut u32;
    const GPIO6_MASK: u32 = 1 << 6;

    // Safety: single-core ISR context, no concurrent access to these registers.
    unsafe {
        if GPIO_STATUS.read_volatile() & GPIO6_MASK != 0 {
            GPIO_STATUS_W1TC.write_volatile(GPIO6_MASK);
            wake::signal_display();
        }
    }
}

/// Read the power button level from the ISR-shared static.
///
/// Returns `true` if the button is physically pressed (pin LOW).
/// Called by `InputDriver::read_raw()` on every input poll cycle.
pub fn power_button_is_low() -> bool {
    critical_section::with(|cs| {
        POWER_BTN
            .borrow_ref_mut(cs)
            .as_mut()
            .map(|btn| btn.is_low())
            .unwrap_or(false)
    })
}

// Hardware Bundles

/// Input subsystem: ADC for button ladders + battery.
///
/// The power button is NOT included here — it lives in the
/// `POWER_BTN` static and is read via `power_button_is_low()`.
pub struct InputHw {
    pub adc: Adc<'static, ADC1<'static>, Blocking>,
    pub row1: AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub row2: AdcPin<GPIO2<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
    pub battery: AdcPin<GPIO0<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
}

/// Display subsystem hardware.
pub struct DisplayHw {
    pub epd: Epd,
}

/// SD card storage hardware.
pub struct StorageHw {
    pub sd: SdStorage<SdSpiDevice>,
}

/// Complete board hardware.
pub struct Board {
    pub input: InputHw,
    pub display: DisplayHw,
    pub storage: StorageHw,
}

impl Board {
    pub fn init(p: Peripherals) -> Self {
        let input = Self::init_input(&p);
        let (display, storage) = Self::init_spi_peripherals(p);
        Board {
            input,
            display,
            storage,
        }
    }

    fn init_input(p: &Peripherals) -> InputHw {
        let mut adc_cfg = AdcConfig::new();

        let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO1.clone_unchecked() },
            Attenuation::_11dB,
        );

        let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO2.clone_unchecked() },
            Attenuation::_11dB,
        );

        let battery = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
            unsafe { p.GPIO0.clone_unchecked() },
            Attenuation::_11dB,
        );

        let adc = Adc::new(unsafe { p.ADC1.clone_unchecked() }, adc_cfg);

        // ── Power button: GPIO interrupt on falling edge ───────
        let mut io = Io::new(unsafe { p.IO_MUX.clone_unchecked() });
        io.set_interrupt_handler(gpio_handler);

        let mut power = Input::new(
            unsafe { p.GPIO3.clone_unchecked() },
            InputConfig::default().with_pull(Pull::Up),
        );
        power.listen(Event::FallingEdge);

        critical_section::with(|cs| {
            POWER_BTN.borrow_ref_mut(cs).replace(power);
        });
        info!("power button: GPIO3 interrupt armed (FallingEdge)");

        InputHw {
            adc,
            row1,
            row2,
            battery,
        }
    }

    /// Initialize SPI bus and all SPI peripherals (display + SD card).
    ///
    /// Three-phase init:
    /// 1. Create bus at 400kHz, send 74-clock preamble
    /// 2. Create SD device, probe card (triggers SD init at 400kHz)
    /// 3. Speed up to 20MHz, create display device
    fn init_spi_peripherals(p: Peripherals) -> (DisplayHw, StorageHw) {
        // Display GPIO
        let epd_cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
        let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
        let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());

        // BUSY pin (GPIO6): arm falling-edge interrupt BEFORE passing
        // to DisplayDriver. The hardware interrupt stays configured after
        // the pin is moved — gpio_handler checks GPIO6 via raw registers
        // since it can't access the pin through DisplayDriver.
        let mut busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));
        busy.listen(Event::FallingEdge);
        info!("display BUSY: GPIO6 interrupt armed (FallingEdge)");

        // SD card CS on GPIO12 (SPIHD). The X4 uses DIO flash mode so
        // GPIO12 is physically free, but esp-hal doesn't expose GPIO12-17
        // for ESP32-C3. Drive it via direct register access.
        let sd_cs = unsafe { raw_gpio::RawOutputPin::new(12) };

        // Phase 1: SPI bus at 400kHz for SD card identification.
        let slow_cfg = spi::master::Config::default().with_frequency(Rate::from_khz(400));

        let mut spi_bus = spi::master::Spi::new(p.SPI2, slow_cfg)
            .unwrap()
            .with_sck(p.GPIO8)
            .with_mosi(p.GPIO10)
            .with_miso(p.GPIO7);

        // 74+ clock cycles with CS deasserted (SD spec requirement).
        // 10 bytes × 8 bits = 80 clocks.
        let _ = spi_bus.write(&[0xFF; 10]);

        // Place bus in static RefCell for shared access.
        let spi_ref: &'static RefCell<SpiBus> = SPI_BUS.init(RefCell::new(spi_bus));

        // Phase 2: SD card init at 400kHz.
        // RefCellDevice::new() returns Result<_, Infallible>, always safe.
        let sd_spi = RefCellDevice::new(spi_ref, sd_cs, Delay::new()).unwrap();
        // SdStorage::new() probes the card internally (calls num_bytes()).
        let sd = SdStorage::new(sd_spi);

        // Phase 3: Speed up to 20MHz for display + normal SD operations.
        let fast_cfg = spi::master::Config::default().with_frequency(Rate::from_mhz(SPI_FREQ_MHZ));
        spi_ref.borrow_mut().apply_config(&fast_cfg).unwrap();
        info!("SPI bus: 400kHz -> {}MHz", SPI_FREQ_MHZ);

        // Create display device on the shared bus.
        let epd_spi = RefCellDevice::new(spi_ref, epd_cs, Delay::new()).unwrap();
        let epd = DisplayDriver::new(epd_spi, dc, rst, busy);

        (DisplayHw { epd }, StorageHw { sd })
    }
}
// //! XTEink X4 Board Support Package (BSP)
// //!
// //! ## SPI Bus Sharing
// //!
// //! The e-paper display and SD card share SPI2 (SCK=GPIO8, MOSI=GPIO10).
// //! SD also uses MISO=GPIO7 (display is write-only, ignores MISO).
// //! Bus arbitration uses `RefCellDevice` from embedded-hal-bus — safe
// //! because we're single-threaded bare-metal and ISRs don't touch SPI.
//
// pub mod button;
// pub mod display;
// pub mod pins;
// pub mod raw_gpio;
// pub mod sdcard;
// pub mod strip;
//
// pub use button::{Button, ROW1_THRESHOLDS, ROW2_THRESHOLDS, decode_ladder};
// pub use display::{DisplayDriver, HEIGHT, SPI_FREQ_MHZ, WIDTH};
// pub use sdcard::SdStorage;
// pub use strip::StripBuffer;
//
// use core::cell::RefCell;
//
// use critical_section::Mutex;
// use embedded_hal_bus::spi::RefCellDevice;
// use esp_hal::{
//     Blocking,
//     analog::adc::{Adc, AdcCalCurve, AdcConfig, AdcPin, Attenuation},
//     delay::Delay,
//     gpio::{Event, Input, InputConfig, Io, Level, Output, OutputConfig, Pull},
//     peripherals::{ADC1, GPIO0, GPIO1, GPIO2, Peripherals},
//     spi,
//     time::Rate,
// };
// use log::info;
// use static_cell::StaticCell;
//
// use crate::kernel::wake;
//
// // Type Aliases
// pub type SpiBus = spi::master::Spi<'static, Blocking>;
// pub type SharedSpiDevice = RefCellDevice<'static, SpiBus, Output<'static>, Delay>;
// pub type SdSpiDevice = RefCellDevice<'static, SpiBus, raw_gpio::RawOutputPin, Delay>;
// pub type Epd = DisplayDriver<SharedSpiDevice, Output<'static>, Output<'static>, Input<'static>>;
//
// // Static SPI bus — shared between display and SD card.
// static SPI_BUS: StaticCell<RefCell<SpiBus>> = StaticCell::new();
//
// // ── Power button interrupt ─────────────────────────────────────
// //
// // The power button (GPIO3, active low) is driven by a hardware GPIO
// // interrupt instead of timer-based polling. On a falling edge the ISR
// // sets the WAKE_BUTTON flag so the CPU wakes from WFI immediately,
// // eliminating the 0–10 ms timer latency for this button.
// //
// // The `Input` lives in a static so the ISR can clear the interrupt
// // and the InputDriver can still read the pin level for debouncing.
//
// static POWER_BTN: Mutex<RefCell<Option<Input<'static>>>> = Mutex::new(RefCell::new(None));
//
// /// GPIO interrupt handler — shared by all GPIO pins.
// ///
// /// Only the power button is currently wired. The handler checks
// /// `is_interrupt_set()` to confirm GPIO3 fired, clears the status,
// /// and signals the wake system. The interrupt stays armed (we call
// /// `clear_interrupt`, not `unlisten`) so subsequent edges fire too.
// #[esp_hal::handler]
// fn gpio_handler() {
//     critical_section::with(|cs| {
//         if let Some(btn) = POWER_BTN.borrow_ref_mut(cs).as_mut() {
//             if btn.is_interrupt_set() {
//                 btn.clear_interrupt();
//                 wake::signal_button();
//             }
//         }
//     });
// }
//
// /// Read the power button level from the ISR-shared static.
// ///
// /// Returns `true` if the button is physically pressed (pin LOW).
// /// Called by `InputDriver::read_raw()` on every input poll cycle.
// pub fn power_button_is_low() -> bool {
//     critical_section::with(|cs| {
//         POWER_BTN
//             .borrow_ref_mut(cs)
//             .as_mut()
//             .map(|btn| btn.is_low())
//             .unwrap_or(false)
//     })
// }
//
// // Hardware Bundles
//
// /// Input subsystem: ADC for button ladders + battery.
// ///
// /// The power button is NOT included here — it lives in the
// /// `POWER_BTN` static and is read via `power_button_is_low()`.
// pub struct InputHw {
//     pub adc: Adc<'static, ADC1<'static>, Blocking>,
//     pub row1: AdcPin<GPIO1<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
//     pub row2: AdcPin<GPIO2<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
//     pub battery: AdcPin<GPIO0<'static>, ADC1<'static>, AdcCalCurve<ADC1<'static>>>,
// }
//
// /// Display subsystem hardware.
// pub struct DisplayHw {
//     pub epd: Epd,
// }
//
// /// SD card storage hardware.
// pub struct StorageHw {
//     pub sd: SdStorage<SdSpiDevice>,
// }
//
// /// Complete board hardware.
// pub struct Board {
//     pub input: InputHw,
//     pub display: DisplayHw,
//     pub storage: StorageHw,
// }
//
// impl Board {
//     pub fn init(p: Peripherals) -> Self {
//         let input = Self::init_input(&p);
//         let (display, storage) = Self::init_spi_peripherals(p);
//         Board {
//             input,
//             display,
//             storage,
//         }
//     }
//
//     fn init_input(p: &Peripherals) -> InputHw {
//         let mut adc_cfg = AdcConfig::new();
//
//         let row1 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
//             unsafe { p.GPIO1.clone_unchecked() },
//             Attenuation::_11dB,
//         );
//
//         let row2 = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
//             unsafe { p.GPIO2.clone_unchecked() },
//             Attenuation::_11dB,
//         );
//
//         let battery = adc_cfg.enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(
//             unsafe { p.GPIO0.clone_unchecked() },
//             Attenuation::_11dB,
//         );
//
//         let adc = Adc::new(unsafe { p.ADC1.clone_unchecked() }, adc_cfg);
//
//         // ── Power button: GPIO interrupt on falling edge ───────
//         let mut io = Io::new(unsafe { p.IO_MUX.clone_unchecked() });
//         io.set_interrupt_handler(gpio_handler);
//
//         let mut power = Input::new(
//             unsafe { p.GPIO3.clone_unchecked() },
//             InputConfig::default().with_pull(Pull::Up),
//         );
//         power.listen(Event::FallingEdge);
//
//         critical_section::with(|cs| {
//             POWER_BTN.borrow_ref_mut(cs).replace(power);
//         });
//         info!("power button: GPIO3 interrupt armed (FallingEdge)");
//
//         InputHw {
//             adc,
//             row1,
//             row2,
//             battery,
//         }
//     }
//
//     /// Initialize SPI bus and all SPI peripherals (display + SD card).
//     ///
//     /// Three-phase init:
//     /// 1. Create bus at 400kHz, send 74-clock preamble
//     /// 2. Create SD device, probe card (triggers SD init at 400kHz)
//     /// 3. Speed up to 20MHz, create display device
//     fn init_spi_peripherals(p: Peripherals) -> (DisplayHw, StorageHw) {
//         // Display GPIO
//         let epd_cs = Output::new(p.GPIO21, Level::High, OutputConfig::default());
//         let dc = Output::new(p.GPIO4, Level::High, OutputConfig::default());
//         let rst = Output::new(p.GPIO5, Level::High, OutputConfig::default());
//         let busy = Input::new(p.GPIO6, InputConfig::default().with_pull(Pull::None));
//
//         // SD card CS on GPIO12 (SPIHD). The X4 uses DIO flash mode so
//         // GPIO12 is physically free, but esp-hal doesn't expose GPIO12-17
//         // for ESP32-C3. Drive it via direct register access.
//         let sd_cs = unsafe { raw_gpio::RawOutputPin::new(12) };
//
//         // Phase 1: SPI bus at 400kHz for SD card identification.
//         let slow_cfg = spi::master::Config::default()
//             .with_frequency(Rate::from_khz(400));
//
//         let mut spi_bus = spi::master::Spi::new(p.SPI2, slow_cfg)
//             .unwrap()
//             .with_sck(p.GPIO8)
//             .with_mosi(p.GPIO10)
//             .with_miso(p.GPIO7);
//
//         // 74+ clock cycles with CS deasserted (SD spec requirement).
//         // 10 bytes × 8 bits = 80 clocks.
//         let _ = spi_bus.write(&[0xFF; 10]);
//
//         // Place bus in static RefCell for shared access.
//         let spi_ref: &'static RefCell<SpiBus> = SPI_BUS.init(RefCell::new(spi_bus));
//
//         // Phase 2: SD card init at 400kHz.
//         // RefCellDevice::new() returns Result<_, Infallible>, always safe.
//         let sd_spi = RefCellDevice::new(spi_ref, sd_cs, Delay::new()).unwrap();
//         // SdStorage::new() probes the card internally (calls num_bytes()).
//         let sd = SdStorage::new(sd_spi);
//
//         // Phase 3: Speed up to 20MHz for display + normal SD operations.
//         let fast_cfg = spi::master::Config::default()
//             .with_frequency(Rate::from_mhz(SPI_FREQ_MHZ));
//         spi_ref.borrow_mut().apply_config(&fast_cfg).unwrap();
//         info!("SPI bus: 400kHz -> {}MHz", SPI_FREQ_MHZ);
//
//         // Create display device on the shared bus.
//         let epd_spi = RefCellDevice::new(spi_ref, epd_cs, Delay::new()).unwrap();
//         let epd = DisplayDriver::new(epd_spi, dc, rst, busy);
//
//         (DisplayHw { epd }, StorageHw { sd })
//     }
// }
