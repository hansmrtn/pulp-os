// Single-pass HTML to plain text converter for EPUB chapter XHTML
//
// Converts XHTML chapter content into plain text suitable for the
// reader's line-wrapping engine. Designed for the well-formed XHTML
// found inside EPUB archives.
//
// Behaviour:
//   • All tags are stripped; block elements emit paragraph breaks
//   • HTML entities are decoded (&amp; &lt; &#x2019; etc.)
//   • Inline whitespace collapses to a single space (HTML rules)
//   • Content inside <script>, <style>, <head> is discarded
//   • Non-ASCII entities map to '?' (our font is ASCII-only for now)
//
// Two entry points:
//   strip_html()          — writes to a separate output Vec
//   strip_html_inplace()  — strips in the decompressed buffer itself
//
// The in-place variant is used for large EPUB chapters.  Because
// stripping only removes or shortens content (tags vanish, entities
// decode to fewer bytes, whitespace collapses), the write cursor
// *never passes* the read cursor.  This lets us work in a single
// buffer with zero extra heap allocation.

use alloc::vec::Vec;

/// Strip HTML tags from `input` XHTML, appending plain text to `output`.
///
/// `output` is **not** cleared — the caller should call `output.clear()`
/// first if starting fresh, or can append multiple chapters.
pub fn strip_html(input: &[u8], output: &mut Vec<u8>) {
    let mut pos = 0;
    let len = input.len();

    // Whitespace collapse state
    let mut last_was_space = true; // suppress leading whitespace
    let mut trailing_newlines: u8 = 1; // how many \n at the end of output

    // Skip-content state: when inside <script>, <style>, or <head>,
    // we discard all text until the matching close tag.
    let mut skip_until: Option<SkipTag> = None;

    while pos < len {
        // ── Skip-content mode ───────────────────────────────────
        if let Some(ref skip) = skip_until {
            // Scan for the closing tag, e.g. </script>
            if let Some(end_pos) = find_close_tag(&input[pos..], skip.name()) {
                pos += end_pos;
                skip_until = None;
            } else {
                break; // no closing tag found — discard rest
            }
            continue;
        }

        let b = input[pos];

        // ── Start of a tag ──────────────────────────────────────
        if b == b'<' {
            pos += 1;
            if pos >= len {
                break;
            }

            // Special constructs: comments, CDATA, DOCTYPE, PI
            if input[pos] == b'!' {
                pos = skip_bang_construct(input, pos);
                continue;
            }
            if input[pos] == b'?' {
                // Processing instruction: skip to ?>
                pos = skip_pi(input, pos);
                continue;
            }

            // Read tag name (may start with '/' for close tags)
            let is_close = input[pos] == b'/';
            if is_close {
                pos += 1;
            }

            let name_start = pos;
            while pos < len && !is_tag_delim(input[pos]) {
                pos += 1;
            }
            let name_end = pos;

            // Lowercase the tag name into a small stack buffer
            let mut name_buf = [0u8; 16];
            let name_len = (name_end - name_start).min(name_buf.len());
            for i in 0..name_len {
                name_buf[i] = input[name_start + i].to_ascii_lowercase();
            }
            let tag_name = &name_buf[..name_len];

            // Check if we should skip this element's content
            if !is_close {
                if let Some(skip) = SkipTag::from_name(tag_name) {
                    skip_until = Some(skip);
                }
            }

            // Block elements insert a paragraph break
            if is_block_element(tag_name) {
                emit_block_break(output, &mut last_was_space, &mut trailing_newlines);
            }

            // <br> and <br/> always force a newline
            if tag_name == b"br" {
                emit_newline(output, &mut last_was_space, &mut trailing_newlines);
            }

            // Skip to end of tag ('>')
            pos = skip_to_gt(input, pos);
            continue;
        }

        // ── HTML entity ─────────────────────────────────────────
        if b == b'&' {
            let (decoded, advance) = decode_entity(input, pos);
            pos += advance;

            match decoded {
                DecodedChar::Byte(c) => {
                    if c == b'\n' {
                        emit_newline(output, &mut last_was_space, &mut trailing_newlines);
                    } else if is_html_ws(c) {
                        if !last_was_space {
                            output.push(b' ');
                            last_was_space = true;
                            trailing_newlines = 0;
                        }
                    } else {
                        output.push(c);
                        last_was_space = false;
                        trailing_newlines = 0;
                    }
                }
                DecodedChar::Unicode(_) => {
                    // Non-ASCII codepoint — emit '?' placeholder
                    output.push(b'?');
                    last_was_space = false;
                    trailing_newlines = 0;
                }
                DecodedChar::None => {
                    // Malformed entity — emit '&' literally
                    output.push(b'&');
                    last_was_space = false;
                    trailing_newlines = 0;
                }
            }
            continue;
        }

        // ── Regular text character ──────────────────────────────
        if is_html_ws(b) {
            // HTML: all whitespace (space, tab, \r, \n) collapses to
            // a single space within inline content.
            if !last_was_space {
                output.push(b' ');
                last_was_space = true;
                trailing_newlines = 0;
            }
        } else {
            output.push(b);
            last_was_space = false;
            trailing_newlines = 0;
        }

        pos += 1;
    }

    // Trim trailing whitespace
    while output.last().is_some_and(|&b| b == b' ' || b == b'\n') {
        output.pop();
    }

    // Ensure the text ends with a newline if non-empty
    if !output.is_empty() {
        output.push(b'\n');
    }
}

