// Single-pass HTML to styled-text converter for EPUB XHTML.
// HtmlStripStream: streaming feed/finish; emits 2-byte [MARKER, tag] style codes.
// strip_html_inplace(): in-place variant for container.xml/OPF/TOC.
// Marker: [0x01, tag]. Inline: B/b I/i. Block: H/h Q/q S(hr).

use alloc::vec::Vec;

pub const MARKER: u8 = 0x01; // escape byte for style markers

pub const BOLD_ON: u8 = b'B';
pub const BOLD_OFF: u8 = b'b';
pub const ITALIC_ON: u8 = b'I';
pub const ITALIC_OFF: u8 = b'i';
pub const HEADING_ON: u8 = b'H';
pub const HEADING_OFF: u8 = b'h';
pub const QUOTE_ON: u8 = b'Q';
pub const QUOTE_OFF: u8 = b'q';

// Standalone
pub const BREAK: u8 = b'S';
pub const IMG_REF: u8 = b'P'; // image ref: [MARKER, IMG_REF, len, path...]

#[inline]
pub const fn is_marker(b: u8) -> bool {
    b == MARKER
}

const TAG_BUF_CAP: usize = 16;
const ENTITY_BUF_CAP: usize = 12;
const BANG_BUF_CAP: usize = 8;
const PENDING_CAP: usize = 16;
const DEFERRED_CAP: usize = 8;
const IMG_SRC_CAP: usize = 128; // 3-byte marker header + up to 125 path bytes

// state machine phases
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
enum Phase {
    Text,
    AfterLt,
    TagName,
    TagBody,
    Entity,
    SkipContent,
    SkipLt,
    SkipCloseName,
    SkipToGt,
    BangProbe,
    Comment,
    Cdata,
    Pi,
    BangOther,
    ImgBody,
    ImgAttrName,
    ImgAttrGap,
    ImgValStart,
    ImgSrcVal,
    ImgSkipVal,
}

impl Default for HtmlStripStream {
    fn default() -> Self {
        Self::new()
    }
}

// stateful streaming HTML-to-styled-text converter; ~80 bytes of state
pub struct HtmlStripStream {
    phase: Phase,

    // tag name accumulation
    tag_buf: [u8; TAG_BUF_CAP],
    tag_len: u8,
    is_close_tag: bool,
    enter_skip: bool, // tag is skip-content; enter SkipContent on >

    // entity accumulation
    entity_buf: [u8; ENTITY_BUF_CAP],
    entity_len: u8,

    // skip content
    skip_target: Option<SkipTag>,
    skip_match: bool, // in SkipToGt: did close tag name match?

    // bang construct probing
    bang_buf: [u8; BANG_BUF_CAP],
    bang_len: u8,

    // terminator matching (comment / CDATA / PI)
    match_pos: u8,

    // output state
    last_was_space: bool,
    trailing_nl: u8, // deferred newlines; flushed before next visible byte; capped at 2
    has_output: bool, // true once any visible char emitted; suppresses leading whitespace

    // deferred open-style markers; open-tag markers (bold on, heading on, etc.)
    // appear AFTER paragraph-break newlines and BEFORE text.
    // close-tag markers go to `pending` immediately (before paragraph newlines).
    deferred: [u8; DEFERRED_CAP],
    deferred_len: u8,

    // pending output: bytes queued by classify_tag or queue_text not yet
    // drained to the caller's output slice
    pending: [u8; PENDING_CAP],
    pend_w: u8,
    pend_r: u8,

    // image src capture (<img src="...">)
    img_src: [u8; IMG_SRC_CAP],
    img_w: u8,         // write cursor (accumulation at [3..]) / drain length
    img_r: u8,         // drain read cursor
    capture_img: bool, // true while inside <img> tag body
    img_is_src: bool,  // current attribute name matched "src"
    img_quote: u8,     // active quote char in attribute value
}

