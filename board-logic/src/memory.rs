// Memory placement policy and allocation budgets (R14, R15).
//
// The OnePage C61 has 320 KiB of HP SRAM ("internal") and one 2 MB PSRAM chip
// mapped through the flash cache MMU ("external"). The X4 has internal RAM
// only. This module holds everything that can be decided without a HAL, so the
// firmware and the host tests run the same code:
//
//   * the C61 address map the placement rules are written against;
//   * which allocation classes may live in PSRAM (R15: DMA buffers/descriptors,
//     ISR-visible data and the runtime heap never may);
//   * per-class and per-pool budgets with checked arithmetic (R14: a request
//     over budget is an `Err`, never a panic and never an out-of-memory spiral);
//   * the PSRAM bring-up state machine (not initialised / ready / degraded) and
//     the fallback to smaller internal limits;
//   * the PSRAM smoke test (`selftest`) over a word-memory trait;
//   * the inventory of every large (>= 4 KiB) allocation in the firmware with
//     its classification, for the T12 migration.
//
// Nothing here touches hardware. The kernel's `board_c61::memory` registers the
// PSRAM region on its own `EspHeap` (never on the global heap, see below) and
// charges every allocation against a `MemoryBudget` of this module.
//
// NOT verified on hardware: PSRAM init, 40 MHz stability, real heap use.

// ---------------------------------------------------------------------------
// units

pub const KIB: usize = 1024;
pub const MIB: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// C61 memory map. Source of truth: esp-hal 1.2.0 `ld/esp32c61/memory.x`
// (RAM, dram2_seg, MEMORY_MAP comment) and esp-metadata-generated 0.5.3
// `psram.extmem_origin` = 1107296256 = 0x4200_0000.
// `scripts/report-c61-memory.sh` diffs these literals against memory.x.

/// Start of HP SRAM (D/IRAM alias used by the linker).
pub const C61_RAM_START: usize = 0x4080_0000;
/// memory.x `RAM`: what the linker gives .trap/.rwtext/.data/.bss/.stack.
pub const C61_RAM_LEN: usize = 0x3EA70;
/// memory.x `dram2_seg`: starts right after `RAM`, usable once the 2nd stage
/// bootloader is done (`#[ram(reclaimed)]`).
pub const C61_RECLAIMED_START: usize = 0x4083_EA70;
pub const C61_RECLAIMED_LEN: usize = 0x1_0000;
/// One past the last internal RAM byte we ever hand out.
pub const C61_RAM_END: usize = C61_RECLAIMED_START + C61_RECLAIMED_LEN;
/// Flash/PSRAM cache window (memory.x `MEMORY_MAP` DROM 0x4200_0000..0x4600_0000;
/// `psram.extmem_origin`). PSRAM is mapped after the flash pages the image
/// uses, so its exact start is only known at run time (`Psram::raw_parts`);
/// anything here is flash-mapped OR PSRAM, never internal.
pub const C61_EXTMEM_START: usize = 0x4200_0000;
pub const C61_EXTMEM_END: usize = 0x4600_0000;

/// Which bus/memory an address belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AddrSpace {
    /// HP SRAM, reachable with the cache off, by DMA and by ISRs.
    InternalRam,
    /// Flash/PSRAM cache window: unusable while the cache is off, not valid
    /// for DMA descriptors/buffers on this firmware.
    ExternalMapped,
    Other,
}

pub const fn addr_space(addr: usize) -> AddrSpace {
    if addr >= C61_RAM_START && addr < C61_RAM_END {
        AddrSpace::InternalRam
    } else if addr >= C61_EXTMEM_START && addr < C61_EXTMEM_END {
        AddrSpace::ExternalMapped
    } else {
        AddrSpace::Other
    }
}

/// True when `[addr, addr + len)` is non-empty and entirely inside internal
/// RAM (no wrap-around). The check DMA buffers and descriptors must pass.
pub const fn range_is_internal(addr: usize, len: usize) -> bool {
    if len == 0 {
        return false;
    }
    let end = match addr.checked_add(len) {
        Some(e) => e,
        None => return false,
    };
    addr >= C61_RAM_START && end <= C61_RAM_END
}

// ---------------------------------------------------------------------------
// clocks (bring-up setting, proposal.md: flash 40 / PSRAM 40)

/// Flash clock the image header selects (`--flash-freq 40mhz`). esp-hal's
/// `PsramConfig::default()` says 80 MHz and would re-clock the flash; the
/// adapter must set 40 explicitly (see `kernel/src/board_c61/memory.rs`).
pub const FLASH_MHZ: u32 = 40;
pub const PSRAM_MHZ: u32 = 40;

// ---------------------------------------------------------------------------
// budgets

/// The OnePage part is 2 MB (proposal.md). A bigger chip is clamped to this.
pub const PSRAM_HW_BYTES: usize = 2 * MIB;
/// Below this the PSRAM is not worth the extra failure modes: degrade.
pub const PSRAM_MIN_BYTES: usize = MIB;
/// Never handed out: allocator metadata, fragmentation, T12 headroom.
pub const PSRAM_RESERVE_BYTES: usize = 448 * KIB;

/// PSRAM class limits (PSRAM mode). Proposals sized from the inventory below
/// (one 96 KiB chapter cache today; PSRAM lets several chapters stay
/// resident); T12 may move bytes between classes, the sum is asserted.
pub const PSRAM_CHAPTER_TEXT_BYTES: usize = 768 * KIB;
pub const PSRAM_IMAGE_DATA_BYTES: usize = 512 * KIB;
pub const PSRAM_PAGE_TABLE_BYTES: usize = 64 * KIB;
pub const PSRAM_ZIP_TOC_BYTES: usize = 256 * KIB;

const _: () = assert!(
    PSRAM_CHAPTER_TEXT_BYTES
        + PSRAM_IMAGE_DATA_BYTES
        + PSRAM_PAGE_TABLE_BYTES
        + PSRAM_ZIP_TOC_BYTES
        + PSRAM_RESERVE_BYTES
        <= PSRAM_HW_BYTES
);

/// Internal heap plan (what T12's `heap_allocator!` calls add up to): a
/// main-RAM part and the bootloader-reclaimed dram2 part, like the X4's
/// 110_592 + 64_000.
pub const INTERNAL_HEAP_MAIN_BYTES: usize = 96 * KIB;
pub const INTERNAL_HEAP_RECLAIMED_BYTES: usize = 64_000;
pub const INTERNAL_HEAP_BYTES: usize = INTERNAL_HEAP_MAIN_BYTES + INTERNAL_HEAP_RECLAIMED_BYTES;

/// Internal limits per class. `Dma`/`IsrData`/`Runtime` are internal-only;
/// the other four are the degraded-mode limits (PSRAM unavailable): the X4
/// runs the same reader inside a 172 KB heap, so these are X4-sized.
pub const INTERNAL_DMA_BYTES: usize = 16 * KIB;
pub const INTERNAL_ISR_BYTES: usize = 4 * KIB;
pub const INTERNAL_RUNTIME_BYTES: usize = 32 * KIB;
pub const INTERNAL_CHAPTER_TEXT_BYTES: usize = 96 * KIB;
pub const INTERNAL_IMAGE_DATA_BYTES: usize = 112 * KIB;
pub const INTERNAL_PAGE_TABLE_BYTES: usize = 8 * KIB;
pub const INTERNAL_ZIP_TOC_BYTES: usize = 32 * KIB;

/// Smallest main stack the image must keep after all statics (esp-hal's own
/// link-time minimum is 8 KiB; the X4 firmware comments plan ~56 KB). The
/// stack is `_stack_start - _stack_end`, i.e. whatever internal RAM the
/// statics leave over; esp-rtos runs the main (embassy) task on it.
pub const STACK_MIN_BYTES: usize = 48 * 1024;
/// Everything the linker places in `RAM` except the stack (.trap, .rwtext,
/// .data, .bss incl. the main heap, .noinit) must fit here.
pub const STATIC_RAM_MAX_BYTES: usize = C61_RAM_LEN - STACK_MIN_BYTES;

const _: () = assert!(INTERNAL_HEAP_MAIN_BYTES < STATIC_RAM_MAX_BYTES);
const _: () = assert!(INTERNAL_HEAP_RECLAIMED_BYTES <= C61_RECLAIMED_LEN);
const _: () = assert!(C61_RAM_END - C61_RAM_START == C61_RAM_LEN + C61_RECLAIMED_LEN);

/// Every reservation is charged rounded up to this (allocator headers).
pub const ALLOC_GRANULE: usize = 16;

// ---------------------------------------------------------------------------
// classes and regions

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Region {
    Internal,
    Psram,
}

impl Region {
    const fn index(self) -> usize {
        match self {
            Region::Internal => 0,
            Region::Psram => 1,
        }
    }
}

