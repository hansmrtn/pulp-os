// Minimal CSS parser for EPUB stylesheets.
// Selectors: tag, .class, tag.class, grouped. Combinators reduced to
// rightmost simple selector. @-rules and pseudo-classes skipped.
// Rule table stack-allocated: MAX_CSS_RULES x ~16B = 2KB.

pub const MAX_CSS_RULES: usize = 128;

// property flag bits (which fields in StyleProps are explicitly set)

pub const PROP_FONT_WEIGHT: u16 = 1 << 0;
pub const PROP_FONT_STYLE: u16 = 1 << 1;
pub const PROP_TEXT_ALIGN: u16 = 1 << 2;
pub const PROP_TEXT_INDENT: u16 = 1 << 3;
pub const PROP_MARGIN_LEFT: u16 = 1 << 4;
pub const PROP_MARGIN_RIGHT: u16 = 1 << 5;
pub const PROP_MARGIN_TOP: u16 = 1 << 6;
pub const PROP_MARGIN_BOTTOM: u16 = 1 << 7;
pub const PROP_DISPLAY: u16 = 1 << 8;
pub const PROP_TEXT_DECORATION: u16 = 1 << 9;

// property value constants

// font-weight
pub const FW_NORMAL: u8 = 0;
pub const FW_BOLD: u8 = 1;

// font-style
pub const FS_NORMAL: u8 = 0;
pub const FS_ITALIC: u8 = 1;

// text-align
pub const TA_LEFT: u8 = 0;
pub const TA_CENTER: u8 = 1;
pub const TA_RIGHT: u8 = 2;
pub const TA_JUSTIFY: u8 = 3;

// display
pub const DISP_DEFAULT: u8 = 0;
pub const DISP_NONE: u8 = 1;
pub const DISP_BLOCK: u8 = 2;
pub const DISP_INLINE: u8 = 3;

// text-decoration (bitmask)
pub const TD_NONE: u8 = 0;
pub const TD_UNDERLINE: u8 = 1;
pub const TD_LINE_THROUGH: u8 = 2;

// resolved CSS properties; `set` tracks which are explicitly specified.
// Lengths in quarter-em units (i8): 1em = 4, 0.5em = 2, 2em = 8.

#[derive(Clone, Copy)]
pub struct StyleProps {
    pub set: u16,
    pub font_weight: u8,
    pub font_style: u8,
    pub text_align: u8,
    pub text_indent: i8,
    pub margin_left: i8,
    pub margin_right: i8,
    pub margin_top: i8,
    pub margin_bottom: i8,
    pub display: u8,
    pub text_decoration: u8,
}

impl StyleProps {
    pub const EMPTY: Self = Self {
        set: 0,
        font_weight: FW_NORMAL,
        font_style: FS_NORMAL,
        text_align: TA_LEFT,
        text_indent: 0,
        margin_left: 0,
        margin_right: 0,
        margin_top: 0,
        margin_bottom: 0,
        display: DISP_DEFAULT,
        text_decoration: TD_NONE,
    };

    // merge other's properties; only override if specificity >= best seen
    fn apply(&mut self, other: &Self, spec: u8, best: &mut [u8; 16]) {
        macro_rules! merge {
            ($field:ident, $bit:expr) => {
                if other.set & (1 << $bit) != 0 && spec >= best[$bit] {
                    self.$field = other.$field;
                    self.set |= 1 << $bit;
                    best[$bit] = spec;
                }
            };
        }
        merge!(font_weight, 0);
        merge!(font_style, 1);
        merge!(text_align, 2);
        merge!(text_indent, 3);
        merge!(margin_left, 4);
        merge!(margin_right, 5);
        merge!(margin_top, 6);
        merge!(margin_bottom, 7);
        merge!(display, 8);
        merge!(text_decoration, 9);
    }

    #[inline]
    pub fn is_bold(&self) -> bool {
        self.set & PROP_FONT_WEIGHT != 0 && self.font_weight == FW_BOLD
    }

    #[inline]
    pub fn is_italic(&self) -> bool {
        self.set & PROP_FONT_STYLE != 0 && self.font_style == FS_ITALIC
    }

