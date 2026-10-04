// Deterministic English fixtures, generated in code (no binary files in the repo):
//   * a plain-text book (LF and CRLF flavours, plus a long-line / edge-case flavour)
//   * a minimal but complete EPUB (EPUB2 NCX or EPUB3 nav, STORED and DEFLATE entries)
//
// The prose is original text written for this test. The generator is a fixed LCG,
// so the bytes are identical on every run and every machine; the golden trace
// (scripts/check-reader-regression.sh) depends on that.

const SENTENCES: &[&str] = &[
    "The lighthouse keeper climbed the spiral stairs each evening, counting the iron steps as his father had taught him.",
    "By the time the lamp caught, the harbour below had already turned the colour of old pewter.",
    "She read the letter twice, folded it along its original creases, and set it under the clock where no one would look.",
    "\u{201C}We should leave before the tide turns,\u{201D} said Marta, \u{201C}or we will be walking home in the dark.\u{201D}",
    "A single gull followed the ferry for most of the crossing, then gave up and wheeled back toward the cliffs.",
    "Nothing in the ledger explained the missing barrels\u{2014}not the dates, not the signatures, not even the careless ink blots.",
    "He had learned long ago that patience was less a virtue than a habit, and habits were cheap to keep.",
    "The market smelled of tar, oranges, and wet rope; it was, she decided, the smell of everywhere she had ever been happy.",
    "It's the small things, he thought, that decide whether a house feels lived in or merely occupied.",
    "Rain drummed on the tin roof for an hour, softened to a whisper, and finally stopped as if embarrassed by the noise.",
    "At the edge of the map someone had written a single word in a careful hand: perhaps.",
    "The professor's lecture on tidal harmonics ran forty minutes over, and nobody in the room seemed to mind.",
    "Supercalifragilisticexpialidocious is not, strictly speaking, a word that appears in any navigational manual.",
    "Visit https://example.org/very/long/path/that/does/not/contain/a/single/space/anywhere/in/it/at/all/ for details.",
    "Caf\u{00E9} owners along the quay paid their rent in fish, fruit, and the occasional favour owed from the winter.",
    "Three short words. Then a much longer sentence that wanders across the line before finally, mercifully, arriving at its point.",
];

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

fn paragraph(rng: &mut Lcg) -> String {
    let n = 1 + (rng.next() % 6) as usize;
    let mut p = String::new();
    for i in 0..n {
        if i > 0 {
            p.push(' ');
        }
        p.push_str(SENTENCES[(rng.next() as usize) % SENTENCES.len()]);
    }
    p
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    CrLf,
}

// `bytes` of English prose in paragraphs (one paragraph per line, blank line
// between paragraphs), with `CHAPTER n` headings every ~12 paragraphs.
pub fn english_txt(bytes: usize, eol: Eol, seed: u64) -> Vec<u8> {
    let nl = match eol {
        Eol::Lf => "\n",
        Eol::CrLf => "\r\n",
    };
    let mut rng = Lcg(seed);
    let mut out = String::new();
    let mut chap = 1;
    let mut count = 0;
    while out.len() < bytes {
        if count % 12 == 0 {
            out.push_str(&format!("CHAPTER {chap}{nl}{nl}"));
            chap += 1;
        }
        out.push_str(&paragraph(&mut rng));
        out.push_str(nl);
        out.push_str(nl);
        count += 1;
    }
    out.into_bytes()
}

