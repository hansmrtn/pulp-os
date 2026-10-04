// R21 / pagination: the real ReaderApp lays out English TXT and EPUB.
// Expected numbers marked "baseline" were produced by the pre-port commit (the same
// tests run against it via `scripts/check-reader-regression.sh head`).
use pulp_os_host::apps::probe::{self, Phase};
use pulp_os_host::fixtures::*;
use pulp_os_host::rig::*;
use pulp_kernel::drivers::sdcard::SdStorage;
use pulp_os_host::board::action::Action;

fn txt_rig(data: &[u8], font: u8, theme: u8) -> Rig {
    let mut r = Rig::with_book("BOOK.TXT", data);
    r.configure(font, theme);
    r.open("BOOK.TXT");
    r
}

fn epub_rig(spec: &EpubSpec, font: u8, theme: u8) -> Rig {
    let mut r = Rig::with_book("BOOK.EPUB", &english_epub(spec));
    r.configure(font, theme);
    r.open("BOOK.EPUB");
    r
}

// (max_lines, line_h, text_w, margin, text_y, text_area_h) per (font, theme); baseline.
// theme: 0 Compact (8 px), 1 Default (16), 2 Relaxed (24), 3 Spacious (40); 480 px screen.
const GEOMETRY: [[(usize, u16, u32, u16, u16, u16); 4]; 5] = [
    [(35, 22, 464, 8, 24, 772), (29, 26, 448, 16, 28, 768), (25, 30, 432, 24, 32, 764), (21, 35, 400, 40, 36, 760)],
    [(29, 26, 464, 8, 24, 772), (24, 31, 448, 16, 28, 768), (21, 36, 432, 24, 32, 764), (18, 41, 400, 40, 36, 760)],
    [(24, 31, 464, 8, 24, 772), (20, 37, 448, 16, 28, 768), (17, 43, 432, 24, 32, 764), (15, 49, 400, 40, 36, 760)],
    [(20, 37, 464, 8, 24, 772), (17, 44, 448, 16, 28, 768), (14, 51, 432, 24, 32, 764), (12, 59, 400, 40, 36, 760)],
    [(16, 47, 464, 8, 24, 772), (13, 56, 448, 16, 28, 768), (11, 65, 432, 24, 32, 764), (10, 75, 400, 40, 36, 760)],
];

// total pages of english_txt(40_000, Lf, seed 1) per (font, theme); baseline.
const TXT_PAGES: [[usize; 4]; 5] = [
    [25, 31, 37, 47],
    [35, 43, 51, 63],
    [49, 61, 75, 91],
    [71, 86, 108, 136],
    [111, 139, 173, 204],
];

fn corpus() -> Vec<u8> {
    english_txt(40_000, Eol::Lf, 1)
}

#[test]
fn txt_geometry_per_font_and_theme_matches_baseline() {
    for f in 0..5u8 {
        for t in 0..4u8 {
            let r = txt_rig(&corpus(), f, t);
            let a = &r.app;
            let got = (
                probe::max_lines(a),
                probe::font_line_h(a),
                probe::text_w(a),
                probe::text_margin(a),
                probe::text_y(a),
                probe::text_area_h(a),
            );
            assert_eq!(got, GEOMETRY[f as usize][t as usize], "font {f} theme {t}");
            // derived rule: text width is the 480 px screen minus both margins
            assert_eq!(got.2, 480 - 2 * got.3 as u32);
        }
    }
}

#[test]
fn txt_page_counts_match_baseline() {
    for f in 0..5u8 {
        for t in 0..4u8 {
            let mut r = txt_rig(&corpus(), f, t);
            let pages = r.walk_forward();
            assert_eq!(pages.len(), TXT_PAGES[f as usize][t as usize], "font {f} theme {t}");
            assert!(probe::fully_indexed(&r.app));
            assert_eq!(probe::total_pages(&r.app), pages.len());
        }
    }
}

