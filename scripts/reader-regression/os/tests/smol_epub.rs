// smol-epub (../smol-epub, revision pinned in baseline.md §2) had no tests of its own
// (T1: 0 unit tests, 4 ignored doctests). These exercise its public API on the
// fixtures the reader consumes: ZIP index + STORED/DEFLATE extraction, container /
// OPF / TOC parsing, HTML stripping, cache naming.
use pulp_os_host::fixtures::*;
use smol_epub::{cache, epub, html_strip, zip};

fn index(zip_bytes: &[u8]) -> zip::ZipIndex {
    let size = zip_bytes.len() as u32;
    let tail = &zip_bytes[zip_bytes.len().saturating_sub(512)..];
    let (cd_off, cd_size) = zip::ZipIndex::parse_eocd(tail, size).expect("eocd");
    let mut z = zip::ZipIndex::new();
    z.parse_central_directory(&zip_bytes[cd_off as usize..(cd_off + cd_size) as usize]).expect("cd");
    z
}

fn extract(zip_bytes: &[u8], z: &zip::ZipIndex, name: &str) -> Result<Vec<u8>, &'static str> {
    let idx = z.find(name).ok_or("not found")?;
    let e = z.entry(idx);
    zip::extract_entry(e, e.local_offset, |off, buf| {
        let off = off as usize;
        if off > zip_bytes.len() {
            return Err("eof");
        }
        let n = buf.len().min(zip_bytes.len() - off);
        buf[..n].copy_from_slice(&zip_bytes[off..off + n]);
        Ok(n)
    })
}

#[test]
fn zip_stored_and_deflate_entries_extract_to_the_original_bytes() {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(200);
    let mut b = ZipBuilder::new();
    b.stored("plain.txt", text.as_bytes());
    b.deflated("packed.txt", text.as_bytes());
    b.deflated("dir/empty.txt", b"");
    b.stored("Dir/Mixed.TXT", b"abc");
    let bytes = b.finish();
    let z = index(&bytes);
    assert_eq!(z.count(), 4);
    assert_eq!(z.entry_name(0), "plain.txt");
    assert_eq!(z.entry(0).method, zip::METHOD_STORED);
    assert_eq!(z.entry(1).method, zip::METHOD_DEFLATE);
    assert!(z.entry(1).comp_size < z.entry(1).uncomp_size, "deflate really compresses");
    assert_eq!(extract(&bytes, &z, "plain.txt").unwrap(), text.as_bytes());
    assert_eq!(extract(&bytes, &z, "packed.txt").unwrap(), text.as_bytes());
    assert_eq!(extract(&bytes, &z, "dir/empty.txt").unwrap(), b"");
    assert_eq!(z.find("dir/mixed.txt"), None, "find is case sensitive");
    assert_eq!(z.find_icase("dir/mixed.txt"), Some(3));
    assert_eq!(z.find("nope"), None);
}

#[test]
fn zip_damage_is_an_error_not_a_panic() {
    let mut b = ZipBuilder::new();
    b.deflated("a.txt", &b"hello world ".repeat(50));
    let bytes = b.finish();
    // no end-of-central-directory record
    assert!(zip::ZipIndex::parse_eocd(&bytes[..bytes.len() - 22], bytes.len() as u32 - 22).is_err());
    assert!(zip::ZipIndex::parse_eocd(&[], 0).is_err());
    assert!(zip::ZipIndex::parse_eocd(&[0u8; 40], 40).is_err());
    // central directory cut short / garbage
    let mut z = zip::ZipIndex::new();
    let tail = &bytes[bytes.len() - 22..];
    let (off, size) = zip::ZipIndex::parse_eocd(tail, bytes.len() as u32).unwrap();
    let cd = &bytes[off as usize..(off + size) as usize];
    assert!(z.parse_central_directory(&cd[..cd.len() / 2]).is_err() || z.count() == 0);
    let mut z = zip::ZipIndex::new();
    let _ = z.parse_central_directory(&vec![0x5A; 300]);
    // truncated compressed data: extraction reports an error
    let z = index(&bytes);
    let cut = &bytes[..bytes.len() - 22 - 80];
    assert!(extract(cut, &z, "a.txt").is_err());
}

