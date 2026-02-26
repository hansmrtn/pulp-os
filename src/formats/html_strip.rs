// Single-pass HTML to plain text converter for EPUB chapter XHTML
//
// Strips tags, decodes entities, collapses whitespace. Block elements
// emit paragraph breaks; <script>/<style>/<head> content is discarded.
// Non-ASCII entities map to '?' (ASCII-only font for now).
//
// Two entry points:
//   strip_html()          — into a separate Vec
//   strip_html_inplace()  — overwrites the buffer (w <= r always holds)

use alloc::vec::Vec;

pub fn strip_html(input: &[u8], output: &mut Vec<u8>) {
    let mut pos = 0;
    let len = input.len();
    let mut last_was_space = true;
    let mut trailing_newlines: u8 = 1;
    let mut skip_until: Option<SkipTag> = None;

    while pos < len {
        if let Some(ref skip) = skip_until {
            if let Some(end_pos) = find_close_tag(&input[pos..], skip.name()) {
                pos += end_pos;
                skip_until = None;
            } else {
                break;
            }
            continue;
        }

        let b = input[pos];

        if b == b'<' {
            pos += 1;
            if pos >= len {
                break;
            }

            if input[pos] == b'!' {
                pos = skip_bang_construct(input, pos);
                continue;
            }
            if input[pos] == b'?' {
                pos = skip_pi(input, pos);
                continue;
            }

            let is_close = input[pos] == b'/';
            if is_close {
                pos += 1;
            }

            let name_start = pos;
            while pos < len && !is_tag_delim(input[pos]) {
                pos += 1;
            }
            let name_end = pos;

            let mut name_buf = [0u8; 16];
            let name_len = (name_end - name_start).min(name_buf.len());
            for i in 0..name_len {
                name_buf[i] = input[name_start + i].to_ascii_lowercase();
            }
            let tag_name = &name_buf[..name_len];

            if !is_close {
                if let Some(skip) = SkipTag::from_name(tag_name) {
                    skip_until = Some(skip);
                }
            }

            if is_block_element(tag_name) {
                emit_block_break(output, &mut last_was_space, &mut trailing_newlines);
            }

            if tag_name == b"br" {
                emit_newline(output, &mut last_was_space, &mut trailing_newlines);
            }

            pos = skip_to_gt(input, pos);
            continue;
        }

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
                    output.push(b'?');
                    last_was_space = false;
                    trailing_newlines = 0;
                }
                DecodedChar::None => {
                    output.push(b'&');
                    last_was_space = false;
                    trailing_newlines = 0;
                }
            }
            continue;
        }

        if is_html_ws(b) {
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

    while output.last().is_some_and(|&b| b == b' ' || b == b'\n') {
        output.pop();
    }
    if !output.is_empty() {
        output.push(b'\n');
    }
}