fn strip_ws(b: &[u8]) -> Vec<u8> {
    // wrapping may drop the space that overflows a line and trims CR/LF, so compare
    // text modulo ASCII whitespace and NBSP
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' | b'\r' | b'\n' | b'\t' => i += 1,
            0xC2 if b.get(i + 1) == Some(&0xA0) => i += 2,
            c => {
                out.push(c);
                i += 1
            }
        }
    }
    out
}

#[test]
fn txt_pages_tile_the_file_without_loss_or_overlap() {
    let data = corpus();
    for (f, t) in [(0u8, 0u8), (2, 1), (4, 3)] {
        let mut r = txt_rig(&data, f, t);
        let pages = r.walk_forward();
        let max = probe::max_lines(&r.app);
        let mut all = Vec::new();
        let offs = probe::offsets(&r.app);
        assert_eq!(offs[0], 0);
        for w in offs.windows(2) {
            assert!(w[0] < w[1], "offsets strictly increase: {offs:?}");
        }
        for (i, p) in pages.iter().enumerate() {
            assert!(p.lines.len() <= max, "page {i} has {} lines > {max}", p.lines.len());
            assert!(!p.lines.is_empty());
            // line spans stay inside the page buffer window and are in order
            let mut prev_end = 0u16;
            for l in &p.lines {
                assert!(l.start >= prev_end);
                prev_end = l.start + l.len;
                all.extend_from_slice(&l.bytes);
            }
            // the page's byte range ends where the next page starts
            if i + 1 < pages.len() {
                let consumed = offs[i + 1] - offs[i];
                assert!(consumed as usize >= prev_end as usize - p.lines[0].start as usize);
            }
        }
        assert_eq!(strip_ws(&all), strip_ws(&data), "font {f} theme {t}: text lost or duplicated");
    }
}

#[test]
fn txt_lines_never_exceed_the_text_width() {
    let data = corpus();
    for (f, t) in [(0u8, 0u8), (2, 1), (3, 2), (4, 3)] {
        let mut r = txt_rig(&data, f, t);
        let w = probe::text_w(&r.app);
        for (pi, p) in r.walk_forward().iter().enumerate() {
            for l in &p.lines {
                let lw = line_width(&r.app, f, l);
                assert!(lw <= w, "font {f} theme {t} page {pi}: line {:?} is {lw}px > {w}px", String::from_utf8_lossy(&l.bytes));
            }
        }
    }
}

#[test]
fn txt_crlf_lays_out_like_lf() {
    let lf = english_txt(20_000, Eol::Lf, 3);
    let crlf = english_txt(20_000, Eol::CrLf, 3);
    assert!(crlf.len() > lf.len());
    let mut a = txt_rig(&lf, 2, 1);
    let mut b = txt_rig(&crlf, 2, 1);
    let (pa, pb) = (a.walk_forward(), b.walk_forward());
    assert_eq!(pa.len(), pb.len());
    for (x, y) in pa.iter().zip(&pb) {
        let tx: Vec<_> = x.lines.iter().map(|l| l.bytes.clone()).collect();
        let ty: Vec<_> = y.lines.iter().map(|l| l.bytes.clone()).collect();
        assert_eq!(tx, ty, "page {}", x.page);
    }
}

#[test]
fn txt_blank_lines_are_kept_and_long_tokens_are_split() {
    let mut r = txt_rig(&edge_txt(), 2, 1);
    let pages = r.walk_forward();
    let lines: Vec<String> = pages
        .iter()
        .flat_map(|p| p.lines.iter().map(|l| String::from_utf8_lossy(&l.bytes).into_owned()))
        .collect();
    // title then three blank lines survive as empty lines (blank paragraph separators)
    assert_eq!(lines[0], "EDGE CASES");
    assert_eq!(&lines[1..4], &["", "", ""]);
    // the 120-column unbreakable token is split into more than one line, none lost
    let w_total: usize = lines.iter().map(|l| l.matches('W').count()).sum();
    assert_eq!(w_total, 120);
    assert!(lines.iter().filter(|l| l.contains('W')).count() >= 2);
    // trailing CR is trimmed from "stray\rcarriage..." only at line end; no panic, text kept
    assert!(lines.iter().any(|l| l.starts_with("last line without a newline")));
    // every line fits
    let w = probe::text_w(&r.app);
    for p in &pages {
        for l in &p.lines {
            assert!(line_width(&r.app, 2, l) <= w);
        }
    }
}