impl HtmlStripStream {
    pub const fn new() -> Self {
        Self {
            phase: Phase::Text,
            tag_buf: [0u8; TAG_BUF_CAP],
            tag_len: 0,
            is_close_tag: false,
            enter_skip: false,
            entity_buf: [0u8; ENTITY_BUF_CAP],
            entity_len: 0,
            skip_target: None,
            skip_match: false,
            bang_buf: [0u8; BANG_BUF_CAP],
            bang_len: 0,
            match_pos: 0,
            last_was_space: true,
            trailing_nl: 0,
            has_output: false,
            deferred: [0u8; DEFERRED_CAP],
            deferred_len: 0,
            pending: [0u8; PENDING_CAP],
            pend_w: 0,
            pend_r: 0,
            img_src: [0u8; IMG_SRC_CAP],
            img_w: 0,
            img_r: 0,
            capture_img: false,
            img_is_src: false,
            img_quote: 0,
        }
    }

    // process a chunk of HTML; returns (consumed, written); call again if input not fully consumed
    pub fn feed(&mut self, input: &[u8], output: &mut [u8]) -> (usize, usize) {
        let ilen = input.len();
        let olen = output.len();
        let mut ip: usize = 0;
        let mut op: usize = 0;

        loop {
            // step 1: drain pending bytes to output
            while self.pend_r < self.pend_w {
                if op >= olen {
                    return (ip, op);
                }
                output[op] = self.pending[self.pend_r as usize];
                op += 1;
                self.pend_r += 1;
            }
            self.pend_r = 0;
            self.pend_w = 0;

            // step 1.5: drain image reference
            if !self.capture_img && self.img_r < self.img_w {
                while self.img_r < self.img_w {
                    if op >= olen {
                        return (ip, op);
                    }
                    output[op] = self.img_src[self.img_r as usize];
                    op += 1;
                    self.img_r += 1;
                }
                self.img_r = 0;
                self.img_w = 0;
            }

            // step 2: check for end of input
            if ip >= ilen {
                return (ip, op);
            }

            // step 3: process one input byte
            let b = input[ip];
            let mut advance = true;

            match self.phase {
                // normal text
                Phase::Text => {
                    if b == MARKER {
                        // literal 0x01 in source; drop silently (SOH never in real EPUBs)
                    } else if b == b'<' {
                        self.phase = Phase::AfterLt;
                    } else if b == b'&' {
                        self.entity_len = 0;
                        self.phase = Phase::Entity;
                    } else if is_html_ws(b) {
                        self.queue_ws();
                    } else {
                        self.queue_text(b);
                    }
                }

                // after '<'
                Phase::AfterLt => match b {
                    b'!' => {
                        self.bang_len = 0;
                        self.phase = Phase::BangProbe;
                    }
                    b'?' => {
                        self.match_pos = 0;
                        self.phase = Phase::Pi;
                    }
                    b'/' => {
                        self.is_close_tag = true;
                        self.tag_len = 0;
                        self.enter_skip = false;
                        self.phase = Phase::TagName;
                    }
                    b'>' => {
                        // empty <>; ignore
                        self.phase = Phase::Text;
                    }
                    _ => {
                        self.is_close_tag = false;
                        self.tag_len = 0;
                        self.enter_skip = false;
                        self.phase = Phase::TagName;
                        advance = false; // reprocess in TagName
                    }
                },

                // accumulating tag name
                Phase::TagName => {
                    if is_tag_delim(b) {
                        self.classify_tag();

                        if b == b'>' {
                            if self.capture_img {
                                self.finish_img_tag();
                            } else {
                                self.finish_tag();
                            }
                        } else if self.capture_img {
                            self.phase = Phase::ImgBody;
                        } else {
                            self.phase = Phase::TagBody;
                        }
                    } else if (self.tag_len as usize) < TAG_BUF_CAP {
                        self.tag_buf[self.tag_len as usize] = b.to_ascii_lowercase();
                        self.tag_len += 1;
                    }
                    // overflow: stop accumulating, keep scanning for delimiter
                }

                // past tag name; skip attributes to '>'
                Phase::TagBody => {
                    if b == b'>' {
                        self.finish_tag();
                    }
                }

                // <img> attribute parsing; capture src="..."
                Phase::ImgBody => {
                    if b == b'>' {
                        self.finish_img_tag();
                    } else if b.is_ascii_alphabetic() || b == b'_' {
                        self.tag_len = 0;
                        self.tag_buf[0] = b.to_ascii_lowercase();
                        self.tag_len = 1;
                        self.phase = Phase::ImgAttrName;
                    }
                    // whitespace, '/', etc: stay
                }

                Phase::ImgAttrName => {
                    if b == b'=' {
                        let name = &self.tag_buf[..self.tag_len as usize];
                        self.img_is_src = name == b"src";
                        self.phase = Phase::ImgValStart;
                    } else if b == b'>' {
                        self.finish_img_tag();
                    } else if is_html_ws(b) || b == b'/' {
                        self.phase = Phase::ImgAttrGap;
                    } else if (self.tag_len as usize) < TAG_BUF_CAP {
                        self.tag_buf[self.tag_len as usize] = b.to_ascii_lowercase();
                        self.tag_len += 1;
                    }
                }

                Phase::ImgAttrGap => {
                    if b == b'=' {
                        let name = &self.tag_buf[..self.tag_len as usize];
                        self.img_is_src = name == b"src";
                        self.phase = Phase::ImgValStart;
                    } else if b == b'>' {
                        self.finish_img_tag();
                    } else if b.is_ascii_alphabetic() || b == b'_' {
                        self.tag_len = 0;
                        self.tag_buf[0] = b.to_ascii_lowercase();
                        self.tag_len = 1;
                        self.phase = Phase::ImgAttrName;
                    }
                    // whitespace, '/': stay
                }

                Phase::ImgValStart => {
                    if b == b'"' || b == b'\'' {
                        self.img_quote = b;
                        self.phase = if self.img_is_src {
                            Phase::ImgSrcVal
                        } else {
                            Phase::ImgSkipVal
                        };
                    } else if b == b'>' {
                        self.finish_img_tag();
                    } else if !is_html_ws(b) {
                        // unquoted attribute value
                        self.img_quote = 0;
                        if self.img_is_src {
                            let pos = self.img_w as usize;
                            if pos < IMG_SRC_CAP {
                                self.img_src[pos] = b;
                                self.img_w += 1;
                            }
                            self.phase = Phase::ImgSrcVal;
                        } else {
                            self.phase = Phase::ImgSkipVal;
                        }
                    }
                }

                Phase::ImgSrcVal => {
                    let done = if self.img_quote != 0 {
                        b == self.img_quote
                    } else {
                        is_html_ws(b) || b == b'>' || b == b'/'
                    };
                    if done {
                        self.phase = Phase::ImgBody;
                        if self.img_quote == 0 && b == b'>' {
                            self.finish_img_tag();
                        }
                    } else {
                        let pos = self.img_w as usize;
                        if pos < IMG_SRC_CAP {
                            self.img_src[pos] = b;
                            self.img_w += 1;
                        }
                    }
                }

                Phase::ImgSkipVal => {
                    let done = if self.img_quote != 0 {
                        b == self.img_quote
                    } else {
                        is_html_ws(b) || b == b'>' || b == b'/'
                    };
                    if done {
                        self.phase = Phase::ImgBody;
                        if self.img_quote == 0 && b == b'>' {
                            self.finish_img_tag();
                        }
                    }
                }

                // entity accumulation
                Phase::Entity => {
                    if b == b';' {
                        let name = &self.entity_buf[..self.entity_len as usize];
                        match resolve_entity(name) {
                            Some(b'\n') => {
                                self.trailing_nl = self.trailing_nl.saturating_add(1).min(2);
                                self.last_was_space = true;
                            }
                            Some(c) if is_html_ws(c) => {
                                self.queue_ws();
                            }
                            Some(c) => {
                                self.queue_text(c);
                            }
                            None => {
                                // unrecognised entity; emit literal '&'
                                self.queue_text(b'&');
                            }
                        }
                        self.phase = Phase::Text;
                    } else if is_entity_char(b) && (self.entity_len as usize) < ENTITY_BUF_CAP {
                        self.entity_buf[self.entity_len as usize] = b;
                        self.entity_len += 1;
                    } else {
                        // invalid char or overflow; emit literal '&'
                        self.queue_text(b'&');
                        self.phase = Phase::Text;
                        advance = false; // reprocess this byte as text
                    }
                }

                // skip content (script / style / head)
                Phase::SkipContent => {
                    if b == b'<' {
                        self.phase = Phase::SkipLt;
                    }
                }

                Phase::SkipLt => {
                    if b == b'/' {
                        self.tag_len = 0;
                        self.phase = Phase::SkipCloseName;
                    } else {
                        self.phase = Phase::SkipContent;
                    }
                }

                Phase::SkipCloseName => {
                    if is_tag_delim(b) || b == b'>' {
                        let matched = if let Some(target) = self.skip_target {
                            let tgt = target.name();
                            let name = &self.tag_buf[..self.tag_len as usize];
                            name.len() == tgt.len()
                                && name.iter().zip(tgt.iter()).all(|(a, t)| *a == *t)
                        } else {
                            false
                        };

                        if b == b'>' {
                            if matched {
                                self.skip_target = None;
                                self.phase = Phase::Text;
                            } else {
                                self.phase = Phase::SkipContent;
                            }
                        } else {
                            self.skip_match = matched;
                            self.phase = Phase::SkipToGt;
                        }
                    } else if (self.tag_len as usize) < TAG_BUF_CAP {
                        self.tag_buf[self.tag_len as usize] = b.to_ascii_lowercase();
                        self.tag_len += 1;
                    }
                }

                Phase::SkipToGt => {
                    if b == b'>' {
                        if self.skip_match {
                            self.skip_target = None;
                            self.phase = Phase::Text;
                        } else {
                            self.phase = Phase::SkipContent;
                        }
                    }
                }

                // bang construct probing (after '<!')
                Phase::BangProbe => {
                    if b == b'>' {
                        self.phase = Phase::Text;
                    } else {
                        let pos = self.bang_len as usize;
                        if pos < BANG_BUF_CAP {
                            self.bang_buf[pos] = b;
                            self.bang_len += 1;
                        }
                        let n = self.bang_len as usize;

                        if n == 1 {
                            match b {
                                b'-' | b'[' => {}
                                _ => self.phase = Phase::BangOther,
                            }
                        } else if self.bang_buf[0] == b'-' {
                            if n == 2 && b == b'-' {
                                self.match_pos = 0;
                                self.phase = Phase::Comment;
                            } else {
                                self.phase = Phase::BangOther;
                            }
                        } else {
                            // bang_buf[0] == '[': check against "[CDATA["
                            const CDATA: &[u8] = b"[CDATA[";
                            if n <= CDATA.len() && b == CDATA[n - 1] {
                                if n == CDATA.len() {
                                    self.match_pos = 0;
                                    self.phase = Phase::Cdata;
                                }
                            } else {
                                self.phase = Phase::BangOther;
                            }
                        }
                    }
                }

                // comment: scanning for '-->'
                Phase::Comment => match self.match_pos {
                    0 => {
                        if b == b'-' {
                            self.match_pos = 1;
                        }
                    }
                    1 => {
                        if b == b'-' {
                            self.match_pos = 2;
                        } else {
                            self.match_pos = 0;
                        }
                    }
                    _ => {
                        if b == b'>' {
                            self.phase = Phase::Text;
                        } else if b != b'-' {
                            self.match_pos = 0;
                        }
                    }
                },

                // CDATA: scanning for ']]>'
                Phase::Cdata => match self.match_pos {
                    0 => {
                        if b == b']' {
                            self.match_pos = 1;
                        }
                    }
                    1 => {
                        if b == b']' {
                            self.match_pos = 2;
                        } else {
                            self.match_pos = 0;
                        }
                    }
                    _ => {
                        if b == b'>' {
                            self.phase = Phase::Text;
                        } else if b != b']' {
                            self.match_pos = 0;
                        }
                    }
                },

                // PI: scanning for '?>'
                Phase::Pi => match self.match_pos {
                    0 => {
                        if b == b'?' {
                            self.match_pos = 1;
                        }
                    }
                    _ => {
                        if b == b'>' {
                            self.phase = Phase::Text;
                        } else if b != b'?' {
                            self.match_pos = 0;
                        }
                    }
                },

                // other bang construct: scanning for '>'
                Phase::BangOther => {
                    if b == b'>' {
                        self.phase = Phase::Text;
                    }
                }
            }

            if advance {
                ip += 1;
            }
        }
    }

