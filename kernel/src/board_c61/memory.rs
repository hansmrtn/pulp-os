// OnePage C61 memory placement (R14, R15): esp-hal PSRAM bring-up, a separate
// PSRAM heap, and budget-checked allocation entry points.
//
// Policy (rules, limits and the failure state machine are in
// `pulp_board_logic::memory`, host-tested; this file only maps them onto
// esp-hal 1.2 / esp-alloc 0.11):
//
//   * The global allocator (`esp_alloc::HEAP`) only ever gets INTERNAL
//     regions (`heap_allocator!`). PSRAM is registered on a second, private
//     `EspHeap` (`PSRAM_HEAP`), NOT through `esp_alloc::psram_allocator!`:
//     that macro adds an External region to the global heap, and a plain
//     `Box`/`Vec` would then fall into PSRAM as soon as the internal region is
//     full (esp-alloc tries every region when no capability is requested).
//     Runtime, executor, ISR-visible data and every DMA buffer would silently
//     lose their internal-memory guarantee (R15).
//   * PSRAM is reachable only through `alloc_external(ExternalClass, ..)`
//     (the enum has no DMA/ISR/runtime variant, so asking for those does not
//     compile) or `alloc(MemClass, ..)`, which places by the budget rule.
//     Every allocation is charged against one `MemoryBudget`; over budget is
//     an `Err`, never a panic and never an out-of-memory spiral.
//   * Statics (SPI DMA `BUFFER`/`DESCRIPTORS`, the main heap, task arena)
//     are internal by construction (the linker never maps PSRAM) and
//     esp-hal's `DmaRxBuf::new` rejects descriptors outside DRAM; the ELF check
//     (`scripts/report-c61-memory.sh`) proves their addresses.
//   * PSRAM failure: `init` returns `Degraded(..)`, nothing is registered, the
//     budget switches to the internal limits and the reader keeps working
//     offline in X4-sized memory. No panic on that path.
//
// Not verified on hardware: PSRAM detect/init, 40 MHz stability, the smoke
// test, real heap use, cache behaviour.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::RefCell;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};

use critical_section::Mutex;
use esp_alloc::{EspHeap, HeapRegion, MemoryCapability};
use esp_hal::peripherals::PSRAM;
use esp_hal::psram::{FlashFreq, Psram, PsramConfig, SpiRamFreq};
use log::{error, info, warn};
pub use pulp_board_logic::memory::{
    ExternalClass, INTERNAL_HEAP_MAIN_BYTES, INTERNAL_HEAP_RECLAIMED_BYTES, MemClass, MemError,
    MemoryBudget, PSRAM_CHAPTER_TEXT_BYTES, PsramFault, PsramStatus, Region, Reservation,
};
use pulp_board_logic::memory::{
    FLASH_MHZ, PSRAM_HW_BYTES, PSRAM_MHZ, PSRAM_MIN_BYTES, WordMem, evaluate_psram,
    range_is_internal, selftest, window_fault,
};

/// PSRAM-only heap. Never the global allocator (see the policy above).
static PSRAM_HEAP: EspHeap = EspHeap::empty();
static BUDGET: Mutex<RefCell<MemoryBudget>> = Mutex::new(RefCell::new(MemoryBudget::new()));
static INIT_DONE: AtomicBool = AtomicBool::new(false);

/// DMA buffers are 4-byte aligned on the C61: internal RAM is not cached
/// (`soc.internal_memory_cached = false` in esp-metadata), same value as
/// `esp_alloc::DmaCompatibleInternalMemory` uses for such chips.
const DMA_ALIGN: usize = 4;

// The flash clock the image header selects and the PSRAM clock are the
// bring-up values of proposal.md; the config below must say the same.
const _: () = assert!(FLASH_MHZ == 40 && PSRAM_MHZ == 40);

unsafe extern "C" {
    // ROM cache maintenance, the same functions esp-hal's DMA code calls
    // (`esp_hal::soc::cache_writeback_addr` is private to the HAL).
    fn Cache_WriteBack_Addr(addr: u32, size: u32);
    fn Cache_Invalidate_Addr(addr: u32, size: u32);
}

/// Push `[addr, addr+size)` out of the data cache and drop the cached copy so
/// the next read really comes from the chip.
#[esp_hal::ram]
fn cache_flush(addr: usize, size: usize) {
    // SAFETY: ROM functions on an address range inside the PSRAM window that
    // this module owns while the smoke test runs.
    unsafe {
        Cache_WriteBack_Addr(addr as u32, size as u32);
        Cache_Invalidate_Addr(addr as u32, size as u32);
    }
}