// ── In-place stripping ──────────────────────────────────────────

/// Strip HTML tags from `buf` in place, truncating it to the
/// stripped length.
///
/// Peak heap = the buffer itself (the decompressed XHTML).  No
/// second allocation is made.  This is critical for large chapters
/// where the decompressed XHTML already fills most of the heap.
///
/// Safety argument (no `unsafe` needed):
///   All reads use `buf[r]` which returns `u8` (Copy) — no borrow
///   is held across writes.  Helper functions like `decode_entity`
///   borrow `buf` as `&[u8]` only for the duration of the call;
///   the borrow is released before any `buf[w] = …` write.
///   The invariant `w <= r` is maintained because every operation
///   either skips input (tags), shortens it (entities, whitespace
///   collapse), or copies 1:1 (regular chars after the gap is open).
pub fn strip_html_inplace(buf: &mut Vec<u8>) {
    let len = buf.len();
    if len == 0 {
        return;
    }

    let mut r: usize = 0; // read cursor  (always >= w)
    let mut w: usize = 0; // write cursor (always <= r)
    let mut last_was_space = true;
    let mut trailing_nl: u8 = 1;
    let mut skip_until: Option<SkipTag> = None;

    while r < len {
        // ── Skip-content mode (inside <script>, <style>, <head>) ─
        if let Some(skip) = skip_until {
            if let Some(end_pos) = find_close_tag(&buf[r..], skip.name()) {
                r += end_pos;
                skip_until = None;
            } else {
                break; // no closing tag — discard rest
            }
            continue;
        }

        let b = buf[r];

        // ── Start of a tag ──────────────────────────────────────
        if b == b'<' {
            r += 1;
            if r >= len {
                break;
            }

            // Comments, CDATA, DOCTYPE
            if buf[r] == b'!' {
                r = skip_bang_construct(buf, r);
                continue;
            }
            // Processing instruction
            if buf[r] == b'?' {
                r = skip_pi(buf, r);
                continue;
            }

            let is_close = buf[r] == b'/';
            if is_close {
                r += 1;
            }

            // Copy tag name into a small stack buffer (lowercase)
            let name_start = r;
            while r < len && !is_tag_delim(buf[r]) {
                r += 1;
            }
            let mut tn = [0u8; 16];
            let tn_len = (r - name_start).min(16);
            for i in 0..tn_len {
                tn[i] = buf[name_start + i].to_ascii_lowercase();
            }
            let tag = &tn[..tn_len];

            // Enter skip-content mode?
            if !is_close {
                if let Some(sk) = SkipTag::from_name(tag) {
                    skip_until = Some(sk);
                }
            }

            // Block element → paragraph break
            if is_block_element(tag) {
                while trailing_nl < 2 {
                    buf[w] = b'\n';
                    w += 1;
                    trailing_nl += 1;
                }
                last_was_space = true;
            }

            // <br> → newline
            if tag == b"br" {
                buf[w] = b'\n';
                w += 1;
                trailing_nl = trailing_nl.saturating_add(1);
                last_was_space = true;
            }

            // Skip to closing '>'
            while r < len && buf[r] != b'>' {
                r += 1;
            }
            if r < len {
                r += 1;
            }
            continue;
        }

        // ── HTML entity ─────────────────────────────────────────
        if b == b'&' {
            // decode_entity borrows buf as &[u8] for the call only;
            // the borrow is released before we write to buf[w].
            let (decoded, advance) = decode_entity(buf, r);
            r += advance;

            match decoded {
                DecodedChar::Byte(c) if c == b'\n' => {
                    buf[w] = b'\n';
                    w += 1;
                    trailing_nl = trailing_nl.saturating_add(1);
                    last_was_space = true;
                }
                DecodedChar::Byte(c) if is_html_ws(c) => {
                    if !last_was_space {
                        buf[w] = b' ';
                        w += 1;
                        last_was_space = true;
                        trailing_nl = 0;
                    }
                }
                DecodedChar::Byte(c) => {
                    buf[w] = c;
                    w += 1;
                    last_was_space = false;
                    trailing_nl = 0;
                }
                DecodedChar::Unicode(_) => {
                    buf[w] = b'?';
                    w += 1;
                    last_was_space = false;
                    trailing_nl = 0;
                }
                DecodedChar::None => {
                    buf[w] = b'&';
                    w += 1;
                    last_was_space = false;
                    trailing_nl = 0;
                }
            }
            continue;
        }

        // ── Regular text character ──────────────────────────────
        if is_html_ws(b) {
            if !last_was_space {
                buf[w] = b' ';
                w += 1;
                last_was_space = true;
                trailing_nl = 0;
            }
        } else {
            buf[w] = b;
            w += 1;
            last_was_space = false;
            trailing_nl = 0;
        }

        r += 1;
    }

    // Trim trailing whitespace, ensure final newline
    while w > 0 && (buf[w - 1] == b' ' || buf[w - 1] == b'\n') {
        w -= 1;
    }
    if w > 0 {
        buf[w] = b'\n';
        w += 1;
    }

    buf.truncate(w);
}