/// What an allocation is for. The class, not the caller, decides where it may
/// live (R15).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MemClass {
    /// DMA buffers and descriptors. Internal only.
    DmaBuffer,
    /// Data an ISR (or code running with the cache off) may touch. Internal only.
    IsrData,
    /// Runtime heap, executor arena, task stacks, critical-section data.
    /// Internal only.
    Runtime,
    /// Decompressed chapter text and its prefetch/page buffers (R14).
    ChapterText,
    /// Decoded images and decoder scratch (R14).
    ImageData,
    /// Page offset tables and other per-book indices (R14).
    PageTable,
    /// ZIP central directory, entry index, EPUB TOC scratch (R14).
    ZipToc,
}

pub const CLASS_COUNT: usize = 7;

impl MemClass {
    pub const ALL: [MemClass; CLASS_COUNT] = [
        MemClass::DmaBuffer,
        MemClass::IsrData,
        MemClass::Runtime,
        MemClass::ChapterText,
        MemClass::ImageData,
        MemClass::PageTable,
        MemClass::ZipToc,
    ];

    const fn index(self) -> usize {
        match self {
            MemClass::DmaBuffer => 0,
            MemClass::IsrData => 1,
            MemClass::Runtime => 2,
            MemClass::ChapterText => 3,
            MemClass::ImageData => 4,
            MemClass::PageTable => 5,
            MemClass::ZipToc => 6,
        }
    }

    /// May this class be placed in PSRAM at all? (R15)
    pub const fn allows_psram(self) -> bool {
        matches!(
            self,
            MemClass::ChapterText | MemClass::ImageData | MemClass::PageTable | MemClass::ZipToc
        )
    }

    pub const fn name(self) -> &'static str {
        match self {
            MemClass::DmaBuffer => "dma",
            MemClass::IsrData => "isr",
            MemClass::Runtime => "runtime",
            MemClass::ChapterText => "chapter-text",
            MemClass::ImageData => "image-data",
            MemClass::PageTable => "page-table",
            MemClass::ZipToc => "zip-toc",
        }
    }

    pub const fn external(self) -> Option<ExternalClass> {
        match self {
            MemClass::ChapterText => Some(ExternalClass::ChapterText),
            MemClass::ImageData => Some(ExternalClass::ImageData),
            MemClass::PageTable => Some(ExternalClass::PageTable),
            MemClass::ZipToc => Some(ExternalClass::ZipToc),
            _ => None,
        }
    }
}

/// The classes that may be placed in PSRAM. The firmware's PSRAM allocation
/// entry point takes this type, so asking it for DMA memory does not compile
/// (the runtime check in `MemoryBudget` is the second line of defence).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ExternalClass {
    ChapterText,
    ImageData,
    PageTable,
    ZipToc,
}

impl From<ExternalClass> for MemClass {
    fn from(c: ExternalClass) -> MemClass {
        match c {
            ExternalClass::ChapterText => MemClass::ChapterText,
            ExternalClass::ImageData => MemClass::ImageData,
            ExternalClass::PageTable => MemClass::PageTable,
            ExternalClass::ZipToc => MemClass::ZipToc,
        }
    }
}

// ---------------------------------------------------------------------------
// errors

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MemError {
    /// A zero-byte request has no placement and is a caller bug.
    ZeroSize,
    /// Alignment is zero or not a power of two.
    InvalidAlign,
    /// `size`/`align` arithmetic overflowed `usize`.
    Overflow,
    /// The class may not live in that region (R15).
    RegionForbidden { class: MemClass, region: Region },
    /// The class' own limit would be exceeded.
    ClassLimit {
        class: MemClass,
        region: Region,
        limit: usize,
        used: usize,
        requested: usize,
    },
    /// The region's pool would be exceeded although the class still has room.
    PoolExhausted {
        region: Region,
        limit: usize,
        used: usize,
        requested: usize,
    },
    /// Releasing more than was reserved for that class/region.
    ReleaseUnderflow { class: MemClass, region: Region },
    /// PSRAM status changed while reservations made under the old status are
    /// still alive (their region would no longer match the accounting).
    StatusChangeWhileLive,
    /// The budget said yes but the allocator had no block (fragmentation).
    OutOfMemory,
}

// ---------------------------------------------------------------------------
// PSRAM bring-up state machine

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SelfTestError {
    /// Fewer words than the test needs.
    TooSmall,
    /// Read-back differed from what was written at `word`.
    Mismatch { word: usize },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PsramFault {
    /// Init finished but reported size 0 (chip id unknown / chip absent).
    NotDetected,
    /// Detected, but below `PSRAM_MIN_BYTES`.
    TooSmall { bytes: usize },
    /// The window esp-hal reported is not a usable PSRAM mapping (outside the
    /// cache window, misaligned or wrapping).
    BadWindow,
    /// The smoke test failed.
    SelfTest(SelfTestError),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PsramStatus {
    /// PSRAM bring-up has not run (or has not finished). Treated like
    /// `Degraded`: nothing is placed in PSRAM until `Ready`.
    NotInitialised,
    /// `bytes` of PSRAM (already clamped to `PSRAM_HW_BYTES`) are usable.
    Ready { bytes: usize },
    /// Bring-up failed; the firmware keeps running on internal RAM with the
    /// smaller internal limits (offline reading stays available).
    Degraded(PsramFault),
}

impl PsramStatus {
    pub const fn is_ready(self) -> bool {
        matches!(self, PsramStatus::Ready { .. })
    }

    /// Short text for logs and a future status line.
    pub const fn describe(self) -> &'static str {
        match self {
            PsramStatus::NotInitialised => "psram: not initialised, internal limits",
            PsramStatus::Ready { .. } => "psram: ready",
            PsramStatus::Degraded(PsramFault::NotDetected) => {
                "psram: not detected, internal limits"
            }
            PsramStatus::Degraded(PsramFault::TooSmall { .. }) => {
                "psram: too small, internal limits"
            }
            PsramStatus::Degraded(PsramFault::BadWindow) => {
                "psram: bad address window, internal limits"
            }
            PsramStatus::Degraded(PsramFault::SelfTest(_)) => {
                "psram: self-test failed, internal limits"
            }
        }
    }
}

/// Decide the status from what bring-up observed: the size esp-hal reports
/// (0 when detection found no chip) and the smoke-test result (run only when
/// the size was acceptable; pass `Ok(())` otherwise, it is not consulted).
pub fn evaluate_psram(detected_bytes: usize, selftest: Result<(), SelfTestError>) -> PsramStatus {
    if detected_bytes == 0 {
        return PsramStatus::Degraded(PsramFault::NotDetected);
    }
    if detected_bytes < PSRAM_MIN_BYTES {
        return PsramStatus::Degraded(PsramFault::TooSmall {
            bytes: detected_bytes,
        });
    }
    match selftest {
        Err(e) => PsramStatus::Degraded(PsramFault::SelfTest(e)),
        Ok(()) => PsramStatus::Ready {
            bytes: if detected_bytes > PSRAM_HW_BYTES {
                PSRAM_HW_BYTES
            } else {
                detected_bytes
            },
        },
    }
}

/// Bytes the PSRAM heap region may cover for `status` (0 unless Ready).
pub const fn psram_heap_bytes(status: PsramStatus) -> usize {
    match status {
        PsramStatus::Ready { bytes } => bytes,
        _ => 0,
    }
}

/// Sanity-check the `(start, bytes)` window `Psram::raw_parts` returns. `None`
/// when it is fine or empty (an empty window is reported by `evaluate_psram` as
/// `NotDetected`). The window must be 4-byte aligned and lie inside the
/// flash/PSRAM cache window without wrapping.
pub fn window_fault(start: usize, bytes: usize) -> Option<PsramFault> {
    if bytes == 0 {
        return None;
    }
    let ok = start % 4 == 0
        && start >= C61_EXTMEM_START
        && matches!(start.checked_add(bytes), Some(end) if end <= C61_EXTMEM_END);
    if ok {
        None
    } else {
        Some(PsramFault::BadWindow)
    }
}

// ---------------------------------------------------------------------------
// PSRAM smoke test

/// Word-addressed memory under test (the kernel adapter does volatile accesses
/// with a cache write-back/invalidate between phases).
pub trait WordMem {
    fn write(&mut self, word: usize, value: u32);
    fn read(&mut self, word: usize) -> u32;
    /// Push written data to the device and drop cached copies, so the next
    /// `read` really comes from the chip. Host fakes need nothing.
    fn sync(&mut self) {}
}

const SELFTEST_SEED: u32 = 0xA5A5_5A5A;

fn pattern(word: usize) -> u32 {
    (word as u32).wrapping_mul(0x9E37_79B9) ^ SELFTEST_SEED
}

/// Smoke test over `words` 32-bit words: distinct value per word at word 0,
/// every power-of-two index (address-line faults / aliasing) and the last
/// word, then the bitwise inverse (stuck bits). Not a RAM test; it catches
/// "chip absent", a dead address bit or a stuck data bit.
pub fn selftest<M: WordMem>(mem: &mut M, words: usize) -> Result<(), SelfTestError> {
    if words < 2 {
        return Err(SelfTestError::TooSmall);
    }
    for invert in [false, true] {
        let want = |w: usize| {
            let p = pattern(w);
            if invert { !p } else { p }
        };
        for w in selftest_words(words) {
            mem.write(w, want(w));
        }
        mem.sync();
        for w in selftest_words(words) {
            if mem.read(w) != want(w) {
                return Err(SelfTestError::Mismatch { word: w });
            }
        }
    }
    Ok(())
}

/// Indices visited by `selftest`, each once: 0, the powers of two below the
/// last word, then the last word (`words >= 2`).
fn selftest_words(words: usize) -> impl Iterator<Item = usize> {
    let last = words - 1;
    let pow2 =
        core::iter::successors(Some(1usize), |&p| p.checked_mul(2)).take_while(move |&p| p < last);
    core::iter::once(0)
        .chain(pow2)
        .chain(core::iter::once(last))
}

// ---------------------------------------------------------------------------
// budget tracker

/// A successful reservation. Hand it back to `MemoryBudget::release`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub class: MemClass,
    pub region: Region,
    /// Bytes charged (request rounded up, see `charge`).
    pub bytes: usize,
}

