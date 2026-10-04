// The golden trace: a deterministic text transcript of what the real reader,
// settings and bookmark code do on the fixtures. scripts/check-reader-regression.sh
// hashes it; the pinned sha256 comes from the pre-port commit, so the work tree
// reproducing it means pagination, navigation, settings text, bookmark bytes and
// rendered pixels did not change.
use std::fmt::Write;

use crate::apps::probe;
use crate::board::action::Action;
use crate::fixtures::*;
use crate::kernel::config::{self, SystemSettings, WifiConfig};
use crate::kernel::bookmarks::BmListEntry;
use crate::rig::*;

fn lines_hash(p: &Pos) -> u64 {
    let mut b = Vec::new();
    for l in &p.lines {
        b.push(l.flags);
        b.push(l.indent);
        b.extend_from_slice(&l.len.to_le_bytes());
        b.extend_from_slice(&l.bytes);
    }
    fnv64(&b)
}

fn page_line(out: &mut String, p: &Pos) {
    writeln!(out, "  c{} p{} off{} n{} h{:016x}", p.chapter, p.page, p.offset, p.lines.len(), lines_hash(p)).unwrap();
}

fn geometry(out: &mut String, r: &Rig) {
    let a = &r.app;
    writeln!(
        out,
        "  geom max_lines={} line_h={} text_w={} margin={} text_y={} area_h={}",
        probe::max_lines(a),
        probe::font_line_h(a),
        probe::text_w(a),
        probe::text_margin(a),
        probe::text_y(a),
        probe::text_area_h(a)
    )
    .unwrap();
}

