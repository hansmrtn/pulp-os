// R21 / navigation: page turns, chapter jumps, boundaries, TOC, quick actions,
// position restore and Back, on the real ReaderApp (same tests run on the pre-port code).
use pulp_os_host::apps::probe::{self, Phase};
use pulp_os_host::apps::{App, Transition};
use pulp_os_host::board::action::{Action, ActionEvent};
use pulp_os_host::fixtures::*;
use pulp_os_host::rig::*;

fn txt(data: &[u8], font: u8, theme: u8) -> Rig {
    let mut r = Rig::with_book("BOOK.TXT", data);
    r.configure(font, theme);
    r.open("BOOK.TXT");
    r
}

fn epub(spec: &EpubSpec) -> Rig {
    let mut r = Rig::with_book("BOOK.EPUB", &english_epub(spec));
    r.configure(2, 1);
    r.open("BOOK.EPUB");
    r
}

fn corpus() -> Vec<u8> {
    english_txt(40_000, Eol::Lf, 1)
}

const QA_FONT: u8 = 1;
const QA_PREV_CH: u8 = 3;
const QA_NEXT_CH: u8 = 4;
const QA_TOC: u8 = 5;

fn qa_ids(r: &Rig) -> Vec<u8> {
    r.app.quick_actions().iter().map(|a| a.id).collect()
}

// ---------------------------------------------------------------- TXT

#[test]
fn txt_forward_then_back_visits_the_same_pages() {
    let mut r = txt(&corpus(), 2, 1);
    let fwd = r.walk_forward();
    assert_eq!(fwd.len(), 61);
    let mut back = r.walk_back();
    back.reverse();
    assert_eq!(fwd, back, "same pages, same text, in reverse order");
    assert_eq!(r.page(), 0);
}

#[test]
fn txt_boundaries_are_no_ops() {
    let mut r = txt(&corpus(), 2, 1);
    let first = r.pos();
    assert_eq!(r.press(Action::Prev), Transition::None);
    assert_eq!(r.pos(), first, "Prev on the first page stays");
    assert_eq!(probe::phase(&r.app), Phase::Ready);
    assert_eq!(r.press(Action::PrevJump), Transition::None);
    assert_eq!(r.pos(), first, "PrevJump on the first page stays");

    r.walk_forward();
    let last = r.pos();
    assert_eq!(last.page, 60);
    assert_eq!(r.press(Action::Next), Transition::None);
    assert_eq!(r.pos(), last, "Next on the last page stays");
    assert_eq!(r.press(Action::NextJump), Transition::None);
    assert_eq!(r.pos(), last, "NextJump on the last page stays");
    assert_eq!(probe::phase(&r.app), Phase::Ready);
}

#[test]
fn txt_jump_moves_ten_pages_and_clamps() {
    let mut r = txt(&corpus(), 2, 1);
    r.walk_forward(); // index everything: 61 pages
    for _ in 0..60 {
        r.press(Action::Prev);
    }
    assert_eq!(r.page(), 0);
    r.press(Action::NextJump);
    assert_eq!(r.page(), 10);
    r.press(Action::NextJump);
    assert_eq!(r.page(), 20);
    r.press(Action::PrevJump);
    assert_eq!(r.page(), 10);
    r.press(Action::PrevJump);
    assert_eq!(r.page(), 0);
    r.press(Action::PrevJump);
    assert_eq!(r.page(), 0);
    for _ in 0..5 {
        r.press(Action::NextJump);
    }
    assert_eq!(r.page(), 50);
    r.press(Action::NextJump);
    assert_eq!(r.page(), 60, "jump clamps to the last page (50 + 10 > 60)");
    r.press(Action::PrevJump);
    assert_eq!(r.page(), 50);
}

