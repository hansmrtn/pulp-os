// WiFi upload mode — connects to a hardcoded network, serves "hello world!"
//
// ┌────────────────────────────────────────────────────────────┐
// │  SET YOUR WIFI CREDENTIALS IN THE CONSTANTS BELOW          │
// └────────────────────────────────────────────────────────────┘
//
// Entered from the Home menu.  Renders connection status on the
// e-paper display and runs a tiny HTTP server on port 80.
// Press BACK to tear down WiFi and return to the home screen.
//
// No embassy tasks are spawned — the network runner, HTTP server
// and back-button monitor are multiplexed with `select`, so
// everything cleans up naturally when the function returns.

use alloc::string::String;
use core::fmt::Write as FmtWrite;

use embassy_futures::select::{Either, select};
use embassy_net::IpListenEndpoint;
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, Timer};
use embedded_io_async::Write as AsyncWrite;
use esp_hal::delay::Delay;
use esp_radio::wifi::{ClientConfig, Config, ModeConfig};
use log::info;

use crate::board::Epd;
use crate::board::action::{Action, ActionEvent, ButtonMapper};
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::kernel::tasks;
use crate::ui::{Alignment, BitmapLabel, CONTENT_TOP, Region};

// ── WiFi credentials (edit these!) ──────────────────────────────────

const SSID: &str = "all_ducks_quack";
const PASSWORD: &str = "pissword";

// ── Layout ──────────────────────────────────────────────────────────

const SCREEN_W: u16 = 480;
const SCREEN_H: u16 = 800;

const HEADING_X: u16 = 16;
const HEADING_W: u16 = SCREEN_W - HEADING_X * 2;

const BODY_X: u16 = 24;
const BODY_W: u16 = SCREEN_W - BODY_X * 2;
const BODY_LINE_GAP: u16 = 10;

const FOOTER_Y: u16 = SCREEN_H - 60;

// ── Public entry point ──────────────────────────────────────────────

