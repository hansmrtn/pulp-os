// App-lifecycle decisions of the OnePage C61 firmware (T12), HAL-free.
//
// The kernel scheduler and the app manager do the I/O; the *decisions* that are
// more than a plumbing call are here so the host tests exercise the code the
// firmware links:
//
//   * `Periodic`            when the 30 s battery measurement and the card-detect
//                           sampling are due (the main loop ticks every 10 ms)
//   * `check_restorable`    R20: is a validated session actually applicable to
//                           *this* card and *this* (offline) firmware
//   * `post_restore`        what to do with the session file after an apply
//                           attempt (decision: keep on success, clear on failure
//                           so bad data can never cause a boot loop)
//   * `opens_quick_menu`    the C61 has no Menu key (X4: Power short press); the
//                           only function without another route is reachable by
//                           a long press of ENTER inside the reader (apps layer
//                           only, no new key semantic in the key driver)
//   * `DisplayHealth`       recovery policy for a failed full refresh (BUSY
//                           timeout / bus error): re-init and retry once, then
//                           give up for this frame and redraw on the next input
//
// None of this knows about GPIO, SPI or the executor.

use crate::action::{Action, ActionEvent};
use crate::session::{APP_ID_MAX, SessionState};

// ---------------------------------------------------------------------------
// periodic work
// ---------------------------------------------------------------------------

/// Fires at most once per `interval_ms` of a monotonic millisecond clock. A
/// caller that is late fires once (no catch-up burst): the next deadline is
/// `now + interval`, not `deadline + interval`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Periodic {
    interval_ms: u64,
    next_ms: u64,
}

impl Periodic {
    /// First firing is `interval_ms` after `now_ms`.
    pub const fn new(interval_ms: u64, now_ms: u64) -> Self {
        Self {
            interval_ms,
            next_ms: now_ms.saturating_add(interval_ms),
        }
    }

    pub fn due(&mut self, now_ms: u64) -> bool {
        if now_ms >= self.next_ms {
            self.next_ms = now_ms.saturating_add(self.interval_ms);
            true
        } else {
            false
        }
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }
}

/// Battery measurement period (X4: 3000 input ticks x 10 ms).
pub const BATTERY_INTERVAL_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// R20: is the restored session applicable
// ---------------------------------------------------------------------------

/// What the running firmware can show.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct RestoreEnv {
    /// App id 4 (Upload) exists only in a `wifi` build.
    pub upload_available: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RestoreReject {
    /// The saved stack contains the Upload app but this firmware has none (the
    /// X4 id mapper would silently turn it into a second Home).
    UploadNotAvailable,
    /// A book is on the saved stack but the file is not on this card.
    ReaderFileMissing,
}

/// Content check on top of `session::SessionState::validate` (which the decoder
/// already ran): the things only the running system knows. Reading position
/// bounds (chapter/page past the end of a changed book) are not checked here:
/// the reader clamps them against the book it opens.
pub fn check_restorable(
    state: &SessionState,
    env: RestoreEnv,
    mut file_exists: impl FnMut(&[u8]) -> bool,
) -> Result<(), RestoreReject> {
    let depth = (state.nav_depth as usize).min(state.nav_stack.len());
    let stack = &state.nav_stack[..depth];
    if !env.upload_available && stack.contains(&APP_ID_MAX) {
        return Err(RestoreReject::UploadNotAvailable);
    }
    if stack.contains(&crate::session::APP_READER) && !file_exists(state.reader_name()) {
        return Err(RestoreReject::ReaderFileMissing);
    }
    Ok(())
}

/// What happens to the saved session once the app layer tried to apply it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PostRestore {
    /// Leave both slot files alone (decision: not deleted after success; the
    /// next sleep writes a newer record anyway).
    Keep,
    /// Delete the slots and boot to Home: the record was valid but could not be
    /// applied, so it must not be retried on every boot.
    ClearAndBootNormally,
}

pub fn post_restore(applied: bool) -> PostRestore {
    if applied {
        PostRestore::Keep
    } else {
        PostRestore::ClearAndBootNormally
    }
}

// ---------------------------------------------------------------------------
// quick menu without a Menu key
// ---------------------------------------------------------------------------

/// What the app manager knows when an event arrives.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MenuKeyContext {
    pub reader_active: bool,
    pub quick_menu_open: bool,
    /// The reader is showing its table of contents (ENTER selects there).
    pub reader_showing_toc: bool,
}