#[test]
fn txt_long_press_jumps_to_indexed_ends_and_repeat_pages() {
    let mut r = txt(&corpus(), 2, 1);
    r.walk_forward();
    for _ in 0..10 {
        r.press(Action::Prev);
    }
    assert_eq!(r.page(), 50);
    r.long_press(Action::NextJump);
    assert_eq!(r.page(), 60, "LongPress(NextJump): last page of the indexed range");
    r.long_press(Action::PrevJump);
    assert_eq!(r.page(), 0, "LongPress(PrevJump): first page");
    // Repeat(Next) pages exactly like Press(Next)
    r.event(ActionEvent::Repeat(Action::Next));
    assert_eq!(r.page(), 1);
    r.event(ActionEvent::Repeat(Action::Prev));
    assert_eq!(r.page(), 0);
    // LongPress(Next/Prev) also turns the page (and shows the position overlay)
    r.long_press(Action::Next);
    assert_eq!(r.page(), 1);
    r.long_press(Action::Prev);
    assert_eq!(r.page(), 0);
}

#[test]
fn back_pops_and_long_back_goes_home() {
    let mut r = txt(&corpus(), 2, 1);
    assert_eq!(r.press(Action::Back), Transition::Pop);
    assert_eq!(r.long_press(Action::Back), Transition::Home);
    // neither moves the page
    assert_eq!(r.page(), 0);
    // Select / Menu on a reading page do nothing in the reader itself
    assert_eq!(r.press(Action::Select), Transition::None);
    assert_eq!(r.long_press(Action::Select), Transition::None);
    assert_eq!(r.page(), 0);
}

#[test]
fn txt_position_restores_to_the_page_containing_the_bookmark_offset() {
    let data = corpus();
    let mut r = txt(&data, 2, 1);
    r.walk_forward();
    let offs = probe::offsets(&r.app);
    let name = b"BOOK.TXT";
    for (target_page, delta) in [(5usize, 0i64), (5, 1), (5, -1), (30, 0), (60, 0)] {
        let off = (offs[target_page] as i64 + delta) as u32;
        r.exit();
        r.k.bookmarks().save(name, off, 0);
        r.open("BOOK.TXT");
        let want = if delta < 0 { target_page - 1 } else { target_page };
        assert_eq!(r.page(), want, "offset {off} (page {target_page}{delta:+})");
        assert_eq!(r.pos().offset, offs[want]);
    }
    // an offset past the end lands on the last page; offset 0 on the first
    r.exit();
    r.k.bookmarks().save(name, 10_000_000, 0);
    r.open("BOOK.TXT");
    assert_eq!(r.page(), 60);
    r.exit();
    r.k.bookmarks().save(name, 0, 0);
    r.open("BOOK.TXT");
    assert_eq!(r.page(), 0);
}

// ---------------------------------------------------------------- EPUB

#[test]
fn epub_next_crosses_into_the_next_chapter_and_prev_comes_back_to_its_last_page() {
    let mut r = epub(&EpubSpec::default());
    assert_eq!((r.app.chapter(), r.page()), (0, 0));
    let ch0_pages = probe::total_pages(&r.app);
    assert_eq!(ch0_pages, 10);
    for _ in 0..ch0_pages - 1 {
        r.press(Action::Next);
    }
    assert_eq!((r.app.chapter(), r.page()), (0, 9));
    let last_of_ch0 = r.pos();
    r.press(Action::Next);
    assert_eq!((r.app.chapter(), r.page()), (1, 0), "Next on a chapter's last page enters the next chapter");
    r.press(Action::Prev);
    assert_eq!((r.app.chapter(), r.page()), (0, 9), "Prev on page 0 enters the previous chapter's last page");
    assert_eq!(r.pos(), last_of_ch0);
}

#[test]
fn epub_book_boundaries_are_no_ops() {
    let mut r = epub(&EpubSpec::default());
    let first = r.pos();
    assert_eq!(r.press(Action::Prev), Transition::None);
    assert_eq!(r.pos(), first, "Prev at the very first page");
    assert_eq!(r.press(Action::PrevJump), Transition::None);
    assert_eq!(r.pos(), first, "PrevJump in chapter 0");
    let all = r.walk_forward();
    assert_eq!(all.len(), 35);
    let last = r.pos();
    assert_eq!((last.chapter, last.page), (3, 8));
    assert_eq!(r.press(Action::Next), Transition::None);
    assert_eq!(r.press(Action::NextJump), Transition::None);
    assert_eq!(r.pos(), last, "end of the book");
    assert_eq!(probe::phase(&r.app), Phase::Ready);
}