    // flush pending state; append terminal newline if content was produced; return bytes written
    pub fn finish(&mut self, output: &mut [u8]) -> usize {
        let mut op: usize = 0;

        // drain remaining pending bytes
        while self.pend_r < self.pend_w && op < output.len() {
            output[op] = self.pending[self.pend_r as usize];
            op += 1;
            self.pend_r += 1;
        }
        self.pend_r = 0;
        self.pend_w = 0;

        // terminal newline
        if self.has_output && op < output.len() {
            output[op] = b'\n';
            op += 1;
        }

        self.phase = Phase::Text;
        op
    }

    #[inline]
    fn push_pending(&mut self, byte: u8) {
        let w = self.pend_w as usize;
        if w < PENDING_CAP {
            self.pending[w] = byte;
            self.pend_w += 1;
        }
    }

    fn push_deferred_marker(&mut self, tag: u8) {
        let n = self.deferred_len as usize;
        if n + 2 <= DEFERRED_CAP {
            self.deferred[n] = MARKER;
            self.deferred[n + 1] = tag;
            self.deferred_len += 2;
        }
    }

    // queue visible text byte; flush deferred newlines and style markers first
    fn queue_text(&mut self, b: u8) {
        // deferred newlines
        if self.has_output && self.trailing_nl > 0 {
            let nl = self.trailing_nl;
            for _ in 0..nl {
                self.push_pending(b'\n');
            }
        }
        self.trailing_nl = 0;

        // deferred open-style markers
        let dlen = self.deferred_len as usize;
        for i in 0..dlen {
            self.push_pending(self.deferred[i]);
        }
        self.deferred_len = 0;

        self.push_pending(b);
        self.last_was_space = false;
        self.has_output = true;
    }

