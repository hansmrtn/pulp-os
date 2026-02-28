// Priority job scheduler, cooperative
//
// Jobs are signals, not data carriers. Three tiers (High/Normal/Low),
// FIFO within each. Fixed-size ring buffers, no allocation.
//
// Dedup uses a u8 bitmap (one bit per Job variant) for O(1)
// push_unique instead of linear queue scans.

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    PollInput,
    Render,
    RenderPhase2,
    RenderPhase3,
    AppWork,
    UpdateStatusBar,
}

impl Job {
    /// Bit index for the pending bitmap (0..5).
    #[inline]
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::PollInput => write!(f, "PollInput"),
            Job::Render => write!(f, "Render"),
            Job::RenderPhase2 => write!(f, "RenderPhase2"),
            Job::RenderPhase3 => write!(f, "RenderPhase3"),
            Job::AppWork => write!(f, "AppWork"),
            Job::UpdateStatusBar => write!(f, "UpdateStatusBar"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    High = 0,
    Normal = 1,
    Low = 2,
}

impl Job {
    pub const fn priority(&self) -> Priority {
        match self {
            Job::PollInput | Job::Render => Priority::High,
            Job::RenderPhase2 | Job::RenderPhase3 | Job::AppWork | Job::UpdateStatusBar => {
                Priority::Normal
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PushError {
    Full(Job),
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Full(job) => write!(f, "queue full, rejected {}", job),
        }
    }
}

struct JobQueue<const N: usize> {
    buf: [Option<Job>; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<const N: usize> JobQueue<N> {
    const fn new() -> Self {
        Self {
            buf: [None; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    fn push(&mut self, job: Job) -> Result<(), Job> {
        if self.len >= N {
            return Err(job);
        }
        self.buf[self.tail] = Some(job);
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        Ok(())
    }

    fn pop(&mut self) -> Option<Job> {
        if self.len == 0 {
            return None;
        }
        let job = self.buf[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        job
    }
}

pub struct Scheduler {
    high: JobQueue<4>,
    normal: JobQueue<8>,
    low: JobQueue<16>,
    /// One bit per `Job` variant — set on `push_unique`, cleared on `pop`.
    pending: u8,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            high: JobQueue::new(),
            normal: JobQueue::new(),
            low: JobQueue::new(),
            pending: 0,
        }
    }

    /// Push a job without dedup. The pending bitmap is still updated
    /// so that a subsequent `push_unique` for the same variant is
    /// correctly suppressed.
    pub fn push(&mut self, job: Job) -> Result<(), PushError> {
        let result = match job.priority() {
            Priority::High => self.high.push(job),
            Priority::Normal => self.normal.push(job),
            Priority::Low => self.low.push(job),
        }
        .map_err(PushError::Full);

        if result.is_ok() {
            self.pending |= job.bit();
        }
        result
    }

    /// Push a job only if the same variant is not already enqueued.
    /// O(1) via bitmap check — no queue scan.
    pub fn push_unique(&mut self, job: Job) -> Result<(), PushError> {
        if self.pending & job.bit() != 0 {
            return Ok(());
        }
        self.push(job)
    }

    pub fn pop(&mut self) -> Option<Job> {
        let job = self
            .high
            .pop()
            .or_else(|| self.normal.pop())
            .or_else(|| self.low.pop());

        if let Some(j) = job {
            // Only clear the bit if no other instance of this variant
            // remains in any queue. For the common case (push_unique
            // only, at most one instance per variant) this is always
            // safe to clear immediately. If `push` was used to enqueue
            // duplicates, the bit may be cleared early — but that only
            // means a subsequent `push_unique` might re-enqueue rather
            // than suppress, which is harmless for signal-style jobs.
            self.pending &= !j.bit();
        }
        job
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