/// Bytes charged for `size` at `align`: rounded up to `max(align, ALLOC_GRANULE)`.
pub fn charge(size: usize, align: usize) -> Result<usize, MemError> {
    if size == 0 {
        return Err(MemError::ZeroSize);
    }
    if align == 0 || !align.is_power_of_two() {
        return Err(MemError::InvalidAlign);
    }
    let unit = if align > ALLOC_GRANULE {
        align
    } else {
        ALLOC_GRANULE
    };
    size.checked_next_multiple_of(unit)
        .ok_or(MemError::Overflow)
}

/// Per-class limit in `region` for the given PSRAM status.
pub const fn class_limit(status: PsramStatus, region: Region, class: MemClass) -> usize {
    match region {
        Region::Internal => match class {
            MemClass::DmaBuffer => INTERNAL_DMA_BYTES,
            MemClass::IsrData => INTERNAL_ISR_BYTES,
            MemClass::Runtime => INTERNAL_RUNTIME_BYTES,
            MemClass::ChapterText => INTERNAL_CHAPTER_TEXT_BYTES,
            MemClass::ImageData => INTERNAL_IMAGE_DATA_BYTES,
            MemClass::PageTable => INTERNAL_PAGE_TABLE_BYTES,
            MemClass::ZipToc => INTERNAL_ZIP_TOC_BYTES,
        },
        Region::Psram => {
            if !status.is_ready() {
                return 0;
            }
            match class {
                MemClass::ChapterText => PSRAM_CHAPTER_TEXT_BYTES,
                MemClass::ImageData => PSRAM_IMAGE_DATA_BYTES,
                MemClass::PageTable => PSRAM_PAGE_TABLE_BYTES,
                MemClass::ZipToc => PSRAM_ZIP_TOC_BYTES,
                _ => 0,
            }
        }
    }
}

/// Pool (sum over all classes) limit of `region` for the given status.
pub const fn pool_limit(status: PsramStatus, region: Region) -> usize {
    match region {
        Region::Internal => INTERNAL_HEAP_BYTES,
        Region::Psram => {
            let bytes = psram_heap_bytes(status);
            if bytes > PSRAM_RESERVE_BYTES {
                bytes - PSRAM_RESERVE_BYTES
            } else {
                0
            }
        }
    }
}

#[derive(Debug)]
pub struct MemoryBudget {
    status: PsramStatus,
    used: [[usize; CLASS_COUNT]; 2],
    pool_used: [usize; 2],
}

impl MemoryBudget {
    /// Starts in `NotInitialised`: internal limits only (fail safe).
    pub const fn new() -> Self {
        Self {
            status: PsramStatus::NotInitialised,
            used: [[0; CLASS_COUNT]; 2],
            pool_used: [0; 2],
        }
    }

    pub const fn status(&self) -> PsramStatus {
        self.status
    }

    /// Record the outcome of PSRAM bring-up. Refused while a class that moves
    /// with the status still has live reservations.
    pub fn set_status(&mut self, status: PsramStatus) -> Result<(), MemError> {
        let moves = |m: &Self| {
            MemClass::ALL
                .iter()
                .filter(|c| c.allows_psram())
                .any(|c| m.used[0][c.index()] != 0 || m.used[1][c.index()] != 0)
        };
        if status != self.status && moves(self) {
            return Err(MemError::StatusChangeWhileLive);
        }
        self.status = status;
        Ok(())
    }

    /// Where `class` goes right now: PSRAM only for PSRAM-capable classes and
    /// only when `Ready`; everything else, and every degraded case, internal.
    pub const fn region_for(&self, class: MemClass) -> Region {
        if class.allows_psram() && self.status.is_ready() {
            Region::Psram
        } else {
            Region::Internal
        }
    }

    pub const fn used(&self, region: Region, class: MemClass) -> usize {
        self.used[region.index()][class.index()]
    }

    pub const fn pool_used(&self, region: Region) -> usize {
        self.pool_used[region.index()]
    }

    pub const fn class_limit(&self, region: Region, class: MemClass) -> usize {
        class_limit(self.status, region, class)
    }

    pub const fn pool_limit(&self, region: Region) -> usize {
        pool_limit(self.status, region)
    }

    /// Reserve with automatic placement (`region_for`).
    pub fn reserve(
        &mut self,
        class: MemClass,
        size: usize,
        align: usize,
    ) -> Result<Reservation, MemError> {
        let region = self.region_for(class);
        self.reserve_in(region, class, size, align)
    }

    /// Reserve in an explicit region. `RegionForbidden` for DMA/ISR/runtime
    /// classes in PSRAM, whatever the status.
    pub fn reserve_in(
        &mut self,
        region: Region,
        class: MemClass,
        size: usize,
        align: usize,
    ) -> Result<Reservation, MemError> {
        if region == Region::Psram && !class.allows_psram() {
            return Err(MemError::RegionForbidden { class, region });
        }
        let bytes = charge(size, align)?;
        let r = region.index();
        let c = class.index();

        let limit = self.class_limit(region, class);
        let used = self.used[r][c];
        match used.checked_add(bytes) {
            Some(total) if total <= limit => {}
            Some(_) => {
                return Err(MemError::ClassLimit {
                    class,
                    region,
                    limit,
                    used,
                    requested: bytes,
                });
            }
            None => return Err(MemError::Overflow),
        }

        let pool = self.pool_limit(region);
        let pool_used = self.pool_used[r];
        match pool_used.checked_add(bytes) {
            Some(total) if total <= pool => {}
            Some(_) => {
                return Err(MemError::PoolExhausted {
                    region,
                    limit: pool,
                    used: pool_used,
                    requested: bytes,
                });
            }
            None => return Err(MemError::Overflow),
        }

        self.used[r][c] = used + bytes;
        self.pool_used[r] = pool_used + bytes;
        Ok(Reservation {
            class,
            region,
            bytes,
        })
    }

    /// Give a reservation back. Refuses (and changes nothing) when the books
    /// hold less than `res.bytes` for that class/region.
    pub fn release(&mut self, res: Reservation) -> Result<(), MemError> {
        let r = res.region.index();
        let c = res.class.index();
        if self.used[r][c] < res.bytes || self.pool_used[r] < res.bytes {
            return Err(MemError::ReleaseUnderflow {
                class: res.class,
                region: res.region,
            });
        }
        self.used[r][c] -= res.bytes;
        self.pool_used[r] -= res.bytes;
        Ok(())
    }
}