    // handle whitespace byte; collapse runs to a single space
    fn queue_ws(&mut self) {
        if self.last_was_space || !self.has_output {
            return;
        }
        self.last_was_space = true;

        // pending newlines already act as word separators
        if self.trailing_nl > 0 {
            return;
        }

        self.push_pending(b' ');
    }

    // classify accumulated tag name; push close markers to pending, open to deferred
    fn classify_tag(&mut self) {
        // copy tag name to a local to avoid borrowing self.tag_buf
        // while mutating self through push_pending / push_deferred
        let mut tn = [0u8; TAG_BUF_CAP];
        let tn_len = self.tag_len as usize;
        tn[..tn_len].copy_from_slice(&self.tag_buf[..tn_len]);
        let name = &tn[..tn_len];
        let is_close = self.is_close_tag;

        // skip-content tags (script/style/head); open only
        if !is_close && let Some(sk) = SkipTag::from_name(name) {
            self.skip_target = Some(sk);
            self.enter_skip = true;
        }

        // close-tag markers go out immediately (before deferred newlines)
        if is_close && let Some(m) = close_style_tag(name) {
            self.push_pending(MARKER);
            self.push_pending(m);
        }

        // block elements set deferred paragraph breaks
        if is_block_element(name) {
            self.trailing_nl = self.trailing_nl.max(2);
            self.last_was_space = true;
        }

        // open-tag markers are deferred (after newlines, before text);
        // inline markers too: <p><b>text -> \n\n[B]text, not [B]\n\ntext
        if !is_close && let Some(m) = open_style_tag(name) {
            self.push_deferred_marker(m);
        }

        // <br>: line break
        if name == b"br" {
            self.trailing_nl = self.trailing_nl.saturating_add(1).min(2);
            self.last_was_space = true;
        }

        // <hr>: scene break marker (deferred)
        if name == b"hr" && !is_close {
            self.push_deferred_marker(BREAK);
        }

        // <img>: enter image-src capture mode
        if name == b"img" && !is_close {
            self.capture_img = true;
            self.img_w = 3; // reserve [0..3] for marker header
            self.img_is_src = false;
        }
    }