/// Volatile word access to the PSRAM window for `selftest`.
struct PsramWords {
    base: *mut u32,
}

impl WordMem for PsramWords {
    fn write(&mut self, word: usize, value: u32) {
        // SAFETY: `selftest` only addresses words below the length it was
        // given, which is inside the window `init` validated.
        unsafe {
            let p = self.base.add(word);
            p.write_volatile(value);
            cache_flush(p as usize, 4);
        }
    }

    fn read(&mut self, word: usize) -> u32 {
        // SAFETY: as in `write`.
        unsafe { self.base.add(word).read_volatile() }
    }
}

/// Bring PSRAM up (40 MHz flash and RAM), validate and smoke-test it, register
/// it on the PSRAM heap and record the result in the budget. Returns the
/// status; every failure is a `Degraded(..)` status, never a panic. Call once,
/// early, after the internal heap exists. A second call returns the stored
/// status.
pub fn init(peripheral: PSRAM<'static>) -> PsramStatus {
    if INIT_DONE.swap(true, Ordering::AcqRel) {
        return status();
    }

    // esp-hal's default is flash 80 MHz and would re-clock the flash the
    // image header set to 40 MHz (80 MHz flash boot-loops on this board per the
    // BSP README), so both clocks are spelled out.
    let config = PsramConfig {
        flash_frequency: FlashFreq::FlashFreq40m,
        ram_frequency: SpiRamFreq::Freq40m,
        ..PsramConfig::default()
    };
    let psram = Psram::new(peripheral, config);
    let (start, size) = psram.raw_parts();
    // Keep the peripheral token for the whole run.
    core::mem::forget(psram);
    let start = start as usize;

    let status = if let Some(fault) = window_fault(start, size) {
        PsramStatus::Degraded(fault)
    } else if size < PSRAM_MIN_BYTES {
        // 0 = chip id unknown / absent; below the minimum = not worth it.
        evaluate_psram(size, Ok(()))
    } else {
        let usable = size.min(PSRAM_HW_BYTES);
        let mut mem = PsramWords {
            base: start as *mut u32,
        };
        let test = selftest(&mut mem, usable / 4);
        evaluate_psram(size, test)
    };

    if let PsramStatus::Ready { bytes } = status {
        // SAFETY: the window is exclusively ours (nothing else maps it), it is
        // 'static, non-empty (>= PSRAM_MIN_BYTES) and no allocator has
        // touched it yet; the smoke test above only left test patterns in it.
        unsafe {
            PSRAM_HEAP.add_region(HeapRegion::new(
                start as *mut u8,
                bytes,
                MemoryCapability::External.into(),
            ));
        }
        info!("psram: {} B at {:#010x}, heap registered", bytes, start);
    } else {
        warn!(
            "{} (window {:#010x}+{:#x}); running on internal memory",
            status.describe(),
            start,
            size
        );
    }

    let stored = critical_section::with(|cs| BUDGET.borrow_ref_mut(cs).set_status(status));
    if let Err(e) = stored {
        // Cannot happen at boot (nothing reserved yet); keep the safe default.
        error!("psram: budget refused status {:?}: {:?}", status, e);
        return PsramStatus::NotInitialised;
    }
    status
}

pub fn status() -> PsramStatus {
    critical_section::with(|cs| BUDGET.borrow_ref(cs).status())
}

/// Bytes currently charged to `class` in `region`.
pub fn used(region: Region, class: MemClass) -> usize {
    critical_section::with(|cs| BUDGET.borrow_ref(cs).used(region, class))
}

pub fn pool_used(region: Region) -> usize {
    critical_section::with(|cs| BUDGET.borrow_ref(cs).pool_used(region))
}

pub fn pool_limit(region: Region) -> usize {
    critical_section::with(|cs| BUDGET.borrow_ref(cs).pool_limit(region))
}

/// A budget-charged, zero-initialised block. Dropping it frees the block and
/// returns the charge.
pub struct MemBuf {
    ptr: NonNull<u8>,
    layout: Layout,
    res: Reservation,
}

// SAFETY: the block is exclusively owned and the allocators it came from are
// critical-section protected.
unsafe impl Send for MemBuf {}

impl MemBuf {
    pub fn region(&self) -> Region {
        self.res.region
    }

    pub fn class(&self) -> MemClass {
        self.res.class
    }

