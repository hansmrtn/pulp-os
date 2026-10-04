// R21 / settings: SETTINGS.TXT parse + write round trip, tolerance, clamping, the
// real SettingsApp editing/saving, and how the saved values reach the reader.
use pulp_os_host::apps::probe;
use pulp_os_host::board::action::Action;
use pulp_os_host::fixtures::*;
use pulp_os_host::kernel::config::{self, SystemSettings, WifiConfig};
use pulp_os_host::rig::*;

fn written(s: &SystemSettings, w: &WifiConfig) -> Vec<u8> {
    let mut buf = [0u8; 512];
    let n = config::write_settings_txt(s, w, &mut buf);
    buf[..n].to_vec()
}

fn wifi(ssid: &str, pass: &str) -> WifiConfig {
    let (_, w) = parse(format!("wifi_ssid={ssid}\nwifi_pass={pass}\n").as_bytes());
    w
}

#[test]
fn defaults_are_the_baseline_values() {
    let s = SystemSettings::defaults();
    assert_eq!(s.sleep_timeout, 10);
    assert_eq!(s.ghost_clear_every, 10);
    assert_eq!(s.book_font_size_idx, 2);
    assert_eq!(s.ui_font_size_idx, 2);
    assert_eq!(s.reading_theme, 1);
    assert!(!s.swap_buttons);
    let (p, w) = parse(b"");
    assert_eq!(p.sleep_timeout, s.sleep_timeout);
    assert_eq!(p.reading_theme, s.reading_theme);
    assert!(!w.has_credentials());
}

#[test]
fn theme_table_is_the_baseline() {
    let t: Vec<_> = (0..4).map(|i| {
        let t = config::reading_theme(i);
        (t.name, t.margin_h, t.margin_v, t.line_spacing_pct)
    }).collect();
    assert_eq!(t, vec![("Compact", 8, 0, 100), ("Default", 16, 4, 120), ("Relaxed", 24, 8, 140), ("Spacious", 40, 12, 160)]);
    // an out-of-range index falls back to the last theme
    assert_eq!(config::reading_theme(200).name, "Spacious");
}

#[test]
fn every_key_is_parsed_by_its_exact_name() {
    let (s, w) = parse(
        b"sleep_timeout=45\nghost_clear=25\nbook_font=4\nui_font=0\nreading_theme=3\nswap_buttons=1\nwifi_ssid=home net\nwifi_pass=p@ss=word\n",
    );
    assert_eq!(
        (s.sleep_timeout, s.ghost_clear_every, s.book_font_size_idx, s.ui_font_size_idx, s.reading_theme, s.swap_buttons),
        (45, 25, 4, 0, 3, true)
    );
    assert_eq!(w.ssid(), "home net");
    assert_eq!(w.password(), "p@ss=word", "value is everything after the first '='");
    assert!(w.has_credentials());
}

#[test]
fn unknown_keys_comments_blank_lines_and_whitespace_are_tolerated() {
    let (s, _) = parse(
        b"# comment\n\n   \nfuture_option=7\nno equals sign here\n=novalue\n  book_font  =  3  \r\nUI_FONT=4\nsleep_timeout = 30\t\r\n#sleep_timeout=99\n",
    );
    assert_eq!(s.book_font_size_idx, 3, "spaces and CR around key/value are trimmed");
    assert_eq!(s.sleep_timeout, 30, "commented-out line ignored");
    assert_eq!(s.ui_font_size_idx, 2, "keys are case sensitive: UI_FONT is unknown");
}

#[test]
fn corrupt_values_keep_the_previous_value() {
    let (s, _) = parse(b"sleep_timeout=abc\nghost_clear=\nbook_font=-1\nui_font=99999\nreading_theme=2x\nswap_buttons=maybe\n");
    let d = SystemSettings::defaults();
    assert_eq!(s.sleep_timeout, d.sleep_timeout);
    assert_eq!(s.ghost_clear_every, d.ghost_clear_every);
    assert_eq!(s.book_font_size_idx, d.book_font_size_idx);
    assert_eq!(s.ui_font_size_idx, d.ui_font_size_idx, "u16 overflow is rejected");
    assert_eq!(s.reading_theme, d.reading_theme);
    assert!(!s.swap_buttons, "anything but 1/true is false");
    let (s, _) = parse(b"swap_buttons=true\n");
    assert!(s.swap_buttons);
    let (s, _) = parse(b"swap_buttons=1\nswap_buttons=0\n");
    assert!(!s.swap_buttons, "last assignment wins");
}

