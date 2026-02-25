// A simple priority-based job scheduler for cooperative multitasking
// NOTE: No dynamic allocation and uses fixed-size queues
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    HandleInput,
    RenderPage,

    PrefetchNext,
    PrefetchPrev,

    LayoutChapter { chapter: u16 },
    CacheChapter { chapter: u16 },
}

impl fmt::Display for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Job::HandleInput => write!(f, "HandleInput"),
            Job::RenderPage => write!(f, "RenderPage"),
            Job::PrefetchNext => write!(f, "PrefetchNext"),
            Job::PrefetchPrev => write!(f, "PrefetchPrev"),
            Job::LayoutChapter { chapter } => write!(f, "LayoutChapter({})", chapter),
            Job::CacheChapter { chapter } => write!(f, "CacheChapter({})", chapter),
        }
    }
}

/// Job priority levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    High = 0,
    Normal = 1,
    Low = 2,
}

impl Job {
    pub const fn priority(&self) -> Priority {
        match self {
            Job::HandleInput | Job::RenderPage => Priority::High,
            Job::PrefetchNext | Job::PrefetchPrev => Priority::Normal,
            Job::LayoutChapter { .. } | Job::CacheChapter { .. } => Priority::Low,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PushError {
    /// Queue for this priority level is full, contains the rejected job
    Full(Job),
}

impl fmt::Display for PushError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PushError::Full(job) => write!(f, "queue full, rejected {}", job),
        }
    }
}

// ring buffer for jobs
pub struct JobQueue<const N: usize> {
    buf: [Option<Job>; N],
    head: usize, // next to read
    tail: usize, // next to write
    len: usize,
}

impl<const N: usize> JobQueue<N> {
    pub const fn new() -> Self {
        Self {
            buf: [None; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, job: Job) -> Result<(), Job> {
        if self.len >= N {
            return Err(job);
        }
        self.buf[self.tail] = Some(job);
        self.tail = (self.tail + 1) % N;
        self.len += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<Job> {
        if self.len == 0 {
            return None;
        }
        let job = self.buf[self.head].take();
        self.head = (self.head + 1) % N;
        self.len -= 1;
        job
    }

    pub fn peek(&self) -> Option<&Job> {
        if self.len == 0 {
            None
        } else {
            self.buf[self.head].as_ref()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.len >= N
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub fn clear(&mut self) {
        while self.pop().is_some() {}
    }

    pub fn contains(&self, job: &Job) -> bool {
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

impl<const N: usize> Default for JobQueue<N> {
    fn default() -> Self {
        Self::new()
    }
}

// The job scheduler
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

    // push a job and returns error with the job if queue is full
    pub fn push(&mut self, job: Job) -> Result<(), PushError> {
        let result = match job.priority() {
            Priority::High => self.high.push(job),
            Priority::Normal => self.normal.push(job),
            Priority::Low => self.low.push(job),
        };
        result.map_err(PushError::Full)
    }

    // push a job dropping silently if queue is full
    pub fn push_or_drop(&mut self, job: Job) -> bool {
        self.push(job).is_ok()
    }

    // push a'job but if queue is full, drop the oldest job of same priority
    pub fn push_replacing(&mut self, job: Job) {
        match job.priority() {
            Priority::High => {
                if self.high.is_full() {
                    self.high.pop();
                }
                let _ = self.high.push(job);
            }
            Priority::Normal => {
                if self.normal.is_full() {
                    self.normal.pop();
                }
                let _ = self.normal.push(job);
            }
            Priority::Low => {
                if self.low.is_full() {
                    self.low.pop();
                }
                let _ = self.low.push(job);
            }
        }
    }

    // Schedule a job only if it's not already queued (dedup that queue).
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

    // the next job to execute
    pub fn pop(&mut self) -> Option<Job> {
        self.high
            .pop()
            .or_else(|| self.normal.pop())
            .or_else(|| self.low.pop())
    }

    pub fn peek(&self) -> Option<&Job> {
        self.high
            .peek()
            .or_else(|| self.normal.peek())
            .or_else(|| self.low.peek())
    }

    pub fn is_empty(&self) -> bool {
        self.high.is_empty() && self.normal.is_empty() && self.low.is_empty()
    }

    pub fn pending(&self) -> usize {
        self.high.len() + self.normal.len() + self.low.len()
    }

    pub fn pending_by_priority(&self, priority: Priority) -> usize {
        match priority {
            Priority::High => self.high.len(),
            Priority::Normal => self.normal.len(),
            Priority::Low => self.low.len(),
        }
    }

    pub fn clear(&mut self) {
        self.high.clear();
        self.normal.clear();
        self.low.clear();
    }

    pub fn clear_priority(&mut self, priority: Priority) {
        match priority {
            Priority::High => self.high.clear(),
            Priority::Normal => self.normal.clear(),
            Priority::Low => self.low.clear(),
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}
