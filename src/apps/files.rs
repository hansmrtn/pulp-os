// Paginated file browser for SD card root directory.
// Background title scanner resolves EPUB titles from OPF metadata.

extern crate alloc;

use alloc::vec::Vec;
use core::fmt::Write as _;

use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::PrimitiveStyle;

use crate::apps::{App, AppContext, AppId, Services, Transition};
use crate::board::action::{Action, ActionEvent};
use crate::board::{SCREEN_H, SCREEN_W};
use crate::drivers::storage::DirEntry;
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::ui::{Alignment, BitmapDynLabel, BitmapLabel, CONTENT_TOP, Region};
use smol_epub::epub::{self, EpubMeta, EpubSpine};
use smol_epub::zip::ZipIndex;

const PAGE_SIZE: usize = 7;

const LIST_X: u16 = 16;
const LIST_W: u16 = 448;

const STATUS_REGION: Region = Region::new(320, CONTENT_TOP + 8, 144, 28);

const ROW_H: u16 = 52;
const ROW_GAP: u16 = 4; // gap between rows (border-to-border)
const HEADER_LIST_GAP: u16 = 8; // gap between heading bottom and first row

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
    error: Option<&'static str>,
    body_font: &'static BitmapFont,
    heading_font: &'static BitmapFont,
    list_y: u16,

    /// Index into the DirCache to resume the title scan from.
    title_scan_idx: usize,
    /// `true` while there are still untitled EPUBs to scan.
    title_scanning: bool,
}

impl FilesApp {
    pub fn new() -> Self {
        let hf = fonts::heading_font(0);
        Self {
            entries: [DirEntry::EMPTY; PAGE_SIZE],
            count: 0,
            total: 0,
            scroll: 0,
            selected: 0,
            needs_load: false,
            stale_cache: false,
            error: None,
            body_font: fonts::body_font(0),
            heading_font: hf,
            list_y: CONTENT_TOP + 8 + hf.line_height + HEADER_LIST_GAP,
            title_scan_idx: 0,
            title_scanning: false,
        }
    }