// ── Block break / newline emission (for strip_html → Vec path) ──

/// Emit a paragraph break (blank line). Collapses consecutive breaks
/// so we never get more than one blank line in a row.
fn emit_block_break(out: &mut Vec<u8>, last_was_space: &mut bool, trailing_newlines: &mut u8) {
    // We want "\n\n" between block elements, but collapse if we
    // already have newlines at the end.
    while *trailing_newlines < 2 {
        out.push(b'\n');
        *trailing_newlines += 1;
    }
    *last_was_space = true;
}

/// Emit a single newline (for <br>).
fn emit_newline(out: &mut Vec<u8>, last_was_space: &mut bool, trailing_newlines: &mut u8) {
    out.push(b'\n');
    *trailing_newlines = (*trailing_newlines).saturating_add(1);
    *last_was_space = true;
}

// ── Block element detection ─────────────────────────────────────

/// Is this tag name a block-level element that should produce a
/// paragraph break in the output?
fn is_block_element(name: &[u8]) -> bool {
    matches!(
        name,
        b"p" | b"div"
            | b"h1"
            | b"h2"
            | b"h3"
            | b"h4"
            | b"h5"
            | b"h6"
            | b"li"
            | b"ul"
            | b"ol"
            | b"dl"
            | b"dt"
            | b"dd"
            | b"tr"
            | b"blockquote"
            | b"section"
            | b"article"
            | b"aside"
            | b"figure"
            | b"figcaption"
            | b"header"
            | b"footer"
            | b"nav"
            | b"pre"
            | b"hr"
            | b"table"
    )
}

// ── Skip-content tags ───────────────────────────────────────────