impl Default for MemoryBudget {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ELF image budget (R22). `scripts/report-c61-memory.sh` extracts the numbers
// from the linked ELF and feeds them to `check_image` through the
// `memreport` example, so the pass/fail rule is this code, not shell.

/// Layout of the main internal RAM region as the linker produced it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ImageMemory {
    /// End address of the last static section in `RAM` (.trap, .rwtext, .data,
    /// .bss incl. the main heap, .noinit).
    pub static_end: usize,
    /// Address and size of `.stack` (`_stack_end`.. `_stack_start`).
    pub stack_addr: usize,
    pub stack_size: usize,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ImageFault {
    /// Statics start below or end above the main RAM region.
    StaticsOutsideRam,
    /// `.stack` does not start where the statics end, or does not end at the
    /// end of the main RAM region (the linker places it in what is left).
    StackNotLastInRam,
    /// Statics above `STATIC_RAM_MAX_BYTES`.
    StaticsOverBudget { used: usize, max: usize },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ImageReport {
    pub static_bytes: usize,
    pub stack_bytes: usize,
    /// `stack_bytes - STACK_MIN_BYTES`: what statics may still grow by.
    pub stack_headroom: usize,
    /// Share of the main RAM region the statics take, in tenths of a percent.
    pub static_permille: usize,
}

pub fn check_image(img: &ImageMemory) -> Result<ImageReport, ImageFault> {
    if img.static_end < C61_RAM_START || img.static_end > C61_RECLAIMED_START {
        return Err(ImageFault::StaticsOutsideRam);
    }
    let stack_end = match img.stack_addr.checked_add(img.stack_size) {
        Some(e) => e,
        None => return Err(ImageFault::StackNotLastInRam),
    };
    if img.stack_addr != img.static_end || stack_end != C61_RECLAIMED_START {
        return Err(ImageFault::StackNotLastInRam);
    }
    let static_bytes = img.static_end - C61_RAM_START;
    if static_bytes > STATIC_RAM_MAX_BYTES {
        return Err(ImageFault::StaticsOverBudget {
            used: static_bytes,
            max: STATIC_RAM_MAX_BYTES,
        });
    }
    // stack == RAM_LEN - statics here, so the budget check above is also the
    // minimum-stack check; headroom is what remains over `STACK_MIN_BYTES`.
    Ok(ImageReport {
        static_bytes,
        stack_bytes: img.stack_size,
        stack_headroom: img.stack_size - STACK_MIN_BYTES,
        static_permille: static_bytes * 1000 / C61_RAM_LEN,
    })
}

/// Placement rule for one allocated ELF section: a writable section (data,
/// bss, stack, RAM code) must sit entirely in internal RAM; a read-only one
/// may also sit entirely in the flash/PSRAM cache window (flash-mapped
/// rodata/text). Empty sections are fine anywhere.
pub fn section_placement_ok(addr: usize, size: usize, writable: bool) -> bool {
    if size == 0 {
        return true;
    }
    if range_is_internal(addr, size) {
        return true;
    }
    if writable {
        return false;
    }
    match addr.checked_add(size) {
        Some(end) => addr >= C61_EXTMEM_START && end <= C61_EXTMEM_END,
        None => false,
    }
}

// ---------------------------------------------------------------------------
// inventory of large allocations (>= 4 KiB, plus the DMA descriptors because
// of R15). Sizes are estimates read from the X4 release ELF
// (`nm --size-sort -S`), the T7 C61 boot ELF and the source constants; rows
// marked "in READER" are inside the 18,972 B `READER` static and are not
// additive. T12 uses this table to move the `Candidate` rows.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AllocKind {
    Static,
    Heap,
    Stack,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Internal RAM is required (R15).
    Required,
    /// May move to PSRAM under its class budget (T12).
    Candidate,
    /// Stays internal for latency or size reasons; a PSRAM move is optional.
    Keep,
}

#[derive(Copy, Clone, Debug)]
pub struct InventoryItem {
    pub name: &'static str,
    pub source: &'static str,
    pub kind: AllocKind,
    pub bytes: usize,
    pub class: MemClass,
    pub placement: Placement,
    pub note: &'static str,
}

const fn item(
    name: &'static str,
    source: &'static str,
    kind: AllocKind,
    bytes: usize,
    class: MemClass,
    placement: Placement,
    note: &'static str,
) -> InventoryItem {
    InventoryItem {
        name,
        source,
        kind,
        bytes,
        class,
        placement,
        note,
    }
}

use AllocKind::{Heap, Stack, Static};
use MemClass::{ChapterText, DmaBuffer, ImageData, Runtime, ZipToc};
use Placement::{Candidate, Keep, Required};

pub const INVENTORY: &[InventoryItem] = &[
    // --- internal, required ---
    item(
        "SPI DMA RX buffer",
        "kernel/src/board_c61/spi.rs dma_rx_buffer!(4096); X4 board/mod.rs:213",
        Static,
        4096,
        DmaBuffer,
        Required,
        "DMA; statics are internal by construction",
    ),
    item(
        "SPI DMA TX buffer",
        "kernel/src/board_c61/spi.rs dma_tx_buffer!(4096); X4 board/mod.rs:214",
        Static,
        4096,
        DmaBuffer,
        Required,
        "DMA; STRIP (4,014 B) is copied through this, never DMA'd directly",
    ),
    item(
        "SPI DMA descriptors",
        "kernel/src/board_c61/spi.rs DESCRIPTORS",
        Static,
        28,
        DmaBuffer,
        Required,
        "below 4 KiB, listed for R15; nm-checked internal",
    ),
    item(
        "C61 main heap",
        "src/bin/c61_boot.rs heap_allocator!(INTERNAL_HEAP_MAIN_BYTES)",
        Static,
        INTERNAL_HEAP_MAIN_BYTES,
        Runtime,
        Required,
        "global allocator, internal region only (inside .bss)",
    ),
    item(
        "C61 reclaimed heap",
        "src/bin/c61_boot.rs heap_allocator!(#[ram(reclaimed)] INTERNAL_HEAP_RECLAIMED_BYTES)",
        Static,
        INTERNAL_HEAP_RECLAIMED_BYTES,
        Runtime,
        Required,
        "global allocator, dram2_seg (.dram2_uninit), internal",
    ),
    item(
        "X4 main heap",
        "src/bin/main.rs heap_allocator!(110_592)",
        Static,
        110_592,
        Runtime,
        Required,
        "X4 only",
    ),
    item(
        "X4 reclaimed heap",
        "src/bin/main.rs heap_allocator!(#[ram(reclaimed)] 64_000)",
        Static,
        64_000,
        Runtime,
        Required,
        "X4 only; dram2 on both chips",
    ),
    item(
        "embassy executor task arena",
        "src/bin/main.rs __embassy_main POOL (X4 11,288; C61 boot 2,320)",
        Static,
        11_288,
        Runtime,
        Required,
        "task futures; internal",
    ),
    item(
        "main stack (.stack)",
        "linker .stack = _stack_start - _stack_end (C61 T8 boot ELF: 132,768; whatever RAM the statics leave)",
        Stack,
        132_768,
        Runtime,
        Required,
        "esp-rtos main task; size = RAM left after statics",
    ),
    item(
        "dir_cache title scan buffer",
        "kernel/src/kernel/dir_cache.rs:50 [0u8; 4096]",
        Stack,
        4096,
        Runtime,
        Required,
        "stack local",
    ),
    item(
        "inflate read buffer",
        "smol-epub async_io.rs:316 / cache.rs READ_BUF_SIZE",
        Stack,
        4096,
        Runtime,
        Required,
        "sync path: stack local; async path: inside the task future (counted in the executor POOL)",
    ),
    item(
        "inflate strip buffer",
        "smol-epub async_io.rs:317 / cache.rs STRIP_BUF_SIZE",
        Stack,
        4096,
        Runtime,
        Required,
        "same as the read buffer",
    ),
    // --- internal, kept ---
    item(
        "StripBuffer STRIP",
        "src/bin/main.rs STRIP (X4 nm: 4,014 B)",
        Static,
        4014,
        Runtime,
        Keep,
        "below 4 KiB, listed because it is the pixel source of every SPI DMA transfer; copied through the DMA TX buffer",
    ),
    item(
        "READER (ReaderApp)",
        "src/bin/main.rs READER",
        Static,
        18_972,
        Runtime,
        Keep,
        "contains the 'in READER' rows below",
    ),
    item(
        "page text buffer (in READER)",
        "src/apps/reader/mod.rs PageState.buf PAGE_BUF",
        Static,
        8192,
        ChapterText,
        Keep,
        "hot: touched per glyph while wrapping",
    ),
    item(
        "DirCache",
        "kernel/src/kernel/dir_cache.rs DIR_CACHE",
        Static,
        10_764,
        ZipToc,
        Keep,
        "128 x DirEntry; UI list, low value to move",
    ),
    // --- PSRAM candidates (T12) ---
    item(
        "chapter cache Vec",
        "src/apps/reader/epubs.rs ch_cache (CHAPTER_CACHE_MAX)",
        Heap,
        98_304,
        ChapterText,
        Candidate,
        "largest reader allocation; read-mostly",
    ),
    item(
        "page prefetch Vec",
        "src/apps/reader/paging.rs prefetch (PAGE_BUF)",
        Heap,
        8192,
        ChapterText,
        Candidate,
        "sequential fill/read",
    ),
    item(
        "chapter inflate window",
        "smol-epub cache.rs WINDOW_SIZE",
        Heap,
        32_768,
        ChapterText,
        Candidate,
        "decompression scratch; hot inner loop, measure first",
    ),
    item(
        "page offset table (in READER)",
        "src/apps/reader/mod.rs PageState.offsets MAX_PAGES x u32",
        Static,
        2048,
        MemClass::PageTable,
        Candidate,
        "below 4 KiB; PSRAM lets MAX_PAGES grow (limit 64 KiB)",
    ),
    item(
        "ZIP entry index (in READER)",
        "smol-epub zip.rs ZipIndex.entries 256 x 20",
        Static,
        5120,
        ZipToc,
        Candidate,
        "estimate (entry size not measured); box it on PSRAM",
    ),
    item(
        "ZIP name pool",
        "smol-epub zip.rs names (try_reserve <= 8192)",
        Heap,
        8192,
        ZipToc,
        Candidate,
        "already heap",
    ),
    item(
        "EPUB TOC",
        "src/apps/reader/mod.rs Box<EpubToc> 256 x 52",
        Heap,
        13_316,
        ZipToc,
        Candidate,
        "already boxed on demand",
    ),
    item(
        "central directory buffer",
        "src/apps/reader/epubs.rs:76, files.rs:561 cd_buf",
        Heap,
        32_768,
        ZipToc,
        Candidate,
        "estimate; equals CD size, bounded by try_reserve and the zip-toc limit",
    ),
    item(
        "decoded page image",
        "src/apps/reader/images.rs DecodedImage.data (480x800 1bpp max)",
        Heap,
        48_000,
        ImageData,
        Candidate,
        "also in work_queue Channel",
    ),
    item(
        "PNG decode dictionary",
        "smol-epub png.rs DICT_SIZE",
        Heap,
        32_768,
        ImageData,
        Candidate,
        "scratch; hot loop, measure first",
    ),
    item(
        "PNG zip inflate window",
        "smol-epub png.rs ZIP_DEFLATE_WINDOW",
        Heap,
        32_768,
        ImageData,
        Candidate,
        "scratch",
    ),
    item(
        "JPEG header read",
        "smol-epub jpeg.rs HEADER_READ",
        Heap,
        32_768,
        ImageData,
        Candidate,
        "scratch",
    ),
    item(
        "JPEG inflate window",
        "smol-epub jpeg.rs DEFLATE_WINDOW",
        Heap,
        32_768,
        ImageData,
        Candidate,
        "scratch",
    ),
];

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn ready() -> MemoryBudget {
        let mut b = MemoryBudget::new();
        b.set_status(PsramStatus::Ready {
            bytes: PSRAM_HW_BYTES,
        })
        .unwrap();
        b
    }