    pub fn set_ui_font_size(&mut self, idx: u8) {
        self.body_font = fonts::body_font(idx);
        self.heading_font = fonts::heading_font(idx);
        self.list_y = CONTENT_TOP + 8 + self.heading_font.line_height + HEADER_LIST_GAP;
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

    fn load_failed(&mut self, msg: &'static str) {
        self.needs_load = false;
        self.error = Some(msg);
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

impl App for FilesApp {
    fn on_enter(&mut self, ctx: &mut AppContext) {
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

    fn on_resume(&mut self, ctx: &mut AppContext) {
        ctx.mark_dirty(Region::new(
            0,
            CONTENT_TOP,
            SCREEN_W,
            SCREEN_H - CONTENT_TOP,
        ));
    }

    fn needs_work(&self) -> bool {
        self.needs_load || self.title_scanning
    }

    fn on_work<SPI: embedded_hal::spi::SpiDevice>(
        &mut self,
        svc: &mut Services<'_, SPI>,
        ctx: &mut AppContext,
    ) {
        // ── page load ───────────────────────────────────────────
        if self.needs_load {
            if self.stale_cache {
                svc.invalidate_dir_cache();
                self.stale_cache = false;
            }

            let mut buf = [DirEntry::EMPTY; PAGE_SIZE];
            match svc.dir_page(self.scroll, &mut buf) {
                Ok(page) => {
                    self.load_page(&buf[..page.count], page.total);
                }
                Err(e) => {
                    log::info!("SD load failed: {}", e);
                    self.load_failed(e);
                }
            }

            ctx.mark_dirty(self.list_region());
            ctx.mark_dirty(STATUS_REGION);
            return; // yield — title scan runs on the next tick
        }

        // ── background title scan (one EPUB per tick) ───────────
        if self.title_scanning {
            if let Some(dirty) = scan_one_epub_title(svc, self.title_scan_idx) {
                self.title_scan_idx = dirty.next_idx;
                if dirty.resolved {
                    // Reload the visible page so the title shows up.
                    self.needs_load = true;
                }
            } else {
                // No more untitled EPUBs — scan complete.
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
        let header_region =
            Region::new(LIST_X, CONTENT_TOP + 8, 300, self.heading_font.line_height);
        BitmapLabel::new(header_region, "Files", self.heading_font)
            .alignment(Alignment::CenterLeft)
            .draw(strip)
            .unwrap();

        if self.total > 0 {
            let mut status = BitmapDynLabel::<20>::new(STATUS_REGION, self.body_font)
                .alignment(Alignment::CenterRight);
            let _ = write!(status, "{}/{}", self.scroll + self.selected + 1, self.total);
            status.draw(strip).unwrap();
        }

        if let Some(msg) = self.error {
            BitmapLabel::new(self.row_region(0), msg, self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        if self.count == 0 && self.needs_load {
            BitmapLabel::new(self.row_region(0), "Loading...", self.body_font)
                .alignment(Alignment::CenterLeft)
                .draw(strip)
                .unwrap();
            return;
        }

        if self.count == 0 && !self.needs_load {
            BitmapLabel::new(self.row_region(0), "No files found", self.body_font)
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

                BitmapLabel::new(region, name, self.body_font)
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

// ── background EPUB title scanner ───────────────────────────────────

struct TitleScanResult {
    /// Resume scanning from this DirCache index next tick.
    next_idx: usize,
    /// `true` if a title was resolved and the visible page may need
    /// refreshing.
    resolved: bool,
}

/// Attempt to resolve the title of one untitled EPUB.
///
/// Parses the EPUB's ZIP central directory, extracts `container.xml`
/// and the OPF, and reads `<dc:title>`.  The result is saved to
/// `TITLES.BIN` and applied to the DirCache entry so subsequent
/// page loads show it.
///
/// Returns `None` when no untitled EPUBs remain from `from` onward.
fn scan_one_epub_title<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    from: usize,
) -> Option<TitleScanResult> {
    let (idx, name_buf, name_len) = svc.next_untitled_epub(from)?;
    let name = core::str::from_utf8(&name_buf[..name_len as usize]).unwrap_or("");
    let next_idx = idx + 1;

    log::info!("titles: scanning {} (idx {})", name, idx);

    let result = (|| -> Result<(), &'static str> {
        // 1. Read EOCD tail
        let file_size = svc.file_size(name)?;
        if file_size < 22 {
            return Err("too small");
        }

        let tail_size = (file_size as usize).min(512);
        let tail_offset = file_size - tail_size as u32;
        let mut buf = [0u8; 512];
        let n = svc.read_file_chunk(name, tail_offset, &mut buf[..tail_size])?;

        let (cd_offset, cd_size) = ZipIndex::parse_eocd(&buf[..n], file_size)?;

        // 2. Read central directory
        let mut cd_buf = Vec::new();
        cd_buf
            .try_reserve_exact(cd_size as usize)
            .map_err(|_| "CD too large")?;
        cd_buf.resize(cd_size as usize, 0);

        let mut total = 0usize;
        while total < cd_buf.len() {
            let rd = svc.read_file_chunk(name, cd_offset + total as u32, &mut cd_buf[total..])?;
            if rd == 0 {
                return Err("CD truncated");
            }
            total += rd;
        }

        let mut zip = ZipIndex::new();
        zip.parse_central_directory(&cd_buf)?;
        drop(cd_buf);

        // 3. Find and parse container.xml → OPF path
        let mut opf_path_buf = [0u8; epub::OPF_PATH_CAP];
        let opf_path_len = if let Some(ci) = zip.find("META-INF/container.xml") {
            let container = smol_epub::zip::extract_entry(
                zip.entry(ci),
                zip.entry(ci).local_offset,
                |off, b| svc.read_file_chunk(name, off, b),
            )?;
            let len = epub::parse_container(&container, &mut opf_path_buf)?;
            drop(container);
            len
        } else {
            epub::find_opf_in_zip(&zip, &mut opf_path_buf)?
        };

        let opf_path =
            core::str::from_utf8(&opf_path_buf[..opf_path_len]).map_err(|_| "bad OPF path")?;

        // 4. Extract OPF and parse title
        let opf_idx = zip
            .find(opf_path)
            .or_else(|| zip.find_icase(opf_path))
            .ok_or("OPF not found")?;

        let opf_data = smol_epub::zip::extract_entry(
            zip.entry(opf_idx),
            zip.entry(opf_idx).local_offset,
            |off, b| svc.read_file_chunk(name, off, b),
        )?;

        let opf_dir = opf_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let mut meta = EpubMeta::new();
        let mut spine = EpubSpine::new();
        epub::parse_opf(&opf_data, opf_dir, &zip, &mut meta, &mut spine)?;
        drop(opf_data);

        let title = meta.title_str();
        if title.is_empty() {
            return Err("no title in OPF");
        }

        // 5. Save to TITLES.BIN and update DirCache
        log::info!("titles: {} -> \"{}\"", name, title);
        let _ = svc.save_title(name, title);
        svc.set_dir_entry_title(idx, title.as_bytes());

        Ok(())
    })();

    if let Err(e) = result {
        log::warn!("titles: {} failed: {}", name, e);
    }

    Some(TitleScanResult {
        next_idx,
        resolved: result.is_ok(),
    })
}