/// `Action::Menu` has no key on the C61 (X4: Power short press). Audit result
/// (baseline T12): the quick menu holds Refresh and Go Home (both have another
/// route: refresh is always a full refresh on the C61, Back long press goes
/// Home), the reader's Book Font (also in Settings), Prev/Next Chapter (also
/// LEFT/RIGHT) and **Contents** (no other route), and Files' Delete File /
/// Delete Cache (destructive, deliberately NOT mapped). So the minimal mapping
/// is: a long press of ENTER (`Action::Select`) inside the reader opens the menu.
/// ENTER does nothing in a reader page (Select is unhandled there), so the short
/// press that precedes the long press is harmless; it is excluded in the TOC,
/// where ENTER selects an entry.
pub fn opens_quick_menu(ev: ActionEvent, ctx: MenuKeyContext) -> bool {
    matches!(ev, ActionEvent::LongPress(Action::Select))
        && ctx.reader_active
        && !ctx.quick_menu_open
        && !ctx.reader_showing_toc
}

// ---------------------------------------------------------------------------
// display failure recovery
// ---------------------------------------------------------------------------

/// Result of reporting a failed refresh attempt.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RefreshStep {
    /// Re-initialise the controller (software reset token) and try once more.
    ReinitAndRetry,
    /// Stop for this frame.
    GiveUp,
}

/// Tracks whether the panel content is stale because a refresh failed. A failed
/// full refresh leaves the controller uninitialised (`ssd1677::Epd` clears
/// `init_done`), so recovery always starts with `init`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayHealth {
    stale: bool,
    failed_frames: u32,
}

impl DisplayHealth {
    pub const fn new() -> Self {
        Self {
            stale: false,
            failed_frames: 0,
        }
    }

    /// A refresh attempt failed. `attempt` is 0 for the first try of a frame.
    pub fn on_failure(&mut self, attempt: u8) -> RefreshStep {
        if attempt == 0 {
            RefreshStep::ReinitAndRetry
        } else {
            self.stale = true;
            self.failed_frames = self.failed_frames.saturating_add(1);
            RefreshStep::GiveUp
        }
    }

    pub fn on_success(&mut self) {
        self.stale = false;
    }

    /// The panel shows old content because the last frame was given up.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    pub fn failed_frames(&self) -> u32 {
        self.failed_frames
    }

    /// Call on every input event: true once per stale period, meaning "force a
    /// full redraw now" (the app already consumed its redraw request for the
    /// frame that was given up). Asking on input and not on a timer keeps a dead
    /// panel from stalling the loop with back-to-back BUSY timeouts.
    pub fn redraw_on_input(&mut self) -> bool {
        core::mem::take(&mut self.stale)
    }
}

/// The two hardware operations of one refresh attempt (the kernel implements
/// them over the SSD1677 driver and the GPIO27 policy; tests use fakes).
pub trait Refresher {
    /// One full-refresh attempt; true = the panel shows the frame. The
    /// implementation logs the failure it saw.
    fn refresh(&mut self) -> bool;
    /// Controller re-init (software reset); true = ready for another attempt.
    fn reinit(&mut self) -> bool;
}

