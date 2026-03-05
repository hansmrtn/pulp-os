fn main() {
    linker_be_nice();
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    generate_bitmap_fonts();
}

fn hint(msg: &str) {
    eprintln!();
    eprintln!("hint: {msg}");
    eprintln!();
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    // --error-handling-script passes two args: kind and symbol
    if args.len() >= 3 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => hint(
                    "`defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                ),
                "_stack_start" => hint("is the linker script `linkall.x` missing?"),
                what if what.starts_with("esp_rtos_") => hint(
                    "`esp-radio` has no scheduler enabled. make sure you have initialized `esp-rtos` or provided an external scheduler."
                ),
                "embedded_test_linker_file_not_added_to_rustflags" => hint(
                    "`embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                ),
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => hint(
                    "did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                ),
                _ => (),
            },
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!(
        "cargo:rustc-link-arg=--error-handling-script={}",
        std::env::current_exe().unwrap().display()
    );
}

// build-time font rasterisation: scan assets/fonts/ for TTFs, classify
// by weight/style, rasterise to 1-bit bitmaps, emit font_data.rs.
//
// five size tiers: XSmall / Small / Medium / Large / XLarge
// two glyph sets per font:
//   ascii 0x20-0x7E (contiguous direct-indexed table)
//   extended unicode (sorted codepoint table, binary-searched at runtime)
//
// extended set covers latin-1 supplement, common punctuation (smart
// quotes, dashes, ellipsis, bullet), and a handful of currency/math
// symbols, enough for the vast majority of european-language epubs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

// body sizes (px): 0=XSmall 1=Small 2=Medium 3=Large 4=XLarge
const BODY_PX: [(f32, &str); 5] = [
    (16.0, "XSMALL"),
    (19.0, "SMALL"),
    (23.0, "MEDIUM"),
    (28.0, "LARGE"),
    (35.0, "XLARGE"),
];

// Heading sizes scale proportionally
const HEADING_PX: [(f32, &str); 5] = [
    (23.0, "XSMALL"),
    (27.0, "SMALL"),
    (32.0, "MEDIUM"),
    (38.0, "LARGE"),
    (46.0, "XLARGE"),
];

// fontdue coverage threshold; values >= this become black
const THRESHOLD: u8 = 100;

// ASCII range (direct-indexed)
const FIRST_CHAR: u8 = 0x20;
const LAST_CHAR: u8 = 0x7E;
const GLYPH_COUNT: usize = (LAST_CHAR - FIRST_CHAR + 1) as usize;

// build the sorted list of extended unicode codepoints to rasterise
fn extended_codepoints() -> Vec<u32> {
    let mut cps: Vec<u32> = Vec::new();

    // latin-1 supplement (0x00A0-0x00FF): accented letters, symbols
    // skip 0x00A0 (NBSP) and 0x00AD (soft hyphen), they are whitespace
    for cp in 0x00A1..=0x00FFu32 {
        if cp == 0x00AD {
            continue; // soft hyphen, handled as whitespace
        }
        cps.push(cp);
    }

    // Latin Extended-A: most common characters in European languages
    // Czech, Polish, Hungarian, Turkish, Romanian, etc.
    let latin_ext_a: &[u32] = &[
        0x0100, 0x0101, // Āā
        0x0102, 0x0103, // Ăă
        0x0104, 0x0105, // Ąą
        0x0106, 0x0107, // Ćć
        0x010C, 0x010D, // Čč
        0x010E, 0x010F, // Ďď
        0x0110, 0x0111, // Đđ
        0x0118, 0x0119, // Ęę
        0x011A, 0x011B, // Ěě
        0x011E, 0x011F, // Ğğ
        0x0130, 0x0131, // İı
        0x0141, 0x0142, // Łł
        0x0143, 0x0144, // Ńń
        0x0147, 0x0148, // Ňň
        0x0150, 0x0151, // Őő
        0x0152, 0x0153, // Œœ
        0x0158, 0x0159, // Řř
        0x015A, 0x015B, // Śś
        0x015E, 0x015F, // Şş
        0x0160, 0x0161, // Šš
        0x0162, 0x0163, // Ţţ
        0x0164, 0x0165, // Ťť
        0x016E, 0x016F, // Ůů
        0x0170, 0x0171, // Űű
        0x0178, // Ÿ
        0x0179, 0x017A, // Źź
        0x017B, 0x017C, // Żż
        0x017D, 0x017E, // Žž
    ];
    cps.extend_from_slice(latin_ext_a);

    // General Punctuation: hyphens, dashes, quotes, ellipsis, bullet
    let punctuation: &[u32] = &[
        0x2010, // ‐ hyphen
        0x2011, // ‑ non-breaking hyphen
        0x2012, // ‒ figure dash
        0x2013, // – en dash
        0x2014, // — em dash
        0x2015, // ― horizontal bar
        0x2018, // ' left single quotation mark
        0x2019, // ' right single quotation mark
        0x201A, // ‚ single low-9 quotation mark
        0x201B, // ‛ single high-reversed-9
        0x201C, // " left double quotation mark
        0x201D, // " right double quotation mark
        0x201E, // „ double low-9 quotation mark
        0x201F, // ‟ double high-reversed-9
        0x2022, // • bullet
        0x2026, // … horizontal ellipsis
        0x2032, // ′ prime
        0x2033, // ″ double prime
        0x2039, // ‹ single left-pointing angle quotation
        0x203A, // › single right-pointing angle quotation
    ];
    cps.extend_from_slice(punctuation);

    // Currency and math
    let symbols: &[u32] = &[
        0x20AC, // € euro sign
        0x2122, // ™ trade mark sign
        0x2212, // − minus sign
    ];
    cps.extend_from_slice(symbols);

    cps.sort();
    cps.dedup();
    cps
}

// find first .ttf in dir whose name contains all keywords (case-insensitive);
// excludes BoldItalic unless explicitly requested
fn find_ttf(dir: &Path, keywords: &[&str]) -> Option<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return None;
    };
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .map(|e| e.eq_ignore_ascii_case("ttf"))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();

    for path in &candidates {
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();

        let all_match = keywords.iter().all(|kw| stem.contains(&kw.to_lowercase()));
        if !all_match {
            continue;
        }

        // reject BoldItalic when looking for just Bold or just Italic
        if keywords.len() == 1 {
            if keywords[0].eq_ignore_ascii_case("Bold") && stem.contains("italic") {
                continue;
            }
            if keywords[0].eq_ignore_ascii_case("Italic") && stem.contains("bold") {
                continue;
            }
        }

        return Some(path.clone());
    }
    None
}