    pub fn addr(&self) -> usize {
        self.ptr.as_ptr() as usize
    }

    pub fn len(&self) -> usize {
        self.layout.size()
    }

    pub fn is_empty(&self) -> bool {
        self.layout.size() == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `len` bytes, initialised (zeroed at allocation), owned.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.layout.size()) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` is unique.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }
}

impl Drop for MemBuf {
    fn drop(&mut self) {
        // SAFETY: allocated from the heap matching `res.region` with `layout`.
        unsafe {
            match self.res.region {
                Region::Internal => {
                    GlobalAlloc::dealloc(&esp_alloc::HEAP, self.ptr.as_ptr(), self.layout)
                }
                Region::Psram => GlobalAlloc::dealloc(&PSRAM_HEAP, self.ptr.as_ptr(), self.layout),
            }
        }
        let released = critical_section::with(|cs| BUDGET.borrow_ref_mut(cs).release(self.res));
        if let Err(e) = released {
            error!("memory: release of {:?} refused: {:?}", self.res, e);
        }
    }
}

fn alloc_in(
    region: Option<Region>,
    class: MemClass,
    size: usize,
    align: usize,
) -> Result<MemBuf, MemError> {
    let res = critical_section::with(|cs| {
        let mut b = BUDGET.borrow_ref_mut(cs);
        match region {
            Some(r) => b.reserve_in(r, class, size, align),
            None => b.reserve(class, size, align),
        }
    })?;

    let give_back = |e: MemError| {
        let _ = critical_section::with(|cs| BUDGET.borrow_ref_mut(cs).release(res));
        e
    };

    let layout = Layout::from_size_align(size, align).map_err(|_| give_back(MemError::Overflow))?;
    // SAFETY: `layout` has non-zero size (the budget rejected size 0).
    let raw = unsafe {
        match res.region {
            // Internal capability only: even if an External region were ever
            // added to the global heap, this request cannot land in it.
            Region::Internal => {
                esp_alloc::HEAP.alloc_caps(MemoryCapability::Internal.into(), layout)
            }
            Region::Psram => GlobalAlloc::alloc(&PSRAM_HEAP, layout),
        }
    };
    let Some(ptr) = NonNull::new(raw) else {
        return Err(give_back(MemError::OutOfMemory));
    };
    // SAFETY: `size` bytes just allocated and owned.
    unsafe { ptr.as_ptr().write_bytes(0, size) };
    Ok(MemBuf { ptr, layout, res })
}

/// Allocate with the budget's own placement: PSRAM for PSRAM-capable classes
/// while it is `Ready`, internal (with the degraded internal limits) otherwise.
/// Classes that may not live in PSRAM are always internal.
pub fn alloc(class: MemClass, size: usize, align: usize) -> Result<MemBuf, MemError> {
    alloc_in(None, class, size, align)
}

/// Allocate in PSRAM only. Takes `ExternalClass`, which has no DMA / ISR /
/// runtime variant (R15); `Err(ClassLimit { limit: 0, .. })` when PSRAM is not
/// `Ready`.
pub fn alloc_external(class: ExternalClass, size: usize, align: usize) -> Result<MemBuf, MemError> {
    alloc_in(Some(Region::Psram), class.into(), size, align)
}

/// Allocate a DMA buffer: internal RAM, `DMA_ALIGN`, and the returned address
/// is re-checked against the internal range before it is handed out.
pub fn alloc_dma(size: usize) -> Result<MemBuf, MemError> {
    let buf = alloc_in(Some(Region::Internal), MemClass::DmaBuffer, size, DMA_ALIGN)?;
    if !range_is_internal(buf.addr(), buf.len()) {
        // dropping `buf` frees it and returns the charge
        return Err(MemError::RegionForbidden {
            class: MemClass::DmaBuffer,
            region: Region::Psram,
        });
    }
    Ok(buf)
}

/// One log line per region and the PSRAM class table (for bring-up logs).
pub fn log_report() {
    info!("{}", status().describe());
    for region in [Region::Internal, Region::Psram] {
        info!(
            "memory {:?}: pool {} / {} B",
            region,
            pool_used(region),
            pool_limit(region)
        );
    }
    info!(
        "heap internal: used {} B, free {} B; psram heap: used {} B, free {} B",
        esp_alloc::HEAP.used(),
        esp_alloc::HEAP.free(),
        PSRAM_HEAP.used(),
        PSRAM_HEAP.free()
    );
}
