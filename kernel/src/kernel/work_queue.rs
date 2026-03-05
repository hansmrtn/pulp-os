// background work queue
//
// offloads CPU-heavy processing (HTML strip, image decode) to a
// dedicated embassy task while the main UI loop stays responsive
//
// generation-based cancellation: bump generation and drain() to
// discard stale work; no explicit cancel signal needed
//
// channel capacity 1 for natural back-pressure; worker drops input
// buffers before sending results so peak heap is bounded

extern crate alloc;

use alloc::vec::Vec;
use core::cell::Cell;

use critical_section::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

// 1-bit decoded image
pub struct DecodedImage {
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
    pub stride: usize,
}

impl core::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("data_len", &self.data.len())
            .field("stride", &self.stride)
            .finish()
    }
}

pub type ImageDecodeFn = fn(&[u8], bool, u16, u16) -> Result<DecodedImage, &'static str>;

static IMAGE_DECODER: Mutex<Cell<Option<ImageDecodeFn>>> = Mutex::new(Cell::new(None));

pub fn register_image_decoder(f: ImageDecodeFn) {
    critical_section::with(|cs| IMAGE_DECODER.borrow(cs).set(Some(f)));
}

fn get_image_decoder() -> ImageDecodeFn {
    critical_section::with(|cs| IMAGE_DECODER.borrow(cs).get())
        .expect("work_queue: no image decoder registered")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BgWorkKind {
    Idle = 0,
    DecodeImage = 1,
}

impl BgWorkKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::DecodeImage => "IMG",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BgStatus {
    pub kind: BgWorkKind,
    pub generation: u16,
}

impl BgStatus {
    pub const IDLE: Self = Self {
        kind: BgWorkKind::Idle,
        generation: 0,
    };

    #[inline]
    pub const fn is_active(&self) -> bool {
        !matches!(self.kind, BgWorkKind::Idle)
    }

    #[inline]
    pub const fn is_active_for(&self, target_gen: u16) -> bool {
        self.is_active() && self.generation == target_gen
    }
}

static STATUS: Mutex<Cell<BgStatus>> = Mutex::new(Cell::new(BgStatus::IDLE));

#[inline]
pub fn status() -> BgStatus {
    critical_section::with(|cs| STATUS.borrow(cs).get())
}

#[inline]
pub fn is_idle() -> bool {
    !status().is_active()
}

fn set_status(s: BgStatus) {
    critical_section::with(|cs| STATUS.borrow(cs).set(s));
}

static ACTIVE_GEN: Mutex<Cell<u16>> = Mutex::new(Cell::new(0));
static GEN_COUNTER: Mutex<Cell<u16>> = Mutex::new(Cell::new(0));

pub fn next_generation() -> u16 {
    critical_section::with(|cs| {
        let c = GEN_COUNTER.borrow(cs);
        let g = c.get().wrapping_add(1);
        c.set(g);
        ACTIVE_GEN.borrow(cs).set(g);
        g
    })
}

#[inline]
pub fn active_generation() -> u16 {
    critical_section::with(|cs| ACTIVE_GEN.borrow(cs).get())
}

pub fn set_active_generation(g: u16) {
    critical_section::with(|cs| ACTIVE_GEN.borrow(cs).set(g));
}

pub enum WorkTask {
    DecodeImage {
        path_hash: u32,
        data: Vec<u8>,
        is_jpeg: bool,
        max_w: u16,
        max_h: u16,
    },
}

pub struct WorkItem {
    pub generation: u16,
    pub task: WorkTask,
}

pub enum WorkOutcome {
    ImageReady { path_hash: u32, image: DecodedImage },
    ImageFailed { path_hash: u32, error: &'static str },
}

pub struct WorkResult {
    pub generation: u16,
    pub outcome: WorkOutcome,
}

impl WorkResult {
    #[inline]
    pub fn is_current(&self) -> bool {
        self.generation == active_generation()
    }
}

static WORK_IN: Channel<CriticalSectionRawMutex, WorkItem, 2> = Channel::new();
static WORK_OUT: Channel<CriticalSectionRawMutex, WorkResult, 2> = Channel::new();

// true if the input channel has room for at least one more item.
#[inline]
pub fn can_submit() -> bool {
    !WORK_IN.is_full()
}

pub fn submit(generation: u16, task: WorkTask) -> bool {
    WORK_IN.try_send(WorkItem { generation, task }).is_ok()
}

#[inline]
pub fn try_recv() -> Option<WorkResult> {
    WORK_OUT.try_receive().ok()
}

pub fn drain() {
    while WORK_IN.try_receive().is_ok() {}
    while WORK_OUT.try_receive().is_ok() {}
}

pub fn reset() -> u16 {
    let g = next_generation();
    drain();
    log::info!("[work] reset -> gen {}", g);
    g
}

#[embassy_executor::task]
pub async fn worker_task() -> ! {
    log::info!("[work] worker ready");

    loop {
        set_status(BgStatus::IDLE);
        let item = WORK_IN.receive().await;

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

                let decode = get_image_decoder();
                let result = decode(&data, is_jpeg, max_w, max_h);
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
                            "[work] img {:#010X}: {}x{} ({}B 1-bit)",
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
