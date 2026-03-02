// WiFi upload mode: HTTP file upload server run from Home menu.
// WiFi credentials are read from _PULP/SETTINGS.TXT (wifi_ssid / wifi_pass).
// GET / -> HTML file-picker form.
// POST /upload -> multipart/form-data streamed to SD root.
// No embassy tasks; runner, server, mDNS and back-button multiplexed with select.
// mDNS advertises as pulp.local so users don't need the DHCP IP.

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

use crate::apps::settings::WifiConfig;
use crate::board::action::{Action, ActionEvent, ButtonMapper};
use crate::board::{Epd, SCREEN_H, SCREEN_W};
use crate::drivers::sdcard::SdStorage;
use crate::drivers::storage;
use crate::drivers::strip::StripBuffer;
use crate::fonts;
use crate::fonts::bitmap::BitmapFont;
use crate::kernel::tasks;
use crate::ui::{Alignment, BitmapLabel, ButtonFeedback, CONTENT_TOP, Region, stack_fmt};

// layout

const HEADING_X: u16 = 16;
const HEADING_W: u16 = SCREEN_W - HEADING_X * 2;

const BODY_X: u16 = 24;
const BODY_W: u16 = SCREEN_W - BODY_X * 2;
const BODY_LINE_GAP: u16 = 10;

const FOOTER_Y: u16 = SCREEN_H - 60;

// HTTP response fragments

const HTTP_200_HTML: &[u8] =
    b"HTTP/1.0 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n";
const HTTP_200_JSON: &[u8] =
    b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n";
const HTTP_200_TEXT: &[u8] =
    b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n";
const HTTP_500_TEXT: &[u8] =
    b"HTTP/1.0 500 Internal Server Error\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n";
const HTTP_404: &[u8] = b"HTTP/1.0 404 Not Found\r\nConnection: close\r\n\r\nNot Found";

// Embedded HTML page with drag-and-drop, file listing, and delete support.
// JS is minimal: XHR upload with progress, fetch /files for listing, POST /delete.
const UPLOAD_PAGE: &[u8] = include_bytes!("../../assets/upload.html");

// mDNS constants

const MDNS_PORT: u16 = 5353;

// "pulp.local" in DNS wire format: length-prefixed labels + NUL
const HOSTNAME_WIRE: [u8; 12] = [
    4, b'p', b'u', b'l', b'p', //
    5, b'l', b'o', b'c', b'a', b'l', //
    0,
];

const MDNS_RESPONSE_LEN: usize = 38;

// max boundary string length
const MAX_BOUNDARY_LEN: usize = 120;

// work buffer for upload data; larger = fewer SD writes
const WORK_BUF_SIZE: usize = 2048;

enum ServerEvent {
    Nothing,
    Uploaded { name: [u8; 13], name_len: u8 },
    UploadFailed,
    Deleted { name: [u8; 13], name_len: u8 },
    DeleteFailed,
}