// edge cases the wrapper has to survive, as TXT:
//   blank-only lines, a line exactly filling the width, an unbreakable token wider
//   than a line, trailing spaces, NBSP, soft hyphen, stray CR, tabs, leading spaces
pub fn edge_txt() -> Vec<u8> {
    let mut s = String::new();
    s.push_str("EDGE CASES\n\n\n\n");
    s.push_str("   leading spaces then text that is long enough to wrap onto a second line of the page\n");
    s.push_str("trailing spaces here        \n");
    s.push_str(&"W".repeat(120));
    s.push('\n');
    s.push_str(&"abcdefghij ".repeat(40));
    s.push('\n');
    s.push_str("non\u{00A0}breaking\u{00A0}space\u{00A0}joined\u{00A0}words\u{00A0}that\u{00A0}go\u{00A0}on\u{00A0}for\u{00A0}quite\u{00A0}a\u{00A0}while\u{00A0}before\u{00A0}any\u{00A0}real\u{00A0}space\n");
    s.push_str("soft\u{00AD}hy\u{00AD}phen\u{00AD}ated\u{00AD}supercalifragilistic\u{00AD}expialidocious\u{00AD}word\u{00AD}run\u{00AD}that\u{00AD}never\u{00AD}breaks\u{00AD}at\u{00AD}a\u{00AD}space\n");
    s.push_str("tab\tseparated\tcolumns\tshould\tnot\tcrash\n");
    s.push_str("stray\rcarriage\rreturns\n");
    s.push_str("curly \u{201C}quotes\u{201D} and \u{2018}singles\u{2019} \u{2014} dashes \u{2013} ellipsis\u{2026} \u{20AC}5 caf\u{00E9} na\u{00EF}ve\n");
    s.push_str("last line without a newline");
    s.into_bytes()
}

// ---------------------------------------------------------------- zip / epub

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[derive(Default)]
pub struct ZipBuilder {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl ZipBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, name: &str, raw: &[u8], deflate: bool) {
        let (method, payload) = if deflate {
            (8u16, miniz_oxide::deflate::compress_to_vec(raw, 6))
        } else {
            (0u16, raw.to_vec())
        };
        let crc = crc32(raw);
        let offset = self.out.len() as u32;
        // local file header
        self.out.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
        self.out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        self.out.extend_from_slice(&0u16.to_le_bytes()); // flags
        self.out.extend_from_slice(&method.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // time
        self.out.extend_from_slice(&0x21u16.to_le_bytes()); // date
        self.out.extend_from_slice(&crc.to_le_bytes());
        self.out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        self.out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(&payload);
        // central directory entry
        self.central.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
        self.central.extend_from_slice(&20u16.to_le_bytes()); // made by
        self.central.extend_from_slice(&20u16.to_le_bytes()); // needed
        self.central.extend_from_slice(&0u16.to_le_bytes());
        self.central.extend_from_slice(&method.to_le_bytes());
        self.central.extend_from_slice(&0u16.to_le_bytes());
        self.central.extend_from_slice(&0x21u16.to_le_bytes());
        self.central.extend_from_slice(&crc.to_le_bytes());
        self.central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        self.central.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        self.central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.central.extend_from_slice(&0u16.to_le_bytes()); // extra
        self.central.extend_from_slice(&0u16.to_le_bytes()); // comment
        self.central.extend_from_slice(&0u16.to_le_bytes()); // disk
        self.central.extend_from_slice(&0u16.to_le_bytes()); // int attrs
        self.central.extend_from_slice(&0u32.to_le_bytes()); // ext attrs
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name.as_bytes());
        self.count += 1;
    }

    pub fn stored(&mut self, name: &str, raw: &[u8]) -> &mut Self {
        self.add(name, raw, false);
        self
    }

    pub fn deflated(&mut self, name: &str, raw: &[u8]) -> &mut Self {
        self.add(name, raw, true);
        self
    }

    pub fn finish(mut self) -> Vec<u8> {
        let cd_offset = self.out.len() as u32;
        let cd_size = self.central.len() as u32;
        self.out.extend_from_slice(&self.central);
        self.out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&cd_size.to_le_bytes());
        self.out.extend_from_slice(&cd_offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TocKind {
    Ncx,
    Nav,
    None,
}

pub struct EpubSpec {
    pub chapters: usize,
    pub paragraphs_per_chapter: usize,
    pub toc: TocKind,
    pub seed: u64,
    // None: chapters alternate DEFLATE / STORED; Some(true|false): all DEFLATE / all STORED
    pub deflate: Option<bool>,
}

impl Default for EpubSpec {
    fn default() -> Self {
        Self { chapters: 4, paragraphs_per_chapter: 14, toc: TocKind::Ncx, seed: 7, deflate: None }
    }
}

pub fn chapter_title(i: usize) -> String {
    format!("Chapter {}: {}", i + 1, ["The Light", "The Letter", "The Crossing", "The Ledger", "The Market", "The Rain"][i % 6])
}

fn chapter_xhtml(i: usize, paragraphs: usize, rng: &mut Lcg) -> String {
    let mut s = String::new();
    s.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\">\n<head><title>");
    s.push_str(&chapter_title(i));
    s.push_str("</title></head>\n<body>\n");
    s.push_str(&format!("<h1>{}</h1>\n", chapter_title(i)));
    for p in 0..paragraphs {
        let text = paragraph(rng).replace('&', "&amp;");
        match p % 5 {
            1 => s.push_str(&format!("<p><b>{}</b> {}</p>\n", "Bold lead-in", text)),
            2 => s.push_str(&format!("<p><i>{}</i></p>\n", text)),
            3 => s.push_str(&format!("<blockquote><p>{}</p></blockquote>\n", text)),
            4 => s.push_str(&format!("<p>{} &amp; more&nbsp;text&mdash;with entities&shy;and &#8220;numeric&#8221; refs.</p>\n", text)),
            _ => s.push_str(&format!("<p>{}</p>\n", text)),
        }
    }
    s.push_str("</body>\n</html>\n");
    s
}

