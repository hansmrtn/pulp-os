// Shared SPI2 bus plan and mutual-exclusion transaction rules (R8).
//
// EPD and SD share one SPI2 bus (SCK22 / MOSI23 / MISO24, board_c61.c:37-38,
// 43, 96); each device has its own chip select (EPD CS25 :39, SD CS26 :58).
// The kernel owns the bus behind a critical-section mutex and routes every
// access through `SpiArbiter::transaction`, which is the only code that moves
// a chip select:
//   * at most one device is selected at any time;
//   * CS is asserted before the first operation and released after the last,
//     also when the closure fails (so a failed SD read cannot leave the bus
//     selected and corrupt the next EPD frame);
//   * a transaction started while another one is open is refused with
//     `BusError::Busy` instead of panicking (R9: no panic on the SD path).
//
// Bus timing and buffer constants are mirrored from the BSP so they are
// reviewable (and tested) off-target.

/// SD cards must be probed at <= 400 kHz (SD spec; ESP-IDF sdspi probes at
/// SDMMC_FREQ_PROBING = 400 kHz). Same value the X4 port uses.
pub const SPI_INIT_KHZ: u32 = 400;
/// Operating clock for both EPD and SD after init: BSP `SPI_FREQ_HZ`
/// (10 MHz, board_c61.c:44) and SD `host.max_freq_khz = 10000` (:391,
/// "10MHz stable clock for shared SPI bus"). (X4 runs 20 MHz; not carried
/// over.)
pub const SPI_OPERATING_KHZ: u32 = 10_000;
/// DMA buffer per direction, internal RAM (R15). Same sizing rationale as X4:
/// SD sector 512 B, EPD strip well below 4 KiB.
pub const SPI_DMA_BUF_BYTES: usize = 4096;
/// SD spec: >= 74 clocks with CS high before CMD0. 10 bytes = 80 clocks.
pub const SD_PRELUDE_BYTES: usize = 10;

/// The two devices on the shared bus.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Device {
    Epd,
    Sd,
}

/// Active-low chip select line (esp-hal `Output` in the kernel).
/// `assert` selects the device (pin low), `release` deselects it (pin high).
pub trait ChipSelect {
    fn assert(&mut self);
    fn release(&mut self);
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BusError<E> {
    /// Another transaction is open; nothing was driven. Holds the owner.
    Busy(Device),
    /// The device operation itself failed (CS was still released).
    Transfer(E),
}

pub struct SpiArbiter<E: ChipSelect, S: ChipSelect> {
    epd_cs: E,
    sd_cs: S,
    owner: Option<Device>,
}

impl<E: ChipSelect, S: ChipSelect> SpiArbiter<E, S> {
    /// Takes both chip selects and drives them to the idle (released) level.
    pub fn new(mut epd_cs: E, mut sd_cs: S) -> Self {
        epd_cs.release();
        sd_cs.release();
        Self {
            epd_cs,
            sd_cs,
            owner: None,
        }
    }

    /// Device currently holding the bus, if any.
    pub fn owner(&self) -> Option<Device> {
        self.owner
    }

    fn cs(&mut self, dev: Device) -> &mut dyn ChipSelect {
        match dev {
            Device::Epd => &mut self.epd_cs,
            Device::Sd => &mut self.sd_cs,
        }
    }

    /// Shutdown only (BSP `board_sleep_enter` drives SD_CS and EPD CS low so
    /// the shared lines cannot back-power the cut-off rail, board_c61.c:341-342).
    /// Drives both chip selects to the asserted (low) level. Refused with
    /// `Busy` while a transaction is open (nothing driven). The arbiter is not
    /// meant to be used for transfers afterwards: the SD/EPD supply is gone.
    pub fn park_selects_low(&mut self) -> Result<(), Device> {
        if let Some(owner) = self.owner {
            return Err(owner);
        }
        self.epd_cs.assert();
        self.sd_cs.assert();
        Ok(())
    }