pub async fn run_upload_mode<SPI>(
    wifi: esp_hal::peripherals::WIFI<'static>,
    epd: &mut Epd,
    strip: &mut StripBuffer,
    delay: &mut Delay,
    sd: &SdStorage<SPI>,
    ui_font_size_idx: u8,
    bumps: &ButtonFeedback,
    wifi_cfg: &WifiConfig,
) where
    SPI: embedded_hal::spi::SpiDevice,
{
    let heading = fonts::heading_font(ui_font_size_idx);
    let body = fonts::chrome_font();

    if !wifi_cfg.has_credentials() {
        render_screen(
            epd,
            strip,
            delay,
            heading,
            body,
            &[
                "No WiFi credentials!",
                "Set wifi_ssid in",
                "_PULP/SETTINGS.TXT",
            ],
            Some("Press BACK to exit"),
            bumps,
            false,
        )
        .await;
        drain_until_back().await;
        return;
    }

    let ssid = wifi_cfg.ssid();
    let password = wifi_cfg.password();

    // screen 1: connecting

    {
        let mut msg_buf = [0u8; 64];
        let msg_len = stack_fmt(&mut msg_buf, |w| {
            let _ = write!(w, "Connecting to '{}'...", ssid);
        });
        let msg = core::str::from_utf8(&msg_buf[..msg_len]).unwrap_or("Connecting...");
        render_screen(epd, strip, delay, heading, body, &[msg], None, bumps, true).await;
    }

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
                bumps,
                false,
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
                bumps,
                false,
            )
            .await;
            drain_until_back().await;
            return;
        }
    };

    let client_cfg = ClientConfig::default()
        .with_ssid(String::from(ssid))
        .with_password(String::from(password));

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
            bumps,
            false,
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
            bumps,
            false,
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: wifi started, connecting to '{}'", ssid);

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
            bumps,
            false,
        )
        .await;
        drain_until_back().await;
        return;
    }

    info!("upload: connected to '{}'", ssid);

    let net_config = embassy_net::Config::dhcpv4(Default::default());
    let seed = {
        let rng = esp_hal::rng::Rng::new();
        (rng.random() as u64) << 32 | rng.random() as u64
    };

    let mut resources = embassy_net::StackResources::<4>::new();
    let (stack, mut runner) = embassy_net::new(interfaces.sta, net_config, &mut resources, seed);

    let got_ip = match select(
        runner.run(),
        select(stack.wait_config_up(), drain_until_back()),
    )
    .await
    {
        Either::Second(Either::First(_)) => true,
        Either::Second(Either::Second(_)) => false,
        _ => unreachable!(),
    };

    if !got_ip {
        info!("upload: user exited during DHCP");
        return;
    }

    let ip_octets: [u8; 4] = if let Some(cfg) = stack.config_v4() {
        cfg.address.address().octets()
    } else {
        [0, 0, 0, 0]
    };

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
        bumps,
        false,
    )
    .await;

    // phase 4: HTTP server + mDNS responder loop

    let mut rx_buf = [0u8; 2048];
    let mut tx_buf = [0u8; 1536];

    loop {
        let inner_result = match select(
            runner.run(),
            select(
                select(
                    serve_one_request(stack, &mut rx_buf, &mut tx_buf, sd),
                    mdns_respond_once(stack, ip_octets),
                ),
                drain_until_back(),
            ),
        )
        .await
        {
            Either::Second(Either::First(inner)) => inner,
            Either::Second(Either::Second(_)) => break, // back pressed
            _ => unreachable!(),
        };

        let event = match inner_result {
            Either::First(ev) => ev,
            Either::Second(()) => ServerEvent::Nothing,
        };

        match event {
            ServerEvent::Uploaded { name, name_len } => {
                let fname = core::str::from_utf8(&name[..name_len as usize]).unwrap_or("???");
                info!("upload: file saved as '{}'", fname);
            }
            ServerEvent::UploadFailed => {
                info!("upload: file upload failed");
            }
            ServerEvent::Deleted { name, name_len } => {
                let fname = core::str::from_utf8(&name[..name_len as usize]).unwrap_or("???");
                info!("upload: deleted '{}'", fname);
            }
            ServerEvent::DeleteFailed => {
                info!("upload: file delete failed");
            }
            ServerEvent::Nothing => {}
        }
    }

    info!("upload: exiting, tearing down WiFi");
}

// HTTP request handling