    fn degraded() -> MemoryBudget {
        let mut b = MemoryBudget::new();
        b.set_status(PsramStatus::Degraded(PsramFault::NotDetected))
            .unwrap();
        b
    }

    // --- constants -------------------------------------------------------

    #[test]
    fn r14_clock_plan_is_flash40_psram40() {
        assert_eq!(FLASH_MHZ, 40);
        assert_eq!(PSRAM_MHZ, 40);
    }

    #[test]
    fn r14_psram_class_limits_and_reserve_fit_the_chip() {
        let sum = PSRAM_CHAPTER_TEXT_BYTES
            + PSRAM_IMAGE_DATA_BYTES
            + PSRAM_PAGE_TABLE_BYTES
            + PSRAM_ZIP_TOC_BYTES;
        assert_eq!(PSRAM_HW_BYTES, 2 * 1024 * 1024);
        assert!(sum + PSRAM_RESERVE_BYTES <= PSRAM_HW_BYTES);
        // the pool is smaller than the sum of the class limits on purpose?
        // no: it must be able to hold all of them at once
        assert_eq!(
            pool_limit(
                PsramStatus::Ready {
                    bytes: PSRAM_HW_BYTES
                },
                Region::Psram
            ),
            PSRAM_HW_BYTES - PSRAM_RESERVE_BYTES
        );
        assert!(
            pool_limit(
                PsramStatus::Ready {
                    bytes: PSRAM_HW_BYTES
                },
                Region::Psram
            ) >= sum
        );
    }

    #[test]
    fn r15_memory_map_matches_memory_x() {
        // esp-hal 1.2.0 ld/esp32c61/memory.x
        assert_eq!(C61_RAM_START, 0x4080_0000);
        assert_eq!(C61_RAM_LEN, 0x3EA70);
        assert_eq!(C61_RECLAIMED_START, C61_RAM_START + C61_RAM_LEN);
        assert_eq!(C61_RECLAIMED_LEN, 0x4084_EA70 - 0x4083_EA70);
        assert_eq!(C61_RAM_END, 0x4084_EA70);
        // esp-metadata-generated psram.extmem_origin
        assert_eq!(C61_EXTMEM_START, 1_107_296_256);
    }

    #[test]
    fn r15_internal_plan_fits_ram_with_stack() {
        assert_eq!(STATIC_RAM_MAX_BYTES + STACK_MIN_BYTES, C61_RAM_LEN);
        assert!(STACK_MIN_BYTES >= 8192, "esp-hal ENSURE_MAIN_STACK_MINIMUM");
        assert_eq!(INTERNAL_HEAP_BYTES, 98_304 + 64_000);
        // DMA limit holds the two 4 KiB SPI buffers with room to spare
        assert!(INTERNAL_DMA_BYTES >= 2 * crate::spi::SPI_DMA_BUF_BYTES);
    }

    // --- address classification (R15) ------------------------------------

    #[test]
    fn r15_addr_space_boundaries() {
        assert_eq!(addr_space(C61_RAM_START - 1), AddrSpace::Other);
        assert_eq!(addr_space(C61_RAM_START), AddrSpace::InternalRam);
        assert_eq!(addr_space(C61_RAM_END - 1), AddrSpace::InternalRam);
        assert_eq!(addr_space(C61_RAM_END), AddrSpace::Other);
        assert_eq!(addr_space(C61_EXTMEM_START - 1), AddrSpace::Other);
        assert_eq!(addr_space(C61_EXTMEM_START), AddrSpace::ExternalMapped);
        assert_eq!(addr_space(C61_EXTMEM_END - 1), AddrSpace::ExternalMapped);
        assert_eq!(addr_space(C61_EXTMEM_END), AddrSpace::Other);
    }

    #[test]
    fn r15_range_is_internal_edges() {
        assert!(range_is_internal(C61_RAM_START, 1));
        assert!(range_is_internal(
            C61_RAM_START,
            C61_RAM_END - C61_RAM_START
        ));
        assert!(range_is_internal(C61_RAM_END - 1, 1));
        // one byte past the end / before the start
        assert!(!range_is_internal(
            C61_RAM_START,
            C61_RAM_END - C61_RAM_START + 1
        ));
        assert!(!range_is_internal(C61_RAM_START - 1, 1));
        assert!(!range_is_internal(C61_RAM_END, 1));
        // empty range is not a valid buffer
        assert!(!range_is_internal(C61_RAM_START, 0));
        // wrap-around must not look internal
        assert!(!range_is_internal(usize::MAX, 2));
        assert!(!range_is_internal(C61_RAM_START, usize::MAX));
    }

    #[test]
    fn r15_psram_and_flash_window_is_never_internal() {
        for a in [
            C61_EXTMEM_START,
            C61_EXTMEM_START + 0x10_0000,
            C61_EXTMEM_END - 1,
        ] {
            assert!(!range_is_internal(a, 1));
            assert_eq!(addr_space(a), AddrSpace::ExternalMapped);
        }
        // a DMA buffer that straddles the end of internal RAM is rejected
        assert!(!range_is_internal(C61_RAM_END - 4, 8));
    }

    // --- classification rules (R15) --------------------------------------

    #[test]
    fn r15_dma_isr_runtime_never_allow_psram() {
        for c in [MemClass::DmaBuffer, MemClass::IsrData, MemClass::Runtime] {
            assert!(!c.allows_psram(), "{:?}", c);
            assert_eq!(c.external(), None);
        }
        for c in [
            MemClass::ChapterText,
            MemClass::ImageData,
            MemClass::PageTable,
            MemClass::ZipToc,
        ] {
            assert!(c.allows_psram(), "{:?}", c);
            assert_eq!(MemClass::from(c.external().unwrap()), c);
        }
    }

    #[test]
    fn r15_explicit_psram_request_for_internal_only_class_is_refused() {
        // also when PSRAM is Ready with plenty of room
        let mut b = ready();
        for c in [MemClass::DmaBuffer, MemClass::IsrData, MemClass::Runtime] {
            assert_eq!(
                b.reserve_in(Region::Psram, c, 64, 4),
                Err(MemError::RegionForbidden {
                    class: c,
                    region: Region::Psram
                })
            );
            assert_eq!(b.pool_used(Region::Psram), 0);
        }
    }

    #[test]
    fn r15_auto_placement_keeps_internal_only_classes_internal_even_when_ready() {
        let mut b = ready();
        for c in [MemClass::DmaBuffer, MemClass::IsrData, MemClass::Runtime] {
            assert_eq!(b.region_for(c), Region::Internal);
            let r = b.reserve(c, 128, 4).unwrap();
            assert_eq!(r.region, Region::Internal);
        }
        assert_eq!(b.pool_used(Region::Psram), 0);
    }

    #[test]
    fn r15_psram_limit_of_internal_only_classes_is_zero() {
        let st = PsramStatus::Ready {
            bytes: PSRAM_HW_BYTES,
        };
        for c in [MemClass::DmaBuffer, MemClass::IsrData, MemClass::Runtime] {
            assert_eq!(class_limit(st, Region::Psram, c), 0);
        }
    }

    // --- budget: boundaries (R14) ----------------------------------------