#[test]
fn epub_jump_goes_to_next_and_previous_chapter_first_page() {
    let mut r = epub(&EpubSpec::default());
    r.press(Action::Next);
    r.press(Action::Next);
    assert_eq!((r.app.chapter(), r.page()), (0, 2));
    r.press(Action::NextJump);
    assert_eq!((r.app.chapter(), r.page()), (1, 0));
    r.press(Action::Next);
    r.press(Action::NextJump);
    assert_eq!((r.app.chapter(), r.page()), (2, 0));
    r.press(Action::PrevJump);
    assert_eq!((r.app.chapter(), r.page()), (1, 0), "PrevJump: previous chapter, first page");
    r.press(Action::PrevJump);
    assert_eq!((r.app.chapter(), r.page()), (0, 0));
    r.press(Action::PrevJump);
    assert_eq!((r.app.chapter(), r.page()), (0, 0));
    // long press: end / start of the current chapter (chapters are fully indexed)
    r.long_press(Action::NextJump);
    assert_eq!((r.app.chapter(), r.page()), (0, 9));
    r.long_press(Action::PrevJump);
    assert_eq!((r.app.chapter(), r.page()), (0, 0));
}

#[test]
fn epub_chapter_text_is_the_same_whether_reached_by_paging_or_jumping() {
    let mut a = epub(&EpubSpec::default());
    let by_paging = {
        let mut seen = None;
        loop {
            if a.app.chapter() == 2 {
                seen = Some(a.pos());
                break;
            }
            a.press(Action::Next);
        }
        seen.unwrap()
    };
    let mut b = epub(&EpubSpec::default());
    b.press(Action::NextJump);
    b.press(Action::NextJump);
    assert_eq!(b.pos(), by_paging);
}

#[test]
fn epub_quick_actions_list_depends_on_the_book() {
    let r = epub(&EpubSpec::default());
    assert_eq!(qa_ids(&r), vec![QA_FONT, QA_PREV_CH, QA_NEXT_CH, QA_TOC]);
    let r = epub(&EpubSpec { toc: TocKind::None, ..Default::default() });
    assert_eq!(qa_ids(&r), vec![QA_FONT, QA_PREV_CH, QA_NEXT_CH]);
    let r = epub(&EpubSpec { chapters: 1, ..Default::default() });
    assert_eq!(qa_ids(&r), vec![QA_FONT, QA_TOC], "single-chapter book: no chapter actions");
    let r = txt(&corpus(), 2, 1);
    assert_eq!(qa_ids(&r), vec![QA_FONT], "TXT: font only");
}

#[test]
fn epub_quick_prev_next_chapter_actions() {
    let mut r = epub(&EpubSpec::default());
    r.app.on_quick_trigger(QA_NEXT_CH, &mut r.ctx);
    r.settle();
    assert_eq!((r.app.chapter(), r.page()), (1, 0));
    r.app.on_quick_trigger(QA_NEXT_CH, &mut r.ctx);
    r.settle();
    r.app.on_quick_trigger(QA_NEXT_CH, &mut r.ctx);
    r.settle();
    assert_eq!(r.app.chapter(), 3);
    r.app.on_quick_trigger(QA_NEXT_CH, &mut r.ctx);
    r.settle();
    assert_eq!(r.app.chapter(), 3, "no chapter after the last");
    r.app.on_quick_trigger(QA_PREV_CH, &mut r.ctx);
    r.settle();
    assert_eq!((r.app.chapter(), r.page()), (2, 0));
    for _ in 0..5 {
        r.app.on_quick_trigger(QA_PREV_CH, &mut r.ctx);
        r.settle();
    }
    assert_eq!(r.app.chapter(), 0, "no chapter before the first");
}

