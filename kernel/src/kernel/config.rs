// system configuration: key=value text in _PULP/SETTINGS.TXT
//
// SystemSettings and WifiConfig are kernel-owned configuration;
// the SettingsApp in apps/ provides the UI for editing them

pub const SETTINGS_FILE: &str = "SETTINGS.TXT";

#[derive(Clone, Copy)]
pub struct SystemSettings {
    pub sleep_timeout: u16,     // minutes idle before sleep; 0 = never
    pub ghost_clear_every: u8,  // partial refreshes before forced full GC
    pub book_font_size_idx: u8, // 0 = Small, 1 = Medium, 2 = Large
    pub ui_font_size_idx: u8,   // 0 = Small, 1 = Medium, 2 = Large
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self::defaults()
    }
}

impl SystemSettings {
    pub const fn defaults() -> Self {
        Self {
            sleep_timeout: 10,
            ghost_clear_every: 10,
            book_font_size_idx: 2,
            ui_font_size_idx: 2,
        }
    }

    pub fn sanitize(&mut self) {
        self.sanitize_with_max_font(Self::DEFAULT_MAX_FONT_IDX);
    }

    pub fn sanitize_with_max_font(&mut self, max_font: u8) {
        self.sleep_timeout = self.sleep_timeout.min(120);
        self.ghost_clear_every = self.ghost_clear_every.clamp(5, 100);
        self.book_font_size_idx = self.book_font_size_idx.min(max_font);
        self.ui_font_size_idx = self.ui_font_size_idx.min(max_font);
    }

    // reasonable default; distros override via sanitize_with_max_font
    const DEFAULT_MAX_FONT_IDX: u8 = 4;
}

pub const WIFI_SSID_CAP: usize = 32;
pub const WIFI_PASS_CAP: usize = 63;

pub struct WifiConfig {
    ssid: [u8; WIFI_SSID_CAP],
    ssid_len: u8,
    pass: [u8; WIFI_PASS_CAP],
    pass_len: u8,
}

impl WifiConfig {
    pub const fn empty() -> Self {
        Self {
            ssid: [0u8; WIFI_SSID_CAP],
            ssid_len: 0,
            pass: [0u8; WIFI_PASS_CAP],
            pass_len: 0,
        }
    }

    pub fn ssid(&self) -> &str {
        core::str::from_utf8(&self.ssid[..self.ssid_len as usize]).unwrap_or("")
    }

    pub fn password(&self) -> &str {
        core::str::from_utf8(&self.pass[..self.pass_len as usize]).unwrap_or("")
    }

    pub fn has_credentials(&self) -> bool {
        self.ssid_len > 0
    }

    fn set_ssid(&mut self, val: &[u8]) {
        let n = val.len().min(WIFI_SSID_CAP);
        self.ssid[..n].copy_from_slice(&val[..n]);
        self.ssid_len = n as u8;
    }

    fn set_pass(&mut self, val: &[u8]) {
        let n = val.len().min(WIFI_PASS_CAP);
        self.pass[..n].copy_from_slice(&val[..n]);
        self.pass_len = n as u8;
    }
}

fn trim(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && matches!(s[start], b' ' | b'\t' | b'\r') {
        start += 1;
    }
    while end > start && matches!(s[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }
    &s[start..end]
}

fn parse_u16(s: &[u8]) -> Option<u16> {
    if s.is_empty() {
        return None;
    }
    let mut val: u16 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val.checked_mul(10)?.checked_add((b - b'0') as u16)?;
    }
    Some(val)
}

fn apply_setting(key: &[u8], val: &[u8], s: &mut SystemSettings, w: &mut WifiConfig) {
    match key {
        b"sleep_timeout" => {
            if let Some(v) = parse_u16(val) {
                s.sleep_timeout = v;
            }
        }
        b"ghost_clear" => {
            if let Some(v) = parse_u16(val) {
                s.ghost_clear_every = v as u8;
            }
        }
        b"book_font" => {
            if let Some(v) = parse_u16(val) {
                s.book_font_size_idx = v as u8;
            }
        }
        b"ui_font" => {
            if let Some(v) = parse_u16(val) {
                s.ui_font_size_idx = v as u8;
            }
        }
        b"wifi_ssid" => w.set_ssid(val),
        b"wifi_pass" => w.set_pass(val),
        _ => {}
    }
}

pub fn parse_settings_txt(data: &[u8], settings: &mut SystemSettings, wifi: &mut WifiConfig) {
    for line in data.split(|&b| b == b'\n') {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        if let Some(eq) = line.iter().position(|&b| b == b'=') {
            let key = trim(&line[..eq]);
            let val = trim(&line[eq + 1..]);
            apply_setting(key, val, settings, wifi);
        }
    }
}

struct TxtWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> TxtWriter<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn put(&mut self, data: &[u8]) {
        let n = data.len().min(self.buf.len() - self.pos);
        self.buf[self.pos..self.pos + n].copy_from_slice(&data[..n]);
        self.pos += n;
    }

    fn put_u16(&mut self, val: u16) {
        if val == 0 {
            self.put(b"0");
            return;
        }
        let mut digits = [0u8; 5];
        let mut i = 5;
        let mut v = val;
        while v > 0 {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        self.put(&digits[i..5]);
    }

    fn kv_num(&mut self, key: &[u8], val: u16) {
        self.put(key);
        self.put(b"=");
        self.put_u16(val);
        self.put(b"\n");
    }

    fn kv_str(&mut self, key: &[u8], val: &[u8]) {
        self.put(key);
        self.put(b"=");
        self.put(val);
        self.put(b"\n");
    }
}

pub fn write_settings_txt(s: &SystemSettings, w: &WifiConfig, buf: &mut [u8]) -> usize {
    let mut wr = TxtWriter::new(buf);
    wr.put(b"# pulp-os settings\n");
    wr.put(b"# lines starting with # are ignored\n\n");
    wr.kv_num(b"sleep_timeout", s.sleep_timeout);
    wr.kv_num(b"ghost_clear", s.ghost_clear_every as u16);
    wr.kv_num(b"book_font", s.book_font_size_idx as u16);
    wr.kv_num(b"ui_font", s.ui_font_size_idx as u16);
    wr.put(b"\n# wifi credentials for upload mode\n");
    wr.kv_str(b"wifi_ssid", &w.ssid[..w.ssid_len as usize]);
    wr.kv_str(b"wifi_pass", &w.pass[..w.pass_len as usize]);
    wr.pos
}