#[test]
fn out_of_range_values_are_clamped_by_sanitize() {
    let (s, _) = parse(b"sleep_timeout=500\nghost_clear=1\nbook_font=9\nui_font=200\nreading_theme=77\n");
    assert_eq!(s.sleep_timeout, 120);
    assert_eq!(s.ghost_clear_every, 5);
    assert_eq!(s.book_font_size_idx, 4);
    assert_eq!(s.ui_font_size_idx, 4);
    assert_eq!(s.reading_theme, 3);
    let (s, _) = parse(b"ghost_clear=300\n");
    assert_eq!(s.ghost_clear_every, 44, "ghost_clear is truncated to u8 before clamping (300 as u8 = 44), baseline");
    let (s, _) = parse(b"sleep_timeout=0\n");
    assert_eq!(s.sleep_timeout, 0, "0 = never sleep is valid");
}

#[test]
fn wifi_credentials_are_kept_and_capped() {
    let w = wifi("ssid-0123456789012345678901234567890123", "x");
    assert_eq!(w.ssid().len(), 32, "SSID capped at 32 bytes");
    let w = wifi("s", &"p".repeat(80));
    assert_eq!(w.password().len(), 63, "password capped at 63 bytes");
    let (_, w) = parse(b"wifi_ssid=\nwifi_pass=secret\n");
    assert!(!w.has_credentials(), "empty SSID = no credentials");
}

#[test]
fn write_then_parse_round_trips_every_field_including_wifi() {
    let mut s = SystemSettings::defaults();
    s.sleep_timeout = 75;
    s.ghost_clear_every = 35;
    s.book_font_size_idx = 3;
    s.ui_font_size_idx = 1;
    s.reading_theme = 2;
    s.swap_buttons = true;
    let w = wifi("My Home WiFi", "correct horse = battery staple");
    let bytes = written(&s, &w);
    let (s2, w2) = parse(&bytes);
    assert_eq!(
        (s2.sleep_timeout, s2.ghost_clear_every, s2.book_font_size_idx, s2.ui_font_size_idx, s2.reading_theme, s2.swap_buttons),
        (75, 35, 3, 1, 2, true)
    );
    assert_eq!(w2.ssid(), "My Home WiFi");
    assert_eq!(w2.password(), "correct horse = battery staple");
    // idempotent: writing what was parsed gives the same bytes
    assert_eq!(written(&s2, &w2), bytes);
}

#[test]
fn serialized_text_is_the_baseline_text() {
    let s = SystemSettings::defaults();
    let w = wifi("ssid", "pw");
    let text = String::from_utf8(written(&s, &w)).unwrap();
    assert_eq!(
        text,
        "# pulp-os settings\n# lines starting with # are ignored\n\n# power settings\nsleep_timeout=10\nghost_clear=10\n\n# font settings\nbook_font=2\nui_font=2\n\n# reading settings (0=Compact, 1=Default, 2=Relaxed, 3=Spacious)\nreading_theme=1\n\n# control settings\nswap_buttons=0\n\n# wifi credentials for upload mode\nwifi_ssid=ssid\nwifi_pass=pw\n"
    );
}

#[test]
fn maximum_credentials_still_fit_the_apps_512_byte_buffer() {
    let mut s = SystemSettings::defaults();
    s.sleep_timeout = 120;
    s.ghost_clear_every = 100;
    let w = wifi(&"s".repeat(32), &"p".repeat(63));
    let bytes = written(&s, &w);
    assert!(bytes.len() < 512, "{} bytes", bytes.len());
    let (_, w2) = parse(&bytes);
    assert_eq!(w2.ssid().len(), 32);
    assert_eq!(w2.password().len(), 63, "last key must not be truncated");
}

#[test]
fn settings_file_is_read_up_to_512_bytes_baseline() {
    // SettingsApp::load reads at most 512 bytes: a key after that point is not seen
    let mut f = String::from("book_font=3\n");
    f.push_str(&"# padding line to push the next key past the read window\n".repeat(10));
    f.push_str("swap_buttons=1\n");
    assert!(f.len() > 512);
    let mut fs = card();
    fs.put("_PULP", config::SETTINGS_FILE, f.as_bytes());
    let mut r = SettingsRig::new(fs);
    r.boot();
    assert_eq!(r.settings().book_font_size_idx, 3);
    assert!(!r.settings().swap_buttons);
}

