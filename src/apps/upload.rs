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
// No embassy tasks are spawned — the network runner, HTTP server,
// mDNS responder and back-button monitor are multiplexed with
// `select`, so everything cleans up naturally when the function
// returns.
//
// A minimal mDNS responder advertises the device as `pulp.local`
// so users can navigate to http://pulp.local/ instead of needing
// to know the DHCP-assigned IP.  The responder only handles A
// queries for "pulp.local" — no service browsing, no AAAA, no
// probing.  Roughly 80 bytes on the wire per response.
//
// NOTE: embassy-net must have the "udp" feature enabled in
//       Cargo.toml for the mDNS responder to compile:
//
//   embassy-net = { version = "0.8", features = [
//       "dhcpv4", "medium-ethernet", "tcp", "udp",
//   ] }

use alloc::string::String;
use core::fmt::Write as FmtWrite;

use embassy_futures::select::{Either, select};
use embassy_net::IpListenEndpoint;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
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

// ── mDNS constants ──────────────────────────────────────────────────

const MDNS_PORT: u16 = 5353;

/// "pulp.local" in DNS wire format: length-prefixed labels + NUL.
const HOSTNAME_WIRE: [u8; 12] = [
    4, b'p', b'u', b'l', b'p', //
    5, b'l', b'o', b'c', b'a', b'l', //
    0,
];

/// Size of a minimal mDNS A-record response for "pulp.local".
///   12  header
/// + 12  answer name (same wire encoding)
/// +  2  TYPE
/// +  2  CLASS
/// +  4  TTL
/// +  2  RDLENGTH
/// +  4  RDATA (IPv4)
/// ────
///   38  total
const MDNS_RESPONSE_LEN: usize = 38;

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

    // 4 sockets: 1 TCP (HTTP) + 1 UDP (mDNS) + headroom
    let mut resources = embassy_net::StackResources::<4>::new();
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

    // Extract raw IPv4 octets for the mDNS responder.
    let ip_octets: [u8; 4] = if let Some(cfg) = stack.config_v4() {
        cfg.address.address().octets()
    } else {
        [0, 0, 0, 0]
    };

    // Format display strings — primary URL and parenthesised IP fallback.
    let mut ip_buf = [0u8; 48];
    let ip_len = stack_fmt(&mut ip_buf, |w| {
        let _ = write!(
            w,
            "({}.{}.{}.{})",
            ip_octets[0], ip_octets[1], ip_octets[2], ip_octets[3]
        );
    });
    let ip_str = core::str::from_utf8(&ip_buf[..ip_len]).unwrap_or("???");

    info!(
        "upload: serving at http://pulp.local/  ({})",
        core::str::from_utf8(&ip_buf[1..ip_len.saturating_sub(1)]).unwrap_or("?")
    );

    render_screen(
        epd,
        strip,
        delay,
        heading,
        body,
        &["http://pulp.local/", ip_str],
        Some("Press BACK to exit"),
    )
    .await;

    // ── Phase 4 — HTTP server + mDNS responder loop ─────────────────
    //
    // Three futures race each iteration:
    //   • serve_one_request  — accept one TCP connection, reply, return
    //   • mdns_respond_once  — answer one mDNS query for pulp.local
    //   • drain_until_back   — wait for BACK button
    //
    // The network runner is always polled in parallel via the outer
    // `select`.  When the HTTP or mDNS future completes we loop;
    // when BACK is pressed we break out.

    let mut rx_buf = [0u8; 1536];
    let mut tx_buf = [0u8; 1536];

    loop {
        match select(
            runner.run(),
            select(
                select(
                    serve_one_request(stack, &mut rx_buf, &mut tx_buf),
                    mdns_respond_once(stack, ip_octets),
                ),
                drain_until_back(),
            ),
        )
        .await
        {
            Either::Second(Either::First(_)) => continue, // served HTTP or mDNS
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

// ── mDNS responder ─────────────────────────────────────────────────

/// Bind a UDP socket on port 5353, wait for one mDNS query that asks
/// for `pulp.local` type-A, and reply with the device's IPv4 address.
///
/// If the incoming packet is not a matching query we silently ignore
/// it and return so the outer loop can re-enter promptly.
///
/// The socket buffers live inside this future's state (~1 KB) and are
/// released when the future is dropped.
async fn mdns_respond_once(stack: embassy_net::Stack<'_>, ip_octets: [u8; 4]) {
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 512];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);

    if socket.bind(MDNS_PORT).is_err() {
        // Port may momentarily be held by the previous iteration's
        // drop glue — back off briefly and let the caller retry.
        Timer::after(Duration::from_millis(100)).await;
        return;
    }

    let mut pkt = [0u8; 256];
    let (n, _remote) = match socket.recv_from(&mut pkt).await {
        Ok(r) => r,
        Err(_) => return,
    };

    if !is_mdns_query_for_pulp(&pkt[..n]) {
        return;
    }

    info!("upload: mDNS query for pulp.local — responding");

    let mut resp = [0u8; MDNS_RESPONSE_LEN];
    let len = build_mdns_response(&mut resp, ip_octets);

    // mDNS responses are sent to the multicast group, not unicast
    // back to the querier.
    let mdns_dest = embassy_net::IpEndpoint::new(
        embassy_net::IpAddress::Ipv4(embassy_net::Ipv4Address::new(224, 0, 0, 251)),
        MDNS_PORT,
    );
    let _ = socket.send_to(&resp[..len], mdns_dest).await;
}