    // transition out of TagName/TagBody on '>'
    fn finish_tag(&mut self) {
        if self.enter_skip {
            self.enter_skip = false;
            self.phase = Phase::SkipContent;
        } else {
            self.phase = Phase::Text;
        }
    }

    // finish <img> tag; emit image-ref marker if src was captured
    fn finish_img_tag(&mut self) {
        let path_len = (self.img_w as usize).saturating_sub(3);
        self.capture_img = false;

        if path_len > 0 {
            // block break before image
            if self.has_output {
                self.trailing_nl = self.trailing_nl.max(2);
            }
            // emit deferred newlines
            if self.has_output && self.trailing_nl > 0 {
                let nl = self.trailing_nl;
                for _ in 0..nl {
                    self.push_pending(b'\n');
                }
            }
            self.trailing_nl = 0;
            // flush deferred open-style markers
            let dlen = self.deferred_len as usize;
            for i in 0..dlen {
                self.push_pending(self.deferred[i]);
            }
            self.deferred_len = 0;
            // fill marker header [MARKER, IMG_REF, path_len];
            // path bytes already at img_src[3..3+path_len]
            self.img_src[0] = MARKER;
            self.img_src[1] = IMG_REF;
            self.img_src[2] = path_len as u8;
            self.img_r = 0;
            // block break after image
            self.trailing_nl = 2;
            self.last_was_space = true;
            self.has_output = true;
        } else {
            self.img_w = 0;
        }

        self.phase = Phase::Text;
    }
}

