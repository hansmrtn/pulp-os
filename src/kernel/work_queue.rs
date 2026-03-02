// Background work queue for pulp-os.
//
// General-purpose async task system that offloads CPU-heavy processing
// to a dedicated Embassy task while the main UI loop stays responsive.
//
//   ┌────────────┐  WorkItem   ┌──────────────┐  WorkResult  ┌────────────┐
//   │ Main Loop  │────────────>│ worker task   │─────────────>│ Main Loop  │
//   │ (SD I/O)   │             │ (CPU only)    │              │ (SD write) │
//   └────────────┘             └──────────────┘               └────────────┘
//
// Cancellation
// ────────────
// Generation-based.  Each logical session (e.g. opening a new book)
// gets a monotonically increasing generation number.  Work items carry
// the generation they were submitted under.  When the generation
// changes, the worker silently discards stale items and results.
//
// No explicit cancel signal is needed — just bump the generation and
// call `drain()` to flush the channels.
//
// Status
// ──────
// A global `BgStatus` snapshot is updated by the worker before and
// after every item.  Any context (status bar draw, reader draw, logs)
// can call `status()` to get a cheap read of what the worker is doing.
//
// Memory
// ──────
// Channel capacity 1 → natural back-pressure; at most one item and
// one result buffered at a time.  The worker drops input buffers
// before sending results so peak heap never holds both simultaneously.
//
// Extending
// ─────────
// Add new variants to `WorkTask` and `WorkOutcome`, then handle them
// in the `worker_task` match.  The queue infrastructure, generation
// tracking, status reporting, and cancellation apply automatically.

extern crate alloc;

use alloc::vec::Vec;
use core::cell::Cell;

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use smol_epub::DecodedImage;

// ── status reporting ────────────────────────────────────────────────

/// What the background worker is currently doing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BgWorkKind {
    /// Worker is idle — waiting for the next item.
    Idle = 0,
    /// Stripping HTML from an XHTML chapter.
    StripChapter = 1,
    /// Decoding a JPEG or PNG image to a 1-bit bitmap.
    DecodeImage = 2,
}

impl BgWorkKind {
    /// Compact label for the status bar and structured logs.
    ///
    /// Returns `""` when idle so callers can skip rendering.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::StripChapter => "CH",
            Self::DecodeImage => "IMG",
        }
    }
}

/// Snapshot of background work state, readable from any context.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BgStatus {
    /// What the worker is currently doing.
    pub kind: BgWorkKind,
    /// Generation of the work in progress (0 when idle).
    pub generation: u16,
}

impl BgStatus {
    /// Constant idle status.
    pub const IDLE: Self = Self {
        kind: BgWorkKind::Idle,
        generation: 0,
    };

    /// Returns `true` if the worker is actively processing something.
    #[inline]
    pub const fn is_active(&self) -> bool {
        !matches!(self.kind, BgWorkKind::Idle)
    }

    /// Returns `true` if work is active *and* belongs to the given generation.
    #[inline]
    pub const fn is_active_for(&self, target_gen: u16) -> bool {
        self.is_active() && self.generation == target_gen
    }
}

static STATUS: Mutex<Cell<BgStatus>> = Mutex::new(Cell::new(BgStatus::IDLE));

/// Read the current background work status.
///
/// Cheap on single-core: just a critical-section load.
#[inline]
pub fn status() -> BgStatus {
    critical_section::with(|cs| STATUS.borrow(cs).get())
}

/// Returns `true` if the worker is not actively processing anything.
#[inline]
pub fn is_idle() -> bool {
    !status().is_active()
}

fn set_status(s: BgStatus) {
    critical_section::with(|cs| STATUS.borrow(cs).set(s));
}

// ── generation tracking ─────────────────────────────────────────────

static ACTIVE_GEN: Mutex<Cell<u16>> = Mutex::new(Cell::new(0));
static GEN_COUNTER: Mutex<Cell<u16>> = Mutex::new(Cell::new(0));

/// Allocate and activate a new generation.
///
/// Returns the new generation number.  Items submitted under previous
/// generations will be silently discarded by the worker.
///
/// Typical call site: the reader's `on_enter` when opening a book.
pub fn next_generation() -> u16 {
    critical_section::with(|cs| {
        let c = GEN_COUNTER.borrow(cs);
        let g = c.get().wrapping_add(1);
        c.set(g);
        ACTIVE_GEN.borrow(cs).set(g);
        g
    })
}