/// Elements whose *content* (not just the tag) should be discarded.
#[derive(Clone, Copy)]
enum SkipTag {
    Script,
    Style,
    Head,
}

impl SkipTag {
    fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"script" => Some(Self::Script),
            b"style" => Some(Self::Style),
            b"head" => Some(Self::Head),
            _ => None,
        }
    }

    fn name(&self) -> &'static [u8] {
        match self {
            Self::Script => b"script",
            Self::Style => b"style",
            Self::Head => b"head",
        }
    }
}

/// Find the closing tag `</name>` in `data`, returning the byte
/// position *after* the closing `>`. Case-insensitive.
fn find_close_tag(data: &[u8], name: &[u8]) -> Option<usize> {
    let mut pos = 0;
    while pos + 2 < data.len() {
        if data[pos] == b'<' && data[pos + 1] == b'/' {
            let tag_start = pos + 2;
            let mut tag_pos = tag_start;
            while tag_pos < data.len() && !is_tag_delim(data[tag_pos]) {
                tag_pos += 1;
            }
            let tag_name = &data[tag_start..tag_pos];
            if tag_name.len() == name.len()
                && tag_name
                    .iter()
                    .zip(name.iter())
                    .all(|(a, b)| a.to_ascii_lowercase() == *b)
            {
                // Skip to '>'
                while tag_pos < data.len() && data[tag_pos] != b'>' {
                    tag_pos += 1;
                }
                return Some(tag_pos + 1);
            }
        }
        pos += 1;
    }
    None
}

// ── Entity decoding ─────────────────────────────────────────────

enum DecodedChar {
    /// ASCII byte that can be emitted directly.
    Byte(u8),
    /// Unicode codepoint > 127 (we emit '?' for these).
    Unicode(u32),
    /// Malformed or unrecognised entity.
    None,
}

/// Decode an HTML entity starting at `input[pos]` (which is '&').
/// Returns the decoded character and the number of bytes consumed.
fn decode_entity(input: &[u8], pos: usize) -> (DecodedChar, usize) {
    debug_assert!(input[pos] == b'&');

    let remaining = &input[pos + 1..];

    // Find the ';' — entities shouldn't be longer than ~10 chars
    let max_scan = remaining.len().min(12);
    let semi = remaining[..max_scan].iter().position(|&b| b == b';');

    let Some(semi) = semi else {
        // No semicolon found — not a valid entity, emit '&' literally
        return (DecodedChar::None, 1);
    };

    let entity = &remaining[..semi];
    let advance = 1 + semi + 1; // '&' + entity + ';'

    // ── Named entities ──────────────────────────────────────────
    let decoded = match entity {
        b"amp" => DecodedChar::Byte(b'&'),
        b"lt" => DecodedChar::Byte(b'<'),
        b"gt" => DecodedChar::Byte(b'>'),
        b"quot" => DecodedChar::Byte(b'"'),
        b"apos" => DecodedChar::Byte(b'\''),
        b"nbsp" => DecodedChar::Byte(b' '),
        b"mdash" | b"emdash" => DecodedChar::Byte(b'-'),
        b"ndash" | b"endash" => DecodedChar::Byte(b'-'),
        b"lsquo" | b"rsquo" | b"sbquo" => DecodedChar::Byte(b'\''),
        b"ldquo" | b"rdquo" | b"bdquo" => DecodedChar::Byte(b'"'),
        b"hellip" => DecodedChar::Byte(b'.'), // ellipsis → just a dot
        b"copy" => DecodedChar::Byte(b'c'),   // ©
        b"reg" => DecodedChar::Byte(b'R'),    // ®
        b"trade" => DecodedChar::Byte(b'T'),  // ™
        b"times" => DecodedChar::Byte(b'x'),
        b"divide" => DecodedChar::Byte(b'/'),
        b"deg" => DecodedChar::Byte(b'*'),
        b"plusmn" => DecodedChar::Byte(b'+'),
        b"frac12" => DecodedChar::Byte(b'/'), // ½
        b"frac14" => DecodedChar::Byte(b'/'), // ¼
        b"frac34" => DecodedChar::Byte(b'/'), // ¾
        _ => {
            // ── Numeric entities ────────────────────────────────
            if entity.starts_with(b"#x") || entity.starts_with(b"#X") {
                // Hex: &#x2019;
                let hex = &entity[2..];
                let val = parse_hex(hex);
                codepoint_to_decoded(val)
            } else if entity.starts_with(b"#") {
                // Decimal: &#8217;
                let dec = &entity[1..];
                let val = parse_decimal(dec);
                codepoint_to_decoded(val)
            } else {
                // Unknown named entity — discard
                DecodedChar::None
            }
        }
    };

    (decoded, advance)
}