    #[inline]
    pub fn is_hidden(&self) -> bool {
        self.set & PROP_DISPLAY != 0 && self.display == DISP_NONE
    }
}

// selector

#[derive(Clone, Copy)]
struct Selector {
    tag: u8,         // tag_id(); 0 = any
    class_hash: u16, // class_hash(); 0 = any
    specificity: u8, // tag=1, class=16, tag+class=17
}

impl Selector {
    const EMPTY: Self = Self {
        tag: 0,
        class_hash: 0,
        specificity: 0,
    };

    fn matches(&self, elem_tag: u8, elem_class: u16) -> bool {
        let tag_ok = self.tag == 0 || self.tag == elem_tag;
        let cls_ok = self.class_hash == 0 || self.class_hash == elem_class;
        tag_ok && cls_ok
    }
}

// CssRule + CssRules

#[derive(Clone, Copy)]
struct CssRule {
    sel: Selector,     // 4 bytes
    props: StyleProps, // 12 bytes
}

impl CssRule {
    const EMPTY: Self = Self {
        sel: Selector::EMPTY,
        props: StyleProps::EMPTY,
    };
}

// parsed CSS rule table, stack-allocated (~2KB)
pub struct CssRules {
    rules: [CssRule; MAX_CSS_RULES],
    count: usize,
}

impl Default for CssRules {
    fn default() -> Self {
        Self::new()
    }
}