/// Read the currently active generation.
#[inline]
pub fn active_generation() -> u16 {
    critical_section::with(|cs| ACTIVE_GEN.borrow(cs).get())
}

/// Explicitly set the active generation.
///
/// Useful on resume when the reader already knows its generation.
pub fn set_active_generation(g: u16) {
    critical_section::with(|cs| ACTIVE_GEN.borrow(cs).set(g));
}

// ── work item / result types ────────────────────────────────────────

/// A unit of CPU-only work to be processed in the background.
///
/// Variants map 1:1 to [`WorkOutcome`] variants.  Add new kinds here
/// and in `WorkOutcome`, then handle them in the `worker_task` match.
pub enum WorkTask {
    /// Strip HTML from decompressed XHTML, producing styled plain text
    /// with inline marker codes.  Result: [`WorkOutcome::ChapterReady`]
    /// or [`WorkOutcome::ChapterFailed`].
    StripChapter {
        /// Spine index (for matching the result to the chapter).
        chapter_idx: u16,
        /// Full uncompressed XHTML bytes.
        xhtml: Vec<u8>,
    },

    /// Decode a JPEG or PNG to a 1-bit Floyd–Steinberg dithered
    /// bitmap.  Result: [`WorkOutcome::ImageReady`] or
    /// [`WorkOutcome::ImageFailed`].
    DecodeImage {
        /// FNV-1a hash of the resolved ZIP path (cache file key).
        path_hash: u32,
        /// Full uncompressed image bytes.
        data: Vec<u8>,
        /// `true` for JPEG, `false` for PNG.
        is_jpeg: bool,
        /// Maximum output width in pixels.
        max_w: u16,
        /// Maximum output height in pixels.
        max_h: u16,
    },
}

/// A work item: a [`WorkTask`] tagged with its generation.
pub struct WorkItem {
    /// Generation this item was submitted under.
    pub generation: u16,
    /// The actual work to perform.
    pub task: WorkTask,
}

