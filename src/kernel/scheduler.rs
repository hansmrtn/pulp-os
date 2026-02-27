// Priority job scheduler, cooperative
//
// Jobs are signals, not data carriers. Three tiers (High/Normal/Low),
// FIFO within each. Fixed-size ring buffers, no allocation.

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    PollInput,
    Render,
    AppWork,
    UpdateStatusBar,
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

    fn contains(&self, job: &Job) -> bool {
        if self.len == 0 {
            return false;
        }
        let mut i = self.head;
        for _ in 0..self.len {
            if let Some(ref j) = self.buf[i]
                && j == job
            {
                return true;
            }
            i = (i + 1) % N;
        }
        false
    }
}

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

    pub fn push(&mut self, job: Job) -> Result<(), PushError> {
        match job.priority() {
            Priority::High => self.high.push(job),
            Priority::Normal => self.normal.push(job),
            Priority::Low => self.low.push(job),
        }
        .map_err(PushError::Full)
    }

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