    #[test]
    fn r14_exactly_filling_a_class_limit_succeeds() {
        let mut b = ready();
        let r = b
            .reserve(MemClass::ChapterText, PSRAM_CHAPTER_TEXT_BYTES, 4)
            .unwrap();
        assert_eq!(r.region, Region::Psram);
        assert_eq!(r.bytes, PSRAM_CHAPTER_TEXT_BYTES);
        assert_eq!(
            b.used(Region::Psram, MemClass::ChapterText),
            PSRAM_CHAPTER_TEXT_BYTES
        );
    }

    #[test]
    fn r14_one_byte_over_the_class_limit_is_refused() {
        let mut b = ready();
        // limit is a multiple of the granule, so +1 charges one more granule
        let e = b
            .reserve(MemClass::ChapterText, PSRAM_CHAPTER_TEXT_BYTES + 1, 4)
            .unwrap_err();
        assert_eq!(
            e,
            MemError::ClassLimit {
                class: MemClass::ChapterText,
                region: Region::Psram,
                limit: PSRAM_CHAPTER_TEXT_BYTES,
                used: 0,
                requested: PSRAM_CHAPTER_TEXT_BYTES + ALLOC_GRANULE,
            }
        );
        // refusal changes nothing
        assert_eq!(b.pool_used(Region::Psram), 0);
    }

    #[test]
    fn r14_fill_then_one_more_granule_is_refused_then_release_allows_again() {
        let mut b = ready();
        let big = b
            .reserve(
                MemClass::ImageData,
                PSRAM_IMAGE_DATA_BYTES - ALLOC_GRANULE,
                4,
            )
            .unwrap();
        let last = b.reserve(MemClass::ImageData, ALLOC_GRANULE, 4).unwrap();
        assert!(matches!(
            b.reserve(MemClass::ImageData, 1, 1),
            Err(MemError::ClassLimit { .. })
        ));
        b.release(last).unwrap();
        let again = b.reserve(MemClass::ImageData, ALLOC_GRANULE, 4).unwrap();
        assert_eq!(again, last);
        b.release(again).unwrap();
        b.release(big).unwrap();
        assert_eq!(b.pool_used(Region::Psram), 0);
        assert_eq!(b.used(Region::Psram, MemClass::ImageData), 0);
    }

    #[test]
    fn r14_each_class_has_its_own_limit() {
        let mut b = ready();
        // exhaust image-data; chapter-text / page-table / zip-toc still work
        b.reserve(MemClass::ImageData, PSRAM_IMAGE_DATA_BYTES, 4)
            .unwrap();
        assert!(b.reserve(MemClass::ImageData, 1, 1).is_err());
        assert!(b.reserve(MemClass::ChapterText, 96 * KIB, 4).is_ok());
        assert!(b.reserve(MemClass::PageTable, 8 * KIB, 4).is_ok());
        assert!(b.reserve(MemClass::ZipToc, 16 * KIB, 4).is_ok());
        // page-table is capped by its own (smaller) limit, not the pool
        assert!(matches!(
            b.reserve(MemClass::PageTable, PSRAM_PAGE_TABLE_BYTES, 4),
            Err(MemError::ClassLimit {
                class: MemClass::PageTable,
                ..
            })
        ));
    }

    #[test]
    fn r14_all_classes_at_their_limits_fit_the_pool_exactly_below_reserve() {
        let mut b = ready();
        for (c, n) in [
            (MemClass::ChapterText, PSRAM_CHAPTER_TEXT_BYTES),
            (MemClass::ImageData, PSRAM_IMAGE_DATA_BYTES),
            (MemClass::PageTable, PSRAM_PAGE_TABLE_BYTES),
            (MemClass::ZipToc, PSRAM_ZIP_TOC_BYTES),
        ] {
            b.reserve(c, n, 4).unwrap();
        }
        assert!(b.pool_used(Region::Psram) <= b.pool_limit(Region::Psram));
        assert!(b.pool_used(Region::Psram) + PSRAM_RESERVE_BYTES <= PSRAM_HW_BYTES);
    }

    #[test]
    fn r14_pool_exhaustion_is_reported_when_the_class_still_has_room() {
        // a smaller chip: 1 MiB -> pool = 1 MiB - reserve = 576 KiB < sum of limits
        let mut b = MemoryBudget::new();
        b.set_status(PsramStatus::Ready { bytes: MIB }).unwrap();
        let pool = b.pool_limit(Region::Psram);
        assert_eq!(pool, MIB - PSRAM_RESERVE_BYTES);
        b.reserve(MemClass::ChapterText, pool - 16 * KIB, 4)
            .unwrap();
        // image-data has its whole 512 KiB left but the pool does not
        let e = b.reserve(MemClass::ImageData, 32 * KIB, 4).unwrap_err();
        assert_eq!(
            e,
            MemError::PoolExhausted {
                region: Region::Psram,
                limit: pool,
                used: pool - 16 * KIB,
                requested: 32 * KIB,
            }
        );
        // what still fits is accepted up to the byte
        assert!(b.reserve(MemClass::ImageData, 16 * KIB, 4).is_ok());
        assert!(b.reserve(MemClass::ImageData, 1, 1).is_err());
    }

    // --- budget: arithmetic ----------------------------------------------

    #[test]
    fn r14_zero_size_is_rejected_in_every_state() {
        for mut b in [MemoryBudget::new(), ready(), degraded()] {
            for c in MemClass::ALL {
                assert_eq!(b.reserve(c, 0, 4), Err(MemError::ZeroSize));
            }
        }
    }

    #[test]
    fn r14_bad_alignment_is_rejected() {
        let mut b = ready();
        assert_eq!(
            b.reserve(MemClass::ZipToc, 64, 0),
            Err(MemError::InvalidAlign)
        );
        assert_eq!(
            b.reserve(MemClass::ZipToc, 64, 3),
            Err(MemError::InvalidAlign)
        );
        assert_eq!(
            b.reserve(MemClass::ZipToc, 64, 24),
            Err(MemError::InvalidAlign)
        );
        assert_eq!(b.pool_used(Region::Psram), 0);
    }

    #[test]
    fn r14_usize_overflow_is_an_error_not_a_wrap() {
        let mut b = ready();
        assert_eq!(
            b.reserve(MemClass::ChapterText, usize::MAX, 4),
            Err(MemError::Overflow)
        );
        assert_eq!(
            b.reserve(MemClass::ChapterText, usize::MAX - 3, 4),
            Err(MemError::Overflow)
        );
        // huge power-of-two alignment
        assert_eq!(
            b.reserve(
                MemClass::ChapterText,
                usize::MAX / 2 + 2,
                1 << (usize::BITS - 1)
            ),
            Err(MemError::Overflow)
        );
        assert_eq!(b.pool_used(Region::Psram), 0);
        // a request that rounds past the limit is a ClassLimit, not an overflow
        assert!(matches!(
            b.reserve(MemClass::ChapterText, usize::MAX / 2, 4),
            Err(MemError::ClassLimit { .. })
        ));
    }

    #[test]
    fn r14_charge_rounds_up_to_the_granule_or_the_alignment() {
        assert_eq!(charge(1, 1), Ok(ALLOC_GRANULE));
        assert_eq!(charge(ALLOC_GRANULE, 4), Ok(ALLOC_GRANULE));
        assert_eq!(charge(ALLOC_GRANULE + 1, 4), Ok(2 * ALLOC_GRANULE));
        assert_eq!(charge(33, 32), Ok(64));
        assert_eq!(charge(64, 64), Ok(64));
        assert_eq!(charge(65, 64), Ok(128));
    }

    #[test]
    fn r14_alignment_is_charged_so_small_blocks_cannot_dodge_the_budget() {
        let mut b = ready();
        let r = b.reserve(MemClass::PageTable, 1, 32).unwrap();
        assert_eq!(r.bytes, 32);
        assert_eq!(b.used(Region::Psram, MemClass::PageTable), 32);
    }

    #[test]
    fn r14_release_underflow_is_refused_and_changes_nothing() {
        let mut b = ready();
        let r = b.reserve(MemClass::ZipToc, 100, 4).unwrap();
        let before = b.pool_used(Region::Psram);
        let bogus = Reservation {
            bytes: r.bytes + 1,
            ..r
        };
        assert_eq!(
            b.release(bogus),
            Err(MemError::ReleaseUnderflow {
                class: MemClass::ZipToc,
                region: Region::Psram
            })
        );
        // wrong region
        let wrong = Reservation {
            region: Region::Internal,
            ..r
        };
        assert!(b.release(wrong).is_err());
        assert_eq!(b.pool_used(Region::Psram), before);
        b.release(r).unwrap();
        // double release
        assert!(b.release(r).is_err());
    }

    // --- failure handling / degradation (R14, R15) -----------------------

    #[test]
    fn r14_fresh_budget_is_not_initialised_and_places_internally() {
        let mut b = MemoryBudget::new();
        assert_eq!(b.status(), PsramStatus::NotInitialised);
        assert!(!b.status().is_ready());
        let r = b.reserve(MemClass::ChapterText, 4096, 4).unwrap();
        assert_eq!(r.region, Region::Internal);
        // PSRAM is refused outright until Ready
        assert!(matches!(
            b.reserve_in(Region::Psram, MemClass::ChapterText, 16, 4),
            Err(MemError::ClassLimit { limit: 0, .. })
        ));
    }