// accept one TCP connection, route the request, send a response
async fn serve_one_request<SPI>(
    stack: embassy_net::Stack<'_>,
    rx_buf: &mut [u8],
    tx_buf: &mut [u8],
    sd: &SdStorage<SPI>,
) -> ServerEvent
where
    SPI: embedded_hal::spi::SpiDevice,
{
    let mut socket = TcpSocket::new(stack, rx_buf, tx_buf);
    socket.set_timeout(Some(Duration::from_secs(30)));

    if socket
        .accept(IpListenEndpoint {
            addr: None,
            port: 80,
        })
        .await
        .is_err()
    {
        Timer::after(Duration::from_millis(200)).await;
        return ServerEvent::Nothing;
    }

    // read HTTP request headers (accumulate until \r\n\r\n)
    let mut hdr = [0u8; 1024];
    let mut hdr_len = 0usize;

    loop {
        match socket.read(&mut hdr[hdr_len..]).await {
            Ok(0) => {
                close_socket(&mut socket).await;
                return ServerEvent::Nothing;
            }
            Ok(n) => {
                hdr_len += n;
                if hdr[..hdr_len].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if hdr_len >= hdr.len() {
                    let _ = socket
                        .write_all(b"HTTP/1.0 431 Headers Too Large\r\n\r\n")
                        .await;
                    close_socket(&mut socket).await;
                    return ServerEvent::Nothing;
                }
            }
            Err(_) => {
                close_socket(&mut socket).await;
                return ServerEvent::Nothing;
            }
        }
    }

    // locate end of headers; body data may follow in the same read
    let headers_end = match find_subsequence(&hdr[..hdr_len], b"\r\n\r\n") {
        Some(p) => p,
        None => {
            close_socket(&mut socket).await;
            return ServerEvent::Nothing;
        }
    };
    let body_offset = headers_end + 4;
    let initial_body = &hdr[body_offset..hdr_len];
    let headers = &hdr[..headers_end];

    // parse request line: "METHOD /path HTTP/x.x"
    let first_line_end = headers
        .iter()
        .position(|&b| b == b'\r')
        .unwrap_or(headers.len());
    let request_line = &headers[..first_line_end];

    let is_get = request_line.starts_with(b"GET ");
    let is_post = request_line.starts_with(b"POST ");

    let path = extract_path(request_line);

    // ── GET / ── serve the upload page HTML ──────────────────────────
    if is_get && path == b"/" {
        let _ = socket.write_all(HTTP_200_HTML).await;
        let _ = socket.write_all(UPLOAD_PAGE).await;
        let _ = socket.flush().await;
        close_socket(&mut socket).await;
        return ServerEvent::Nothing;
    }

    // ── GET /files ── JSON array of {name, size} ────────────────────
    if is_get && path == b"/files" {
        let _ = socket.write_all(HTTP_200_JSON).await;

        let mut entries = [storage::DirEntry::EMPTY; 64];
        let count = match storage::list_root_files(sd, &mut entries) {
            Ok(n) => n,
            Err(_) => {
                let _ = socket.write_all(b"[]").await;
                let _ = socket.flush().await;
                close_socket(&mut socket).await;
                return ServerEvent::Nothing;
            }
        };

        let _ = socket.write_all(b"[").await;
        let mut json_buf = [0u8; 80]; // per-entry scratch: {"name":"XXXXXXXX.XXX","size":4294967295}
        for i in 0..count {
            let e = &entries[i];
            let name = e.name_str();
            let mut pos = 0usize;
            let prefix = b"{\"name\":\"";
            json_buf[..prefix.len()].copy_from_slice(prefix);
            pos += prefix.len();
            let nb = name.as_bytes();
            json_buf[pos..pos + nb.len()].copy_from_slice(nb);
            pos += nb.len();
            let mid = b"\",\"size\":";
            json_buf[pos..pos + mid.len()].copy_from_slice(mid);
            pos += mid.len();
            // format u32 without alloc
            pos += fmt_u32(e.size, &mut json_buf[pos..]);
            json_buf[pos] = b'}';
            pos += 1;
            if i + 1 < count {
                json_buf[pos] = b',';
                pos += 1;
            }
            let _ = socket.write_all(&json_buf[..pos]).await;
        }
        let _ = socket.write_all(b"]").await;
        let _ = socket.flush().await;
        close_socket(&mut socket).await;
        return ServerEvent::Nothing;
    }

    // ── POST /upload ── multipart file upload ───────────────────────
    if is_post && path == b"/upload" {
        let boundary = match find_boundary(headers) {
            Some(b) => b,
            None => {
                send_error_response(&mut socket, "Missing multipart boundary").await;
                close_socket(&mut socket).await;
                return ServerEvent::UploadFailed;
            }
        };

        match handle_upload(&mut socket, sd, boundary, initial_body).await {
            Ok((name_buf, name_len)) => {
                let _ = socket.write_all(HTTP_200_TEXT).await;
                let _ = socket.write_all(b"OK").await;
                let _ = socket.flush().await;
                close_socket(&mut socket).await;
                return ServerEvent::Uploaded {
                    name: name_buf,
                    name_len,
                };
            }
            Err(e) => {
                info!("upload: handle_upload error: {}", e);
                send_error_response(&mut socket, e).await;
                close_socket(&mut socket).await;
                return ServerEvent::UploadFailed;
            }
        }
    }

    // ── POST /delete ── body = filename to delete ───────────────────
    if is_post && path == b"/delete" {
        // read body (filename); may need to read more beyond initial_body
        let content_len = extract_content_length(headers).unwrap_or(0);
        let max_body = content_len.min(13); // 8.3 filename max
        let mut body = [0u8; 16];
        let have = initial_body.len().min(body.len());
        body[..have].copy_from_slice(&initial_body[..have]);
        let mut body_len = have;

        while body_len < max_body && body_len < body.len() {
            match socket.read(&mut body[body_len..]).await {
                Ok(0) => break,
                Ok(n) => body_len += n,
                Err(_) => break,
            }
        }

        let name = match core::str::from_utf8(&body[..body_len]) {
            Ok(s) => s.trim(),
            Err(_) => {
                send_error_response(&mut socket, "Invalid filename").await;
                close_socket(&mut socket).await;
                return ServerEvent::DeleteFailed;
            }
        };

        if name.is_empty() || name.len() > 12 {
            send_error_response(&mut socket, "Invalid filename").await;
            close_socket(&mut socket).await;
            return ServerEvent::DeleteFailed;
        }

        // copy name into fixed buffer before passing to storage
        let mut name_buf = [0u8; 13];
        let name_bytes = name.as_bytes();
        name_buf[..name_bytes.len()].copy_from_slice(name_bytes);
        let name_len = name_bytes.len() as u8;

        match storage::delete_file(sd, name) {
            Ok(()) => {
                let _ = socket.write_all(HTTP_200_TEXT).await;
                let _ = socket.write_all(b"OK").await;
                let _ = socket.flush().await;
                close_socket(&mut socket).await;
                return ServerEvent::Deleted {
                    name: name_buf,
                    name_len,
                };
            }
            Err(e) => {
                info!("upload: delete failed for '{}': {}", name, e);
                send_error_response(&mut socket, e).await;
                close_socket(&mut socket).await;
                return ServerEvent::DeleteFailed;
            }
        }
    }

    // fallback: 404
    let _ = socket.write_all(HTTP_404).await;
    let _ = socket.flush().await;
    close_socket(&mut socket).await;
    ServerEvent::Nothing
}