// ------------------------------------------------------ SettingsApp (real)

#[test]
fn settings_app_loads_defaults_when_there_is_no_file() {
    let mut r = SettingsRig::new(card());
    assert!(!r.app.is_loaded());
    r.boot();
    assert!(r.app.is_loaded());
    assert_eq!(r.settings().book_font_size_idx, 2);
    assert!(r.file().is_none(), "loading never writes");
}

#[test]
fn settings_app_loads_a_saved_file_and_sanitizes_it() {
    let mut fs = card();
    fs.put("_PULP", config::SETTINGS_FILE, b"sleep_timeout=900\nbook_font=3\nswap_buttons=1\nwifi_ssid=home\nwifi_pass=pw\n");
    let mut r = SettingsRig::new(fs);
    r.boot();
    let s = r.settings();
    assert_eq!((s.sleep_timeout, s.book_font_size_idx, s.swap_buttons), (120, 3, true));
    assert_eq!(r.app.wifi_config().ssid(), "home");
}

// rows: 0 sleep, 1 ghost clear, 2 book font, 3 ui font, 4 theme, 5 swap
fn edit(r: &mut SettingsRig, row: usize, key: Action, times: usize) {
    r.enter();
    for _ in 0..row {
        r.press(Action::Next);
    }
    for _ in 0..times {
        r.press(key);
    }
}

#[test]
fn settings_app_edits_steps_clamps_and_saves() {
    let mut r = SettingsRig::new(card());
    r.boot();

    edit(&mut r, 0, Action::NextJump, 1);
    assert_eq!(r.settings().sleep_timeout, 15, "sleep +5");
    edit(&mut r, 0, Action::NextJump, 40);
    assert_eq!(r.settings().sleep_timeout, 120, "sleep caps at 120");
    edit(&mut r, 0, Action::PrevJump, 100);
    assert_eq!(r.settings().sleep_timeout, 0, "sleep 0 = never");
    edit(&mut r, 0, Action::NextJump, 1);
    assert_eq!(r.settings().sleep_timeout, 5);

    edit(&mut r, 1, Action::PrevJump, 10);
    assert_eq!(r.settings().ghost_clear_every, 5, "ghost clear floors at 5");
    edit(&mut r, 1, Action::NextJump, 30);
    assert_eq!(r.settings().ghost_clear_every, 100, "ghost clear caps at 100");

    edit(&mut r, 2, Action::NextJump, 9);
    assert_eq!(r.settings().book_font_size_idx, 4, "book font caps at XLarge");
    edit(&mut r, 2, Action::PrevJump, 9);
    assert_eq!(r.settings().book_font_size_idx, 0);
    edit(&mut r, 3, Action::NextJump, 2);
    assert_eq!(r.settings().ui_font_size_idx, 4);
    edit(&mut r, 4, Action::NextJump, 9);
    assert_eq!(r.settings().reading_theme, 3);
    edit(&mut r, 4, Action::PrevJump, 9);
    assert_eq!(r.settings().reading_theme, 0);
    edit(&mut r, 5, Action::NextJump, 1);
    assert!(r.settings().swap_buttons);
    edit(&mut r, 5, Action::PrevJump, 1);
    assert!(!r.settings().swap_buttons, "swap toggles both ways");
    edit(&mut r, 5, Action::NextJump, 1);

    assert!(r.file().is_none(), "nothing is written until the scheduler's background tick");
    r.tick();
    let file = r.file().expect("saved");
    let (s, _) = parse(&file);
    assert_eq!(
        (s.sleep_timeout, s.ghost_clear_every, s.book_font_size_idx, s.ui_font_size_idx, s.reading_theme, s.swap_buttons),
        (5, 100, 0, 4, 0, true)
    );

    // a fresh boot reads back exactly what was saved
    let mut fs = card();
    fs.put("_PULP", config::SETTINGS_FILE, &file);
    let mut r2 = SettingsRig::new(fs);
    r2.boot();
    let t = r2.settings();
    assert_eq!((t.sleep_timeout, t.book_font_size_idx, t.swap_buttons), (5, 0, true));
}