fn generate_bitmap_fonts() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = Path::new(&out_dir).join("font_data.rs");
    let mut out = fs::File::create(&dest).unwrap();

    let font_dir = Path::new("assets/fonts");

    // discover TTFs and classify by style
    let regular = find_ttf(font_dir, &["Regular"]);
    let bold = find_ttf(font_dir, &["Bold"]);
    let italic = find_ttf(font_dir, &["Italic"]);

    // re-run build if any TTF changes
    if let Some(ref p) = regular {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    if let Some(ref p) = bold {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    if let Some(ref p) = italic {
        println!("cargo:rerun-if-changed={}", p.display());
    }
    println!("cargo:rerun-if-changed=assets/fonts");

    let ext_codepoints = extended_codepoints();

    // header
    writeln!(out, "// AUTO-GENERATED by build.rs - do not edit").unwrap();
    writeln!(
        out,
        "use crate::fonts::bitmap::{{BitmapFont, BitmapGlyph, GLYPH_COUNT}};"
    )
    .unwrap();
    writeln!(out).unwrap();

    let has_regular = regular.is_some();
    writeln!(out, "pub const HAS_REGULAR: bool = {};", has_regular).unwrap();

    // emit the total number of extended codepoints (used by bitmap.rs)
    writeln!(
        out,
        "pub const EXT_GLYPH_COUNT: usize = {};",
        ext_codepoints.len()
    )
    .unwrap();
    writeln!(out).unwrap();

    // regular
    if let Some(ref path) = regular {
        let data = fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
            .expect("failed to parse regular TTF");
        eprintln!(
            "cargo:warning=font: rasterising {} ({} glyphs, {} ext codepoints) body {:.0}/{:.0}/{:.0}/{:.0}/{:.0} px heading {:.0}/{:.0}/{:.0}/{:.0}/{:.0} px",
            path.file_name().unwrap().to_string_lossy(),
            font.glyph_count(),
            ext_codepoints.len(),
            BODY_PX[0].0,
            BODY_PX[1].0,
            BODY_PX[2].0,
            BODY_PX[3].0,
            BODY_PX[4].0,
            HEADING_PX[0].0,
            HEADING_PX[1].0,
            HEADING_PX[2].0,
            HEADING_PX[3].0,
            HEADING_PX[4].0,
        );
        for (px, suffix) in &BODY_PX {
            emit_font(
                &mut out,
                &font,
                &format!("REGULAR_BODY_{suffix}"),
                *px,
                &ext_codepoints,
            );
        }
        for (px, suffix) in &HEADING_PX {
            emit_font(
                &mut out,
                &font,
                &format!("REGULAR_HEADING_{suffix}"),
                *px,
                &ext_codepoints,
            );
        }
    } else {
        for (_px, suffix) in &BODY_PX {
            emit_stub(&mut out, &format!("REGULAR_BODY_{suffix}"));
        }
        for (_px, suffix) in &HEADING_PX {
            emit_stub(&mut out, &format!("REGULAR_HEADING_{suffix}"));
        }
    }

    // bold
    if let Some(ref path) = bold {
        let data = fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
            .expect("failed to parse bold TTF");
        eprintln!(
            "cargo:warning=font: rasterising {} body {:.0}/{:.0}/{:.0}/{:.0}/{:.0} px",
            path.file_name().unwrap().to_string_lossy(),
            BODY_PX[0].0,
            BODY_PX[1].0,
            BODY_PX[2].0,
            BODY_PX[3].0,
            BODY_PX[4].0,
        );
        for (px, suffix) in &BODY_PX {
            emit_font(
                &mut out,
                &font,
                &format!("BOLD_BODY_{suffix}"),
                *px,
                &ext_codepoints,
            );
        }
    } else {
        for (_px, suffix) in &BODY_PX {
            emit_stub(&mut out, &format!("BOLD_BODY_{suffix}"));
        }
    }

    // italic
    if let Some(ref path) = italic {
        let data = fs::read(path).unwrap();
        let font = fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default())
            .expect("failed to parse italic TTF");
        eprintln!(
            "cargo:warning=font: rasterising {} body {:.0}/{:.0}/{:.0}/{:.0}/{:.0} px",
            path.file_name().unwrap().to_string_lossy(),
            BODY_PX[0].0,
            BODY_PX[1].0,
            BODY_PX[2].0,
            BODY_PX[3].0,
            BODY_PX[4].0,
        );
        for (px, suffix) in &BODY_PX {
            emit_font(
                &mut out,
                &font,
                &format!("ITALIC_BODY_{suffix}"),
                *px,
                &ext_codepoints,
            );
        }
    } else {
        for (_px, suffix) in &BODY_PX {
            emit_stub(&mut out, &format!("ITALIC_BODY_{suffix}"));
        }
    }
}