    #[test]
    fn r14_evaluate_psram_state_machine() {
        let ok = Ok(());
        assert_eq!(
            evaluate_psram(0, ok),
            PsramStatus::Degraded(PsramFault::NotDetected)
        );
        assert_eq!(
            evaluate_psram(PSRAM_MIN_BYTES - 1, ok),
            PsramStatus::Degraded(PsramFault::TooSmall {
                bytes: PSRAM_MIN_BYTES - 1
            })
        );
        assert_eq!(
            evaluate_psram(PSRAM_MIN_BYTES, ok),
            PsramStatus::Ready {
                bytes: PSRAM_MIN_BYTES
            }
        );
        assert_eq!(
            evaluate_psram(PSRAM_HW_BYTES, ok),
            PsramStatus::Ready {
                bytes: PSRAM_HW_BYTES
            }
        );
        // bigger chips are clamped to the 2 MB budget
        assert_eq!(
            evaluate_psram(8 * MIB, ok),
            PsramStatus::Ready {
                bytes: PSRAM_HW_BYTES
            }
        );
        let bad = Err(SelfTestError::Mismatch { word: 7 });
        assert_eq!(
            evaluate_psram(PSRAM_HW_BYTES, bad),
            PsramStatus::Degraded(PsramFault::SelfTest(SelfTestError::Mismatch { word: 7 }))
        );
        // size problems win over the (unconsulted) test result
        assert_eq!(
            evaluate_psram(0, bad),
            PsramStatus::Degraded(PsramFault::NotDetected)
        );
    }

    #[test]
    fn r14_degraded_budget_places_psram_classes_internally_with_internal_limits() {
        let mut b = degraded();
        for c in [
            MemClass::ChapterText,
            MemClass::ImageData,
            MemClass::PageTable,
            MemClass::ZipToc,
        ] {
            assert_eq!(b.region_for(c), Region::Internal);
            assert_eq!(b.class_limit(Region::Psram, c), 0);
        }
        // internal chapter limit applies, not the 768 KiB PSRAM one
        assert!(
            b.reserve(MemClass::ChapterText, INTERNAL_CHAPTER_TEXT_BYTES, 4)
                .is_ok()
        );
        assert!(matches!(
            b.reserve(MemClass::ChapterText, 1, 1),
            Err(MemError::ClassLimit {
                region: Region::Internal,
                limit: INTERNAL_CHAPTER_TEXT_BYTES,
                ..
            })
        ));
        // a 200 KiB chapter that PSRAM mode accepts is refused when degraded
        let mut d = degraded();
        assert!(d.reserve(MemClass::ChapterText, 200 * KIB, 4).is_err());
        let mut r = ready();
        assert!(r.reserve(MemClass::ChapterText, 200 * KIB, 4).is_ok());
    }

    #[test]
    fn r14_degraded_internal_pool_caps_the_sum_of_classes() {
        let mut b = degraded();
        b.reserve(MemClass::ChapterText, INTERNAL_CHAPTER_TEXT_BYTES, 4)
            .unwrap();
        // image alone (112 KiB) fits its class limit but not what is left of
        // the 158 KiB heap after the chapter cache
        let e = b
            .reserve(MemClass::ImageData, INTERNAL_IMAGE_DATA_BYTES, 4)
            .unwrap_err();
        assert!(matches!(
            e,
            MemError::PoolExhausted {
                region: Region::Internal,
                ..
            }
        ));
        assert_eq!(b.pool_limit(Region::Internal), INTERNAL_HEAP_BYTES);
    }

    #[test]
    fn r14_status_change_with_live_reservations_is_refused() {
        let mut b = MemoryBudget::new();
        let r = b.reserve(MemClass::ChapterText, 4096, 4).unwrap(); // internal
        assert_eq!(
            b.set_status(PsramStatus::Ready {
                bytes: PSRAM_HW_BYTES
            }),
            Err(MemError::StatusChangeWhileLive)
        );
        assert_eq!(b.status(), PsramStatus::NotInitialised);
        b.release(r).unwrap();
        assert!(
            b.set_status(PsramStatus::Ready {
                bytes: PSRAM_HW_BYTES
            })
            .is_ok()
        );
        // Ready -> Degraded at run time is refused while PSRAM blocks are alive
        let p = b.reserve(MemClass::ImageData, 4096, 4).unwrap();
        assert_eq!(p.region, Region::Psram);
        assert_eq!(
            b.set_status(PsramStatus::Degraded(PsramFault::NotDetected)),
            Err(MemError::StatusChangeWhileLive)
        );
        b.release(p).unwrap();
        assert!(
            b.set_status(PsramStatus::Degraded(PsramFault::NotDetected))
                .is_ok()
        );
        // same status is a no-op even with live reservations
        let q = b.reserve(MemClass::ImageData, 4096, 4).unwrap();
        assert!(
            b.set_status(PsramStatus::Degraded(PsramFault::NotDetected))
                .is_ok()
        );
        b.release(q).unwrap();
    }

    #[test]
    fn r14_internal_only_reservations_do_not_block_a_status_change() {
        let mut b = MemoryBudget::new();
        let dma = b.reserve(MemClass::DmaBuffer, 4096, 4).unwrap();
        assert!(
            b.set_status(PsramStatus::Ready {
                bytes: PSRAM_HW_BYTES
            })
            .is_ok()
        );
        assert_eq!(b.used(Region::Internal, MemClass::DmaBuffer), 4096);
        b.release(dma).unwrap();
    }

    #[test]
    fn r14_ready_status_describes_itself_and_degraded_says_why() {
        assert!(
            PsramStatus::Ready { bytes: MIB }
                .describe()
                .contains("ready")
        );
        assert!(
            PsramStatus::Degraded(PsramFault::NotDetected)
                .describe()
                .contains("not detected")
        );
        assert!(
            PsramStatus::Degraded(PsramFault::TooSmall { bytes: 1 })
                .describe()
                .contains("too small")
        );
        assert!(
            PsramStatus::Degraded(PsramFault::SelfTest(SelfTestError::TooSmall))
                .describe()
                .contains("self-test")
        );
        assert!(
            PsramStatus::NotInitialised
                .describe()
                .contains("not initialised")
        );
    }

    // --- smoke test -------------------------------------------------------

    struct Fake {
        words: Vec<u32>,
        /// word index whose bit `b` reads as stuck at 0
        stuck0: Option<(usize, u32)>,
        /// index masked with this on every access (dead address line)
        addr_mask: usize,
        syncs: usize,
    }

    impl Fake {
        fn new(n: usize) -> Self {
            Fake {
                words: vec![0; n],
                stuck0: None,
                addr_mask: usize::MAX,
                syncs: 0,
            }
        }
    }

    impl WordMem for Fake {
        fn write(&mut self, word: usize, value: u32) {
            let i = word & self.addr_mask;
            self.words[i] = value;
        }
        fn read(&mut self, word: usize) -> u32 {
            let i = word & self.addr_mask;
            let v = self.words[i];
            match self.stuck0 {
                Some((w, bit)) if w == i => v & !(1 << bit),
                _ => v,
            }
        }
        fn sync(&mut self) {
            self.syncs += 1;
        }
    }

    #[test]
    fn r14_selftest_passes_on_healthy_memory_and_syncs_between_phases() {
        let mut m = Fake::new(1 << 10);
        assert_eq!(selftest(&mut m, 1 << 10), Ok(()));
        // one sync per pass (normal + inverted): reads never see the cache
        assert_eq!(m.syncs, 2);
    }

    #[test]
    fn r14_selftest_visits_word0_every_power_of_two_and_the_last_word() {
        let v: Vec<usize> = selftest_words(100).collect();
        assert_eq!(v, vec![0, 1, 2, 4, 8, 16, 32, 64, 99]);
        // last word that is itself a power of two is visited once
        let v: Vec<usize> = selftest_words(65).collect();
        assert_eq!(v, vec![0, 1, 2, 4, 8, 16, 32, 64]);
        let v: Vec<usize> = selftest_words(2).collect();
        assert_eq!(v, vec![0, 1]);
    }

    #[test]
    fn r14_selftest_catches_a_stuck_data_bit() {
        for bit in [0u32, 7, 31] {
            let mut m = Fake::new(1 << 10);
            m.stuck0 = Some((64, bit));
            // the normal pass or the inverted pass must see the stuck bit
            assert_eq!(
                selftest(&mut m, 1 << 10),
                Err(SelfTestError::Mismatch { word: 64 }),
                "bit {bit}"
            );
        }
    }

    #[test]
    fn r14_selftest_catches_a_dead_address_line() {
        let mut m = Fake::new(1 << 10);
        m.addr_mask = !(1usize << 5); // bit 5 never reaches the chip
        assert!(matches!(
            selftest(&mut m, 1 << 10),
            Err(SelfTestError::Mismatch { .. })
        ));
    }