#[test]
fn settings_app_keeps_wifi_credentials_when_saving_other_values() {
    let mut fs = card();
    fs.put("_PULP", config::SETTINGS_FILE, b"wifi_ssid=cafe\nwifi_pass=latte\n");
    let mut r = SettingsRig::new(fs);
    r.boot();
    edit(&mut r, 2, Action::NextJump, 1);
    r.tick();
    let (_, w) = parse(&r.file().unwrap());
    assert_eq!(w.ssid(), "cafe");
    assert_eq!(w.password(), "latte", "credentials survive a settings save (offline build keeps the keys)");
}

#[test]
fn a_failed_save_is_retried_on_the_next_tick() {
    let mut r = SettingsRig::new(card());
    r.boot();
    edit(&mut r, 5, Action::NextJump, 1);
    r.k.sd().eject();
    r.tick(); // card gone: save fails, state stays dirty, no panic
    let mut fs = card();
    fs.put("", "X", b"");
    r.k = pulp_os_host::kernel::Kernel::new(pulp_kernel::drivers::sdcard::SdStorage::mounted(fs));
    r.tick();
    assert!(r.file().is_some(), "saved on the retry");
}

// ---------------------------------------------- settings -> reader, buttons

#[test]
fn saved_book_font_and_theme_decide_the_reader_layout() {
    // AppManager::load_eager_settings -> propagate_fonts: settings values go to the reader
    let mut fs = card();
    fs.put("_PULP", config::SETTINGS_FILE, b"book_font=4\nreading_theme=3\n");
    fs.put("", "BOOK.TXT", &english_txt(20_000, Eol::Lf, 2));
    let mut sr = SettingsRig::new(fs);
    sr.boot();
    let s = sr.settings();
    let mut r = Rig::with_book("BOOK.TXT", &english_txt(20_000, Eol::Lf, 2));
    r.configure(s.book_font_size_idx, s.reading_theme);
    r.open("BOOK.TXT");
    assert_eq!(probe::max_lines(&r.app), 10);
    assert_eq!(probe::text_w(&r.app), 400);
    assert_eq!(probe::theme_idx(&r.app), 3);
}

#[test]
fn swap_buttons_setting_selects_the_button_mapping_x4() {
    use pulp_os_host::board::action::{ActionEvent, ButtonMapper};
    use pulp_os_host::board::button::Button;
    let mut m = ButtonMapper::new();
    let default = [
        (Button::Back, Action::Back),
        (Button::Confirm, Action::Select),
        (Button::Left, Action::PrevJump),
        (Button::Right, Action::NextJump),
        (Button::VolUp, Action::Prev),
        (Button::VolDown, Action::Next),
        (Button::Power, Action::Menu),
    ];
    for (b, a) in default {
        assert_eq!(m.map_button(b), a);
    }
    // AppManager::sync_button_config: mapper.set_swap(settings.swap_buttons)
    let (s, _) = parse(b"swap_buttons=1\n");
    m.set_swap(s.swap_buttons);
    assert!(m.is_swapped());
    let swapped = [
        (Button::Back, Action::PrevJump),
        (Button::Confirm, Action::NextJump),
        (Button::Left, Action::Back),
        (Button::Right, Action::Select),
        (Button::VolUp, Action::Prev),
        (Button::VolDown, Action::Next),
        (Button::Power, Action::Menu),
    ];
    for (b, a) in swapped {
        assert_eq!(m.map_button(b), a, "{b:?}");
    }
    use pulp_os_host::drivers::input::Event;
    assert_eq!(m.map_event(Event::LongPress(Button::Left)), ActionEvent::LongPress(Action::Back));
}

#[cfg(feature = "tree")]
#[test]
fn swap_buttons_setting_selects_the_button_mapping_c61() {
    // OnePage C61: the same setting drives pulp_board_logic::keys (the C61 mapper)
    use pulp_board_logic::keys::{Key, map_event};
    use pulp_board_logic::input::Event;
    use pulp_os_host::board::action::ActionEvent;
    let (s, _) = parse(b"swap_buttons=1\n");
    assert_eq!(Key::Back.action(false), Action::Back);
    assert_eq!(Key::Back.action(s.swap_buttons), Action::PrevJump);
    assert_eq!(Key::Left.action(s.swap_buttons), Action::Back);
    assert_eq!(map_event(Event::Press(Key::Left), s.swap_buttons), ActionEvent::Press(Action::Back));
}