// stream a multipart upload body to SD; initial_body = body bytes read with headers.
// returns sanitised 8.3 filename on success.
async fn handle_upload<SPI>(
    socket: &mut TcpSocket<'_>,
    sd: &SdStorage<SPI>,
    boundary: &[u8],
    initial_body: &[u8],
) -> Result<([u8; 13], u8), &'static str>
where
    SPI: embedded_hal::spi::SpiDevice,
{
    if boundary.len() > MAX_BOUNDARY_LEN {
        return Err("boundary too long");
    }

    // build end-of-file-data marker: \r\n--<boundary>
    let em_len = 4 + boundary.len();
    let mut end_marker_buf = [0u8; MAX_BOUNDARY_LEN + 4];
    end_marker_buf[0] = b'\r';
    end_marker_buf[1] = b'\n';
    end_marker_buf[2] = b'-';
    end_marker_buf[3] = b'-';
    end_marker_buf[4..em_len].copy_from_slice(boundary);
    let end_marker = &end_marker_buf[..em_len];

    // phase A: skip multipart preamble, find file data start.
    // accumulate until \r\n\r\n (blank line ending part headers);
    // everything after is file data.

    let mut work = [0u8; WORK_BUF_SIZE];
    let init_len = initial_body.len().min(work.len());
    work[..init_len].copy_from_slice(&initial_body[..init_len]);
    let mut filled = init_len;

    let (file_name_buf, file_name_len) = loop {
        if let Some(pos) = find_subsequence(&work[..filled], b"\r\n\r\n") {
            let part_headers = &work[..pos];

            // extract raw filename from Content-Disposition
            let raw_name = extract_filename(part_headers).ok_or("no filename in upload")?;
            let (name_buf, name_len) = sanitize_83(raw_name);
            if name_len == 0 {
                return Err("invalid filename");
            }

            // shift remaining file data to front of work buffer
            let file_start = pos + 4;
            work.copy_within(file_start..filled, 0);
            filled -= file_start;

            break (name_buf, name_len);
        }

        if filled >= work.len() {
            return Err("part headers too large");
        }

        let n = socket
            .read(&mut work[filled..])
            .await
            .map_err(|_| "read error")?;
        if n == 0 {
            return Err("connection closed during headers");
        }
        filled += n;
    };

    let name_str = core::str::from_utf8(&file_name_buf[..file_name_len as usize])
        .map_err(|_| "filename encoding error")?;

    info!("upload: receiving file '{}'", name_str);

    // create (or truncate) file on SD
    storage::write_file(sd, name_str, &[])?;

    // phase B: stream file data to SD.
    // keep last end_marker.len() bytes unwritten (holdback) to detect
    // boundary spanning two TCP reads; everything before is safe to flush.

    let mut total_written: u32 = 0;

    loop {
        // check for end marker in current buffer
        if let Some(pos) = find_subsequence(&work[..filled], end_marker) {
            // write remaining file data before the marker
            if pos > 0 {
                storage::append_root_file(sd, name_str, &work[..pos])?;
                total_written += pos as u32;
            }
            info!("upload: complete, {} bytes written", total_written);
            return Ok((file_name_buf, file_name_len));
        }

        // flush safe prefix (everything except holdback zone)
        if filled > end_marker.len() {
            let safe = filled - end_marker.len();
            storage::append_root_file(sd, name_str, &work[..safe])?;
            total_written += safe as u32;

            // compact: move holdback to front
            work.copy_within(safe..filled, 0);
            filled = end_marker.len();
        }

        // read more data from network
        let n = socket
            .read(&mut work[filled..])
            .await
            .map_err(|_| "read error during upload")?;
        if n == 0 {
            // connection closed before end marker found;
            // write what we have (may include partial boundary junk)
            if filled > 0 {
                let _ = storage::append_root_file(sd, name_str, &work[..filled]);
            }
            return Err("upload incomplete");
        }
        filled += n;
    }
}