pub fn trace() -> String {
    let mut out = String::new();

    // 1. TXT, every font size x reading theme
    let txt = english_txt(40_000, Eol::Lf, 1);
    for f in 0..5u8 {
        for t in 0..4u8 {
            let mut r = Rig::with_book("BOOK.TXT", &txt);
            r.configure(f, t);
            r.open("BOOK.TXT");
            writeln!(out, "txt-lf font={f} theme={t} size={}", probe::file_size(&r.app)).unwrap();
            geometry(&mut out, &r);
            let pages = r.walk_forward();
            writeln!(out, "  pages={} indexed={}", pages.len(), probe::fully_indexed(&r.app)).unwrap();
            for p in &pages {
                page_line(&mut out, p);
            }
        }
    }

    // 2. CRLF flavour
    let crlf = english_txt(20_000, Eol::CrLf, 3);
    let mut r = Rig::with_book("BOOK.TXT", &crlf);
    r.configure(2, 1);
    r.open("BOOK.TXT");
    writeln!(out, "txt-crlf font=2 theme=1 size={}", probe::file_size(&r.app)).unwrap();
    for p in r.walk_forward() {
        page_line(&mut out, &p);
    }

    // 3. edge cases, full line text
    for (f, t) in [(2u8, 1u8), (0, 0), (4, 3)] {
        let mut r = Rig::with_book("BOOK.TXT", &edge_txt());
        r.configure(f, t);
        r.open("BOOK.TXT");
        writeln!(out, "txt-edge font={f} theme={t}").unwrap();
        for p in r.walk_forward() {
            page_line(&mut out, &p);
            for l in &p.lines {
                writeln!(out, "    f{} i{} {:?}", l.flags, l.indent, String::from_utf8_lossy(&l.bytes)).unwrap();
            }
        }
    }

    // 4. EPUB (NCX), every font x theme: every page of every chapter
    for f in 0..5u8 {
        for t in 0..4u8 {
            let mut r = Rig::with_book("BOOK.EPUB", &english_epub(&EpubSpec::default()));
            r.configure(f, t);
            r.open("BOOK.EPUB");
            writeln!(
                out,
                "epub-ncx font={f} theme={t} title={:?} spine={} toc={:?}",
                probe::epub_title(&r.app),
                probe::spine_len(&r.app),
                probe::toc_titles(&r.app)
            )
            .unwrap();
            geometry(&mut out, &r);
            for p in r.walk_forward() {
                page_line(&mut out, &p);
            }
        }
    }

    // 5. EPUB with an EPUB3 nav, without a TOC, all-stored, all-deflated
    for (label, spec) in [
        ("nav", EpubSpec { toc: TocKind::Nav, ..Default::default() }),
        ("none", EpubSpec { toc: TocKind::None, ..Default::default() }),
        ("stored", EpubSpec { deflate: Some(false), ..Default::default() }),
        ("deflated", EpubSpec { deflate: Some(true), chapters: 6, ..Default::default() }),
    ] {
        let mut r = Rig::with_book("BOOK.EPUB", &english_epub(&spec));
        r.configure(2, 1);
        r.open("BOOK.EPUB");
        writeln!(out, "epub-{label} spine={} toc={:?} toc_idx={:?}", probe::spine_len(&r.app), probe::toc_titles(&r.app), probe::toc_spine_idx(&r.app)).unwrap();
        for p in r.walk_forward() {
            page_line(&mut out, &p);
        }
    }

    // 6. navigation transcript
    let nav = |out: &mut String, r: &mut Rig, label: &str, a: Action, long: bool| {
        let t = if long { r.long_press(a) } else { r.press(a) };
        let p = r.pos();
        writeln!(out, "  {label:<14} -> {:?} c{} p{} off{} h{:016x}", t, p.chapter, p.page, p.offset, lines_hash(&p)).unwrap();
    };
    let mut r = Rig::with_book("BOOK.TXT", &txt);
    r.configure(2, 1);
    r.open("BOOK.TXT");
    writeln!(out, "nav txt").unwrap();
    for (label, a, long) in [
        ("prev@first", Action::Prev, false),
        ("next", Action::Next, false),
        ("next", Action::Next, false),
        ("jump+10", Action::NextJump, false),
        ("jump+10", Action::NextJump, false),
        ("jump-10", Action::PrevJump, false),
        ("long-next-jump", Action::NextJump, true),
        ("long-prev-jump", Action::PrevJump, true),
        ("back", Action::Back, false),
        ("long-back", Action::Back, true),
        ("select", Action::Select, false),
    ] {
        nav(&mut out, &mut r, label, a, long);
    }
    let mut r = Rig::with_book("BOOK.EPUB", &english_epub(&EpubSpec::default()));
    r.configure(2, 1);
    r.open("BOOK.EPUB");
    writeln!(out, "nav epub").unwrap();
    for (label, a, long) in [
        ("prev@first", Action::Prev, false),
        ("next", Action::Next, false),
        ("jump-ch", Action::NextJump, false),
        ("jump-ch", Action::NextJump, false),
        ("long-next-jump", Action::NextJump, true),
        ("next@ch-end", Action::Next, false),
        ("prev@ch-start", Action::Prev, false),
        ("prev-jump", Action::PrevJump, false),
        ("long-prev-jump", Action::PrevJump, true),
        ("next-jump", Action::NextJump, false),
        ("next-jump", Action::NextJump, false),
        ("next-jump@end", Action::NextJump, false),
        ("back", Action::Back, false),
    ] {
        nav(&mut out, &mut r, label, a, long);
    }

    // 7. rendered pixels (strip data of the real draw() for a few pages)
    let mut r = Rig::with_book("BOOK.TXT", &txt);
    r.configure(2, 1);
    r.open("BOOK.TXT");
    writeln!(out, "render txt").unwrap();
    let pages = r.walk_forward();
    for idx in [0usize, 3, pages.len() - 1] {
        while r.page() > idx {
            r.press(Action::Prev);
        }
        while r.page() < idx {
            r.press(Action::Next);
        }
        writeln!(out, "  page {idx} strips={:016x}", r.render_hash()).unwrap();
    }
    let mut r = Rig::with_book("BOOK.EPUB", &english_epub(&EpubSpec::default()));
    r.configure(2, 1);
    r.open("BOOK.EPUB");
    writeln!(out, "render epub").unwrap();
    writeln!(out, "  c0 p0 strips={:016x}", r.render_hash()).unwrap();
    r.press(Action::NextJump);
    r.press(Action::NextJump);
    for _ in 0..3 {
        r.press(Action::Next);
    }
    writeln!(out, "  c{} p{} strips={:016x}", r.app.chapter(), r.page(), r.render_hash()).unwrap();

    // 8. settings text
    let mut s = SystemSettings::defaults();
    let mut w = WifiConfig::empty();
    let mut buf = [0u8; 512];
    let n = config::write_settings_txt(&s, &w, &mut buf);
    writeln!(out, "settings defaults:\n{}", String::from_utf8_lossy(&buf[..n])).unwrap();
    let src = b"sleep_timeout=45\nghost_clear=25\nbook_font=4\nui_font=0\nreading_theme=3\nswap_buttons=true\nwifi_ssid=home net\nwifi_pass=p@ss=word\nunknown=1\nbook_font=zzz\n";
    config::parse_settings_txt(src, &mut s, &mut w);
    s.sanitize();
    let n = config::write_settings_txt(&s, &w, &mut buf);
    writeln!(out, "settings custom:\n{}", String::from_utf8_lossy(&buf[..n])).unwrap();

    // 9. bookmark bytes
    let mut k = crate::kernel::Kernel::new(pulp_kernel::drivers::sdcard::SdStorage::mounted(card()));
    k.bookmarks_load();
    for (i, n) in ["BOOK.TXT", "NOVEL.EPU", "A.TXT", "B.TXT", "C.EPUB"].iter().enumerate() {
        k.bookmarks().save(n.as_bytes(), 1000 * (i as u32 + 1), i as u16);
    }
    k.bookmarks().remove(b"A.TXT");
    k.bookmarks().save(b"BOOK.TXT", 424242, 9);
    k.bookmarks_flush();
    let bytes = k
        .sd()
        .with_fs(|fs| fs.get("_PULP", crate::kernel::bookmarks::BOOKMARK_FILE).cloned())
        .flatten()
        .unwrap();
    writeln!(out, "bookmarks BKMK.BIN {} bytes h{:016x}", bytes.len(), fnv64(&bytes)).unwrap();
    for rec in bytes.chunks(48) {
        let hex: String = rec.iter().map(|b| format!("{b:02x}")).collect();
        writeln!(out, "  {hex}").unwrap();
    }
    let mut list = [BmListEntry::EMPTY; 16];
    let n = k.bookmarks().load_all(&mut list);
    let names: Vec<_> = list[..n].iter().map(|e| (e.filename_str().to_string(), e.chapter)).collect();
    writeln!(out, "bookmark list {names:?}").unwrap();

    out
}
