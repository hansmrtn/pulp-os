// R21 / R20 seam: reader position -> C61 session record (real pulp-board-logic
// save/restore, real lifecycle decisions) -> reader reopened on the rebooted card.
// Also the "bookmarks and session do not interfere" contract.
use pulp_board_logic::lifecycle::{PostRestore, RestoreEnv, RestoreReject, check_restorable, post_restore};
use pulp_board_logic::power::{DelayMs, PeripheralPower, RailPin};
use pulp_board_logic::session::{
    self, APP_HOME, APP_READER, BootDecision, NormalBootReason, SLOT_A_FILE, SLOT_B_FILE, SessionState, SessionStore,
    Slot, StoreError, clear_session, restore_session, save_session,
};
use pulp_kernel::drivers::sdcard::{FakeFs, SdStorage};
use pulp_os_host::board::action::Action;
use pulp_os_host::fixtures::*;
use pulp_os_host::kernel::bookmarks::BOOKMARK_FILE;
use pulp_os_host::rig::*;

struct NoDelay;
impl DelayMs for NoDelay {
    fn delay_ms(&mut self, _ms: u32) {}
}
struct Pin;
impl RailPin for Pin {
    fn set_high(&mut self) {}
    fn set_low(&mut self) {}
}

// kernel/src/board_c61/session.rs `SdSessionStore` over drivers::storage, here over the card shim
struct CardStore<'a>(&'a SdStorage);
fn slot_file(s: Slot) -> &'static str {
    match s {
        Slot::A => SLOT_A_FILE,
        Slot::B => SLOT_B_FILE,
    }
}
impl SessionStore for CardStore<'_> {
    fn read(&mut self, slot: Slot, buf: &mut [u8]) -> Result<usize, StoreError> {
        if !self.0.is_mounted() {
            return Err(StoreError::NoCard);
        }
        match self.0.with_fs(|fs| fs.get("_PULP", slot_file(slot)).cloned()).flatten() {
            None => Err(StoreError::NotFound),
            Some(d) => {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                Ok(n)
            }
        }
    }
    fn write(&mut self, slot: Slot, data: &[u8]) -> Result<(), StoreError> {
        self.0.with_fs(|fs| fs.put("_PULP", slot_file(slot), data)).ok_or(StoreError::NoCard)
    }
    fn delete(&mut self, slot: Slot) -> Result<(), StoreError> {
        self.0.with_fs(|fs| fs.remove("_PULP", slot_file(slot))).map(|_| ()).ok_or(StoreError::NoCard)
    }
}

fn sd_active() -> PeripheralPower<Pin> {
    let mut p = PeripheralPower::new(Pin);
    p.power_cycle(&mut NoDelay).unwrap();
    let permit = p.begin_sd_init().unwrap();
    p.finish_sd_init(permit, true);
    p
}

const FONT: u8 = 2;
const THEME: u8 = 1;
const ENV: RestoreEnv = RestoreEnv { upload_available: false };

fn book(name: &str) -> Vec<u8> {
    if name.ends_with(".EPUB") {
        english_epub(&EpubSpec::default())
    } else {
        english_txt(40_000, Eol::Lf, 1)
    }
}

fn reading(name: &str, turns: &[Action]) -> Rig {
    let mut r = Rig::with_book(name, &book(name));
    r.configure(FONT, THEME);
    r.open(name);
    for &a in turns {
        r.press(a);
    }
    r
}

// AppManager::collect_session, reader part (accessors -> record fields)
fn collect(r: &Rig, name: &str) -> SessionState {
    let mut s = SessionState::home();
    s.nav_depth = 2;
    s.nav_stack = [APP_HOME, APP_READER, 0, 0];
    s.set_reader_filename(name.as_bytes()).unwrap();
    s.reader_is_epub = r.app.is_epub();
    s.reader_chapter = r.app.chapter();
    s.reader_page = r.app.page() as u16;
    s.reader_byte_offset = r.app.byte_offset();
    s.reader_font_size = r.app.font_size_idx();
    s
}

// the sleep sequence: bookmark flush, then session save while the SD rail is up
fn sleep(r: &mut Rig, name: &str) -> FakeFs {
    r.save_position();
    r.k.bookmarks_flush();
    let power = sd_active();
    let state = collect(r, name);
    save_session(&power, &mut CardStore(r.k.sd()), &state).expect("session saved");
    r.k.sd().snapshot().unwrap()
}

// Kernel::boot (C61) + AppManager::apply_session, reader part; returns the reopened rig
// or None when the session was not applied (normal boot to Home)
fn wake(card: FakeFs, name: &str) -> (Rig, Option<PostRestore>, FakeFs) {
    let mut r2 = Rig::new(card);
    let power = sd_active();
    let decision = restore_session(&power, &mut CardStore(r2.k.sd()));
    let mut post = None;
    if let BootDecision::Restore(res) = decision {
        let st = res.state;
        let exists = {
            let sd = r2.k.sd();
            let names = st.reader_name().to_vec();
            sd.with_fs(|fs| fs.get("", std::str::from_utf8(&names).unwrap()).is_some()).unwrap()
        };
        let applied = check_restorable(&st, ENV, |_| exists).is_ok();
        if applied {
            r2.app.restore_state(st.reader_name(), st.reader_is_epub, st.reader_chapter, st.reader_page as usize, st.reader_byte_offset, st.reader_font_size);
            r2.configure(FONT, THEME);
            r2.open(name);
        }
        post = Some(post_restore(applied));
        if post == Some(PostRestore::ClearAndBootNormally) {
            clear_session(&power, &mut CardStore(r2.k.sd())).unwrap();
        }
    }
    let snap = r2.k.sd().snapshot().unwrap();
    (r2, post, snap)
}