// HTTP helpers

// extract request path from a request line like "GET /path HTTP/1.1"
fn extract_path(line: &[u8]) -> &[u8] {
    // skip method
    let start = match line.iter().position(|&b| b == b' ') {
        Some(p) => p + 1,
        None => return b"/",
    };
    // find end of path
    let rest = &line[start..];
    let end = rest.iter().position(|&b| b == b' ').unwrap_or(rest.len());
    // strip query string
    let path = &rest[..end];
    let qmark = path.iter().position(|&b| b == b'?').unwrap_or(path.len());
    &path[..qmark]
}

// extract multipart boundary from headers block
fn find_boundary(headers: &[u8]) -> Option<&[u8]> {
    // search for "boundary=" case-insensitively
    let marker = b"boundary=";
    let pos = headers
        .windows(marker.len())
        .position(|w| w.eq_ignore_ascii_case(marker))?;
    let start = pos + marker.len();
    let rest = &headers[start..];

    if rest.is_empty() {
        return None;
    }

    // handle quoted or unquoted value
    if rest[0] == b'"' {
        let inner = &rest[1..];
        let end = inner.iter().position(|&b| b == b'"')?;
        if end == 0 {
            return None;
        }
        Some(&inner[..end])
    } else {
        let end = rest
            .iter()
            .position(|&b| b == b'\r' || b == b'\n' || b == b';' || b == b' ')
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        Some(&rest[..end])
    }
}

