//! Raw GPIO output for pins not exposed by esp-hal (e.g. flash pins on ESP32-C3).
//!
//! The XTEink X4 uses DIO flash mode, freeing GPIO12 (SPIHD) and GPIO13 (SPIWP)
//! for general use. esp-hal 1.0 doesn't generate peripheral types for GPIO12-17
//! on ESP32-C3, so we drive the pin via direct register writes.
const GPIO_OUT_W1TS: u32 = 0x6000_4008; // Set output high (write-1-to-set)
const GPIO_OUT_W1TC: u32 = 0x6000_400C; // Set output low  (write-1-to-clear)
const GPIO_ENABLE_W1TS: u32 = 0x6000_4024; // Enable output (write-1-to-set)
const IO_MUX_BASE: u32 = 0x6000_9000; // IO_MUX register base
const IO_MUX_PIN_STRIDE: u32 = 0x04; // Each pin has a 4-byte register

// Minimal output-only GPIO driver using direct register access.
pub struct RawOutputPin {
    mask: u32, // Bit mask for this pin (1 << pin_number)
}

impl RawOutputPin {
    // Configure a GPIO as push-pull output, initially HIGH.
    //
    // Safety: Caller must ensure
    // - The pin is physically available (not connected to active flash lines)
    // - No other driver is controlling the same pin
    pub unsafe fn new(pin: u8) -> Self {
        let mask = 1u32 << pin;

        // Configure IO_MUX: select GPIO function (function 1), enable output
        let mux_reg = (IO_MUX_BASE + pin as u32 * IO_MUX_PIN_STRIDE) as *mut u32;
        // Bits [14:12] = FUN_DRV (drive strength, default 2)
        // Bits [11:10] = 0 (no pull-up/down)
        // Bit  [9]     = FUN_IE (input enable) = 0
        // Bits [2:0]   = MCU_SEL (function select) = 1 (GPIO)
        //
        // read-modify-write to preserve reserved bits, but set function to GPIO.
        let val = mux_reg.read_volatile();
        let val = (val & !0b111) | 1; // MCU_SEL = 1 (GPIO function)
        mux_reg.write_volatile(val);

        // enable output for this pin
        // GPIO_FUNCn_OUT_SEL_CFG register (base 0x60004554, stride 4)
        let out_sel = (0x6000_4554 + pin as u32 * 4) as *mut u32;
        out_sel.write_volatile(0x80); // SIG_OUT = 128 (simple GPIO output)

        // Enable output
        (GPIO_ENABLE_W1TS as *mut u32).write_volatile(mask);

        // Drive HIGH initially (CS deasserted)
        (GPIO_OUT_W1TS as *mut u32).write_volatile(mask);

        Self { mask }
    }
}

impl embedded_hal::digital::ErrorType for RawOutputPin {
    type Error = core::convert::Infallible;
}

impl embedded_hal::digital::OutputPin for RawOutputPin {
    #[inline]
    fn set_high(&mut self) -> Result<(), Self::Error> {
        unsafe {
            (GPIO_OUT_W1TS as *mut u32).write_volatile(self.mask);
        }
        Ok(())
    }

    #[inline]
    fn set_low(&mut self) -> Result<(), Self::Error> {
        unsafe {
            (GPIO_OUT_W1TC as *mut u32).write_volatile(self.mask);
        }
        Ok(())
    }
}
