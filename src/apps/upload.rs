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
use crate::kernel::tasks;

// ── WiFi credentials (edit these!) ──────────────────────────────────

const SSID: &str = "all_ducks_quack";
const PASSWORD: &str = "pissword";

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
    // ── Phase 1 — Initialise radio & WiFi ───────────────────────────

    render_status(
        epd,
        strip,
        delay,
        &["Upload Mode", "", "Initialising radio..."],
    )
    .await;

    let radio = match esp_radio::init() {
        Ok(r) => r,
        Err(e) => {
            info!("upload: radio init failed: {:?}", e);
            render_status(
                epd,
                strip,
                delay,
                &[
                    "Upload Mode",
                    "",
                    "Radio init failed!",
                    "",
                    "Press BACK to exit",
                ],
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
            render_status(
                epd,
                strip,
                delay,
                &[
                    "Upload Mode",
                    "",
                    "WiFi init failed!",
                    "",
                    "Press BACK to exit",
                ],
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
        render_status(
            epd,
            strip,
            delay,
            &[
                "Upload Mode",
                "",
                "WiFi config error!",
                "",
                "Press BACK to exit",
            ],
        )
        .await;
        drain_until_back().await;
        return;
    }

    if let Err(e) = wifi_ctrl.start_async().await {
        info!("upload: start failed: {:?}", e);
        render_status(
            epd,
            strip,
            delay,
            &[
                "Upload Mode",
                "",
                "WiFi start failed!",
                "",
                "Press BACK to exit",
            ],
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: wifi started, connecting to '{}'", SSID);

    // ── Phase 2 — Connect to AP ─────────────────────────────────────

    {
        let mut msg_buf = [0u8; 64];
        let msg_len;
        {
            let mut w = StackFmt::new(&mut msg_buf);
            let _ = write!(w, "Connecting to '{}'...", SSID);
            msg_len = w.len();
        }
        let msg = core::str::from_utf8(&msg_buf[..msg_len]).unwrap_or("Connecting...");
        render_status(epd, strip, delay, &["Upload Mode", "", msg]).await;
    }

    if let Err(e) = wifi_ctrl.connect_async().await {
        info!("upload: connect failed: {:?}", e);
        render_status(
            epd,
            strip,
            delay,
            &[
                "Upload Mode",
                "",
                "Connection failed!",
                "",
                "Press BACK to exit",
            ],
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: connected to '{}'", SSID);

    // ── Phase 3 — DHCP ──────────────────────────────────────────────

    render_status(
        epd,
        strip,
        delay,
        &["Upload Mode", "", "Connected!", "Obtaining IP address..."],
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
    let ip_len;
    {
        let mut w = StackFmt::new(&mut ip_buf);
        if let Some(cfg) = stack.config_v4() {
            let _ = write!(w, "http://{}/", cfg.address.address());
        } else {
            let _ = write!(w, "(no IP address)");
        }
        ip_len = w.len();
    }
    let ip_str = core::str::from_utf8(&ip_buf[..ip_len]).unwrap_or("???");

    info!("upload: serving at {}", ip_str);

    render_status(
        epd,
        strip,
        delay,
        &["Upload Mode", "", ip_str, "", "Press BACK to exit"],
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

/// Full-refresh the e-paper with a set of horizontally-centred text
/// lines.  Uses the small mono font (6×13) so we don't depend on the
/// bitmap font atlas.
async fn render_status(epd: &mut Epd, strip: &mut StripBuffer, delay: &mut Delay, lines: &[&str]) {
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::mono_font::ascii::FONT_6X13;
    use embedded_graphics::pixelcolor::BinaryColor;
    use embedded_graphics::prelude::*;
    use embedded_graphics::text::Text;

    let style = MonoTextStyle::new(&FONT_6X13, BinaryColor::On);

    epd.full_refresh_async(strip, delay, &|s: &mut StripBuffer| {
        let start_y: i32 = 350;
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue; // blank line = vertical spacer
            }
            let y = start_y + (i as i32) * 24;
            let x = ((480 - line.len() as i32 * 6) / 2).max(8);
            let _ = Text::new(line, Point::new(x, y), style).draw(s);
        }
    })
    .await;
}

// ── Stack-based fmt::Write adapter ──────────────────────────────────

/// Tiny `core::fmt::Write` backed by a `&mut [u8]` — avoids a heap
/// allocation when all we need is a short formatted string.
struct StackFmt<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> StackFmt<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn len(&self) -> usize {
        self.pos
    }
}

impl core::fmt::Write for StackFmt<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let room = self.buf.len() - self.pos;
        let n = bytes.len().min(room);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}