// extract raw filename bytes from multipart part headers
fn extract_filename(headers: &[u8]) -> Option<&[u8]> {
    let marker = b"filename=\"";
    let pos = headers
        .windows(marker.len())
        .position(|w| w.eq_ignore_ascii_case(marker))?;
    let start = pos + marker.len();
    let rest = &headers[start..];
    let end = rest.iter().position(|&b| b == b'"')?;
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

// sanitise raw filename to valid FAT 8.3; returns (buf, len)
fn sanitize_83(raw: &[u8]) -> ([u8; 13], u8) {
    // strip path components
    let name = match raw.iter().rposition(|&b| b == b'/' || b == b'\\') {
        Some(p) => &raw[p + 1..],
        None => raw,
    };

    // split into base and extension at last dot
    let (base_src, ext_src) = match name.iter().rposition(|&b| b == b'.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, &[] as &[u8]),
    };

    let mut out = [0u8; 13];
    let mut pos: usize = 0;

    // base name: up to 8 chars, uppercased
    for &b in base_src.iter() {
        if pos >= 8 {
            break;
        }
        if is_valid_83_char(b) {
            out[pos] = b.to_ascii_uppercase();
            pos += 1;
        }
    }

    // fallback if base is empty after filtering
    if pos == 0 {
        out[..6].copy_from_slice(b"UPLOAD");
        pos = 6;
    }

    // extension: up to 3 chars, uppercased
    if !ext_src.is_empty() {
        out[pos] = b'.';
        pos += 1;
        let ext_start = pos;
        for &b in ext_src.iter() {
            if pos - ext_start >= 3 {
                break;
            }
            if is_valid_83_char(b) {
                out[pos] = b.to_ascii_uppercase();
                pos += 1;
            }
        }
        // remove dot if no valid extension chars
        if pos == ext_start {
            pos -= 1;
        }
    }

    (out, pos as u8)
}

fn is_valid_83_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'~' | b'!' | b'#' | b'$' | b'&')
}

// send a plain-text error response
async fn send_error_response(socket: &mut TcpSocket<'_>, msg: &str) {
    let _ = socket.write_all(HTTP_500_TEXT).await;
    let _ = socket.write_all(msg.as_bytes()).await;
    let _ = socket.flush().await;
}

// format a u32 into decimal ASCII; returns number of bytes written
fn fmt_u32(mut n: u32, buf: &mut [u8]) -> usize {
    if n == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 10];
    let mut pos = 0;
    while n > 0 {
        tmp[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        pos += 1;
    }
    for i in 0..pos {
        buf[i] = tmp[pos - 1 - i];
    }
    pos
}

// extract Content-Length value from headers
fn extract_content_length(headers: &[u8]) -> Option<usize> {
    let marker = b"content-length:";
    let pos = headers
        .windows(marker.len())
        .position(|w| w.eq_ignore_ascii_case(marker))?;
    let start = pos + marker.len();
    let rest = &headers[start..];
    // skip whitespace
    let trimmed = rest.iter().position(|&b| b != b' ' && b != b'\t')?;
    let rest = &rest[trimmed..];
    let end = rest
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(rest.len());
    let digits = &rest[..end];
    let mut val: usize = 0;
    for &b in digits {
        if b.is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b - b'0') as usize);
        } else {
            break;
        }
    }
    Some(val)
}

// gracefully close a TCP socket
async fn close_socket(socket: &mut TcpSocket<'_>) {
    Timer::after(Duration::from_millis(50)).await;
    socket.close();
    Timer::after(Duration::from_millis(50)).await;
    socket.abort();
}

// find first occurrence of needle in haystack
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// mDNS responder