#[test]
fn txt_font_change_while_reading_keeps_the_page_table_baseline_quirk() {
    // Baseline behaviour (identical in the pre-port code, see baseline.md T13): the
    // book font is applied by `set_book_font_size` (apply_font_metrics) before
    // on_resume / on_quick_cycle_update look at it, so a mid-read font change
    // re-wraps the *current* page with the new line budget but does not rebuild the
    // page table; the pages after it keep the old offsets until the book is reopened.
    // Captured here so a port change in this area is noticed, not because it is ideal.
    for via_resume in [true, false] {
        let mut r = txt_rig(&corpus(), 2, 1);
        for _ in 0..3 {
            r.press(Action::Next);
        }
        let before = probe::offsets(&r.app);
        assert_eq!(r.page(), 3);
        assert_eq!(probe::max_lines(&r.app), GEOMETRY[2][1].0);
        if via_resume {
            r.suspend();
            r.app.set_book_font_size(0);
            r.resume();
        } else {
            use pulp_os_host::apps::App;
            r.app.on_quick_cycle_update(1, 0, &mut r.ctx);
            r.settle();
        }
        assert_eq!(probe::max_lines(&r.app), GEOMETRY[0][1].0, "new font budget applied");
        assert_eq!(r.page(), 3);
        assert_eq!(probe::offsets(&r.app)[..before.len()], before[..], "page table not rebuilt");
        assert_eq!(probe::font_line_h(&r.app), GEOMETRY[0][1].1);
    }
}

#[test]
fn txt_reopening_after_a_font_change_repaginates_from_the_bookmark() {
    let data = corpus();
    let mut r = txt_rig(&data, 2, 1);
    for _ in 0..6 {
        r.press(Action::Next);
    }
    let off = r.pos().offset;
    r.save_position();
    r.exit();
    // new font, same book, same card state: reopen resumes on the page containing `off`
    r.configure(0, 1);
    r.open("BOOK.TXT");
    let offs = probe::offsets(&r.app);
    assert_eq!(offs[0], 0);
    let cur = offs[r.page()];
    assert!(cur <= off, "page start {cur} <= bookmark {off}");
    if let Some(&next) = offs.get(r.page() + 1) {
        assert!(next > off, "next page {next} starts after the bookmark {off}");
    }
    assert_eq!(probe::max_lines(&r.app), GEOMETRY[0][1].0);
}

#[test]
fn txt_page_counts_grow_with_font_size_and_theme_spacing() {
    for t in 0..4 {
        for f in 1..5 {
            assert!(TXT_PAGES[f][t] > TXT_PAGES[f - 1][t]);
        }
    }
    for f in 0..5 {
        for t in 1..4 {
            assert!(TXT_PAGES[f][t] > TXT_PAGES[f][t - 1]);
        }
    }
}

#[test]
fn txt_empty_and_tiny_files() {
    let r = txt_rig(b"", 2, 1);
    assert_eq!(probe::phase(&r.app), Phase::Ready);
    assert_eq!(probe::line_count(&r.app), 0);
    assert_eq!(probe::total_pages(&r.app), 1);
    assert!(probe::fully_indexed(&r.app));

    let r = txt_rig(b"x", 2, 1);
    assert_eq!(r.lines(), vec!["x".to_string()]);
    assert_eq!(probe::total_pages(&r.app), 1);

    let mut r = txt_rig(b"one\r\ntwo\r\n", 2, 1);
    assert_eq!(r.lines(), vec!["one".to_string(), "two".to_string()]);
    assert_eq!(r.walk_forward().len(), 1);
}