#[test]
fn epub_toc_overlay_select_jumps_to_the_chapter() {
    let mut r = epub(&EpubSpec::default());
    r.press(Action::NextJump); // chapter 1
    r.app.on_quick_trigger(QA_TOC, &mut r.ctx);
    assert_eq!(probe::phase(&r.app), Phase::Toc);
    assert_eq!(probe::toc_selected(&r.app), 1, "opens on the current chapter's entry");
    r.press(Action::Next);
    r.press(Action::Next);
    assert_eq!(probe::toc_selected(&r.app), 3);
    r.press(Action::Next);
    assert_eq!(probe::toc_selected(&r.app), 0, "selection wraps");
    r.press(Action::Prev);
    assert_eq!(probe::toc_selected(&r.app), 3, "and wraps backwards");
    r.press(Action::Prev);
    assert_eq!(probe::toc_selected(&r.app), 2);
    // Back closes the overlay without leaving the page
    assert_eq!(r.press(Action::Back), Transition::None);
    assert_eq!(probe::phase(&r.app), Phase::Ready);
    assert_eq!(r.app.chapter(), 1);
    // reopen, select entry 2 with Select
    r.app.on_quick_trigger(QA_TOC, &mut r.ctx);
    r.press(Action::Next);
    assert_eq!(probe::toc_selected(&r.app), 2);
    r.press(Action::Select);
    assert_eq!(probe::phase(&r.app), Phase::Ready);
    assert_eq!((r.app.chapter(), r.page()), (2, 0));
    // NextJump also selects (X4 right-hand key)
    r.app.on_quick_trigger(QA_TOC, &mut r.ctx);
    r.press(Action::Next);
    r.press(Action::NextJump);
    assert_eq!((r.app.chapter(), r.page()), (3, 0));
}

#[test]
fn epub_background_caching_does_not_change_the_pages() {
    let walk = |idle: usize| {
        let mut r = epub(&EpubSpec::default());
        r.idle(idle);
        r.walk_forward()
    };
    let cold = walk(0);
    let warm = walk(40);
    assert_eq!(cold, warm);
    let mut r = epub(&EpubSpec::default());
    r.idle(40);
    assert_eq!(probe::chapter_cached_count(&r.app), probe::spine_len(&r.app), "idle time caches every chapter");
}

#[test]
fn epub_reopen_uses_the_chapter_cache_and_gives_identical_pages() {
    let mut r = epub(&EpubSpec::default());
    let first = r.walk_forward();
    r.idle(40);
    r.exit();
    // the cache file now exists on the card; reopening goes through check_cache() == hit
    let cache_files: Vec<_> = r.k.sd().with_fs(|fs| fs.names()).unwrap();
    assert!(cache_files.iter().any(|n| n.starts_with("_PULP/") && !n.ends_with("BKMK.BIN")), "{cache_files:?}");
    r.open("BOOK.EPUB");
    let second = r.walk_forward();
    assert_eq!(first, second);
}

#[test]
fn epub_bookmark_restores_chapter_and_page_and_clamps_bad_values() {
    let name = b"BOOK.EPUB";
    let mut r = epub(&EpubSpec::default());
    // go to chapter 2, page 3 and bookmark it
    r.press(Action::NextJump);
    r.press(Action::NextJump);
    for _ in 0..3 {
        r.press(Action::Next);
    }
    let want = r.pos();
    assert_eq!((want.chapter, want.page), (2, 3));
    r.save_position();
    r.exit();
    r.open("BOOK.EPUB");
    assert_eq!(r.pos(), want, "chapter + page restored from the bookmark");

    // chapter past the end of the spine clamps to the last chapter
    r.exit();
    r.k.bookmarks().save(name, 0, 99);
    r.open("BOOK.EPUB");
    assert_eq!((r.app.chapter(), r.page()), (3, 0));
    // an offset beyond the chapter's text lands on its last page
    r.exit();
    r.k.bookmarks().save(name, 10_000_000, 1);
    r.open("BOOK.EPUB");
    assert_eq!((r.app.chapter(), r.page()), (1, 6));
}

#[test]
fn save_position_only_writes_while_a_page_is_showing() {
    let mut r = Rig::with_book("BOOK.TXT", &corpus());
    r.configure(2, 1);
    r.app.save_position(r.k.bookmarks());
    assert!(!r.k.bookmarks().is_dirty(), "nothing open: no bookmark");
    r.open("BOOK.TXT");
    r.press(Action::Next);
    r.save_position();
    assert!(r.k.bookmarks().is_dirty());
    let slot = r.k.bookmarks().find(b"BOOK.TXT").unwrap();
    assert_eq!(slot.byte_offset, r.pos().offset);
    assert_eq!(slot.chapter, 0);
}