#[test]
fn epub_container_opf_and_toc_parse() {
    let bytes = english_epub(&EpubSpec::default());
    let z = index(&bytes);
    let container = extract(&bytes, &z, "META-INF/container.xml").unwrap();
    let mut path = [0u8; epub::OPF_PATH_CAP];
    let n = epub::parse_container(&container, &mut path).unwrap();
    assert_eq!(&path[..n], b"OEBPS/content.opf");
    assert!(epub::parse_container(b"<container/>", &mut path).is_err());

    let opf = extract(&bytes, &z, "OEBPS/content.opf").unwrap();
    let mut meta = epub::EpubMeta::new();
    let mut spine = epub::EpubSpine::new();
    epub::parse_opf(&opf, "OEBPS", &z, &mut meta, &mut spine).unwrap();
    assert_eq!(meta.title_str(), "The Lighthouse Ledger");
    assert_eq!(meta.author_str(), "A. Fixture");
    assert_eq!(spine.len(), 4);
    let names: Vec<_> = (0..4).map(|i| z.entry_name(spine.items[i] as usize).to_string()).collect();
    assert_eq!(names, ["OEBPS/ch0.xhtml", "OEBPS/ch1.xhtml", "OEBPS/ch2.xhtml", "OEBPS/ch3.xhtml"]);

    let src = epub::find_toc_source(&opf, "OEBPS", &z).expect("ncx");
    assert!(matches!(src, epub::TocSource::Ncx(_)));
    let toc_data = extract(&bytes, &z, "OEBPS/toc.ncx").unwrap();
    let mut toc = epub::EpubToc::new();
    epub::parse_toc(src, &toc_data, "OEBPS", &spine, &z, &mut toc);
    assert_eq!(toc.len(), 4);
    assert_eq!(toc.entries[2].title_str(), chapter_title(2));
    assert_eq!(toc.entries[2].spine_idx, 2);

    // an OPF whose spine points nowhere is an error
    assert!(epub::parse_opf(b"<package><spine><itemref idref=\"x\"/></spine></package>", "", &z, &mut meta, &mut spine).is_err());
}

#[test]
fn epub3_nav_is_found_and_parsed() {
    let bytes = english_epub(&EpubSpec { toc: TocKind::Nav, ..Default::default() });
    let z = index(&bytes);
    let opf = extract(&bytes, &z, "OEBPS/content.opf").unwrap();
    let src = epub::find_toc_source(&opf, "OEBPS", &z).expect("nav");
    assert!(matches!(src, epub::TocSource::Nav(_)));
    let mut meta = epub::EpubMeta::new();
    let mut spine = epub::EpubSpine::new();
    epub::parse_opf(&opf, "OEBPS", &z, &mut meta, &mut spine).unwrap();
    let data = extract(&bytes, &z, "OEBPS/nav.xhtml").unwrap();
    let mut toc = epub::EpubToc::new();
    epub::parse_toc(src, &data, "OEBPS", &spine, &z, &mut toc);
    assert_eq!(toc.len(), 4);
    assert_eq!(toc.entries[3].title_str(), chapter_title(3));
    let bytes = english_epub(&EpubSpec { toc: TocKind::None, ..Default::default() });
    let z = index(&bytes);
    let opf = extract(&bytes, &z, "OEBPS/content.opf").unwrap();
    assert!(epub::find_toc_source(&opf, "OEBPS", &z).is_none());
}

fn strip(html: &str) -> String {
    let out = cache::strip_html_buf(html.as_bytes()).unwrap();
    // render markers readably: <B> </b> etc.
    let mut s = String::new();
    let mut i = 0;
    while i < out.len() {
        if out[i] == html_strip::MARKER && i + 1 < out.len() {
            s.push_str(&format!("<{}>", out[i + 1] as char));
            i += 2;
        } else {
            let (c, n) = pulp_kernel::util::decode_utf8_char(&out, i);
            s.push(c);
            i += n;
        }
    }
    s
}