#[test]
fn txt_page_table_is_capped_at_512_pages() {
    // 1.6 MB of text is far more than 512 pages at any size; baseline behaviour: the
    // index stops at MAX_PAGES (512) and the rest of the file is not reachable by paging
    let big = english_txt(1_600_000, Eol::Lf, 5);
    let mut r = txt_rig(&big, 2, 1);
    let pages = r.walk_forward();
    assert_eq!(pages.len(), 512);
    assert_eq!(probe::total_pages(&r.app), 512);
    assert!(probe::fully_indexed(&r.app));
    assert!(probe::offsets(&r.app)[511] < big.len() as u32);
}

#[test]
fn txt_open_failures_become_an_error_page_not_a_panic() {
    // no such file
    let mut r = Rig::new(card());
    r.open("GONE.TXT");
    assert_eq!(probe::phase(&r.app), Phase::Error);
    assert_eq!(probe::error_kind(&r.app), Some(pulp_os_host::error::ErrorKind::OpenFile));

    // no card
    let mut r = Rig::new(card());
    r.k.sd().eject();
    r.open("BOOK.TXT");
    assert_eq!(probe::phase(&r.app), Phase::Error);
    assert_eq!(probe::error_kind(&r.app), Some(pulp_os_host::error::ErrorKind::NoCard));

    // read error mid-open
    let mut r = Rig::with_book("BOOK.TXT", &corpus());
    r.k.sd().fail_next_reads(100);
    r.open("BOOK.TXT");
    assert_eq!(probe::phase(&r.app), Phase::Error);
    assert_eq!(probe::error_kind(&r.app), Some(pulp_os_host::error::ErrorKind::ReadFailed));
}

// ------------------------------------------------------------------- EPUB

#[test]
fn epub_metadata_spine_and_toc_ncx_and_nav() {
    for kind in [TocKind::Ncx, TocKind::Nav] {
        let r = epub_rig(&EpubSpec { toc: kind, ..Default::default() }, 2, 1);
        assert_eq!(probe::phase(&r.app), Phase::Ready, "{kind:?}");
        assert_eq!(probe::epub_title(&r.app), "The Lighthouse Ledger");
        assert_eq!(probe::spine_len(&r.app), 4);
        assert_eq!(probe::toc_len(&r.app), 4, "{kind:?}");
        let want: Vec<String> = (0..4).map(chapter_title).collect();
        assert_eq!(probe::toc_titles(&r.app), want, "{kind:?}");
        assert_eq!(probe::toc_spine_idx(&r.app), vec![0, 1, 2, 3], "{kind:?}");
    }
    let r = epub_rig(&EpubSpec { toc: TocKind::None, ..Default::default() }, 2, 1);
    assert_eq!(probe::toc_len(&r.app), 0);
}

// pages per chapter of EpubSpec::default() per (font, theme) (2,1) and (0,0); baseline
#[test]
fn epub_chapter_page_counts_match_baseline() {
    for (f, t, want) in [(2u8, 1u8, [10usize, 7, 9, 9]), (0, 0, [4, 3, 4, 4])] {
        let mut r = epub_rig(&EpubSpec::default(), f, t);
        let pages = r.walk_forward();
        let mut per = [0usize; 4];
        for p in &pages {
            per[p.chapter as usize] += 1;
        }
        assert_eq!(per, want, "font {f} theme {t}");
        assert_eq!(probe::spine_len(&r.app), 4);
    }
}