/// Strip HTML in place. The write cursor never passes the read cursor
/// because stripping only removes or shortens content.
pub fn strip_html_inplace(buf: &mut Vec<u8>) {
    let len = buf.len();
    if len == 0 {
        return;
    }

    let mut r: usize = 0;
    let mut w: usize = 0;
    let mut last_was_space = true;
    let mut trailing_nl: u8 = 1;
    let mut skip_until: Option<SkipTag> = None;

    while r < len {
        if let Some(skip) = skip_until {
            if let Some(end_pos) = find_close_tag(&buf[r..], skip.name()) {
                r += end_pos;
                skip_until = None;
            } else {
                break;
            }
            continue;
        }

        let b = buf[r];

        if b == b'<' {
            r += 1;
            if r >= len {
                break;
            }

            if buf[r] == b'!' {
                r = skip_bang_construct(buf, r);
                continue;
            }
            if buf[r] == b'?' {
                r = skip_pi(buf, r);
                continue;
            }

            let is_close = buf[r] == b'/';
            if is_close {
                r += 1;
            }

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

            if !is_close {
                if let Some(sk) = SkipTag::from_name(tag) {
                    skip_until = Some(sk);
                }
            }

            if is_block_element(tag) {
                while trailing_nl < 2 {
                    buf[w] = b'\n';
                    w += 1;
                    trailing_nl += 1;
                }
                last_was_space = true;
            }

            if tag == b"br" {
                buf[w] = b'\n';
                w += 1;
                trailing_nl = trailing_nl.saturating_add(1);
                last_was_space = true;
            }

            while r < len && buf[r] != b'>' {
                r += 1;
            }
            if r < len {
                r += 1;
            }
            continue;
        }

        if b == b'&' {
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

    while w > 0 && (buf[w - 1] == b' ' || buf[w - 1] == b'\n') {
        w -= 1;
    }
    if w > 0 {
        buf[w] = b'\n';
        w += 1;
    }

    buf.truncate(w);
}

// -- block break / newline emission --

fn emit_block_break(out: &mut Vec<u8>, last_was_space: &mut bool, trailing_newlines: &mut u8) {
    while *trailing_newlines < 2 {
        out.push(b'\n');
        *trailing_newlines += 1;
    }
    *last_was_space = true;
}

fn emit_newline(out: &mut Vec<u8>, last_was_space: &mut bool, trailing_newlines: &mut u8) {
    out.push(b'\n');
    *trailing_newlines = (*trailing_newlines).saturating_add(1);
    *last_was_space = true;
}

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

// -- skip-content tags --

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

// -- entity decoding --

enum DecodedChar {
    Byte(u8),
    Unicode(()),
    None,
}

fn decode_entity(input: &[u8], pos: usize) -> (DecodedChar, usize) {
    debug_assert!(input[pos] == b'&');

    let remaining = &input[pos + 1..];
    let max_scan = remaining.len().min(12);
    let semi = remaining[..max_scan].iter().position(|&b| b == b';');

    let Some(semi) = semi else {
        return (DecodedChar::None, 1);
    };

    let entity = &remaining[..semi];
    let advance = 1 + semi + 1;

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
        b"hellip" => DecodedChar::Byte(b'.'),
        b"copy" => DecodedChar::Byte(b'c'),
        b"reg" => DecodedChar::Byte(b'R'),
        b"trade" => DecodedChar::Byte(b'T'),
        b"times" => DecodedChar::Byte(b'x'),
        b"divide" => DecodedChar::Byte(b'/'),
        b"deg" => DecodedChar::Byte(b'*'),
        b"plusmn" => DecodedChar::Byte(b'+'),
        b"frac12" | b"frac14" | b"frac34" => DecodedChar::Byte(b'/'),
        _ => {
            if entity.starts_with(b"#x") || entity.starts_with(b"#X") {
                codepoint_to_decoded(parse_hex(&entity[2..]))
            } else if entity.starts_with(b"#") {
                codepoint_to_decoded(parse_decimal(&entity[1..]))
            } else {
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

fn codepoint_to_decoded(cp: u32) -> DecodedChar {
    match cp {
        0 => DecodedChar::None,
        0x0001..=0x007F => DecodedChar::Byte(cp as u8),
        0x00A0 => DecodedChar::Byte(b' '),
        0x00AD => DecodedChar::Byte(b'-'),
        0x2013 | 0x2014 => DecodedChar::Byte(b'-'),
        0x2018 | 0x2019 | 0x201A => DecodedChar::Byte(b'\''),
        0x201C | 0x201D | 0x201E => DecodedChar::Byte(b'"'),
        0x2022 => DecodedChar::Byte(b'*'),
        0x2026 => DecodedChar::Byte(b'.'),
        _ => DecodedChar::Unicode(()),
    }
}

// -- scanning helpers --

#[inline]
fn is_html_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

#[inline]
fn is_tag_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

fn skip_to_gt(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() {
        if data[pos] == b'>' {
            return pos + 1;
        }
        pos += 1;
    }
    data.len()
}

fn skip_bang_construct(data: &[u8], pos: usize) -> usize {
    let rest = &data[pos..];

    if rest.starts_with(b"!--") {
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
        let mut p = pos + 8;
        while p + 2 < data.len() {
            if data[p] == b']' && data[p + 1] == b']' && data[p + 2] == b'>' {
                return p + 3;
            }
            p += 1;
        }
        return data.len();
    }

    skip_to_gt(data, pos)
}

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
