// Direct register GPIO for pins esp-hal does not expose.
// DIO flash mode frees GPIO12/13; esp-hal 1.0 has no peripheral types
// for GPIO12..17 on ESP32-C3. Only OutputPin is implemented.

const GPIO_OUT_W1TS: u32 = 0x6000_4008; // set output high
const GPIO_OUT_W1TC: u32 = 0x6000_400C; // set output low
const GPIO_ENABLE_W1TS: u32 = 0x6000_4024; // enable output
const IO_MUX_BASE: u32 = 0x6000_9000;
const IO_MUX_PIN_STRIDE: u32 = 0x04;

pub struct RawOutputPin {
    mask: u32,
}

impl RawOutputPin {
    // safety: pin must not be in use by flash or another driver
    pub unsafe fn new(pin: u8) -> Self {
        let mask = 1u32 << pin;

        let mux_reg = (IO_MUX_BASE + pin as u32 * IO_MUX_PIN_STRIDE) as *mut u32;

        unsafe {
            // IO_MUX: MCU_SEL[14:12] = 1 selects GPIO function
            let val = mux_reg.read_volatile();
            let val = (val & !(0b111 << 12)) | (1 << 12);
            mux_reg.write_volatile(val);

            // GPIO_FUNCn_OUT_SEL_CFG: 0x80 = simple GPIO output
            let out_sel = (0x6000_4554 + pin as u32 * 4) as *mut u32;
            out_sel.write_volatile(0x80);

            (GPIO_ENABLE_W1TS as *mut u32).write_volatile(mask);

            (GPIO_OUT_W1TS as *mut u32).write_volatile(mask);
        }

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