fn parse_hex(bytes: &[u8]) -> u32 {
    let mut val = 0u32;
    for &b in bytes {
        let nibble = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => return 0,
        };
        val = val.wrapping_mul(16).wrapping_add(nibble);
    }
    val
}

fn parse_decimal(bytes: &[u8]) -> u32 {
    let mut val = 0u32;
    for &b in bytes {
        if b.is_ascii_digit() {
            val = val.wrapping_mul(10).wrapping_add((b - b'0') as u32);
        } else {
            return 0;
        }
    }
    val
}

/// Map a Unicode codepoint to our output representation.
fn codepoint_to_decoded(cp: u32) -> DecodedChar {
    if cp == 0 {
        return DecodedChar::None;
    }

    // Common Unicode → ASCII mappings for readability
    match cp {
        // ASCII range: emit directly
        0x0001..=0x007F => DecodedChar::Byte(cp as u8),
        // Non-breaking space
        0x00A0 => DecodedChar::Byte(b' '),
        // Soft hyphen
        0x00AD => DecodedChar::Byte(b'-'),
        // En dash, em dash
        0x2013 | 0x2014 => DecodedChar::Byte(b'-'),
        // Left/right single quotes, single low-9 quote
        0x2018 | 0x2019 | 0x201A => DecodedChar::Byte(b'\''),
        // Left/right double quotes, double low-9 quote
        0x201C | 0x201D | 0x201E => DecodedChar::Byte(b'"'),
        // Bullet
        0x2022 => DecodedChar::Byte(b'*'),
        // Horizontal ellipsis
        0x2026 => DecodedChar::Byte(b'.'),
        // Everything else: non-ASCII
        _ => DecodedChar::Unicode(cp),
    }
}

// ── Scanning helpers ────────────────────────────────────────────

#[inline]
fn is_html_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

#[inline]
fn is_tag_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

/// Advance past `>` starting from `pos`. Returns position after `>`.
fn skip_to_gt(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() {
        if data[pos] == b'>' {
            return pos + 1;
        }
        pos += 1;
    }
    data.len()
}

/// Skip a `<!` construct: comment, CDATA, or DOCTYPE.
/// `pos` points to the `!` after `<`.
fn skip_bang_construct(data: &[u8], pos: usize) -> usize {
    let rest = &data[pos..];

    if rest.starts_with(b"!--") {
        // Comment: <!-- ... -->
        let mut p = pos + 3;
        while p + 2 < data.len() {
            if data[p] == b'-' && data[p + 1] == b'-' && data[p + 2] == b'>' {
                return p + 3;
            }
            p += 1;
        }
        return data.len();
    }

    if rest.starts_with(b"![CDATA[") {
        // CDATA section: <![CDATA[ ... ]]>
        let mut p = pos + 8;
        while p + 2 < data.len() {
            if data[p] == b']' && data[p + 1] == b']' && data[p + 2] == b'>' {
                return p + 3;
            }
            p += 1;
        }
        return data.len();
    }

    // DOCTYPE or other: skip to >
    skip_to_gt(data, pos)
}

/// Skip a processing instruction `<?...?>`.
/// `pos` points to the `?` after `<`.
fn skip_pi(data: &[u8], pos: usize) -> usize {
    let mut p = pos + 1;
    while p + 1 < data.len() {
        if data[p] == b'?' && data[p + 1] == b'>' {
            return p + 2;
        }
        p += 1;
    }
    data.len()
}
