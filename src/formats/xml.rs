// Minimal XML tag/attribute scanner for EPUB metadata
//
// Not a general-purpose parser. Handles the two XML dialects we need:
// container.xml (find rootfile full-path) and OPF (metadata, manifest,
// spine). Single-pass, forward-only, namespace-aware, lenient.

pub fn get_attr<'a>(tag_bytes: &'a [u8], attr_name: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;
    let len = tag_bytes.len();

    // skip past tag name
    while pos < len && !is_ws(tag_bytes[pos]) && tag_bytes[pos] != b'>' && tag_bytes[pos] != b'/' {
        pos += 1;
    }

    while pos < len {
        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }
        if pos >= len || tag_bytes[pos] == b'>' || tag_bytes[pos] == b'/' {
            break;
        }

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

        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }
        if pos >= len || tag_bytes[pos] != b'=' {
            continue;
        }
        pos += 1;
        while pos < len && is_ws(tag_bytes[pos]) {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        let quote = tag_bytes[pos];
        if quote != b'"' && quote != b'\'' {
            while pos < len && !is_ws(tag_bytes[pos]) && tag_bytes[pos] != b'>' {
                pos += 1;
            }
            continue;
        }
        pos += 1;

        let value_start = pos;
        while pos < len && tag_bytes[pos] != quote {
            pos += 1;
        }
        let value_end = pos;
        if pos < len {
            pos += 1;
        }

        if &tag_bytes[name_start..name_end] == attr_name {
            return Some(&tag_bytes[value_start..value_end]);
        }
    }

    None
}

// text content of the first element matching tag_name; namespace-aware ("title" matches "dc:title")
pub fn tag_text<'a>(data: &'a [u8], tag_name: &[u8]) -> Option<&'a [u8]> {
    let mut pos = 0;

    while pos < data.len() {
        let Some(lt) = find_byte(&data[pos..], b'<') else {
            break;
        };
        let lt = pos + lt;
        pos = lt + 1;

        if pos >= data.len() {
            break;
        }

        let first = data[pos];
        if first == b'/' || first == b'?' || first == b'!' {
            pos = skip_construct(data, pos - 1);
            continue;
        }

        let name_start = pos;
        while pos < data.len() && !is_tag_delim(data[pos]) {
            pos += 1;
        }
        let name = &data[name_start..pos];

        if !tag_name_matches(name, tag_name) {
            pos = skip_to_gt(data, pos);
            continue;
        }

        let tag_end = skip_to_gt(data, pos);
        if tag_end > 0 && tag_end - 1 < data.len() && data[tag_end - 1] == b'/' {
            pos = tag_end;
            continue;
        }
        pos = tag_end;

        let text_start = pos;
        while pos + 1 < data.len() {
            if data[pos] == b'<' && data[pos + 1] == b'/' {
                return Some(trim_ws(&data[text_start..pos]));
            }
            pos += 1;
        }
        break;
    }

    None
}

// call cb for every opening tag matching tag_name; callback receives raw bytes between < and >
pub fn for_each_tag<'a>(data: &'a [u8], tag_name: &[u8], mut cb: impl FnMut(&'a [u8])) {
    let mut pos = 0;

    while pos < data.len() {
        let Some(lt) = find_byte(&data[pos..], b'<') else {
            break;
        };
        let lt = pos + lt;
        pos = lt + 1;

        if pos >= data.len() {
            break;
        }

        let first = data[pos];
        if first == b'/' || first == b'?' || first == b'!' {
            pos = skip_construct(data, lt);
            continue;
        }

        let name_start = pos;
        while pos < data.len() && !is_tag_delim(data[pos]) {
            pos += 1;
        }
        let name = &data[name_start..pos];

        if !tag_name_matches(name, tag_name) {
            pos = skip_to_gt(data, pos);
            continue;
        }

        let content_start = name_start;
        let mut end = pos;
        while end < data.len() && data[end] != b'>' {
            end += 1;
        }

        cb(&data[content_start..end]);

        pos = if end < data.len() { end + 1 } else { end };
    }
}

// "dc:title" matches target "title"; "item" matches "item"
fn tag_name_matches(full_name: &[u8], target: &[u8]) -> bool {
    if full_name == target {
        return true;
    }
    if full_name.len() > target.len() + 1 {
        let colon_pos = full_name.len() - target.len() - 1;
        if full_name[colon_pos] == b':' && &full_name[colon_pos + 1..] == target {
            return true;
        }
    }
    false
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|&b| b == needle)
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
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

fn skip_construct(data: &[u8], lt_pos: usize) -> usize {
    let pos = lt_pos + 1;
    if pos >= data.len() {
        return data.len();
    }

    match data[pos] {
        b'/' => skip_to_gt(data, pos),
        b'?' => {
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
            let rest = &data[pos + 1..];
            if rest.starts_with(b"--") {
                let mut p = pos + 3;
                while p + 2 < data.len() {
                    if data[p] == b'-' && data[p + 1] == b'-' && data[p + 2] == b'>' {
                        return p + 3;
                    }
                    p += 1;
                }
                data.len()
            } else if rest.starts_with(b"[CDATA[") {
                let mut p = pos + 8;
                while p + 2 < data.len() {
                    if data[p] == b']' && data[p + 1] == b']' && data[p + 2] == b'>' {
                        return p + 3;
                    }
                    p += 1;
                }
                data.len()
            } else {
                skip_to_gt(data, pos)
            }
        }
        _ => skip_to_gt(data, lt_pos),
    }
}

fn trim_ws(data: &[u8]) -> &[u8] {
    let start = data.iter().position(|b| !is_ws(*b)).unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|b| !is_ws(*b))
        .map(|p| p + 1)
        .unwrap_or(start);
    if start >= end { &[] } else { &data[start..end] }
}