    /// Run `f` (bus operations) with `dev` selected: assert CS, run, release
    /// CS. `f` must not call back into the arbiter (the kernel holds the bus
    /// mutex across the whole call); if it does, the inner call is refused
    /// with `Busy`.
    pub fn transaction<R, Er>(
        &mut self,
        dev: Device,
        f: impl FnOnce() -> Result<R, Er>,
    ) -> Result<R, BusError<Er>> {
        if let Some(owner) = self.owner {
            return Err(BusError::Busy(owner));
        }
        self.owner = Some(dev);
        self.cs(dev).assert();
        let r = f();
        self.cs(dev).release();
        self.owner = None;
        r.map_err(BusError::Transfer)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::vec;
    use std::vec::Vec;

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    enum Ev {
        Assert(Device),
        Release(Device),
        Bus(&'static str),
    }

    type Log = Rc<RefCell<Vec<Ev>>>;

    struct Cs(Device, Log);
    impl ChipSelect for Cs {
        fn assert(&mut self) {
            self.1.borrow_mut().push(Ev::Assert(self.0));
        }
        fn release(&mut self) {
            self.1.borrow_mut().push(Ev::Release(self.0));
        }
    }

    fn rig() -> (SpiArbiter<Cs, Cs>, Log) {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let arb = SpiArbiter::new(Cs(Device::Epd, log.clone()), Cs(Device::Sd, log.clone()));
        log.borrow_mut().clear(); // drop the idle-release events from new()
        (arb, log)
    }

    // replay the log and check the bus invariants: at most one CS asserted
    // at a time, and bus traffic only while exactly its device is selected
    fn assert_exclusive(log: &Log) {
        let mut selected: Option<Device> = None;
        for ev in log.borrow().iter() {
            match *ev {
                Ev::Assert(d) => {
                    assert_eq!(selected, None, "second CS asserted while {selected:?} held");
                    selected = Some(d);
                }
                Ev::Release(d) => {
                    assert_eq!(
                        selected,
                        Some(d),
                        "release of a device that was not selected"
                    );
                    selected = None;
                }
                Ev::Bus(_) => assert!(selected.is_some(), "bus traffic with no CS asserted"),
            }
        }
        assert_eq!(selected, None, "CS left asserted");
    }

    #[test]
    fn r8_bus_pins_are_the_bsp_spi2_pins() {
        use crate::pins::*;
        assert_eq!((SPI_SCK, SPI_MOSI, SPI_MISO), (22, 23, 24));
        assert_eq!((EPD_CS, SD_CS), (25, 26));
    }

    #[test]
    fn r8_bus_plan_matches_bsp_and_sd_spec() {
        assert_eq!(SPI_INIT_KHZ, 400);
        assert_eq!(SPI_OPERATING_KHZ, 10_000);
        assert!(SPI_INIT_KHZ <= 400, "SD probe clock");
        assert!(
            SD_PRELUDE_BYTES * 8 >= 74,
            "SD spec: >= 74 clocks before CMD0"
        );
        assert!(
            SPI_DMA_BUF_BYTES >= 512,
            "one SD sector must fit one DMA chunk"
        );
    }

    #[test]
    fn r8_new_releases_both_chip_selects() {
        let log: Log = Rc::new(RefCell::new(Vec::new()));
        let _ = SpiArbiter::new(Cs(Device::Epd, log.clone()), Cs(Device::Sd, log.clone()));
        assert_eq!(
            *log.borrow(),
            vec![Ev::Release(Device::Epd), Ev::Release(Device::Sd)]
        );
    }

    #[test]
    fn r8_transaction_is_assert_ops_release_for_the_right_device() {
        let (mut a, log) = rig();
        let l = log.clone();
        a.transaction(Device::Sd, || {
            l.borrow_mut().push(Ev::Bus("cmd"));
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(
            *log.borrow(),
            vec![
                Ev::Assert(Device::Sd),
                Ev::Bus("cmd"),
                Ev::Release(Device::Sd)
            ]
        );
        assert_eq!(a.owner(), None);
    }

    #[test]
    fn r8_alternating_epd_and_sd_never_overlap() {
        let (mut a, log) = rig();
        for dev in [Device::Epd, Device::Sd, Device::Sd, Device::Epd, Device::Sd] {
            let l = log.clone();
            a.transaction(dev, || {
                l.borrow_mut().push(Ev::Bus("x"));
                Ok::<_, ()>(())
            })
            .unwrap();
        }
        assert_exclusive(&log);
        assert_eq!(log.borrow().len(), 15);
    }

    #[test]
    fn r8_cs_is_released_when_the_transfer_fails() {
        let (mut a, log) = rig();
        let l = log.clone();
        let r: Result<(), _> = a.transaction(Device::Sd, || {
            l.borrow_mut().push(Ev::Bus("read"));
            Err::<(), _>("crc")
        });
        assert_eq!(r, Err(BusError::Transfer("crc")));
        assert_eq!(a.owner(), None);
        assert_exclusive(&log);
        // bus is usable again for the other device
        let l = log.clone();
        a.transaction(Device::Epd, || {
            l.borrow_mut().push(Ev::Bus("frame"));
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_exclusive(&log);
    }

    #[test]
    fn r8_reentrant_transaction_is_refused_without_driving_any_cs() {
        // models the closure calling back into the arbiter while it is held:
        // the arbiter is `&mut`, so the real path cannot even compile; the
        // kernel guards the equivalent case (RefCell already borrowed) with
        // try_borrow_mut -> Busy. Here: owner is set while f runs.
        let (mut a, log) = rig();
        a.owner = Some(Device::Epd); // simulate "inside an EPD transaction"
        log.borrow_mut().clear();
        let r = a.transaction(Device::Sd, || Ok::<_, ()>(()));
        assert_eq!(r, Err(BusError::Busy(Device::Epd)));
        assert!(log.borrow().is_empty(), "refused transaction drove a CS");
        assert_eq!(a.owner(), Some(Device::Epd), "owner must be untouched");
    }

    #[test]
    fn r19_shutdown_parks_both_selects_low_only_when_idle() {
        let (mut a, log) = rig();
        a.park_selects_low().unwrap();
        assert_eq!(
            *log.borrow(),
            vec![Ev::Assert(Device::Epd), Ev::Assert(Device::Sd)]
        );
        // refused (and nothing driven) while a transaction owns the bus
        let (mut a, log) = rig();
        a.owner = Some(Device::Sd);
        assert_eq!(a.park_selects_low(), Err(Device::Sd));
        assert!(log.borrow().is_empty());
    }
}
