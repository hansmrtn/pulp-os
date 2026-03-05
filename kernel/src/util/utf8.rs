// UTF-8 decoding utilities for no_std environments
//
// provides both iterator-based and single-char decoding interfaces
// for processing UTF-8 byte slices without std::str

// decode one UTF-8 character at buf[pos]
// returns (char, byte_length); malformed sequences yield '\u{FFFD}'
// panics if pos >= buf.len()
#[inline]
pub fn decode_utf8_char(buf: &[u8], pos: usize) -> (char, usize) {
    let b0 = buf[pos];

    // ASCII fast path
    if b0 < 0x80 {
        return (b0 as char, 1);
    }

    // Determine expected sequence length from lead byte
    let (mut cp, expected) = if b0 < 0xC0 {
        // Stray continuation byte
        return ('\u{FFFD}', 1);
    } else if b0 < 0xE0 {
        ((b0 as u32) & 0x1F, 2)
    } else if b0 < 0xF0 {
        ((b0 as u32) & 0x0F, 3)
    } else if b0 < 0xF8 {
        ((b0 as u32) & 0x07, 4)
    } else {
        // Invalid lead byte
        return ('\u{FFFD}', 1);
    };

    // Check if we have enough bytes
    let len = buf.len();
    if pos + expected > len {
        return ('\u{FFFD}', len - pos);
    }

    // Decode continuation bytes
    for i in 1..expected {
        let cont = buf[pos + i];
        if cont & 0xC0 != 0x80 {
            // Invalid continuation byte
            return ('\u{FFFD}', i);
        }
        cp = (cp << 6) | (cont as u32 & 0x3F);
    }

    let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
    (ch, expected)
}

// iterator over UTF-8 characters in a byte slice
// invalid sequences yield U+FFFD
pub struct Utf8Iter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Utf8Iter<'a> {
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn position(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

impl Iterator for Utf8Iter<'_> {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        if self.pos >= self.data.len() {
            return None;
        }

        let (ch, len) = decode_utf8_char(self.data, self.pos);
        self.pos += len;
        Some(ch)
    }
}