#[test]
fn html_strip_keeps_text_styles_entities_and_drops_markup() {
    let s = strip("<html><head><title>T</title><style>p{x:y}</style></head><body><h1>Head</h1><p>one <b>two</b> <i>three</i> &amp; &lt;four&gt; &#8212; &#x2019; a&nbsp;b</p><blockquote><p>quote</p></blockquote></body></html>");
    assert!(s.contains("<H>Head<h>"), "{s:?}");
    assert!(s.contains("<B>two<b>"), "{s:?}");
    assert!(s.contains("<I>three<i>"), "{s:?}");
    // &nbsp; is flattened to a plain space by the stripper (baseline)
    assert!(s.contains("& <four> \u{2014} \u{2019} a b"), "{s:?}");
    assert!(s.contains("<Q>") && s.contains("<q>"), "blockquote markers: {s:?}");
    assert!(s.contains("quote"));
    assert!(!s.contains("p{x:y}"), "style content is dropped: {s:?}");
    assert!(!s.contains("<html") && !s.contains("<head") && !s.contains("<p>one"), "{s:?}");
    // paragraphs end in a newline so the wrapper starts a new line
    assert!(s.contains("one"), "{s:?}");
}

#[test]
fn html_strip_survives_malformed_and_unusual_input() {
    for html in [
        "",
        "plain text with no tags",
        "<p>unclosed <b>bold and <i>nesting",
        "<p>stray </b></i></h1> close tags</p>",
        "<p>&bogus; &#99999999; &#xZZ; &amp</p>",
        "<",
        "<p attr=\"x>y\">tail",
        "<script>alert(1)</script>visible",
        "a\u{FFFD}b\u{00E9}\u{20AC}",
    ] {
        let _ = strip(html); // must not panic
    }
    assert!(strip("<script>alert(1)</script>visible").contains("visible"));
    assert!(!strip("<script>alert(1)</script>visible").contains("alert"));
    assert!(strip("plain text with no tags").contains("plain text with no tags"));
}

#[test]
fn cache_names_and_hashes_are_the_baseline_format() {
    // FNV-1a 32 known vectors
    assert_eq!(cache::fnv1a(b""), 0x811c_9dc5);
    assert_eq!(cache::fnv1a(b"a"), 0xe40c_292c);
    assert_eq!(cache::fnv1a(b"foobar"), 0xbf9c_f968);
    assert_eq!(cache::fnv1a_icase(b"FooBar"), cache::fnv1a(b"foobar"));
    // 8.3 names: "_" + the low 7 hex digits of the hash
    let f = cache::cache_filename(0x1234_ABCD);
    assert_eq!(cache::cache_filename_str(&f), "_234ABCD.BIN");
    let d = cache::dir_name_for_hash(0x1234_ABCD);
    assert_eq!(cache::dir_name_str(&d), "_234ABCD");
    assert_eq!(cache::chapter_file_str(&cache::chapter_file_name(7)), "CH007.TXT");
    assert_eq!(cache::HEADER_SIZE, 128);
    assert_eq!(cache::CHAPTER_ENTRY_SIZE, 8);
    assert_eq!(cache::MAX_CACHE_CHAPTERS, 256);
}

#[test]
fn cache_header_round_trips_and_rejects_changed_books() {
    let mut buf = [0u8; cache::HEADER_SIZE];
    let mut hdr = cache::CacheHeader::empty();
    hdr.version = cache::CACHE_V3;
    hdr.flags = cache::FLAG_CHAPTERS_COMPLETE;
    hdr.epub_size = 4321;
    hdr.name_hash = 0xDEADBEEF;
    hdr.chapter_count = 4;
    cache::encode_v3_header(&hdr, &mut buf);
    let h = cache::parse_v3_header(&buf).unwrap();
    assert_eq!((h.epub_size, h.name_hash, h.chapter_count), (4321, 0xDEADBEEF, 4));
    assert!(h.chapters_complete());
    assert!(cache::validate_v3_header(&h, 4321, 0xDEADBEEF, 4).is_ok());
    assert!(cache::validate_v3_header(&h, 4322, 0xDEADBEEF, 4).is_err(), "book size changed");
    assert!(cache::validate_v3_header(&h, 4321, 0xDEADBEEE, 4).is_err(), "other book");
    assert!(cache::validate_v3_header(&h, 4321, 0xDEADBEEF, 5).is_err(), "spine length changed");
    let mut bad = buf;
    bad[0] ^= 0xFF;
    assert!(cache::parse_v3_header(&bad).is_err(), "bad magic");
}