    #[test]
    fn r14_selftest_rejects_a_region_too_small_to_test() {
        let mut m = Fake::new(4);
        assert_eq!(selftest(&mut m, 0), Err(SelfTestError::TooSmall));
        assert_eq!(selftest(&mut m, 1), Err(SelfTestError::TooSmall));
        assert_eq!(selftest(&mut m, 2), Ok(()));
    }

    #[test]
    fn r14_selftest_failure_degrades_the_budget() {
        let mut m = Fake::new(1 << 10);
        m.stuck0 = Some((1, 3));
        let st = evaluate_psram(PSRAM_HW_BYTES, selftest(&mut m, 1 << 10));
        assert!(matches!(st, PsramStatus::Degraded(PsramFault::SelfTest(_))));
        let mut b = MemoryBudget::new();
        b.set_status(st).unwrap();
        assert_eq!(b.region_for(MemClass::ChapterText), Region::Internal);
    }

    // --- inventory --------------------------------------------------------

    #[test]
    fn r15_inventory_required_rows_are_internal_only_classes_or_runtime_stack() {
        for it in INVENTORY.iter().filter(|i| i.placement == Required) {
            assert!(
                !it.class.allows_psram(),
                "{} marked Required but class {:?}",
                it.name,
                it.class
            );
        }
    }

    #[test]
    fn r14_inventory_candidates_have_psram_capable_classes_and_fit_their_limits() {
        let st = PsramStatus::Ready {
            bytes: PSRAM_HW_BYTES,
        };
        for it in INVENTORY.iter().filter(|i| i.placement == Candidate) {
            assert!(it.class.allows_psram(), "{}", it.name);
            assert!(
                it.bytes <= class_limit(st, Region::Psram, it.class),
                "{} ({} B) exceeds the {:?} PSRAM limit",
                it.name,
                it.bytes,
                it.class
            );
        }
    }

    #[test]
    fn r15_inventory_dma_rows_are_required_and_static() {
        let dma: Vec<_> = INVENTORY
            .iter()
            .filter(|i| i.class == MemClass::DmaBuffer)
            .collect();
        assert!(dma.len() >= 3);
        for it in dma {
            assert_eq!(it.placement, Required, "{}", it.name);
            assert_eq!(it.kind, Static, "{}", it.name);
        }
    }

    #[test]
    fn inventory_rows_are_at_least_4k_except_the_documented_small_ones() {
        for it in INVENTORY {
            // rows under 4 KiB must say why they are listed
            let small_ok = it.note.starts_with("below 4 KiB");
            assert!(
                it.bytes >= 4096 || small_ok,
                "{} is {} B",
                it.name,
                it.bytes
            );
            assert!(!it.name.is_empty() && !it.source.is_empty() && !it.note.is_empty());
        }
    }

    #[test]
    fn r15_the_spi_dma_rows_match_the_shared_constant() {
        for name in ["SPI DMA RX buffer", "SPI DMA TX buffer"] {
            let it = INVENTORY.iter().find(|i| i.name == name).unwrap();
            assert_eq!(it.bytes, crate::spi::SPI_DMA_BUF_BYTES);
        }
    }

    #[test]
    fn r14_the_current_reader_allocations_fit_the_psram_plan_with_headroom() {
        // everything marked Candidate, all at once, in the class they belong to
        let mut b = ready();
        for it in INVENTORY.iter().filter(|i| i.placement == Candidate) {
            b.reserve(it.class, it.bytes, 4)
                .unwrap_or_else(|e| panic!("{}: {:?}", it.name, e));
        }
        assert!(b.pool_used(Region::Psram) < b.pool_limit(Region::Psram));
    }

    // --- PSRAM window sanity (R14) -----------------------------------------

    #[test]
    fn r14_window_fault_accepts_a_page_aligned_window_in_the_cache_range() {
        assert_eq!(
            window_fault(C61_EXTMEM_START + 0x10_0000, PSRAM_HW_BYTES),
            None
        );
        // empty is NotDetected's business, not a bad window
        assert_eq!(window_fault(0, 0), None);
    }

    #[test]
    fn r14_window_fault_rejects_out_of_range_misaligned_and_wrapping_windows() {
        let bad = Some(PsramFault::BadWindow);
        assert_eq!(window_fault(C61_RAM_START, MIB), bad); // internal RAM is not PSRAM
        assert_eq!(window_fault(C61_EXTMEM_START - 4, MIB), bad);
        assert_eq!(window_fault(C61_EXTMEM_START + 2, MIB), bad);
        assert_eq!(window_fault(C61_EXTMEM_END - MIB + 4, MIB), bad); // one word past the end
        assert_eq!(window_fault(C61_EXTMEM_END - MIB, MIB), None); // exactly to the end
        assert_eq!(window_fault(usize::MAX - 3, 8), bad); // wraps
        assert_eq!(window_fault(C61_EXTMEM_START, usize::MAX), bad);
    }

    #[test]
    fn r14_bad_window_degrades_with_a_readable_reason() {
        let st = PsramStatus::Degraded(PsramFault::BadWindow);
        assert!(st.describe().contains("window"));
        let mut b = MemoryBudget::new();
        b.set_status(st).unwrap();
        assert_eq!(b.region_for(MemClass::ImageData), Region::Internal);
        assert_eq!(psram_heap_bytes(st), 0);
    }

    // --- ELF image budget (R22) ---------------------------------------------

    fn image(static_bytes: usize) -> ImageMemory {
        ImageMemory {
            static_end: C61_RAM_START + static_bytes,
            stack_addr: C61_RAM_START + static_bytes,
            stack_size: C61_RAM_LEN - static_bytes,
        }
    }

    #[test]
    fn r22_check_image_reports_headroom_over_the_stack_minimum() {
        let r = check_image(&image(0x156e0)).unwrap();
        assert_eq!(r.static_bytes, 0x156e0);
        assert_eq!(r.stack_bytes, C61_RAM_LEN - 0x156e0);
        assert_eq!(r.stack_headroom, r.stack_bytes - STACK_MIN_BYTES);
        assert_eq!(r.static_permille, 0x156e0 * 1000 / C61_RAM_LEN);
    }

    #[test]
    fn r22_check_image_statics_exactly_at_the_budget_pass_one_byte_more_fails() {
        let ok = check_image(&image(STATIC_RAM_MAX_BYTES)).unwrap();
        assert_eq!(ok.stack_bytes, STACK_MIN_BYTES);
        assert_eq!(ok.stack_headroom, 0);
        // the stack shrinks by the same byte, so the stack rule trips first
        // only when it is the smaller number; here statics are over budget
        assert_eq!(
            check_image(&image(STATIC_RAM_MAX_BYTES + 1)),
            Err(ImageFault::StaticsOverBudget {
                used: STATIC_RAM_MAX_BYTES + 1,
                max: STATIC_RAM_MAX_BYTES
            })
        );
    }

    #[test]
    fn r22_check_image_rejects_inconsistent_layouts() {
        let mut img = image(0x1000);
        img.stack_addr += 4; // gap between statics and stack
        assert_eq!(check_image(&img), Err(ImageFault::StackNotLastInRam));
        let mut img = image(0x1000);
        img.stack_size += 4; // stack runs past the end of RAM
        assert_eq!(check_image(&img), Err(ImageFault::StackNotLastInRam));
        let mut img = image(0x1000);
        img.stack_size = usize::MAX; // overflow
        assert_eq!(check_image(&img), Err(ImageFault::StackNotLastInRam));
        let img = ImageMemory {
            static_end: C61_RAM_START - 1,
            stack_addr: C61_RAM_START - 1,
            stack_size: C61_RAM_LEN + 1,
        };
        assert_eq!(check_image(&img), Err(ImageFault::StaticsOutsideRam));
        let img = ImageMemory {
            static_end: C61_RECLAIMED_START + 1,
            stack_addr: C61_RECLAIMED_START + 1,
            stack_size: 0,
        };
        assert_eq!(check_image(&img), Err(ImageFault::StaticsOutsideRam));
    }

    #[test]
    fn r15_writable_sections_must_be_internal_read_only_may_be_flash_mapped() {
        // .bss in RAM
        assert!(section_placement_ok(0x4080_2a88, 0x12c58, true));
        // .text / .rodata in the flash window
        assert!(section_placement_ok(0x4200_697c, 0x14092, false));
        // a writable section in the PSRAM/flash window is refused
        assert!(!section_placement_ok(0x4200_0000, 16, true));
        // a read-only one straddling the end of both windows is refused
        assert!(!section_placement_ok(C61_EXTMEM_END - 8, 16, false));
        assert!(!section_placement_ok(C61_RAM_END - 8, 16, false));
        // outside everything
        assert!(!section_placement_ok(0, 16, false));
        // empty sections are ignored
        assert!(section_placement_ok(0, 0, true));
    }
}
