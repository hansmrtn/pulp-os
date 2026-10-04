// Test rig: drives the real ReaderApp / SettingsApp the way the firmware's app
// manager and scheduler do (on_enter -> run background until the page is ready ->
// on_event), on the in-memory SD shim. No logic of the apps is reimplemented here.
use std::future::Future;
use std::task::{Context, Poll, Waker};

use crate::apps::probe::{self, Phase};
use crate::apps::reader::ReaderApp;
use crate::apps::settings::SettingsApp;
use crate::apps::{App, AppContext, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::kernel::config::{self, SystemSettings, WifiConfig};
use crate::kernel::Kernel;
use pulp_kernel::drivers::sdcard::{FakeFs, SdStorage};

pub fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..1_000_000 {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
    panic!("future did not complete");
}

// a card as the firmware boots it: `_PULP/` exists
pub fn card() -> FakeFs {
    let mut fs = FakeFs::new();
    fs.mkdir("_PULP");
    fs
}

pub struct Rig {
    pub k: Kernel,
    pub ctx: AppContext,
    pub app: Box<ReaderApp>,
}

impl Rig {
    pub fn new(fs: FakeFs) -> Self {
        let mut k = Kernel::new(SdStorage::mounted(fs));
        // boot: the kernel loads the bookmark cache before any app runs
        k.bookmarks_load();
        Self {
            k,
            ctx: AppContext::new(),
            app: Box::new(ReaderApp::new()),
        }
    }

    pub fn with_book(name: &str, data: &[u8]) -> Self {
        let mut fs = card();
        fs.put("", name, data);
        Self::new(fs)
    }

    // AppManager::propagate_fonts: settings -> reader before the first on_enter
    pub fn configure(&mut self, book_font: u8, theme: u8) {
        self.app.set_book_font_size(book_font);
        self.app.set_reading_theme(theme);
    }

    pub fn open(&mut self, name: &str) {
        self.ctx.set_message(name.as_bytes());
        let mut h = self.k.handle();
        self.app.on_enter(&mut self.ctx, &mut h);
        drop(h);
        self.settle();
    }

    pub fn step(&mut self) {
        let mut h = self.k.handle();
        block_on(self.app.background(&mut self.ctx, &mut h));
    }

    // run the background state machine until a page (or the TOC / an error) is up
    pub fn settle(&mut self) {
        for _ in 0..4000 {
            if probe::phase(&self.app) != Phase::Loading {
                return;
            }
            self.step();
        }
        panic!("reader did not settle, state {}", probe::state_name(&self.app));
    }

    // scheduler idle time while a page is showing (background chapter caching)
    pub fn idle(&mut self, steps: usize) {
        for _ in 0..steps {
            self.step();
        }
    }

    pub fn event(&mut self, ev: ActionEvent) -> Transition {
        let t = self.app.on_event(ev, &mut self.ctx);
        self.settle();
        t
    }

    pub fn press(&mut self, a: Action) -> Transition {
        self.event(ActionEvent::Press(a))
    }

    pub fn long_press(&mut self, a: Action) -> Transition {
        self.event(ActionEvent::LongPress(a))
    }

    pub fn suspend(&mut self) {
        self.app.on_suspend();
    }

    pub fn resume(&mut self) {
        let mut h = self.k.handle();
        self.app.on_resume(&mut self.ctx, &mut h);
        drop(h);
        self.settle();
    }

    pub fn exit(&mut self) {
        self.app.on_exit();
    }

    pub fn save_position(&mut self) {
        self.app.save_position(self.k.bookmarks());
    }

    pub fn page(&self) -> usize {
        probe::page(&self.app)
    }
    pub fn total(&self) -> usize {
        probe::total_pages(&self.app)
    }
    pub fn lines(&self) -> Vec<String> {
        probe::page_lines(&self.app)
    }
}

// SettingsApp as the firmware loads it at boot and edits it
pub struct SettingsRig {
    pub k: Kernel,
    pub ctx: AppContext,
    pub app: Box<SettingsApp>,
}

impl SettingsRig {
    pub fn new(fs: FakeFs) -> Self {
        Self {
            k: Kernel::new(SdStorage::mounted(fs)),
            ctx: AppContext::new(),
            app: Box::new(SettingsApp::new()),
        }
    }

    // AppManager::load_eager_settings (the `load_eager` half)
    pub fn boot(&mut self) {
        let mut h = self.k.handle();
        self.app.load_eager(&mut h);
    }

    pub fn enter(&mut self) {
        let mut h = self.k.handle();
        self.app.on_enter(&mut self.ctx, &mut h);
    }

    pub fn press(&mut self, a: Action) -> Transition {
        self.app.on_event(ActionEvent::Press(a), &mut self.ctx)
    }

    // scheduler background tick (saves when something changed)
    pub fn tick(&mut self) {
        let mut h = self.k.handle();
        block_on(self.app.background(&mut self.ctx, &mut h));
    }

    pub fn file(&self) -> Option<Vec<u8>> {
        self.k
            .sd()
            .with_fs(|fs| fs.get("_PULP", config::SETTINGS_FILE).cloned())
            .flatten()
    }

    pub fn settings(&self) -> SystemSettings {
        *self.app.system_settings()
    }
}

pub fn parse(data: &[u8]) -> (SystemSettings, WifiConfig) {
    let mut s = SystemSettings::defaults();
    let mut w = WifiConfig::empty();
    config::parse_settings_txt(data, &mut s, &mut w);
    s.sanitize();
    (s, w)
}

// ------------------------------------------------------------------ helpers

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pos {
    pub chapter: u16,
    pub page: usize,
    pub offset: u32,
    pub lines: Vec<probe::Line>,
}

impl Rig {
    pub fn pos(&self) -> Pos {
        let off = probe::offsets(&self.app)
            .get(probe::page(&self.app))
            .copied()
            .unwrap_or(0);
        Pos {
            chapter: self.app.chapter(),
            page: probe::page(&self.app),
            offset: off,
            lines: probe::lines(&self.app),
        }
    }

    // Press(Next) until the position stops changing: every page of a TXT, or every
    // page of every chapter of an EPUB (a chapter's last Next enters the next chapter)
    pub fn walk_forward(&mut self) -> Vec<Pos> {
        let mut out = vec![self.pos()];
        loop {
            self.press(Action::Next);
            let p = self.pos();
            if (p.chapter, p.page, p.offset) == {
                let l = out.last().unwrap();
                (l.chapter, l.page, l.offset)
            } {
                return out;
            }
            out.push(p);
            assert!(out.len() < 5000, "walk_forward runaway");
        }
    }

    pub fn walk_back(&mut self) -> Vec<Pos> {
        let mut out = vec![self.pos()];
        loop {
            self.press(Action::Prev);
            let p = self.pos();
            if (p.chapter, p.page, p.offset) == {
                let l = out.last().unwrap();
                (l.chapter, l.page, l.offset)
            } {
                return out;
            }
            out.push(p);
            assert!(out.len() < 5000, "walk_back runaway");
        }
    }

    // render the current page through the real draw() into the 12 physical strips
    // of the 800x480 panel (Deg270 = 480x800 portrait) and hash the bytes (FNV-1a 64)
    pub fn render_hash(&self) -> u64 {
        use crate::drivers::strip::{STRIP_COUNT, StripBuffer};
        use pulp_board_logic::ssd1677::Rotation;
        let mut strip = StripBuffer::new();
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for i in 0..STRIP_COUNT {
            strip.begin_strip(Rotation::Deg270, i);
            self.app.draw(&mut strip);
            for &b in strip.data() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        h
    }
}

pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// pixel width of one laid-out line in the page's regular/heading/bold font
pub fn line_width(app: &ReaderApp, font_idx: u8, l: &probe::Line) -> u32 {
    use crate::fonts::{FontSet, Style};
    let fs = FontSet::for_size(font_idx);
    let style = if l.flags & 4 != 0 {
        Style::Heading
    } else if l.flags & 1 != 0 {
        Style::Bold
    } else if l.flags & 2 != 0 {
        Style::Italic
    } else {
        Style::Regular
    };
    let mut w = 0u32;
    let mut i = 0;
    let b = &l.bytes;
    while i < b.len() {
        if b[i] == smol_epub::html_strip::MARKER && i + 1 < b.len() {
            i += 2;
            continue;
        }
        let (ch, n) = pulp_kernel::util::decode_utf8_char(b, i);
        w += match ch {
            '\u{00AD}' => 0,
            c if (c as u32) < 0x20 => 0, // tab / CR: skipped by the wrapper, zero advance
            '\u{00A0}' => fs.advance(' ', style) as u32,
            _ => fs.advance(ch, style) as u32,
        };
        i += n;
    }
    w
}