/// One frame with the recovery policy: attempt, and on failure re-init and retry
/// exactly once; a second failure (or a failed re-init) gives up on the frame
/// and leaves `health` stale. Never loops: at most two `refresh` calls.
pub fn run_refresh<R: Refresher>(health: &mut DisplayHealth, r: &mut R) -> bool {
    let mut attempt: u8 = 0;
    loop {
        if r.refresh() {
            health.on_success();
            return true;
        }
        match health.on_failure(attempt) {
            RefreshStep::GiveUp => return false,
            RefreshStep::ReinitAndRetry => {
                if !r.reinit() {
                    // nothing left to retry with
                    health.on_failure(1);
                    return false;
                }
                attempt = 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::session::{APP_READER, SessionState};

    fn env(upload: bool) -> RestoreEnv {
        RestoreEnv {
            upload_available: upload,
        }
    }

    fn reader_state(name: &[u8]) -> SessionState {
        let mut s = SessionState::home();
        s.nav_depth = 2;
        s.nav_stack = [0, APP_READER, 0, 0];
        s.set_reader_filename(name).unwrap();
        s
    }

    // ---- Periodic ----

    #[test]
    fn periodic_first_fires_one_interval_after_creation() {
        let mut p = Periodic::new(30_000, 1_000);
        assert!(!p.due(1_000));
        assert!(!p.due(30_999));
        assert!(p.due(31_000));
    }

    #[test]
    fn periodic_does_not_refire_inside_the_interval() {
        let mut p = Periodic::new(100, 0);
        assert!(p.due(100));
        assert!(!p.due(150));
        assert!(!p.due(199));
        assert!(p.due(200));
    }

    #[test]
    fn periodic_late_caller_fires_once_not_a_burst() {
        let mut p = Periodic::new(100, 0);
        // main loop was blocked for 5 s (full refresh): one firing, not 50
        assert!(p.due(5_000));
        assert!(!p.due(5_000));
        assert!(!p.due(5_099));
        assert!(p.due(5_100));
    }

    #[test]
    fn periodic_saturates_instead_of_wrapping_at_the_end_of_time() {
        let mut p = Periodic::new(100, u64::MAX - 10);
        assert!(!p.due(u64::MAX - 1));
        assert!(p.due(u64::MAX));
        assert!(!p.due(0));
    }

    #[test]
    fn battery_period_is_30_seconds_like_the_x4() {
        assert_eq!(BATTERY_INTERVAL_MS, 30_000);
    }

    // ---- check_restorable ----

    #[test]
    fn r20_home_only_session_is_always_restorable() {
        let s = SessionState::home();
        assert_eq!(check_restorable(&s, env(false), |_| false), Ok(()));
    }

    #[test]
    fn r20_reader_session_needs_the_book_on_this_card() {
        let s = reader_state(b"BOOK.EPUB");
        let mut asked: Option<std::vec::Vec<u8>> = None;
        let r = check_restorable(&s, env(false), |n| {
            asked = Some(n.to_vec());
            true
        });
        assert_eq!(r, Ok(()));
        assert_eq!(asked.as_deref(), Some(&b"BOOK.EPUB"[..]));
        assert_eq!(
            check_restorable(&s, env(false), |_| false),
            Err(RestoreReject::ReaderFileMissing)
        );
    }

    #[test]
    fn r20_file_check_only_runs_when_the_reader_is_on_the_stack() {
        let mut s = SessionState::home();
        s.nav_depth = 2;
        s.nav_stack = [0, 1, 0, 0]; // Home -> Files
        assert_eq!(
            check_restorable(&s, env(false), |_| panic!("no file lookup expected")),
            Ok(())
        );
    }

    #[test]
    fn r20_upload_on_the_stack_is_rejected_by_offline_firmware() {
        let mut s = SessionState::home();
        s.nav_depth = 2;
        s.nav_stack = [0, 4, 0, 0];
        assert_eq!(
            check_restorable(&s, env(false), |_| true),
            Err(RestoreReject::UploadNotAvailable)
        );
        assert_eq!(check_restorable(&s, env(true), |_| true), Ok(()));
    }

    #[test]
    fn r20_only_the_live_part_of_the_stack_counts() {
        // stale entries above nav_depth must not reject (validate() zeroes them
        // anyway, this pins the slice bound)
        let mut s = SessionState::home();
        s.nav_depth = 1;
        s.nav_stack = [0, 4, 2, 0];
        assert_eq!(check_restorable(&s, env(false), |_| false), Ok(()));
    }

    #[test]
    fn r20_reader_below_another_app_is_checked_too() {
        let mut s = reader_state(b"A.TXT");
        s.nav_depth = 3;
        s.nav_stack = [0, APP_READER, 3, 0]; // Home -> Reader -> Settings
        assert_eq!(
            check_restorable(&s, env(false), |_| false),
            Err(RestoreReject::ReaderFileMissing)
        );
    }

    // ---- post_restore ----

    #[test]
    fn r20_session_is_kept_after_a_successful_apply() {
        assert_eq!(post_restore(true), PostRestore::Keep);
    }

    #[test]
    fn r20_session_is_cleared_when_applying_fails_so_it_cannot_loop() {
        assert_eq!(post_restore(false), PostRestore::ClearAndBootNormally);
    }

    // ---- quick menu mapping ----

    fn menu_ctx() -> MenuKeyContext {
        MenuKeyContext {
            reader_active: true,
            quick_menu_open: false,
            reader_showing_toc: false,
        }
    }

    #[test]
    fn menu_enter_long_press_in_the_reader_opens_the_menu() {
        assert!(opens_quick_menu(
            ActionEvent::LongPress(Action::Select),
            menu_ctx()
        ));
    }

    #[test]
    fn menu_only_a_long_press_of_select_counts() {
        for ev in [
            ActionEvent::Press(Action::Select),
            ActionEvent::Release(Action::Select),
            ActionEvent::Repeat(Action::Select),
            ActionEvent::LongPress(Action::Back),
            ActionEvent::LongPress(Action::Next),
            ActionEvent::LongPress(Action::NextJump),
            ActionEvent::LongPress(Action::Menu),
        ] {
            assert!(!opens_quick_menu(ev, menu_ctx()), "{:?}", ev);
        }
    }

    #[test]
    fn menu_never_opens_outside_the_reader() {
        // Files/Home/Settings: ENTER already acted on the press (open item)
        let ctx = MenuKeyContext {
            reader_active: false,
            ..menu_ctx()
        };
        assert!(!opens_quick_menu(
            ActionEvent::LongPress(Action::Select),
            ctx
        ));
    }

    #[test]
    fn menu_does_not_reopen_while_open_or_in_the_toc() {
        let open = MenuKeyContext {
            quick_menu_open: true,
            ..menu_ctx()
        };
        let toc = MenuKeyContext {
            reader_showing_toc: true,
            ..menu_ctx()
        };
        let ev = ActionEvent::LongPress(Action::Select);
        assert!(!opens_quick_menu(ev, open));
        assert!(!opens_quick_menu(ev, toc));
    }

    // ---- DisplayHealth ----

    #[test]
    fn display_first_failure_retries_after_reinit() {
        let mut h = DisplayHealth::new();
        assert_eq!(h.on_failure(0), RefreshStep::ReinitAndRetry);
        assert!(!h.is_stale(), "one failed attempt alone is not yet stale");
    }

    #[test]
    fn display_second_failure_gives_up_and_marks_the_frame_stale() {
        let mut h = DisplayHealth::new();
        assert_eq!(h.on_failure(0), RefreshStep::ReinitAndRetry);
        assert_eq!(h.on_failure(1), RefreshStep::GiveUp);
        assert!(h.is_stale());
        assert_eq!(h.failed_frames(), 1);
    }

    #[test]
    fn display_stale_frame_is_redrawn_once_on_the_next_input() {
        let mut h = DisplayHealth::new();
        h.on_failure(1);
        assert!(h.redraw_on_input());
        assert!(!h.redraw_on_input(), "only once per stale period");
        assert!(!h.is_stale());
    }

    #[test]
    fn display_success_clears_stale_and_input_asks_for_nothing() {
        let mut h = DisplayHealth::new();
        h.on_failure(1);
        h.on_success();
        assert!(!h.is_stale());
        assert!(!h.redraw_on_input());
    }

    // ---- run_refresh ----

    struct FakeRefresher {
        // result of each refresh attempt, in order; the last one repeats
        results: std::vec::Vec<bool>,
        reinit_ok: bool,
        refreshes: u32,
        reinits: u32,
    }

    impl FakeRefresher {
        fn new(results: &[bool], reinit_ok: bool) -> Self {
            Self {
                results: results.to_vec(),
                reinit_ok,
                refreshes: 0,
                reinits: 0,
            }
        }
    }

    impl Refresher for FakeRefresher {
        fn refresh(&mut self) -> bool {
            let i = (self.refreshes as usize).min(self.results.len() - 1);
            self.refreshes += 1;
            self.results[i]
        }
        fn reinit(&mut self) -> bool {
            self.reinits += 1;
            self.reinit_ok
        }
    }

    #[test]
    fn r11_refresh_success_needs_no_reinit() {
        let mut h = DisplayHealth::new();
        let mut r = FakeRefresher::new(&[true], true);
        assert!(run_refresh(&mut h, &mut r));
        assert_eq!((r.refreshes, r.reinits), (1, 0));
        assert!(!h.is_stale());
    }

    #[test]
    fn r11_refresh_failure_reinits_and_retries_once_then_shows() {
        let mut h = DisplayHealth::new();
        let mut r = FakeRefresher::new(&[false, true], true);
        assert!(run_refresh(&mut h, &mut r));
        assert_eq!((r.refreshes, r.reinits), (2, 1));
        assert!(!h.is_stale());
        assert_eq!(h.failed_frames(), 0);
    }

    #[test]
    fn r11_refresh_stuck_panel_gives_up_after_exactly_two_attempts() {
        let mut h = DisplayHealth::new();
        let mut r = FakeRefresher::new(&[false], true); // BUSY never releases
        assert!(!run_refresh(&mut h, &mut r));
        assert_eq!(
            (r.refreshes, r.reinits),
            (2, 1),
            "bounded: no endless retry"
        );
        assert!(h.is_stale());
        assert_eq!(h.failed_frames(), 1);
    }

    #[test]
    fn r11_failed_reinit_stops_without_a_second_refresh() {
        let mut h = DisplayHealth::new();
        let mut r = FakeRefresher::new(&[false], false);
        assert!(!run_refresh(&mut h, &mut r));
        assert_eq!((r.refreshes, r.reinits), (1, 1));
        assert!(h.is_stale());
    }

    #[test]
    fn r11_given_up_frame_is_redrawn_on_the_next_input_and_can_recover() {
        let mut h = DisplayHealth::new();
        let mut dead = FakeRefresher::new(&[false], true);
        assert!(!run_refresh(&mut h, &mut dead));
        assert!(h.redraw_on_input());
        let mut ok = FakeRefresher::new(&[true], true);
        assert!(run_refresh(&mut h, &mut ok));
        assert!(!h.is_stale());
    }

    #[test]
    fn display_failed_frame_count_saturates_and_survives_success() {
        let mut h = DisplayHealth::new();
        h.on_failure(1);
        h.on_success();
        h.on_failure(1);
        assert_eq!(h.failed_frames(), 2);
    }
}