pub fn english_epub(spec: &EpubSpec) -> Vec<u8> {
    let mut rng = Lcg(spec.seed);
    let mut z = ZipBuilder::new();
    z.stored("mimetype", b"application/epub+zip");
    z.deflated(
        "META-INF/container.xml",
        b"<?xml version=\"1.0\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>",
    );

    let mut manifest = String::new();
    let mut spine = String::new();
    for i in 0..spec.chapters {
        manifest.push_str(&format!(
            "<item id=\"ch{i}\" href=\"ch{i}.xhtml\" media-type=\"application/xhtml+xml\"/>\n"
        ));
        spine.push_str(&format!("<itemref idref=\"ch{i}\"/>\n"));
    }
    let (toc_attr, toc_item) = match spec.toc {
        TocKind::Ncx => (
            " toc=\"ncx\"",
            "<item id=\"ncx\" href=\"toc.ncx\" media-type=\"application/x-dtbncx+xml\"/>\n",
        ),
        TocKind::Nav => (
            "",
            "<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n",
        ),
        TocKind::None => ("", ""),
    };
    let opf = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"2.0\" unique-identifier=\"id\">\n<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>The Lighthouse Ledger</dc:title><dc:creator>A. Fixture</dc:creator><dc:identifier id=\"id\">urn:uuid:00000000-0000-0000-0000-000000000013</dc:identifier><dc:language>en</dc:language></metadata>\n<manifest>\n{manifest}{toc_item}</manifest>\n<spine{toc_attr}>\n{spine}</spine>\n</package>"
    );
    z.deflated("OEBPS/content.opf", opf.as_bytes());

    match spec.toc {
        TocKind::Ncx => {
            let mut nav = String::from("<?xml version=\"1.0\"?>\n<ncx xmlns=\"http://www.daisy.org/z3986/2005/ncx/\" version=\"2005-1\"><navMap>\n");
            for i in 0..spec.chapters {
                nav.push_str(&format!(
                    "<navPoint id=\"n{i}\" playOrder=\"{}\"><navLabel><text>{}</text></navLabel><content src=\"ch{i}.xhtml\"/></navPoint>\n",
                    i + 1,
                    chapter_title(i)
                ));
            }
            nav.push_str("</navMap></ncx>");
            z.stored("OEBPS/toc.ncx", nav.as_bytes());
        }
        TocKind::Nav => {
            let mut nav = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>Contents</title></head><body><nav epub:type=\"toc\"><ol>\n");
            for i in 0..spec.chapters {
                nav.push_str(&format!("<li><a href=\"ch{i}.xhtml\">{}</a></li>\n", chapter_title(i)));
            }
            nav.push_str("</ol></nav></body></html>");
            z.deflated("OEBPS/nav.xhtml", nav.as_bytes());
        }
        TocKind::None => {}
    }

    for i in 0..spec.chapters {
        let x = chapter_xhtml(i, spec.paragraphs_per_chapter, &mut rng);
        // alternate STORED / DEFLATE so both extraction paths are exercised
        let deflate = spec.deflate.unwrap_or(i % 2 == 0);
        if deflate {
            z.deflated(&format!("OEBPS/ch{i}.xhtml"), x.as_bytes());
        } else {
            z.stored(&format!("OEBPS/ch{i}.xhtml"), x.as_bytes());
        }
    }
    z.finish()
}