/// Run the upload-mode server.  Blocks (async) until the user presses
/// BACK.  WiFi hardware is initialised on entry and torn down on
/// return via `Drop`.
pub async fn run_upload_mode(
    wifi: esp_hal::peripherals::WIFI<'static>,
    epd: &mut Epd,
    strip: &mut StripBuffer,
    delay: &mut Delay,
) {
    let heading = fonts::heading_font(0);
    let body = fonts::body_font(0);

    // ── Phase 1 — Initialise radio & WiFi ───────────────────────────

    render_screen(
        epd,
        strip,
        delay,
        heading,
        body,
        &["Initialising radio..."],
        None,
    )
    .await;

    let radio = match esp_radio::init() {
        Ok(r) => r,
        Err(e) => {
            info!("upload: radio init failed: {:?}", e);
            render_screen(
                epd,
                strip,
                delay,
                heading,
                body,
                &["Radio init failed!"],
                Some("Press BACK to exit"),
            )
            .await;
            drain_until_back().await;
            return;
        }
    };

    let (mut wifi_ctrl, interfaces) = match esp_radio::wifi::new(&radio, wifi, Config::default()) {
        Ok(pair) => pair,
        Err(e) => {
            info!("upload: wifi::new failed: {:?}", e);
            render_screen(
                epd,
                strip,
                delay,
                heading,
                body,
                &["WiFi init failed!"],
                Some("Press BACK to exit"),
            )
            .await;
            drain_until_back().await;
            return;
        }
    };

    let client_cfg = ClientConfig::default()
        .with_ssid(String::from(SSID))
        .with_password(String::from(PASSWORD));

    if let Err(e) = wifi_ctrl.set_config(&ModeConfig::Client(client_cfg)) {
        info!("upload: set_config failed: {:?}", e);
        render_screen(
            epd,
            strip,
            delay,
            heading,
            body,
            &["WiFi config error!"],
            Some("Press BACK to exit"),
        )
        .await;
        drain_until_back().await;
        return;
    }

    if let Err(e) = wifi_ctrl.start_async().await {
        info!("upload: start failed: {:?}", e);
        render_screen(
            epd,
            strip,
            delay,
            heading,
            body,
            &["WiFi start failed!"],
            Some("Press BACK to exit"),
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: wifi started, connecting to '{}'", SSID);

    // ── Phase 2 — Connect to AP ─────────────────────────────────────

    {
        let mut msg_buf = [0u8; 64];
        let msg_len = stack_fmt(&mut msg_buf, |w| {
            let _ = write!(w, "Connecting to '{}'...", SSID);
        });
        let msg = core::str::from_utf8(&msg_buf[..msg_len]).unwrap_or("Connecting...");
        render_screen(epd, strip, delay, heading, body, &[msg], None).await;
    }

    if let Err(e) = wifi_ctrl.connect_async().await {
        info!("upload: connect failed: {:?}", e);
        render_screen(
            epd,
            strip,
            delay,
            heading,
            body,
            &["Connection failed!"],
            Some("Press BACK to exit"),
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: connected to '{}'", SSID);

    // ── Phase 3 — DHCP ──────────────────────────────────────────────

    render_screen(
        epd,
        strip,
        delay,
        heading,
        body,
        &["Connected!", "Obtaining IP address..."],
        None,
    )
    .await;

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = {
        let rng = esp_hal::rng::Rng::new();
        (rng.random() as u64) << 32 | rng.random() as u64
    };

    let mut resources = embassy_net::StackResources::<3>::new();
    let (stack, mut runner) = embassy_net::new(interfaces.sta, net_config, &mut resources, seed);

    // Poll the network runner while waiting for DHCP *or* BACK.
    let got_ip = loop {
        match select(
            runner.run(),
            select(stack.wait_config_up(), drain_until_back()),
        )
        .await
        {
            Either::Second(Either::First(_)) => break true, // DHCP done
            Either::Second(Either::Second(_)) => break false, // back pressed
            // runner.run() returns `!` — this arm is unreachable
            _ => unreachable!(),
        }
    };

    if !got_ip {
        info!("upload: user exited during DHCP");
        return;
    }

    // Format IP address
    let mut ip_buf = [0u8; 48];
    let ip_len = stack_fmt(&mut ip_buf, |w| {
        if let Some(cfg) = stack.config_v4() {
            let _ = write!(w, "http://{}/", cfg.address.address());
        } else {
            let _ = write!(w, "(no IP address)");
        }
    });
    let ip_str = core::str::from_utf8(&ip_buf[..ip_len]).unwrap_or("???");

    info!("upload: serving at {}", ip_str);

    render_screen(
        epd,
        strip,
        delay,
        heading,
        body,
        &[ip_str],
        Some("Press BACK to exit"),
    )
    .await;

    // ── Phase 4 — HTTP server loop ──────────────────────────────────

    let mut rx_buf = [0u8; 1536];
    let mut tx_buf = [0u8; 1536];

    loop {
        match select(
            runner.run(),
            select(
                serve_one_request(stack, &mut rx_buf, &mut tx_buf),
                drain_until_back(),
            ),
        )
        .await
        {
            Either::Second(Either::First(_)) => continue, // served a request
            Either::Second(Either::Second(_)) => break,   // back pressed
            _ => unreachable!(),
        }
    }

    info!("upload: exiting, tearing down WiFi");
    // radio + wifi_ctrl dropped here → radio hardware deinit
}

// ── HTTP serving ────────────────────────────────────────────────────

/// Accept one TCP connection on port 80, read the request, reply
/// with "hello world!", then close the socket.
async fn serve_one_request(stack: embassy_net::Stack<'_>, rx_buf: &mut [u8], tx_buf: &mut [u8]) {
    let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
    socket.set_timeout(Some(Duration::from_secs(10)));

    if socket
        .accept(IpListenEndpoint {
            addr: None,
            port: 80,
        })
        .await
        .is_err()
    {
        Timer::after(Duration::from_millis(200)).await;
        return;
    }

    // Drain request headers (read until blank line delimiter)
    let mut hdr = [0u8; 512];
    let mut pos = 0usize;
    loop {
        match socket.read(&mut hdr[pos..]).await {
            Ok(0) => break,
            Ok(n) => {
                pos += n;
                if hdr[..pos].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if pos >= hdr.len() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let _ = socket
        .write_all(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello world!")
        .await;
    let _ = socket.flush().await;

    Timer::after(Duration::from_millis(50)).await;
    socket.close();
    Timer::after(Duration::from_millis(50)).await;
    socket.abort();
}

// ── Input helpers ───────────────────────────────────────────────────

/// Drain the input-event channel until a BACK press is detected.
async fn drain_until_back() {
    let mapper = ButtonMapper::new();
    loop {
        let hw = tasks::INPUT_EVENTS.receive().await;
        let ev = mapper.map_event(hw);
        if matches!(
            ev,
            ActionEvent::Press(Action::Back) | ActionEvent::LongPress(Action::Back)
        ) {
            return;
        }
    }
}

// ── Display helpers ─────────────────────────────────────────────────

/// Full-refresh the e-paper with the upload-mode screen layout:
///
///     ┌──────────────────────────┐
///     │      Upload Mode         │  ← heading font, centred
///     │                          │
///     │                          │
///     │     body line 1          │  ← body font, centred
///     │     body line 2          │
///     │                          │
///     │                          │
///     │     footer hint          │  ← body font, centred, near bottom
///     └──────────────────────────┘
///
async fn render_screen(
    epd: &mut Epd,
    strip: &mut StripBuffer,
    delay: &mut Delay,
    heading: &'static BitmapFont,
    body: &'static BitmapFont,
    lines: &[&str],
    footer: Option<&str>,
) {
    let heading_h = heading.line_height;
    let body_h = body.line_height;
    let body_stride = body_h + BODY_LINE_GAP;

    // Heading region: just below the status-bar area
    let heading_region = Region::new(HEADING_X, CONTENT_TOP + 12, HEADING_W, heading_h);

    // Body lines: vertically centred between heading and footer
    let body_area_top = CONTENT_TOP + 12 + heading_h + 40;
    let body_area_bottom = FOOTER_Y.saturating_sub(20);
    let body_area_h = body_area_bottom.saturating_sub(body_area_top);
    let total_body_h = if lines.is_empty() {
        0
    } else {
        (lines.len() as u16 - 1) * body_stride + body_h
    };
    let body_start_y = body_area_top + body_area_h.saturating_sub(total_body_h) / 2;

    // Footer region: near the bottom of the screen
    let footer_region = Region::new(BODY_X, FOOTER_Y, BODY_W, body_h);

    epd.full_refresh_async(strip, delay, &|s: &mut StripBuffer| {
        // Heading
        BitmapLabel::new(heading_region, "Upload Mode", heading)
            .alignment(Alignment::Center)
            .draw(s)
            .unwrap();

        // Body lines
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let y = body_start_y + (i as u16) * body_stride;
            let region = Region::new(BODY_X, y, BODY_W, body_h);
            BitmapLabel::new(region, line, body)
                .alignment(Alignment::Center)
                .draw(s)
                .unwrap();
        }

        // Footer
        if let Some(text) = footer {
            BitmapLabel::new(footer_region, text, body)
                .alignment(Alignment::Center)
                .draw(s)
                .unwrap();
        }
    })
    .await;
}

// ── Stack-based fmt helper ──────────────────────────────────────────

/// Format into a stack buffer, return the number of bytes written.
fn stack_fmt(buf: &mut [u8], f: impl FnOnce(&mut StackWriter<'_>)) -> usize {
    let mut w = StackWriter { buf, pos: 0 };
    f(&mut w);
    w.pos
}

struct StackWriter<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl core::fmt::Write for StackWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let room = self.buf.len() - self.pos;
        let n = bytes.len().min(room);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}