// in-place HTML stripper: operates on a complete buffer, produces plain text
// without style markers. write cursor never passes read cursor (w <= r always).
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

            if !is_close && let Some(sk) = SkipTag::from_name(tag) {
                skip_until = Some(sk);
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
            let (decoded, advance) = decode_entity_inplace(buf, r);
            r += advance;

            match decoded {
                DecodedInplace::Byte(b'\n') => {
                    buf[w] = b'\n';
                    w += 1;
                    trailing_nl = trailing_nl.saturating_add(1);
                    last_was_space = true;
                }
                DecodedInplace::Byte(c) if is_html_ws(c) => {
                    if !last_was_space {
                        buf[w] = b' ';
                        w += 1;
                        last_was_space = true;
                        trailing_nl = 0;
                    }
                }
                DecodedInplace::Byte(c) => {
                    buf[w] = c;
                    w += 1;
                    last_was_space = false;
                    trailing_nl = 0;
                }

                DecodedInplace::None => {
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

// marker byte for formatting tags; used by classify_tag
fn open_style_tag(tag: &[u8]) -> Option<u8> {
    match tag {
        b"b" | b"strong" => Some(BOLD_ON),
        b"i" | b"em" | b"cite" => Some(ITALIC_ON),
        b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" => Some(HEADING_ON),
        b"blockquote" => Some(QUOTE_ON),
        _ => None,
    }
}

fn close_style_tag(tag: &[u8]) -> Option<u8> {
    match tag {
        b"b" | b"strong" => Some(BOLD_OFF),
        b"i" | b"em" | b"cite" => Some(ITALIC_OFF),
        b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" => Some(HEADING_OFF),
        b"blockquote" => Some(QUOTE_OFF),
        _ => None,
    }
}

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

// resolve entity name to output byte; None for unrecognised
fn resolve_entity(name: &[u8]) -> Option<u8> {
    match name {
        b"amp" => Some(b'&'),
        b"lt" => Some(b'<'),
        b"gt" => Some(b'>'),
        b"quot" => Some(b'"'),
        b"apos" => Some(b'\''),
        b"nbsp" => Some(b' '),
        b"mdash" | b"emdash" => Some(b'-'),
        b"ndash" | b"endash" => Some(b'-'),
        b"lsquo" | b"rsquo" | b"sbquo" => Some(b'\''),
        b"ldquo" | b"rdquo" | b"bdquo" => Some(b'"'),
        b"hellip" => Some(b'.'),
        b"copy" => Some(b'c'),
        b"reg" => Some(b'R'),
        b"trade" => Some(b'T'),
        b"times" => Some(b'x'),
        b"divide" => Some(b'/'),
        b"deg" => Some(b'*'),
        b"plusmn" => Some(b'+'),
        b"frac12" | b"frac14" | b"frac34" => Some(b'/'),
        _ => {
            if name.starts_with(b"#x") || name.starts_with(b"#X") {
                codepoint_to_byte(parse_hex(&name[2..]))
            } else if name.starts_with(b"#") {
                codepoint_to_byte(parse_decimal(&name[1..]))
            } else {
                None
            }
        }
    }
}

fn codepoint_to_byte(cp: u32) -> Option<u8> {
    match cp {
        0 => None,
        0x0001..=0x007F => Some(cp as u8),
        0x00A0 => Some(b' '), // nbsp
        0x00AD => Some(b'-'), // soft hyphen
        0x2013 | 0x2014 => Some(b'-'),
        0x2018..=0x201A => Some(b'\''),
        0x201C..=0x201E => Some(b'"'),
        0x2022 => Some(b'*'),
        0x2026 => Some(b'.'),
        _ => Some(b'?'), // unicode placeholder
    }
}

// in-place entity decoding; separate from resolve_entity
enum DecodedInplace {
    Byte(u8),
    None,
}

fn decode_entity_inplace(input: &[u8], pos: usize) -> (DecodedInplace, usize) {
    debug_assert!(input[pos] == b'&');

    let remaining = &input[pos + 1..];
    let max_scan = remaining.len().min(12);
    let semi = remaining[..max_scan].iter().position(|&b| b == b';');

    let Some(semi) = semi else {
        return (DecodedInplace::None, 1);
    };

    let entity = &remaining[..semi];
    let advance = 1 + semi + 1;

    let decoded = match entity {
        b"amp" => DecodedInplace::Byte(b'&'),
        b"lt" => DecodedInplace::Byte(b'<'),
        b"gt" => DecodedInplace::Byte(b'>'),
        b"quot" => DecodedInplace::Byte(b'"'),
        b"apos" => DecodedInplace::Byte(b'\''),
        b"nbsp" => DecodedInplace::Byte(b' '),
        b"mdash" | b"emdash" => DecodedInplace::Byte(b'-'),
        b"ndash" | b"endash" => DecodedInplace::Byte(b'-'),
        b"lsquo" | b"rsquo" | b"sbquo" => DecodedInplace::Byte(b'\''),
        b"ldquo" | b"rdquo" | b"bdquo" => DecodedInplace::Byte(b'"'),
        b"hellip" => DecodedInplace::Byte(b'.'),
        b"copy" => DecodedInplace::Byte(b'c'),
        b"reg" => DecodedInplace::Byte(b'R'),
        b"trade" => DecodedInplace::Byte(b'T'),
        b"times" => DecodedInplace::Byte(b'x'),
        b"divide" => DecodedInplace::Byte(b'/'),
        b"deg" => DecodedInplace::Byte(b'*'),
        b"plusmn" => DecodedInplace::Byte(b'+'),
        b"frac12" | b"frac14" | b"frac34" => DecodedInplace::Byte(b'/'),
        _ => {
            if entity.starts_with(b"#x") || entity.starts_with(b"#X") {
                codepoint_to_decoded_inplace(parse_hex(&entity[2..]))
            } else if entity.starts_with(b"#") {
                codepoint_to_decoded_inplace(parse_decimal(&entity[1..]))
            } else {
                DecodedInplace::None
            }
        }
    };

    (decoded, advance)
}

fn codepoint_to_decoded_inplace(cp: u32) -> DecodedInplace {
    match cp {
        0 => DecodedInplace::None,
        0x0001..=0x007F => DecodedInplace::Byte(cp as u8),
        0x00A0 => DecodedInplace::Byte(b' '),
        0x00AD => DecodedInplace::Byte(b'-'),
        0x2013 | 0x2014 => DecodedInplace::Byte(b'-'),
        0x2018..=0x201A => DecodedInplace::Byte(b'\''),
        0x201C..=0x201E => DecodedInplace::Byte(b'"'),
        0x2022 => DecodedInplace::Byte(b'*'),
        0x2026 => DecodedInplace::Byte(b'.'),
        _ => DecodedInplace::Byte(b'?'),
    }
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

#[inline]
fn is_html_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

#[inline]
fn is_tag_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

#[inline]
fn is_entity_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'#'
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