/// Outcome of a completed work item.
pub enum WorkOutcome {
    /// Chapter HTML-strip succeeded.
    ChapterReady { chapter_idx: u16, text: Vec<u8> },
    /// Chapter HTML-strip failed.
    ChapterFailed {
        chapter_idx: u16,
        error: &'static str,
    },
    /// Image decode succeeded.
    ImageReady { path_hash: u32, image: DecodedImage },
    /// Image decode failed.
    ImageFailed { path_hash: u32, error: &'static str },
}

/// A completed result: a [`WorkOutcome`] tagged with its generation.
pub struct WorkResult {
    /// Generation this result belongs to.
    pub generation: u16,
    /// The outcome of the work.
    pub outcome: WorkOutcome,
}

impl WorkResult {
    /// Returns `true` if this result is still relevant (its generation
    /// matches the currently active generation).
    #[inline]
    pub fn is_current(&self) -> bool {
        self.generation == active_generation()
    }
}

// ── channels ────────────────────────────────────────────────────────

/// Main loop → worker.  Capacity 1 for back-pressure.
static WORK_IN: Channel<CriticalSectionRawMutex, WorkItem, 1> = Channel::new();

/// Worker → main loop.  Capacity 1.
static WORK_OUT: Channel<CriticalSectionRawMutex, WorkResult, 1> = Channel::new();

// ── public API (called from main loop) ──────────────────────────────

/// Submit a work item for background processing.
///
/// Returns `true` if the item was accepted, `false` if the channel is
/// full (a previous item hasn't been consumed by the worker yet).
///
/// The caller should generally call [`try_recv`] first to collect any
/// pending result before submitting the next item.
pub fn submit(generation: u16, task: WorkTask) -> bool {
    WORK_IN.try_send(WorkItem { generation, task }).is_ok()
}

/// Non-blocking poll for a completed result.
///
/// Returns `None` if the worker hasn't finished the current item yet,
/// or if no items are in flight.
#[inline]
pub fn try_recv() -> Option<WorkResult> {
    WORK_OUT.try_receive().ok()
}

/// Drain all queued items and pending results.
///
/// Call after [`next_generation`] (or [`set_active_generation`]) to
/// discard stale work from a previous session.  Any item currently
/// being processed will complete, but its result will be discarded by
/// the worker's post-processing generation check.
pub fn drain() {
    while WORK_IN.try_receive().is_ok() {}
    while WORK_OUT.try_receive().is_ok() {}
}

/// Convenience: bump to a new generation and drain stale work.
///
/// Returns the new generation number.
pub fn reset() -> u16 {
    let g = next_generation();
    drain();
    log::info!("[work] reset -> gen {}", g);
    g
}

// ── worker task ─────────────────────────────────────────────────────

/// Background Embassy task that processes [`WorkItem`]s.
///
/// Spawn once at startup:
///
/// ```rust,ignore
/// spawner.spawn(work_queue::worker_task()).unwrap();
/// ```
///
/// The task idles with zero CPU when the channel is empty.  Each item
/// is processed sequentially; peak memory is bounded by a single
/// input buffer + a single output buffer.
#[embassy_executor::task]
pub async fn worker_task() -> ! {
    log::info!("[work] worker ready");

    loop {
        // ── idle: wait for next item ────────────────────────────
        set_status(BgStatus::IDLE);
        let item = WORK_IN.receive().await;

        // ── pre-processing generation check ─────────────────────
        let g = item.generation;
        if g != active_generation() {
            log::info!(
                "[work] skip stale item (gen {} != active {})",
                g,
                active_generation()
            );
            drop(item);
            continue;
        }

        match item.task {
            // ── chapter HTML stripping ──────────────────────────
            WorkTask::StripChapter { chapter_idx, xhtml } => {
                set_status(BgStatus {
                    kind: BgWorkKind::StripChapter,
                    generation: g,
                });

                let src_len = xhtml.len();
                log::info!(
                    "[work] ch{}: strip {} bytes (gen {})",
                    chapter_idx,
                    src_len,
                    g,
                );

                let result = smol_epub::cache::strip_html_buf(&xhtml);
                // Free the source XHTML immediately — the result Vec
                // is the only heap allocation we carry forward.
                drop(xhtml);

                // Post-processing generation check: the user may have
                // changed books while we were processing.
                if g != active_generation() {
                    log::info!("[work] ch{}: discarded (gen {} stale)", chapter_idx, g,);
                    continue;
                }

                let outcome = match result {
                    Ok(text) => {
                        log::info!(
                            "[work] ch{}: {} → {} bytes",
                            chapter_idx,
                            src_len,
                            text.len(),
                        );
                        WorkOutcome::ChapterReady { chapter_idx, text }
                    }
                    Err(e) => {
                        log::warn!("[work] ch{}: strip failed: {}", chapter_idx, e);
                        WorkOutcome::ChapterFailed {
                            chapter_idx,
                            error: e,
                        }
                    }
                };

                WORK_OUT
                    .send(WorkResult {
                        generation: g,
                        outcome,
                    })
                    .await;
            }

            // ── image decoding ──────────────────────────────────
            WorkTask::DecodeImage {
                path_hash,
                data,
                is_jpeg,
                max_w,
                max_h,
            } => {
                set_status(BgStatus {
                    kind: BgWorkKind::DecodeImage,
                    generation: g,
                });

                let fmt = if is_jpeg { "JPEG" } else { "PNG" };
                log::info!(
                    "[work] img {:#010X}: decode {} ({} bytes, {}x{}, gen {})",
                    path_hash,
                    fmt,
                    data.len(),
                    max_w,
                    max_h,
                    g,
                );

                let result = if is_jpeg {
                    smol_epub::jpeg::decode_jpeg_fit(&data, max_w, max_h)
                } else {
                    smol_epub::png::decode_png_fit(&data, max_w, max_h)
                };
                // Free compressed source before sending decoded bitmap.
                drop(data);

                if g != active_generation() {
                    log::info!(
                        "[work] img {:#010X}: discarded (gen {} stale)",
                        path_hash,
                        g,
                    );
                    continue;
                }

                let outcome = match result {
                    Ok(image) => {
                        log::info!(
                            "[work] img {:#010X}: {}×{} ({}B 1-bit)",
                            path_hash,
                            image.width,
                            image.height,
                            image.data.len(),
                        );
                        WorkOutcome::ImageReady { path_hash, image }
                    }
                    Err(e) => {
                        log::warn!("[work] img {:#010X}: decode failed: {}", path_hash, e,);
                        WorkOutcome::ImageFailed {
                            path_hash,
                            error: e,
                        }
                    }
                };

                WORK_OUT
                    .send(WorkResult {
                        generation: g,
                        outcome,
                    })
                    .await;
            }
        }
    }
}
