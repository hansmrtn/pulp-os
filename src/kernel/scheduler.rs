// Priority-based job scheduler for cooperative multitasking.
//
// Jobs are signals, not data carriers. State lives in the app/driver
// that handles the job. The scheduler only decides execution order.
//
// No dynamic allocation — fixed-size ring buffer queues per priority tier.
use core::fmt;

/// Schedulable units of work.
///
/// Jobs carry no payload. The handler reads state from wherever it
/// lives (app struct, driver, context) when the job executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    // ── High priority: interactive, latency-sensitive ──────────
    /// Poll the input driver for debounced button events.
    PollInput,
    /// Flush pending redraw (full or partial) to the display.
    Render,

    // ── Normal priority: responsive but deferrable ─────────────
    /// Run the active app's `on_work()` with OS services.
    /// Generic — the kernel doesn't know what the app will do.
    /// Replaces per-app job variants (no new jobs when adding apps).
    AppWork,
    /// Sample battery ADC and refresh the status bar text.
    UpdateStatusBar,

    // ── Low priority: speculative / background ─────────────────
    // (Reserved for future work: prefetch, layout, cache)
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::PollInput => write!(f, "PollInput"),
            Job::Render => write!(f, "Render"),
            Job::AppWork => write!(f, "AppWork"),
            Job::UpdateStatusBar => write!(f, "UpdateStatusBar"),
        }
    }
}

/// Job priority levels (lower numeric value = higher priority).
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
            Job::AppWork | Job::UpdateStatusBar => Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PushError {
    /// Queue for this priority level is full; contains the rejected job.
    Full(Job),
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Full(job) => write!(f, "queue full, rejected {}", job),
        }
    }
}

// ── Ring buffer ────────────────────────────────────────────────

struct JobQueue<const N: usize> {
    buf: [Option<Job>; N],
    head: usize, // next to read
    tail: usize, // next to write
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

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn contains(&self, job: &Job) -> bool {
        if self.len == 0 {
            return false;
        }
        let mut i = self.head;
        for _ in 0..self.len {
            if let Some(ref j) = self.buf[i] {
                if j == job {
                    return true;
                }
            }
            i = (i + 1) % N;
        }
        false
    }
}

// ── Scheduler ──────────────────────────────────────────────────

/// Priority-based job scheduler.
///
/// `pop()` always returns the highest-priority pending job (FIFO
/// within each tier). Jobs can enqueue follow-on work during
/// execution — the drain loop picks it up immediately.
pub struct Scheduler {
    high: JobQueue<4>,
    normal: JobQueue<8>,
    low: JobQueue<16>,
}

impl Scheduler {
    pub const fn new() -> Self {
        Self {
            high: JobQueue::new(),
            normal: JobQueue::new(),
            low: JobQueue::new(),
        }
    }

    /// Push a job; returns error if the priority queue is full.
    pub fn push(&mut self, job: Job) -> Result<(), PushError> {
        let result = match job.priority() {
            Priority::High => self.high.push(job),
            Priority::Normal => self.normal.push(job),
            Priority::Low => self.low.push(job),
        };
        result.map_err(PushError::Full)
    }

    /// Schedule a job only if it's not already queued (dedup).
    /// Primary method for enqueuing — prevents duplicate work.
    pub fn push_unique(&mut self, job: Job) -> Result<(), PushError> {
        match job.priority() {
            Priority::High => {
                if self.high.contains(&job) {
                    return Ok(());
                }
                self.high.push(job).map_err(PushError::Full)
            }
            Priority::Normal => {
                if self.normal.contains(&job) {
                    return Ok(());
                }
                self.normal.push(job).map_err(PushError::Full)
            }
            Priority::Low => {
                if self.low.contains(&job) {
                    return Ok(());
                }
                self.low.push(job).map_err(PushError::Full)
            }
        }
    }

    /// Pop the highest-priority pending job.
    pub fn pop(&mut self) -> Option<Job> {
        self.high
            .pop()
            .or_else(|| self.normal.pop())
            .or_else(|| self.low.pop())
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