/// Return `true` if `pkt` is a DNS/mDNS **query** whose first
/// question is "pulp.local" with QTYPE A (or ANY) and QCLASS IN.
fn is_mdns_query_for_pulp(pkt: &[u8]) -> bool {
    // Minimum size: 12 (header) + 12 (QNAME) + 2 (QTYPE) + 2 (QCLASS)
    if pkt.len() < 28 {
        return false;
    }

    // QR bit (bit 15 of flags) must be 0 → standard query.
    let flags = u16::from_be_bytes([pkt[2], pkt[3]]);
    if flags & 0x8000 != 0 {
        return false;
    }

    // At least one question.
    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]);
    if qdcount < 1 {
        return false;
    }

    // ── Match QNAME at offset 12 against "pulp.local" ──────────────
    //
    // Wire encoding: 04 70 75 6C 70  05 6C 6F 63 61 6C  00
    //                 ^p  u  l  p     ^l  o  c  a  l     ^root

    let qname = &pkt[12..24];

    if qname[0] != 4 || qname[5] != 5 || qname[11] != 0 {
        return false;
    }
    if !qname[1..5].eq_ignore_ascii_case(b"pulp") {
        return false;
    }
    if !qname[6..11].eq_ignore_ascii_case(b"local") {
        return false;
    }

    // QTYPE immediately follows QNAME.
    let qtype = u16::from_be_bytes([pkt[24], pkt[25]]);
    // QCLASS with the unicast-response bit (bit 15) masked off.
    let qclass = u16::from_be_bytes([pkt[26], pkt[27]]) & 0x7FFF;

    // Accept A (1) or ANY (255), class IN (1).
    (qtype == 1 || qtype == 255) && qclass == 1
}

/// Write a minimal mDNS A-record response for "pulp.local" into
/// `buf` and return the number of bytes written ([`MDNS_RESPONSE_LEN`]).
///
/// The caller must ensure `buf.len() >= MDNS_RESPONSE_LEN` (38).
fn build_mdns_response(buf: &mut [u8], ip: [u8; 4]) -> usize {
    let r = &mut buf[..MDNS_RESPONSE_LEN];

    // ── Header (12 bytes) ───────────────────────────────────────────
    r[0..2].copy_from_slice(&[0x00, 0x00]); // ID — 0 for mDNS
    r[2..4].copy_from_slice(&[0x84, 0x00]); // Flags: QR=1, AA=1
    r[4..6].copy_from_slice(&[0x00, 0x00]); // QDCOUNT = 0
    r[6..8].copy_from_slice(&[0x00, 0x01]); // ANCOUNT = 1
    r[8..10].copy_from_slice(&[0x00, 0x00]); // NSCOUNT = 0
    r[10..12].copy_from_slice(&[0x00, 0x00]); // ARCOUNT = 0

    // ── Answer RR ───────────────────────────────────────────────────
    r[12..24].copy_from_slice(&HOSTNAME_WIRE); // NAME
    r[24..26].copy_from_slice(&[0x00, 0x01]); // TYPE  = A
    r[26..28].copy_from_slice(&[0x80, 0x01]); // CLASS = IN + cache-flush
    r[28..32].copy_from_slice(&[0x00, 0x00, 0x00, 0x78]); // TTL = 120 s
    r[32..34].copy_from_slice(&[0x00, 0x04]); // RDLENGTH = 4
    r[34..38].copy_from_slice(&ip); // RDATA = IPv4 address

    MDNS_RESPONSE_LEN
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
///     │   http://pulp.local/     │  ← body font, centred
///     │     (192.168.1.42)       │
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