struct RasterGlyph {
    advance: u8,
    offset_x: i8,
    offset_y: i8,
    width: u8,
    height: u8,
    bits: Vec<u8>,
}

fn rasterize_char(font: &fontdue::Font, ch: char, px: f32) -> RasterGlyph {
    let (metrics, coverage) = font.rasterize(ch, px);
    let w = metrics.width;
    let h = metrics.height;
    let row_bytes = w.div_ceil(8);

    // pack coverage to 1-bit MSB-first
    let mut bits = Vec::with_capacity(row_bytes * h);
    for y in 0..h {
        for bx in 0..row_bytes {
            let mut byte = 0u8;
            for bit in 0..8usize {
                let x = bx * 8 + bit;
                if x < w && coverage[y * w + x] >= THRESHOLD {
                    byte |= 1 << (7 - bit);
                }
            }
            bits.push(byte);
        }
    }

    // offset_y: baseline to top row (y-down screen space).
    // fontdue ymin = baseline to bottom edge; top = ymin+h above baseline;
    // negate for screen coords.
    let offset_y = -metrics.ymin - (h as i32);

    RasterGlyph {
        advance: (metrics.advance_width + 0.5) as u8,
        offset_x: (metrics.xmin).clamp(-128, 127) as i8,
        offset_y: offset_y.clamp(-128, 127) as i8,
        width: w.min(255) as u8,
        height: h.min(255) as u8,
        bits,
    }
}

