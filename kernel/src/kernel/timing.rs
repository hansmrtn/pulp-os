// timing constants for the kernel scheduler and tasks
//
// timing values that control polling intervals, debouncing,
// coalescing, and housekeeping. some of these may
// may become runtime-configurable in the future

// main scheduler tick interval (ms)
// controls how often the event loop wakes to check for work
pub const TICK_MS: u64 = 10;

// input task poll interval (ms)
// ADC button sampling rate; should probably(?) match or equal TICK_MS
pub const INPUT_TICK_MS: u64 = 10;

// button debounce window (ms)
// raw input must be stable for this duration before registering
pub const DEBOUNCE_MS: u64 = 15;

// long-press detection threshold (ms)
// holding a button for this duration generates a LongPress event
pub const LONG_PRESS_MS: u64 = 1000;

// key repeat interval (ms)
// after long-press, generates Repeat events at this rate
pub const REPEAT_MS: u64 = 150;

// ADC oversampling count
pub const ADC_OVERSAMPLE: u32 = 4;

// status log interval (s)
pub const STATUS_INTERVAL_SECS: u64 = 5;

// SD card presence check interval (s)
pub const SD_CHECK_INTERVAL_SECS: u64 = 30;

// bookmark flush interval (seconds)
pub const BOOKMARK_FLUSH_INTERVAL_SECS: u64 = 30;

// bookmark flush stagger delay (seconds)
// offset from sd check to avoid simultaneous sd operations
pub const BOOKMARK_FLUSH_STAGGER_SECS: u64 = 2;

// initial housekeeping delay (seconds)
// delay before first housekeeping cycle to let boot settle
pub const HOUSEKEEPING_INITIAL_DELAY_SECS: u64 = 5;

// coalesce window for batch dirty marks (ms)
// batch multiple rapid dirty marks into a single refresh
pub const COALESCE_WINDOW_MS: u64 = 50;

// battery read interval in input task ticks
// ticks * 10 ms = seconds between battery reads
pub const BATTERY_INTERVAL_TICKS: u32 = 3000;