#[test]
fn epub_markup_is_laid_out_with_styles_indent_and_decoded_entities() {
    let mut r = epub_rig(&EpubSpec { toc: TocKind::Ncx, ..Default::default() }, 2, 1);
    let pages = r.walk_forward();
    let ch0: Vec<_> = pages.iter().filter(|p| p.chapter == 0).collect();
    // chapter title is an <h1>: the first line of the chapter carries the heading marker
    use smol_epub::html_strip::{HEADING_ON, MARKER};
    assert_eq!(&ch0[0].lines[0].bytes[..2], &[MARKER, HEADING_ON], "heading marker");
    let all_lines: Vec<(u8, u8, String)> = pages
        .iter()
        .flat_map(|p| {
            p.lines.iter().map(|l| {
                let txt: Vec<u8> = {
                    let mut o = vec![];
                    let mut i = 0;
                    while i < l.bytes.len() {
                        if l.bytes[i] == smol_epub::html_strip::MARKER && i + 1 < l.bytes.len() {
                            i += 2;
                            continue;
                        }
                        o.push(l.bytes[i]);
                        i += 1;
                    }
                    o
                };
                (l.flags, l.indent, String::from_utf8_lossy(&txt).into_owned())
            })
        })
        .collect();
    let joined: String = all_lines.iter().map(|l| l.2.as_str()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("Chapter 1: The Light"));
    let marked = |m: u8| pages.iter().flat_map(|p| &p.lines).any(|l| l.bytes.windows(2).any(|w| w == [MARKER, m]));
    assert!(marked(smol_epub::html_strip::ITALIC_ON), "italic runs present");
    assert!(marked(smol_epub::html_strip::BOLD_ON), "bold runs present");
    assert!(all_lines.iter().any(|l| l.1 >= 1), "blockquote lines are indented");
    // entities: &amp; -> & (twice in the fixture text pattern), &mdash;, &#8220;/&#8221;, &nbsp;
    assert!(joined.contains(" & more"), "&amp; decoded");
    assert!(!joined.contains("&amp;") && !joined.contains("&nbsp;") && !joined.contains("&mdash;"));
    assert!(joined.contains('\u{2014}'), "em dash");
    assert!(joined.contains('\u{201C}') && joined.contains('\u{201D}'), "numeric refs decoded");
    // no tag text leaks into the page
    assert!(!joined.contains('<') && !joined.contains("xmlns"));
}

#[test]
fn epub_stored_and_deflate_entries_give_the_same_pages() {
    let walk = |deflate| {
        let mut r = epub_rig(&EpubSpec { deflate: Some(deflate), ..Default::default() }, 2, 1);
        r.walk_forward()
            .into_iter()
            .map(|p| (p.chapter, p.page, p.offset, p.lines.iter().map(|l| l.bytes.clone()).collect::<Vec<_>>()))
            .collect::<Vec<_>>()
    };
    let a = walk(true);
    let b = walk(false);
    assert_eq!(a.len(), 10 + 7 + 9 + 9);
    assert_eq!(a, b);
}

#[test]
fn rendered_pages_are_not_blank_and_differ_page_to_page() {
    // the real draw() through the real StripBuffer: same hash for the same page,
    // different hashes for different pages, and not the all-white frame
    use pulp_os_host::drivers::strip::{STRIP_BUF_SIZE, STRIP_COUNT};
    let mut r = txt_rig(&corpus(), 2, 1);
    let h0 = r.render_hash();
    assert_eq!(h0, r.render_hash(), "rendering is deterministic");
    let mut white = 0xcbf2_9ce4_8422_2325u64;
    for _ in 0..STRIP_COUNT {
        for _ in 0..STRIP_BUF_SIZE {
            white ^= 0xFF;
            white = white.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    assert_ne!(h0, white, "page 0 draws something");
    r.press(Action::Next);
    assert_ne!(h0, r.render_hash(), "page 1 looks different from page 0");
    r.press(Action::Prev);
    assert_eq!(h0, r.render_hash(), "going back restores the same pixels");
}