async fn mdns_respond_once(stack: embassy_net::Stack<'_>, ip_octets: [u8; 4]) {
    let mut rx_meta = [PacketMetadata::EMPTY; 2];
    let mut rx_buf = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 2];
    let mut tx_buf = [0u8; 512];

    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx_buf, &mut tx_meta, &mut tx_buf);

    if socket.bind(MDNS_PORT).is_err() {
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

    let mdns_dest = embassy_net::IpEndpoint::new(
        embassy_net::IpAddress::Ipv4(embassy_net::Ipv4Address::new(224, 0, 0, 251)),
        MDNS_PORT,
    );
    let _ = socket.send_to(&resp[..len], mdns_dest).await;
}

fn is_mdns_query_for_pulp(pkt: &[u8]) -> bool {
    if pkt.len() < 28 {
        return false;
    }

    let flags = u16::from_be_bytes([pkt[2], pkt[3]]);
    if flags & 0x8000 != 0 {
        return false;
    }

    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]);
    if qdcount < 1 {
        return false;
    }

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

    let qtype = u16::from_be_bytes([pkt[24], pkt[25]]);
    let qclass = u16::from_be_bytes([pkt[26], pkt[27]]) & 0x7FFF;

    (qtype == 1 || qtype == 255) && qclass == 1
}

fn build_mdns_response(buf: &mut [u8], ip: [u8; 4]) -> usize {
    let r = &mut buf[..MDNS_RESPONSE_LEN];

    r[0..2].copy_from_slice(&[0x00, 0x00]);
    r[2..4].copy_from_slice(&[0x84, 0x00]);
    r[4..6].copy_from_slice(&[0x00, 0x00]);
    r[6..8].copy_from_slice(&[0x00, 0x01]);
    r[8..10].copy_from_slice(&[0x00, 0x00]);
    r[10..12].copy_from_slice(&[0x00, 0x00]);

    r[12..24].copy_from_slice(&HOSTNAME_WIRE);
    r[24..26].copy_from_slice(&[0x00, 0x01]);
    r[26..28].copy_from_slice(&[0x80, 0x01]);
    r[28..32].copy_from_slice(&[0x00, 0x00, 0x00, 0x78]);
    r[32..34].copy_from_slice(&[0x00, 0x04]);
    r[34..38].copy_from_slice(&ip);

    MDNS_RESPONSE_LEN
}

// input helpers

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

// display helpers

async fn render_screen(
    epd: &mut Epd,
    strip: &mut StripBuffer,
    delay: &mut Delay,
    heading: &'static BitmapFont,
    body: &'static BitmapFont,
    lines: &[&str],
    footer: Option<&str>,
    bumps: &ButtonFeedback,
    full_refresh: bool,
) {
    let heading_h = heading.line_height;
    let body_h = body.line_height;
    let body_stride = body_h + BODY_LINE_GAP;

    let heading_region = Region::new(HEADING_X, CONTENT_TOP + 12, HEADING_W, heading_h);

    let body_area_top = CONTENT_TOP + 12 + heading_h + 40;
    let body_area_bottom = FOOTER_Y.saturating_sub(20);
    let body_area_h = body_area_bottom.saturating_sub(body_area_top);
    let total_body_h = if lines.is_empty() {
        0
    } else {
        (lines.len() as u16 - 1) * body_stride + body_h
    };
    let body_start_y = body_area_top + body_area_h.saturating_sub(total_body_h) / 2;

    let footer_region = Region::new(BODY_X, FOOTER_Y, BODY_W, body_h);

    let draw = |s: &mut StripBuffer| {
        BitmapLabel::new(heading_region, "Upload", heading)
            .alignment(Alignment::CenterLeft)
            .draw(s)
            .unwrap();

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

        if let Some(text) = footer {
            BitmapLabel::new(footer_region, text, body)
                .alignment(Alignment::Center)
                .draw(s)
                .unwrap();
        }

        bumps.draw(s);
    };

    if full_refresh {
        epd.full_refresh_async(strip, delay, &draw).await;
    } else {
        epd.partial_refresh_async(strip, delay, 0, 0, SCREEN_W, SCREEN_H, &draw)
            .await;
    }
}