impl CssRules {
    pub const fn new() -> Self {
        Self {
            rules: [CssRule::EMPTY; MAX_CSS_RULES],
            count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.count = 0;
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    // parse stylesheet; may be called multiple times to accumulate rules
    pub fn parse(&mut self, css: &[u8]) {
        let mut pos: usize = 0;

        while pos < css.len() {
            pos = skip_ws_comments(css, pos);
            if pos >= css.len() {
                break;
            }

            // skip @-rules (may contain nested blocks)
            if css[pos] == b'@' {
                pos = skip_at_rule(css, pos);
                continue;
            }

            // selector(s) run until '{'
            let sel_start = pos;
            let Some(brace) = scan_to_byte(css, pos, b'{') else {
                break;
            };
            let sel_text = &css[sel_start..brace];
            pos = brace + 1;

            // declarations run until '}'
            let Some(end) = scan_to_byte(css, pos, b'}') else {
                break;
            };
            let decl_text = &css[pos..end];
            pos = end + 1;

            // parse the declaration block
            let props = parse_declarations(decl_text);
            if props.set == 0 {
                continue; // no usable properties
            }

            // split grouped selectors on ',' and add a rule for each
            for sel_part in sel_text.split(|&b| b == b',') {
                let sel = parse_selector(sel_part);
                if sel.specificity == 0 && sel.tag == 0 && sel.class_hash == 0 {
                    continue; // unparseable
                }
                if self.count < MAX_CSS_RULES {
                    self.rules[self.count] = CssRule { sel, props };
                    self.count += 1;
                }
            }
        }
    }

    // resolve effective style for tag + class; merged by specificity
    pub fn resolve(&self, tag_name: &[u8], class_name: &[u8]) -> StyleProps {
        let tid = tag_id(tag_name);
        let chash = if class_name.is_empty() {
            0
        } else {
            class_hash(class_name)
        };

        let mut result = StyleProps::EMPTY;
        let mut best = [0u8; 16];

        for rule in &self.rules[..self.count] {
            if rule.sel.matches(tid, chash) {
                result.apply(&rule.props, rule.sel.specificity, &mut best);
            }
        }

        result
    }

    // resolve by pre-computed tag ID and class hash
    pub fn resolve_by_id(&self, tid: u8, chash: u16) -> StyleProps {
        let mut result = StyleProps::EMPTY;
        let mut best = [0u8; 16];

        for rule in &self.rules[..self.count] {
            if rule.sel.matches(tid, chash) {
                result.apply(&rule.props, rule.sel.specificity, &mut best);
            }
        }

        result
    }
}

// CSS parser internals

// parse a single (possibly compound) selector
fn parse_selector(raw: &[u8]) -> Selector {
    let raw = trim_css(raw);
    if raw.is_empty() {
        return Selector::EMPTY;
    }

    // take only the rightmost simple selector (ignore descendant/child combinators)
    let mut last_start = 0;
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b' ' || raw[i] == b'>' || raw[i] == b'+' || raw[i] == b'~' {
            let next = i + 1;
            // skip whitespace after combinator
            let mut j = next;
            while j < raw.len() && raw[j] == b' ' {
                j += 1;
            }
            if j < raw.len() {
                last_start = j;
            }
            i = j;
        } else {
            i += 1;
        }
    }

    let sel = trim_css(&raw[last_start..]);
    if sel.is_empty() || sel == b"*" {
        return Selector::EMPTY;
    }

    // strip pseudo-classes/elements (:hover, ::before, etc.)
    let sel = if let Some(p) = sel.iter().position(|&b| b == b':') {
        &sel[..p]
    } else {
        sel
    };

    // strip #id (take everything before '#')
    let sel = if let Some(p) = sel.iter().position(|&b| b == b'#') {
        if p == 0 {
            // bare #id selector; can't match by tag/class, skip
            return Selector::EMPTY;
        }
        &sel[..p]
    } else {
        sel
    };

    // split tag.class
    let (tag_part, class_part) = if let Some(dot) = sel.iter().position(|&b| b == b'.') {
        (&sel[..dot], &sel[dot + 1..])
    } else {
        (sel, &[] as &[u8])
    };

    let tid = if tag_part.is_empty() {
        0
    } else {
        tag_id(tag_part)
    };
    let chash = if class_part.is_empty() {
        0
    } else {
        class_hash(class_part)
    };

    let specificity = match (tid != 0, chash != 0) {
        (false, false) => 0,
        (true, false) => 1,  // tag only
        (false, true) => 16, // class only
        (true, true) => 17,  // tag + class
    };

    Selector {
        tag: tid,
        class_hash: chash,
        specificity,
    }
}

// parse declaration block (between { and })
fn parse_declarations(block: &[u8]) -> StyleProps {
    let mut props = StyleProps::EMPTY;

    // split on ';', handle each property:value pair
    for decl in block.split(|&b| b == b';') {
        let decl = trim_css(decl);
        if decl.is_empty() {
            continue;
        }

        // split on first ':'
        let Some(colon) = decl.iter().position(|&b| b == b':') else {
            continue;
        };
        let prop_name = trim_css(&decl[..colon]);
        let prop_value = trim_css(&decl[colon + 1..]);

        if prop_name.is_empty() || prop_value.is_empty() {
            continue;
        }

        parse_property(prop_name, prop_value, &mut props);
    }

    props
}

// map CSS property name + value to StyleProps fields
fn parse_property(name: &[u8], value: &[u8], props: &mut StyleProps) {
    match name {
        b"font-weight" => {
            props.font_weight = match value {
                v if v.starts_with(b"bold") => FW_BOLD,
                v if starts_with_digit(v) && parse_int(v) >= 600 => FW_BOLD,
                _ => FW_NORMAL,
            };
            props.set |= PROP_FONT_WEIGHT;
        }

        b"font-style" => {
            props.font_style = if value.starts_with(b"italic") || value.starts_with(b"oblique") {
                FS_ITALIC
            } else {
                FS_NORMAL
            };
            props.set |= PROP_FONT_STYLE;
        }

        b"text-align" => {
            props.text_align = match value {
                v if v.starts_with(b"center") => TA_CENTER,
                v if v.starts_with(b"right") => TA_RIGHT,
                v if v.starts_with(b"justify") => TA_JUSTIFY,
                _ => TA_LEFT,
            };
            props.set |= PROP_TEXT_ALIGN;
        }

        b"text-indent" => {
            props.text_indent = parse_length_qem(value);
            props.set |= PROP_TEXT_INDENT;
        }

        b"margin-left" | b"padding-left" => {
            props.margin_left = parse_length_qem(value);
            props.set |= PROP_MARGIN_LEFT;
        }

        b"margin-right" | b"padding-right" => {
            props.margin_right = parse_length_qem(value);
            props.set |= PROP_MARGIN_RIGHT;
        }

        b"margin-top" | b"padding-top" => {
            props.margin_top = parse_length_qem(value);
            props.set |= PROP_MARGIN_TOP;
        }

        b"margin-bottom" | b"padding-bottom" => {
            props.margin_bottom = parse_length_qem(value);
            props.set |= PROP_MARGIN_BOTTOM;
        }

        b"display" => {
            props.display = match value {
                v if v.starts_with(b"none") => DISP_NONE,
                v if v.starts_with(b"block") => DISP_BLOCK,
                v if v.starts_with(b"inline") => DISP_INLINE,
                _ => DISP_DEFAULT,
            };
            props.set |= PROP_DISPLAY;
        }

        b"text-decoration" | b"text-decoration-line" => {
            props.text_decoration = if value.starts_with(b"underline") {
                TD_UNDERLINE
            } else if value.starts_with(b"line-through") {
                TD_LINE_THROUGH
            } else {
                TD_NONE
            };
            props.set |= PROP_TEXT_DECORATION;
        }

        // shorthand: margin: v1 [v2 [v3 [v4]]]
        b"margin" | b"padding" => {
            parse_margin_shorthand(value, props);
        }

        _ => {} // unknown property; ignore
    }
}

// parse margin shorthand (1-4 values)
fn parse_margin_shorthand(value: &[u8], props: &mut StyleProps) {
    let mut vals = [0i8; 4];
    let mut count = 0usize;

    // split value on whitespace, parse each part
    let mut pos = 0;
    let value = trim_css(value);

    while pos < value.len() && count < 4 {
        // skip whitespace
        while pos < value.len() && value[pos] == b' ' {
            pos += 1;
        }
        if pos >= value.len() {
            break;
        }
        // find end of token
        let start = pos;
        while pos < value.len() && value[pos] != b' ' {
            pos += 1;
        }
        vals[count] = parse_length_qem(&value[start..pos]);
        count += 1;
    }

    match count {
        1 => {
            props.margin_top = vals[0];
            props.margin_right = vals[0];
            props.margin_bottom = vals[0];
            props.margin_left = vals[0];
            props.set |=
                PROP_MARGIN_TOP | PROP_MARGIN_RIGHT | PROP_MARGIN_BOTTOM | PROP_MARGIN_LEFT;
        }
        2 => {
            props.margin_top = vals[0];
            props.margin_bottom = vals[0];
            props.margin_left = vals[1];
            props.margin_right = vals[1];
            props.set |=
                PROP_MARGIN_TOP | PROP_MARGIN_RIGHT | PROP_MARGIN_BOTTOM | PROP_MARGIN_LEFT;
        }
        3 => {
            props.margin_top = vals[0];
            props.margin_left = vals[1];
            props.margin_right = vals[1];
            props.margin_bottom = vals[2];
            props.set |=
                PROP_MARGIN_TOP | PROP_MARGIN_RIGHT | PROP_MARGIN_BOTTOM | PROP_MARGIN_LEFT;
        }
        4 => {
            props.margin_top = vals[0];
            props.margin_right = vals[1];
            props.margin_bottom = vals[2];
            props.margin_left = vals[3];
            props.set |=
                PROP_MARGIN_TOP | PROP_MARGIN_RIGHT | PROP_MARGIN_BOTTOM | PROP_MARGIN_LEFT;
        }
        _ => {}
    }
}

// tag ID mapping: lowercase tag name -> compact u8 for selector matching.
// 0 = unknown/any; known tags get stable IDs.

pub fn tag_id(name: &[u8]) -> u8 {
    match name {
        b"p" => 1,
        b"div" => 2,
        b"span" => 3,
        b"h1" => 4,
        b"h2" => 5,
        b"h3" => 6,
        b"h4" => 7,
        b"h5" => 8,
        b"h6" => 9,
        b"em" => 10,
        b"i" => 11,
        b"b" => 12,
        b"strong" => 13,
        b"a" => 14,
        b"blockquote" => 15,
        b"ul" => 16,
        b"ol" => 17,
        b"li" => 18,
        b"pre" => 19,
        b"code" => 20,
        b"body" => 21,
        b"section" => 22,
        b"article" => 23,
        b"figure" => 24,
        b"figcaption" => 25,
        b"cite" => 26,
        b"small" => 27,
        b"sup" => 28,
        b"sub" => 29,
        b"table" => 30,
        b"tr" => 31,
        b"td" => 32,
        b"th" => 33,
        b"header" => 34,
        b"footer" => 35,
        b"aside" => 36,
        b"nav" => 37,
        b"dl" => 38,
        b"dt" => 39,
        b"dd" => 40,
        b"abbr" => 41,
        _ => 0,
    }
}

// class hash: FNV-1a folded to 16 bits.
// 0 reserved for "no class constraint"; hash of 0 is mapped to 1.

pub fn class_hash(name: &[u8]) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in name {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    let h16 = ((h >> 16) ^ h) as u16;
    if h16 == 0 { 1 } else { h16 }
}

// CSS length parsing

// parse CSS length to quarter-em units; handles em/rem/px/pt/0
fn parse_length_qem(val: &[u8]) -> i8 {
    let val = trim_css(val);
    if val.is_empty() || val == b"0" || val == b"auto" || val == b"normal" {
        return 0;
    }

    let (neg, rest) = if val[0] == b'-' {
        (true, &val[1..])
    } else {
        (false, val)
    };

    // parse integer + fractional parts
    let mut whole: i32 = 0;
    let mut frac: i32 = 0; // hundredths
    let mut i = 0;
    let mut seen_dot = false;
    let mut frac_digits = 0u8;

    while i < rest.len() {
        let b = rest[i];
        if b.is_ascii_digit() {
            if seen_dot {
                if frac_digits < 2 {
                    frac = frac * 10 + (b - b'0') as i32;
                    frac_digits += 1;
                }
            } else {
                whole = whole.saturating_mul(10).saturating_add((b - b'0') as i32);
            }
        } else if b == b'.' && !seen_dot {
            seen_dot = true;
        } else {
            break;
        }
        i += 1;
    }

    // normalise fractional part to hundredths
    if frac_digits == 1 {
        frac *= 10;
    }

    let unit = trim_css(&rest[i..]);

    // convert to quarter-em (4 qem = 1em)
    let qem = if unit.starts_with(b"px") || unit.starts_with(b"pt") {
        // 16px ~= 1em -> 4px ~= 1 qem
        let total_px_100 = whole * 100 + frac;
        (total_px_100 + 200) / 400
    } else {
        // em, rem, or unknown: treat as em
        whole * 4 + (frac * 4 + 50) / 100
    };

    let signed = if neg { -qem } else { qem };
    signed.clamp(-126, 126) as i8
}

// scanning helpers

fn trim_css(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|b| !is_css_ws(*b))
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !is_css_ws(*b))
        .map(|p| p + 1)
        .unwrap_or(start);
    if start >= end { &[] } else { &data[start..end] }
}