fn emit_font(
    out: &mut fs::File,
    font: &fontdue::Font,
    name: &str,
    px: f32,
    ext_codepoints: &[u32],
) {
    // line metrics
    let lm = font
        .horizontal_line_metrics(px)
        .expect("font has no horizontal metrics");
    let line_height = lm.new_line_size.ceil() as u16;
    let ascent = lm.ascent.ceil() as u16;

    // ascii glyphs (direct-indexed 0x20-0x7E)

    let mut ascii_glyphs: Vec<RasterGlyph> = Vec::with_capacity(GLYPH_COUNT);
    let mut ascii_bits_total: usize = 0;

    for code in FIRST_CHAR..=LAST_CHAR {
        let g = rasterize_char(font, code as char, px);
        ascii_bits_total += g.bits.len();
        ascii_glyphs.push(g);
    }

    // emit ASCII glyph table
    writeln!(out, "static {name}_GLYPHS: [BitmapGlyph; GLYPH_COUNT] = [").unwrap();
    let mut offset: u16 = 0;
    for (i, g) in ascii_glyphs.iter().enumerate() {
        let ch = (FIRST_CHAR + i as u8) as char;
        writeln!(
            out,
            "    BitmapGlyph {{ advance: {:>2}, offset_x: {:>3}, offset_y: {:>4}, width: {:>2}, height: {:>2}, bitmap_offset: {:>5} }}, // {:?}",
            g.advance, g.offset_x, g.offset_y, g.width, g.height, offset, ch
        ).unwrap();
        offset += g.bits.len() as u16;
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // emit ASCII bitmap data
    writeln!(out, "static {name}_BITMAPS: [u8; {ascii_bits_total}] = [").unwrap();
    emit_bitmap_bytes(out, &ascii_glyphs);
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // extended unicode glyphs (sorted by codepoint)

    let mut ext_glyphs: Vec<RasterGlyph> = Vec::with_capacity(ext_codepoints.len());
    let mut ext_bits_total: usize = 0;

    for &cp in ext_codepoints {
        if let Some(ch) = char::from_u32(cp) {
            let g = rasterize_char(font, ch, px);
            ext_bits_total += g.bits.len();
            ext_glyphs.push(g);
        } else {
            // invalid codepoint, push a zero-width space placeholder
            ext_glyphs.push(RasterGlyph {
                advance: 0,
                offset_x: 0,
                offset_y: 0,
                width: 0,
                height: 0,
                bits: Vec::new(),
            });
        }
    }

    let ext_count = ext_codepoints.len();

    // emit extended codepoint lookup array
    writeln!(out, "static {name}_EXT_CP: [u32; {ext_count}] = [").unwrap();
    let mut col = 0;
    for &cp in ext_codepoints {
        if col == 0 {
            write!(out, "    ").unwrap();
        }
        write!(out, "0x{cp:04X},").unwrap();
        col += 1;
        if col >= 10 {
            writeln!(out).unwrap();
            col = 0;
        }
    }
    if col > 0 {
        writeln!(out).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // emit extended glyph table
    writeln!(
        out,
        "static {name}_EXT_GLYPHS: [BitmapGlyph; {ext_count}] = ["
    )
    .unwrap();
    let mut offset: u16 = 0;
    for (i, g) in ext_glyphs.iter().enumerate() {
        let cp = ext_codepoints[i];
        let ch_display = char::from_u32(cp)
            .map(|c| format!("{:?}", c))
            .unwrap_or_else(|| format!("U+{cp:04X}"));
        writeln!(
            out,
            "    BitmapGlyph {{ advance: {:>2}, offset_x: {:>3}, offset_y: {:>4}, width: {:>2}, height: {:>2}, bitmap_offset: {:>5} }}, // {}",
            g.advance, g.offset_x, g.offset_y, g.width, g.height, offset, ch_display
        ).unwrap();
        offset += g.bits.len() as u16;
    }
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // emit extended bitmap data
    writeln!(out, "static {name}_EXT_BITMAPS: [u8; {ext_bits_total}] = [").unwrap();
    emit_bitmap_bytes(out, &ext_glyphs);
    writeln!(out, "];").unwrap();
    writeln!(out).unwrap();

    // BitmapFont struct

    writeln!(out, "pub static {name}: BitmapFont = BitmapFont {{").unwrap();
    writeln!(out, "    glyphs: &{name}_GLYPHS,").unwrap();
    writeln!(out, "    bitmaps: &{name}_BITMAPS,").unwrap();
    writeln!(out, "    ext_codepoints: &{name}_EXT_CP,").unwrap();
    writeln!(out, "    ext_glyphs: &{name}_EXT_GLYPHS,").unwrap();
    writeln!(out, "    ext_bitmaps: &{name}_EXT_BITMAPS,").unwrap();
    writeln!(out, "    line_height: {line_height},").unwrap();
    writeln!(out, "    ascent: {ascent},").unwrap();
    writeln!(out, "}};").unwrap();
    writeln!(out).unwrap();
}

fn emit_stub(out: &mut fs::File, name: &str) {
    writeln!(
        out,
        "pub static {name}: BitmapFont = BitmapFont {{\
         glyphs: &[BitmapGlyph {{ advance: 0, offset_x: 0, offset_y: 0, width: 0, height: 0, bitmap_offset: 0 }}; GLYPH_COUNT], \
         bitmaps: &[], \
         ext_codepoints: &[], \
         ext_glyphs: &[], \
         ext_bitmaps: &[], \
         line_height: 13, \
         ascent: 13 \
         }};"
    )
    .unwrap();
    writeln!(out).unwrap();
}

fn emit_bitmap_bytes(out: &mut fs::File, glyphs: &[RasterGlyph]) {
    let mut col = 0;
    for g in glyphs {
        for &b in &g.bits {
            if col == 0 {
                write!(out, "    ").unwrap();
            }
            write!(out, "0x{b:02X},").unwrap();
            col += 1;
            if col >= 16 {
                writeln!(out).unwrap();
                col = 0;
            }
        }
    }
    if col > 0 {
        writeln!(out).unwrap();
    }
}
