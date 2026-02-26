// Minimal XML tag/attribute scanner for EPUB metadata
//
// NOT a general-purpose XML parser. This is a byte-level scanner
// optimised for the two XML dialects we actually encounter:
//
//   1. META-INF/container.xml  (find <rootfile full-path="..."/>)
//   2. *.opf package document  (metadata, manifest items, spine)
//
// Design constraints:
//   • no_std, no alloc — operates purely on borrowed byte slices
//   • single-pass, forward-only — no DOM, no tree, no backtracking
//   • namespace-aware matching — "dc:title" matches query "title"
//   • lenient — skips malformed constructs instead of erroring
//
// The public API is three functions:
//   tag_text()     — text content of first matching element
//   for_each_tag() — iterate over every opening tag with a given name
//   get_attr()     — extract a named attribute from raw tag bytes

// ── Attribute extraction ────────────────────────────────────────

/// Extract the value of attribute `attr_name` from raw tag content.
///
/// `tag_bytes` is everything between `<` and `>` for an opening tag,
/// e.g. for `<item id="ch1" href="chapter1.xhtml"/>` it would be
/// `item id="ch1" href="chapter1.xhtml"/`.
///
/// Returns the attribute value without quotes, or `None`.
pub fn get_attr<'a>(tag_bytes: &'a [u8], attr_name: &[u8]) -> Option<&'a [u8]> {
    // Skip past the tag name first
    let mut pos = 0;
    let len = tag_bytes.len();

    // Advance past the tag name
    while pos < len && !is_ws(tag_bytes[pos]) && tag_bytes[pos] != b'>' && tag_bytes[pos] != b'/' {
        pos += 1;
    }

    // Now scan for attributes
    while pos < len {
        // Skip whitespace
        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }

        if pos >= len || tag_bytes[pos] == b'>' || tag_bytes[pos] == b'/' {
            break;
        }

        // Read attribute name
        let name_start = pos;
        while pos < len
            && tag_bytes[pos] != b'='
            && !is_ws(tag_bytes[pos])
            && tag_bytes[pos] != b'>'
            && tag_bytes[pos] != b'/'
        {
            pos += 1;
        }
        let name_end = pos;

        // Skip whitespace around '='
        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }

        if pos >= len || tag_bytes[pos] != b'=' {
            // Attribute without value (e.g. `disabled`) — skip it
            continue;
        }
        pos += 1; // skip '='

        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }

        if pos >= len {
            break;
        }

        // Read quoted value
        let quote = tag_bytes[pos];
        if quote != b'"' && quote != b'\'' {
            // Unquoted value — skip to next whitespace
            while pos < len && !is_ws(tag_bytes[pos]) && tag_bytes[pos] != b'>' {
                pos += 1;
            }
            continue;
        }
        pos += 1; // skip opening quote

        let value_start = pos;
        while pos < len && tag_bytes[pos] != quote {
            pos += 1;
        }
        let value_end = pos;

        if pos < len {
            pos += 1; // skip closing quote
        }

        // Check if this attribute name matches the query
        let name = &tag_bytes[name_start..name_end];
        if name == attr_name {
            return Some(&tag_bytes[value_start..value_end]);
        }
    }

    None
}

// ── Tag scanning ────────────────────────────────────────────────

/// Get the text content of the first element matching `tag_name`.
///
/// Searches `data` for `<tag_name ...>text</tag_name>` and returns
/// the text between the opening and closing tags. Namespace prefixes
/// are matched flexibly: searching for `"title"` will match both
/// `<title>` and `<dc:title>`.
///
/// Returns `None` if no matching element is found or if the element
/// is self-closing / empty.
pub fn tag_text<'a>(data: &'a [u8], tag_name: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;

    while pos < data.len() {
        // Find next '<'
        let Some(lt) = find_byte(&data[pos..], b'<') else {
            break;
        };
        let lt = pos + lt;
        pos = lt + 1;

        if pos >= data.len() {
            break;
        }

        // Skip close tags, PIs, comments, CDATA, DOCTYPE
        let first = data[pos];
        if first == b'/' || first == b'?' || first == b'!' {
            pos = skip_construct(&data, pos - 1);
            continue;
        }

        // Read tag name
        let name_start = pos;
        while pos < data.len() && !is_tag_delim(data[pos]) {
            pos += 1;
        }
        let name = &data[name_start..pos];

        if !tag_name_matches(name, tag_name) {
            // Skip to end of this tag
            pos = skip_to_gt(&data, pos);
            continue;
        }

        // Check if self-closing before '>'
        let tag_end = skip_to_gt(&data, pos);

        // Look backwards from '>' for '/'
        if tag_end > 0 && tag_end - 1 < data.len() && data[tag_end - 1] == b'/' {
            // Self-closing: <tag ... /> — no text content
            pos = tag_end;
            continue;
        }

        pos = tag_end; // now past '>'

        // Collect text until we see `</`
        let text_start = pos;
        while pos + 1 < data.len() {
            if data[pos] == b'<' && data[pos + 1] == b'/' {
                return Some(trim_ws(&data[text_start..pos]));
            }
            pos += 1;
        }

        // No closing tag found — return what we have
        break;
    }

    None
}