#[inline]
fn is_css_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
}

fn skip_ws_comments(css: &[u8], mut pos: usize) -> usize {
    loop {
        while pos < css.len() && is_css_ws(css[pos]) {
            pos += 1;
        }
        if pos + 1 < css.len() && css[pos] == b'/' && css[pos + 1] == b'*' {
            pos += 2;
            while pos + 1 < css.len() {
                if css[pos] == b'*' && css[pos + 1] == b'/' {
                    pos += 2;
                    break;
                }
                pos += 1;
            }
        } else {
            break;
        }
    }
    pos
}

// skip @-rule including nested brace blocks
fn skip_at_rule(css: &[u8], pos: usize) -> usize {
    let mut p = pos + 1; // skip '@'
    while p < css.len() {
        if css[p] == b'{' {
            // block @-rule; count braces
            let mut depth = 1u32;
            p += 1;
            while p < css.len() && depth > 0 {
                match css[p] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
                p += 1;
            }
            return p;
        }
        if css[p] == b';' {
            // statement @-rule (@import, @charset)
            return p + 1;
        }
        p += 1;
    }
    css.len()
}

fn scan_to_byte(css: &[u8], pos: usize, needle: u8) -> Option<usize> {
    css[pos..]
        .iter()
        .position(|&b| b == needle)
        .map(|i| pos + i)
}

fn starts_with_digit(val: &[u8]) -> bool {
    val.first().is_some_and(|b| b.is_ascii_digit())
}

fn parse_int(val: &[u8]) -> i32 {
    let mut n: i32 = 0;
    for &b in val {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as i32);
        } else {
            break;
        }
    }
    n
}
