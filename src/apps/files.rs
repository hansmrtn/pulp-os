// paginated file browser for SD card root directory
// background title scanner resolves EPUB titles from OPF metadata

use alloc::vec::Vec;
use core::fmt::Write as _;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::apps::{App, AppContext, AppId, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::storage::DirEntry;
use crate::drivers::strip::StripBuffer;
use crate::error::{Error, ErrorKind};
use crate::fonts;
use crate::kernel::KernelHandle;
use crate::ui::{
    Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, FULL_CONTENT_W, HEADER_W, LARGE_MARGIN,
    Region, SECTION_GAP, TITLE_Y_OFFSET,
};
use smol_epub::epub::{self, EpubMeta, EpubSpine};
use smol_epub::zip::ZipIndex;

const PAGE_SIZE: usize = 7;

const LIST_X: u16 = LARGE_MARGIN;
const LIST_W: u16 = FULL_CONTENT_W;

const TITLE_Y: u16 = CONTENT_TOP + TITLE_Y_OFFSET;

const STATUS_W: u16 = 144;
const STATUS_X: u16 = SCREEN_W - LARGE_MARGIN - STATUS_W;
const FILES_STATUS_Y: u16 = TITLE_Y;
const FILES_STATUS_H: u16 = 28;
const STATUS_REGION: Region = Region::new(STATUS_X, FILES_STATUS_Y, STATUS_W, FILES_STATUS_H);

const ROW_H: u16 = 52;
const ROW_GAP: u16 = 4;

const HEADER_LIST_GAP: u16 = SECTION_GAP;

impl Default for FilesApp {
    fn default() -> Self {
        Self::new()
    }
}

pub struct FilesApp {
    entries: [DirEntry; PAGE_SIZE],
    count: usize,
    total: usize,
    scroll: usize,
    selected: usize,
    needs_load: bool,
    stale_cache: bool,
    error: Option<Error>,
    ui_fonts: fonts::UiFonts,
    list_y: u16,

    title_scan_idx: usize,
    title_scanning: bool,
    title_reload: bool,
}

impl FilesApp {
    pub fn new() -> Self {
        let uf = fonts::UiFonts::for_size(0);
        Self {
            entries: [DirEntry::EMPTY; PAGE_SIZE],
            count: 0,
            total: 0,
            scroll: 0,
            selected: 0,
            needs_load: false,
            stale_cache: false,
            error: None,
            ui_fonts: uf,
            list_y: TITLE_Y + uf.heading.line_height + HEADER_LIST_GAP,
            title_scan_idx: 0,
            title_scanning: false,
            title_reload: false,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.ui_fonts = fonts::UiFonts::for_size(idx);
        self.list_y = TITLE_Y + self.ui_fonts.heading.line_height + HEADER_LIST_GAP;
    }

    // Session state accessors for RTC persistence
    #[inline]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    #[inline]
    pub fn selected(&self) -> usize {
        self.selected
    }

    #[inline]
    pub fn total(&self) -> usize {
        self.total
    }

    /// Restore files state from RTC session data
    pub fn restore_state(&mut self, scroll: usize, selected: usize, total: usize) {
        self.scroll = scroll;
        self.selected = selected;
        self.total = total;
        self.needs_load = true; // trigger page reload
        log::info!(
            "files: restore_state scroll={} selected={} total={}",
            scroll,
            selected,
            total
        );
    }

    fn selected_entry(&self) -> Option<&DirEntry> {
        if self.selected < self.count {
            Some(&self.entries[self.selected])
        } else {
            None
        }
    }

    fn load_page(&mut self, entries: &[DirEntry], total: usize) {
        let n = entries.len().min(PAGE_SIZE);
        self.entries[..n].clone_from_slice(&entries[..n]);
        self.count = n;
        self.total = total;
        self.needs_load = false;
        self.error = None;
        if self.selected >= self.count && self.count > 0 {
            self.selected = self.count - 1;
        }
    }

    fn load_failed(&mut self, e: Error) {
        self.needs_load = false;
        self.error = Some(e);
        self.count = 0;
    }

    fn row_region(&self, index: usize) -> Region {
        Region::new(
            LIST_X,
            self.list_y + index as u16 * (ROW_H + ROW_GAP),
            LIST_W,
            ROW_H,
        )
    }

    fn list_region(&self) -> Region {
        Region::new(
            LIST_X,
            self.list_y,
            LIST_W,
            (ROW_H + ROW_GAP) * PAGE_SIZE as u16,
        )
    }

    fn move_up(&mut self, ctx: &mut AppContext) {
        if self.selected > 0 {
            ctx.mark_dirty(self.row_region(self.selected));
            self.selected -= 1;
            ctx.mark_dirty(self.row_region(self.selected));
            ctx.mark_dirty(STATUS_REGION);
        } else if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(1);
            self.needs_load = true;
        } else if self.total > 0 {
            self.scroll = self.total.saturating_sub(PAGE_SIZE);
            self.selected = self.total.saturating_sub(self.scroll) - 1;
            self.needs_load = true;
        }
    }

    fn move_down(&mut self, ctx: &mut AppContext) {
        if self.selected + 1 < self.count {
            ctx.mark_dirty(self.row_region(self.selected));
            self.selected += 1;
            ctx.mark_dirty(self.row_region(self.selected));
            ctx.mark_dirty(STATUS_REGION);
        } else if self.scroll + self.count < self.total {
            self.scroll += 1;
            self.needs_load = true;
        } else if self.total > 0 {
            self.scroll = 0;
            self.selected = 0;
            self.needs_load = true;
        }
    }

    fn jump_up(&mut self) {
        if self.scroll > 0 {
            self.scroll = self.scroll.saturating_sub(PAGE_SIZE);
            self.selected = 0;
            self.needs_load = true;
        } else {
            self.selected = 0;
        }
    }

    fn jump_down(&mut self) {
        let remaining = self.total.saturating_sub(self.scroll + self.count);
        if remaining > 0 {
            self.scroll += PAGE_SIZE.min(remaining + self.count - 1);
            self.selected = 0;
            self.needs_load = true;
        } else if self.count > 0 {
            self.selected = self.count - 1;
        }
    }
}

impl App<AppId> for FilesApp {
    fn on_enter(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        self.scroll = 0;
        self.selected = 0;
        self.needs_load = true;
        self.stale_cache = true;
        self.error = None;
        self.title_scan_idx = 0;
        self.title_scanning = true;
        ctx.mark_dirty(Region::new(
            0,
            CONTENT_TOP,
            SCREEN_W,
            SCREEN_H - CONTENT_TOP,
        ));
    }

    fn on_exit(&mut self) {
        self.count = 0;
        self.title_scanning = false;
    }

    fn on_suspend(&mut self) {}

    fn on_resume(&mut self, ctx: &mut AppContext, _k: &mut KernelHandle<'_>) {
        ctx.mark_dirty(Region::new(
            0,
            CONTENT_TOP,
            SCREEN_W,
            SCREEN_H - CONTENT_TOP,
        ));
    }

    async fn background(&mut self, ctx: &mut AppContext, k: &mut KernelHandle<'_>) {
        if self.needs_load {
            if self.stale_cache {
                k.invalidate_dir_cache();
                self.stale_cache = false;
            }

            let mut buf = [DirEntry::EMPTY; PAGE_SIZE];
            match k.dir_page(self.scroll, &mut buf) {
                Ok(page) => {
                    self.load_page(&buf[..page.count], page.total);
                }
                Err(e) => {
                    log::info!("SD load failed: {}", e);
                    self.load_failed(e);
                }
            }

            if self.title_reload {
                self.title_reload = false;
                ctx.mark_dirty_coalesced(self.list_region());
                ctx.mark_dirty_coalesced(STATUS_REGION);
            } else {
                ctx.mark_dirty(self.list_region());
                ctx.mark_dirty(STATUS_REGION);
            }
            return;
        }

        if self.title_scanning {
            if let Some(dirty) = scan_one_epub_title(k, self.title_scan_idx) {
                self.title_scan_idx = dirty.next_idx;
                if dirty.resolved {
                    self.needs_load = true;
                    self.title_reload = true;
                }
            } else {
                self.title_scanning = false;
                log::info!("titles: scan complete");
            }
        }
    }

    fn on_event(&mut self, event: ActionEvent, ctx: &mut AppContext) -> Transition {
        match event {
            ActionEvent::Press(Action::Back) => Transition::Pop,
            ActionEvent::LongPress(Action::Back) => Transition::Home,

            ActionEvent::Press(Action::Prev) | ActionEvent::Repeat(Action::Prev) => {
                self.move_up(ctx);
                Transition::None
            }

            ActionEvent::Press(Action::Next) | ActionEvent::Repeat(Action::Next) => {
                self.move_down(ctx);
                Transition::None
            }

            ActionEvent::Press(Action::PrevJump) => {
                self.jump_up();
                if !self.needs_load {
                    ctx.mark_dirty(self.list_region());
                    ctx.mark_dirty(STATUS_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::NextJump) => {
                self.jump_down();
                if !self.needs_load {
                    ctx.mark_dirty(self.list_region());
                    ctx.mark_dirty(STATUS_REGION);
                }
                Transition::None
            }

            ActionEvent::Press(Action::Select) => {
                if let Some(entry) = self.selected_entry() {
                    if entry.is_dir {
                        Transition::None
                    } else {
                        ctx.set_message(entry.name_str().as_bytes());
                        Transition::Push(AppId::Reader)
                    }
                } else {
                    Transition::None
                }
            }

            _ => Transition::None,
        }
    }

    fn draw(&self, strip: &mut StripBuffer) {
        let header_region = Region::new(
            LIST_X,
            TITLE_Y,
            HEADER_W,
            self.ui_fonts.heading.line_height,
        );
        BitmapLabel::new(header_region, "Files", self.ui_fonts.heading)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.total > 0 {
            let mut status = BitmapDynLabel::<24>::new(STATUS_REGION, self.ui_fonts.body)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "{}/{}", self.scroll + self.selected + 1, self.total);
            if self.title_scanning {
                let _ = write!(status, " ...");
            }
            status.draw(strip).unwrap();
        }

        if let Some(e) = self.error {
            let mut label = BitmapDynLabel::<32>::new(self.row_region(0), self.ui_fonts.body)
                .alignment(Alignment::CenterLeft);
            let _ = core::fmt::Write::write_fmt(&mut label, format_args!("{}", e));
            label.draw(strip).unwrap();
            return;
        }

        if self.count == 0 && self.needs_load {
            BitmapLabel::new(self.row_region(0), "Loading...", self.ui_fonts.body)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        if self.count == 0 && !self.needs_load {
            BitmapLabel::new(self.row_region(0), "No files found", self.ui_fonts.body)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        for i in 0..PAGE_SIZE {
            let region = self.row_region(i);

            if i < self.count {
                let entry = &self.entries[i];
                let name = entry.display_name();

                BitmapLabel::new(region, name, self.ui_fonts.body)
                    .alignment(Alignment::CenterLeft)
                    .inverted(i == self.selected)
                    .draw(strip)
                    .unwrap();
            } else {
                region
                    .to_rect()
                    .into_styled(PrimitiveStyle::with_fill(BinaryColor::Off))
                    .draw(strip)
                    .unwrap();
            }
        }
    }
}

struct TitleScanResult {
    next_idx: usize,
    resolved: bool,
}

fn scan_one_epub_title(k: &mut KernelHandle<'_>, from: usize) -> Option<TitleScanResult> {
    let (idx, name_buf, name_len) = k.dir_cache_mut().next_untitled_epub(from)?;
    let name = core::str::from_utf8(&name_buf[..name_len as usize]).unwrap_or("");
    let next_idx = idx + 1;

    log::info!("titles: scanning {} (idx {})", name, idx);

    let result = (|| -> crate::error::Result<()> {
        let file_size = k.file_size(name)?;
        if file_size < 22 {
            return Err(Error::new(ErrorKind::InvalidData, "title_scan: too small"));
        }

        let tail_size = (file_size as usize).min(512);
        let tail_offset = file_size - tail_size as u32;
        let mut buf = [0u8; 512];
        let n = k.read_chunk(name, tail_offset, &mut buf[..tail_size])?;

        // ZipIndex::parse_eocd returns Result<_, &'static str>;
        // the From<&'static str> impl on Error converts automatically via ?
        let (cd_offset, cd_size) = ZipIndex::parse_eocd(&buf[..n], file_size)?;

        let mut cd_buf = Vec::new();
        cd_buf
            .try_reserve_exact(cd_size as usize)
            .map_err(|_| Error::new(ErrorKind::OutOfMemory, "title_scan: CD alloc"))?;
        cd_buf.resize(cd_size as usize, 0);

        let mut total = 0usize;
        while total < cd_buf.len() {
            let rd = k.read_chunk(name, cd_offset + total as u32, &mut cd_buf[total..])?;
            if rd == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "title_scan: CD truncated",
                ));
            }
            total += rd;
        }

        let mut zip = ZipIndex::new();
        zip.parse_central_directory(&cd_buf)?;
        drop(cd_buf);

        let mut opf_path_buf = [0u8; epub::OPF_PATH_CAP];
        let opf_path_len = if let Some(ci) = zip.find("META-INF/container.xml") {
            let container = smol_epub::zip::extract_entry(
                zip.entry(ci),
                zip.entry(ci).local_offset,
                |off, b| {
                    k.read_chunk(name, off, b)
                        .map_err(|e: Error| -> &'static str { e.into() })
                },
            )?;
            let len = epub::parse_container(&container, &mut opf_path_buf)?;
            drop(container);
            len
        } else {
            epub::find_opf_in_zip(&zip, &mut opf_path_buf)?
        };

        let opf_path = core::str::from_utf8(&opf_path_buf[..opf_path_len])
            .map_err(|_| Error::new(ErrorKind::BadEncoding, "title_scan: OPF path"))?;

        let opf_idx = zip
            .find(opf_path)
            .or_else(|| zip.find_icase(opf_path))
            .ok_or(Error::new(ErrorKind::NotFound, "title_scan: OPF entry"))?;

        let opf_data = smol_epub::zip::extract_entry(
            zip.entry(opf_idx),
            zip.entry(opf_idx).local_offset,
            |off, b| {
                k.read_chunk(name, off, b)
                    .map_err(|e: Error| -> &'static str { e.into() })
            },
        )?;

        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut meta = EpubMeta::new();
        let mut spine = EpubSpine::new();
        epub::parse_opf(&opf_data, opf_dir, &zip, &mut meta, &mut spine)?;
        drop(opf_data);

        let title = meta.title_str();
        if title.is_empty() {
            return Err(Error::new(
                ErrorKind::ParseFailed,
                "title_scan: no title in OPF",
            ));
        }

        log::info!("titles: {} -> \"{}\"", name, title);
        let _ = k.save_title(name, title);
        k.dir_cache_mut().set_entry_title(idx, title.as_bytes());

        Ok(())
    })();

    if let Err(e) = &result {
        log::warn!("titles: {} failed: {}", name, e);
    }

    Some(TitleScanResult {
        next_idx,
        resolved: result.is_ok(),
    })
}