#[test]
fn txt_session_restores_the_same_page_and_text() {
    let name = "BOOK.TXT";
    let mut r = reading(name, &[Action::Next; 7]);
    let want = r.pos();
    assert_eq!(want.page, 7);
    let card = sleep(&mut r, name);
    let (r2, post, _) = wake(card, name);
    assert_eq!(post, Some(PostRestore::Keep));
    assert_eq!(r2.pos(), want);
    assert_eq!(r2.render_hash(), r.render_hash(), "identical pixels after wake");
}

#[test]
fn epub_session_restores_chapter_page_and_text() {
    let name = "BOOK.EPUB";
    let mut turns = vec![Action::NextJump, Action::NextJump];
    turns.extend([Action::Next; 3]);
    let mut r = reading(name, &turns);
    let want = r.pos();
    assert_eq!((want.chapter, want.page), (2, 3));
    let card = sleep(&mut r, name);
    let (r2, post, _) = wake(card, name);
    assert_eq!(post, Some(PostRestore::Keep));
    assert_eq!(r2.pos(), want);
    assert_eq!(r2.render_hash(), r.render_hash());
}

#[test]
fn corrupt_session_boots_normally_and_the_book_resumes_from_its_bookmark() {
    let name = "BOOK.TXT";
    let mut r = reading(name, &[Action::Next; 5]);
    let want = r.pos();
    let mut card = sleep(&mut r, name);
    // damage both slots (flip a byte in each)
    for f in [SLOT_A_FILE, SLOT_B_FILE] {
        if let Some(d) = card.get("_PULP", f).cloned() {
            let mut d = d;
            d[20] ^= 0xFF;
            card.put("_PULP", f, &d);
        }
    }
    let power = sd_active();
    let mut probe_card = Rig::new(card.clone());
    match restore_session(&power, &mut CardStore(probe_card.k.sd())) {
        BootDecision::NormalBoot(NormalBootReason::Corrupt(_)) => {}
        other => panic!("expected a normal boot, got {other:?}"),
    }
    // normal boot = Home; the user opens the book again: it resumes from the bookmark
    let (mut r2, post, _) = wake(card, name);
    assert_eq!(post, None, "no session applied");
    r2.configure(FONT, THEME);
    r2.open(name);
    assert_eq!(r2.pos(), want, "the bookmark is independent of the session record");
}

#[test]
fn corrupt_session_and_no_bookmark_starts_from_page_one() {
    let name = "BOOK.TXT";
    let mut r = reading(name, &[Action::Next; 5]);
    let mut card = sleep(&mut r, name);
    card.remove("_PULP", BOOKMARK_FILE);
    for f in [SLOT_A_FILE, SLOT_B_FILE] {
        card.remove("_PULP", f);
    }
    card.put("_PULP", SLOT_A_FILE, &[0xFF; 80]);
    let (mut r2, post, _) = wake(card, name);
    assert_eq!(post, None);
    r2.configure(FONT, THEME);
    r2.open(name);
    assert_eq!(r2.page(), 0);
}

#[test]
fn a_session_for_a_missing_book_is_cleared_and_leaves_bookmarks_alone() {
    let name = "BOOK.TXT";
    let mut r = reading(name, &[Action::Next; 3]);
    let mut card = sleep(&mut r, name);
    card.remove("", name); // the book is no longer on the card
    let bm_before = card.get("_PULP", BOOKMARK_FILE).cloned();
    let (_, post, snap) = wake(card, name);
    assert_eq!(post, Some(PostRestore::ClearAndBootNormally));
    assert!(snap.get("_PULP", SLOT_A_FILE).is_none() && snap.get("_PULP", SLOT_B_FILE).is_none());
    assert_eq!(snap.get("_PULP", BOOKMARK_FILE).cloned(), bm_before, "bookmarks untouched");
    // and the policy function itself
    let mut st = SessionState::home();
    st.nav_depth = 2;
    st.nav_stack = [APP_HOME, APP_READER, 0, 0];
    st.set_reader_filename(b"GONE.TXT").unwrap();
    assert_eq!(check_restorable(&st, ENV, |_| false), Err(RestoreReject::ReaderFileMissing));
}

#[test]
fn saving_a_session_does_not_touch_bookmarks_and_vice_versa() {
    let name = "BOOK.TXT";
    let mut r = reading(name, &[Action::Next; 2]);
    r.save_position();
    r.k.bookmarks_flush();
    let bm = r.k.sd().with_fs(|fs| fs.get("_PULP", BOOKMARK_FILE).cloned()).flatten().unwrap();
    // session saves: two saves alternate slots, bookmarks bytes stay identical
    let power = sd_active();
    let state = collect(&r, name);
    for _ in 0..3 {
        save_session(&power, &mut CardStore(r.k.sd()), &state).unwrap();
    }
    let bm2 = r.k.sd().with_fs(|fs| fs.get("_PULP", BOOKMARK_FILE).cloned()).flatten().unwrap();
    assert_eq!(bm, bm2);
    // bookmark flushes leave the session slots alone
    let a = r.k.sd().with_fs(|fs| fs.get("_PULP", SLOT_A_FILE).cloned()).flatten().unwrap();
    let b = r.k.sd().with_fs(|fs| fs.get("_PULP", SLOT_B_FILE).cloned()).flatten().unwrap();
    r.press(Action::Next);
    r.save_position();
    r.k.bookmarks_flush();
    assert_eq!(r.k.sd().with_fs(|fs| fs.get("_PULP", SLOT_A_FILE).cloned()).flatten().unwrap(), a);
    assert_eq!(r.k.sd().with_fs(|fs| fs.get("_PULP", SLOT_B_FILE).cloned()).flatten().unwrap(), b);
    assert_eq!(session::RECORD_LEN, 80);
}