/// Call `cb` for every opening tag whose name matches `tag_name`.
///
/// The callback receives the raw bytes between `<` and `>` (exclusive),
/// e.g. for `<item id="ch1" href="x"/>` the callback gets
/// `b"item id=\"ch1\" href=\"x\"/"`.
///
/// Namespace prefixes are handled: searching for `"item"` matches
/// both `<item>` and `<opf:item>`.
pub fn for_each_tag<'a>(data: &'a [u8], tag_name: &[u8], mut cb: impl FnMut(&'a [u8])) {
    let mut pos = 0;

    while pos < data.len() {
        // Find next '<'
        let Some(lt) = find_byte(&data[pos..], b'<') else {
            break;
        };
        let lt = pos + lt;
        pos = lt + 1;

        if pos >= data.len() {
            break;
        }

        // Skip close tags, PIs, comments, CDATA, DOCTYPE
        let first = data[pos];
        if first == b'/' || first == b'?' || first == b'!' {
            pos = skip_construct(&data, lt);
            continue;
        }

        // Read tag name
        let name_start = pos;
        while pos < data.len() && !is_tag_delim(data[pos]) {
            pos += 1;
        }
        let name = &data[name_start..pos];

        if !tag_name_matches(name, tag_name) {
            pos = skip_to_gt(&data, pos);
            continue;
        }

        // Found a match — find the end of this tag's '<' ... '>'
        let content_start = name_start;
        let mut end = pos;
        while end < data.len() && data[end] != b'>' {
            end += 1;
        }
        // content_end is before '>'
        let content_end = end;

        cb(&data[content_start..content_end]);

        pos = if end < data.len() { end + 1 } else { end };
    }
}

// ── Tag name matching ───────────────────────────────────────────

/// Check if `full_name` matches `target`, accounting for XML
/// namespace prefixes. `b"dc:title"` matches target `b"title"`,
/// and `b"item"` matches target `b"item"`.
fn tag_name_matches(full_name: &[u8], target: &[u8]) -> bool {
    if full_name == target {
        return true;
    }

    // Check for namespace prefix: "prefix:localname"
    // Match if the part after ':' equals target
    if full_name.len() > target.len() + 1 {
        let colon_pos = full_name.len() - target.len() - 1;
        if full_name[colon_pos] == b':' && &full_name[colon_pos + 1..] == target {
            return true;
        }
    }

    false
}

// ── Internal helpers ────────────────────────────────────────────

/// Find the first occurrence of `needle` in `haystack`.
fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

/// Is this byte a delimiter that ends a tag name?
#[inline]
fn is_tag_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}

/// Advance past '>' starting from `pos`. Returns the position
/// *after* the '>'. If no '>' is found, returns `data.len()`.
fn skip_to_gt(data: &[u8], mut pos: usize) -> usize {
    while pos < data.len() {
        if data[pos] == b'>' {
            return pos + 1;
        }
        pos += 1;
    }
    data.len()
}

/// Skip an XML construct that starts at `lt_pos` (the '<' position).
/// Handles `</...>`, `<?...?>`, `<!--...-->`, `<!...>`, and CDATA.
fn skip_construct(data: &[u8], lt_pos: usize) -> usize {
    let pos = lt_pos + 1;
    if pos >= data.len() {
        return data.len();
    }

    match data[pos] {
        b'/' => {
            // Close tag </...> — just skip to '>'
            skip_to_gt(data, pos)
        }
        b'?' => {
            // Processing instruction <?...?> — find "?>"
            let mut p = pos + 1;
            while p + 1 < data.len() {
                if data[p] == b'?' && data[p + 1] == b'>' {
                    return p + 2;
                }
                p += 1;
            }
            data.len()
        }
        b'!' => {
            // Comment <!--...--> or DOCTYPE or CDATA
            let rest = &data[pos + 1..];
            if rest.starts_with(b"--") {
                // Comment: skip to "-->"
                let mut p = pos + 3; // past "!--"
                while p + 2 < data.len() {
                    if data[p] == b'-' && data[p + 1] == b'-' && data[p + 2] == b'>' {
                        return p + 3;
                    }
                    p += 1;
                }
                data.len()
            } else if rest.starts_with(b"[CDATA[") {
                // CDATA: skip to "]]>"
                let mut p = pos + 8; // past "![CDATA["
                while p + 2 < data.len() {
                    if data[p] == b']' && data[p + 1] == b']' && data[p + 2] == b'>' {
                        return p + 3;
                    }
                    p += 1;
                }
                data.len()
            } else {
                // DOCTYPE or other <!...> — skip to '>'
                skip_to_gt(data, pos)
            }
        }
        _ => skip_to_gt(data, lt_pos),
    }
}

/// Trim leading and trailing ASCII whitespace from a byte slice.
fn trim_ws(data: &[u8]) -> &[u8] {
    let start = data.iter().position(|b| !is_ws(*b)).unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !is_ws(*b))
        .map(|p| p + 1)
        .unwrap_or(start);
    if start >= end { &[] } else { &data[start..end] }
}
