//! Receives files and page URLs from the browser extension over 127.0.0.1.
//!
//! Every request needs the pairing token, and browser pages are refused by origin
//! (only `chrome-extension://` may call us). Uploads are written to a temp inbox on
//! the request's own thread; the UI thread then imports them like any other file.

use eframe::egui;
use serde_json::json;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
};
use tiny_http::{Header, Method, Request, Response};

pub enum WebMsg {
    /// A received file in the inbox, with the page it came from.
    File { path: PathBuf, url: String, folder: Option<String> },
    /// A page to hand to yt-dlp.
    Ytdlp { url: String },
}

/// (id, name, depth) of the open library's folders, in menu order.
pub type FolderList = Vec<(String, String, usize)>;

/// A caller that had the pairing code, while OrangeLink has been open.
#[derive(Clone, Debug)]
pub struct Client {
    /// `chrome-extension://<id>`, or empty for a program that sends no Origin.
    pub origin: String,
    pub last_seen: std::time::SystemTime,
    pub requests: u64,
    /// Refused because only another extension is allowed.
    pub blocked: bool,
}

/// Who has called, and which extension alone may call (None: any with the code).
#[derive(Default)]
struct Callers {
    clients: Vec<Client>,
    only: Option<String>,
}

impl Callers {
    /// Whether `origin` may go on; either way it is listed.
    fn admit(&mut self, origin: &str) -> bool {
        let ok = self.only.as_deref().is_none_or(|o| o == origin);
        let now = std::time::SystemTime::now();
        match self.clients.iter_mut().find(|c| c.origin == origin) {
            Some(c) => {
                c.last_seen = now;
                c.requests += 1;
                c.blocked = !ok;
            }
            None => self.clients.push(Client { origin: origin.to_string(), last_seen: now, requests: 1, blocked: !ok }),
        }
        ok
    }
}

pub struct WebServer {
    server: Arc<tiny_http::Server>,
    folders: Arc<Mutex<FolderList>>,
    max_bytes: Arc<AtomicU64>,
    callers: Arc<Mutex<Callers>>,
    pub rx: Receiver<WebMsg>,
    pub port: u16,
    pub token: String,
}

impl WebServer {
    pub fn start(port: u16, token: String, ctx: egui::Context) -> Result<Self, String> {
        // A server restarted with a new pairing code frees its port just after being dropped.
        let server = Arc::new(crate::mcp::bind_local(port)?);
        // Port 0 lets the system pick one (the tests); report the one we got.
        let port = server.server_addr().to_ip().map_or(port, |a| a.port());
        // Clean leftovers from a previous process once at first bind (not on
        // restarts triggered by settings edits while imports may still be queued).
        static INBOX_CLEANED: std::sync::Once = std::sync::Once::new();
        INBOX_CLEANED.call_once(|| {
            let _ = fs::remove_dir_all(inbox_dir());
        });
        let (tx, rx) = channel();
        let folders = Arc::new(Mutex::new(Vec::new()));
        let max_bytes = Arc::new(AtomicU64::new(DEFAULT_MAX_GB * GB));
        let callers = Arc::new(Mutex::new(Callers::default()));
        let (s, t, f, m, who) = (server.clone(), token.clone(), folders.clone(), max_bytes.clone(), callers.clone());
        let in_flight = Arc::new(AtomicUsize::new(0));
        thread::spawn(move || {
            for req in s.incoming_requests() {
                // Checked before a thread is started: other programs on this computer
                // can't open threads without the pairing code.
                let Some(req) = admit(req, &t, &who) else { continue };
                if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    fail_close(req, 503, t!("OrangeLink 正在處理其他檔案，請稍後再試。"));
                    continue;
                }
                // Own thread per request: a large upload must not block the others.
                let (tx, f, ctx, in_flight) = (tx.clone(), f.clone(), ctx.clone(), in_flight.clone());
                let max = m.load(Ordering::Relaxed);
                thread::spawn(move || {
                    struct Guard(Arc<AtomicUsize>);
                    impl Drop for Guard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _guard = Guard(in_flight);
                    handle(req, &f, max, &tx, &ctx);
                });
            }
        });
        Ok(Self { server, folders, max_bytes, callers, rx, port, token })
    }

    /// Largest file the extension may send; bigger uploads are refused with 413.
    pub fn set_max_bytes(&self, n: u64) {
        self.max_bytes.store(n, Ordering::Relaxed);
    }

    /// The callers so far, most recent first.
    pub fn clients(&self) -> Vec<Client> {
        let mut list = crate::lock(&self.callers).clients.clone();
        list.sort_by_key(|a| std::cmp::Reverse(a.last_seen));
        list
    }

    /// Only this extension's origin may call (None: any that has the pairing code).
    pub fn set_only(&self, origin: Option<String>) {
        crate::lock(&self.callers).only = origin;
    }

    /// Publishes the folders the extension may offer and file uploads may target.
    pub fn set_folders(&self, list: FolderList) {
        *crate::lock(&self.folders) = list;
    }
}

impl Drop for WebServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// Requests handled at once; more are told to try again.
const MAX_IN_FLIGHT: usize = 8;
/// At most this many bytes are drained on a rejected worker request before closing.
const MAX_DRAIN: u64 = 1 << 20;

pub const GB: u64 = 1 << 30;
pub const DEFAULT_MAX_GB: u64 = 4;

fn inbox_dir() -> PathBuf {
    std::env::temp_dir().join("OrangeLink-inbox")
}

pub fn new_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// What the user pastes into the extension.
pub fn pairing_code(port: u16, token: &str) -> String {
    format!("{port}-{token}")
}

fn reply(req: Request, status: u16, body: serde_json::Value) {
    let header = Header::from_bytes("Content-Type", "application/json").expect("valid header");
    let _ = req.respond(Response::from_string(body.to_string()).with_status_code(status).with_header(header));
}

fn fail(req: Request, status: u16, msg: &str) {
    reply(req, status, json!({ "error": msg }));
}

/// Sends an error and closes the connection immediately (without reading the body).
fn fail_close(req: Request, status: u16, msg: &str) {
    let ct = Header::from_bytes("Content-Type", "application/json").expect("valid header");
    let close = Header::from_bytes("Connection", "close").expect("valid header");
    let _ = req.respond(
        Response::from_string(json!({ "error": msg }).to_string())
            .with_status_code(status)
            .with_header(ct)
            .with_header(close),
    );
}

/// Drains at most `MAX_DRAIN` bytes on a rejected worker request so small uploads
/// see the HTTP status cleanly without letting huge/stalled uploads tie up workers.
fn drain_capped(req: &mut Request) {
    if req.body_length().is_some_and(|n| (n as u64) <= MAX_DRAIN) {
        let _ = std::io::copy(&mut req.as_reader().take(MAX_DRAIN), &mut std::io::sink());
    }
}

fn header_value(req: &Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

/// Answers requests from web pages, non-local hosts, or without the pairing code
/// immediately without reading the body on the single accept thread (prevents Slowloris DoS).
fn admit(req: Request, token: &str, callers: &Mutex<Callers>) -> Option<Request> {
    if header_value(&req, "Host").is_some_and(|h| !crate::mcp::is_local_host(&h)) {
        fail_close(req, 403, "forbidden host");
        return None;
    }
    let origin = header_value(&req, "Origin").unwrap_or_default();
    if !origin.is_empty() && !origin.starts_with("chrome-extension://") {
        fail_close(req, 403, "forbidden origin");
        return None;
    }
    if !header_value(&req, "X-OrangeLink-Token").is_some_and(|t| crate::mcp::same_token(&t, token)) {
        fail_close(req, 401, t!("配對碼不正確"));
        return None;
    }
    if !crate::lock(callers).admit(&origin) {
        fail_close(req, 403, t!("OrangeLink 只允許另一個擴充功能連線；可在 OrangeLink 設定 → 瀏覽器擴充功能 更改。"));
        return None;
    }
    Some(req)
}

fn handle(mut req: Request, folders: &Mutex<FolderList>, max_bytes: u64, tx: &Sender<WebMsg>, ctx: &egui::Context) {
    let (path, query) = {
        let url = req.url();
        let (p, q) = url.split_once('?').unwrap_or((url, ""));
        (p.to_string(), q.to_string())
    };
    let msg = match (req.method(), path.as_str()) {
        (Method::Get, "/ping") => {
            return reply(req, 200, json!({ "name": "OrangeLink", "version": env!("CARGO_PKG_VERSION"), "max_bytes": max_bytes }));
        }
        (Method::Get, "/folders") => {
            let list: Vec<_> = crate::lock(folders).iter().map(|(id, name, depth)| json!({ "id": id, "name": name, "depth": depth })).collect();
            return reply(req, 200, json!({ "folders": list }));
        }
        (Method::Post, "/ytdlp") => {
            let url = query_param(&query, "url").unwrap_or_default();
            if !is_web_url(&url) {
                return fail(req, 400, t!("網址不正確"));
            }
            WebMsg::Ytdlp { url }
        }
        (Method::Post, "/file") => {
            let name = query_param(&query, "name").unwrap_or_default();
            let mime = query_param(&query, "type").unwrap_or_default();
            let url = query_param(&query, "url").filter(|u| is_web_url(u)).unwrap_or_default();
            let name = file_name(&name, &mime);
            if is_executable(&name) {
                drain_capped(&mut req);
                return fail_close(req, 415, &t!("已封鎖可執行檔「{name}」，避免誤開造成危險。", name));
            }
            let folder = query_param(&query, "folder").filter(|f| !f.is_empty());
            // The menu can be a moment behind, e.g. right after switching library.
            if folder.as_ref().is_some_and(|f| !crate::lock(folders).iter().any(|(id, ..)| id == f)) {
                drain_capped(&mut req);
                return fail_close(req, 409, t!("這個資料夾已不存在，選單已更新，請重新選擇。"));
            }
            match save_upload(&mut req, &name, max_bytes) {
                Ok(path) => {
                    mark_from_web(&path, &url);
                    WebMsg::File { path, url, folder }
                }
                Err((status, e)) => return fail_close(req, status, &e),
            }
        }
        _ => return fail(req, 404, "not found"),
    };
    if tx.send(msg).is_err() {
        return fail(req, 503, t!("OrangeLink 正在關閉"));
    }
    ctx.request_repaint();
    reply(req, 200, json!({ "ok": true }));
}

/// Streams the request body into a fresh inbox folder; a cut-off or oversized upload
/// leaves nothing. Errors carry the HTTP status to reply with.
fn save_upload(req: &mut Request, name: &str, max_bytes: u64) -> Result<PathBuf, (u16, String)> {
    let too_large = || (413, t!("檔案超過 {gb} GB 的上限，可在 OrangeLink 設定調整。", gb = format!("{:.1}", max_bytes as f64 / GB as f64)));
    let expected = req.body_length();
    if expected.is_some_and(|n| n as u64 > max_bytes) {
        drain_capped(req);
        return Err(too_large());
    }
    let dir = inbox_dir().join(crate::model::new_id());
    fs::create_dir_all(&dir).map_err(|e| (500, t!("無法建立暫存資料夾：{e}", e)))?;
    let path = dir.join(name);
    let result = (|| {
        let mut out = fs::File::create(&path).map_err(|e| (500, t!("無法寫入暫存檔：{e}", e)))?;
        // One byte past the limit is enough to know it is too large.
        let copied = std::io::copy(&mut req.as_reader().take(max_bytes.saturating_add(1)), &mut out).map_err(|e| (500, t!("接收檔案失敗：{e}", e)))?;
        if copied > max_bytes {
            return Err(too_large());
        }
        if copied == 0 {
            return Err((500, t!("收到的檔案是空的").to_string()));
        }
        if expected.is_some_and(|n| n as u64 != copied) {
            return Err((500, t!("檔案傳輸中斷").to_string()));
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            Err(e)
        }
    }
}

/// Marks a received file as downloaded from the internet, as browsers do (the
/// "Mark of the Web": a `Zone.Identifier` stream), so Windows still warns, and
/// Office opens it in Protected View, when it is opened from the library later.
/// The import copies it with the file. A drive without such streams (FAT, exFAT)
/// just goes without.
#[cfg(windows)]
fn mark_from_web(path: &Path, page: &str) {
    let clean: String = page.chars().filter(|c| !c.is_control()).collect();
    let mut zone = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if !clean.is_empty() {
        zone.push_str(&format!("ReferrerUrl={clean}\r\n"));
    }
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    if let Err(e) = fs::write(&stream, zone) {
        crate::log::info("webclip", format!("無法標記網路來源：{e}"));
    }
}

#[cfg(not(windows))]
fn mark_from_web(_path: &Path, _page: &str) {}

/// Removes a received file and its inbox folder. Refuses anything outside the inbox.
pub fn remove_received(path: &Path) {
    if let Some(dir) = path.parent().filter(|d| d.parent() == Some(inbox_dir().as_path())) {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Only http(s) without control characters or newlines (which could inject INI
/// lines into Zone.Identifier or break external tools).
fn is_web_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_control()) {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%')
            .then(|| b.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) => {
                out.push(v);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Files Windows runs when opened. Checked on the final name, after `file_name` has
/// removed the trailing dots and spaces Windows would ignore.
fn is_executable(name: &str) -> bool {
    const BLOCKED: &[&str] = &[
        "exe", "com", "scr", "pif", "cpl", "dll", "sys", "ocx", "msi", "msp", "mst", "msix", "msixbundle", "appx",
        "appxbundle", "application", "bat", "cmd", "ps1", "psm1", "psd1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta",
        "jar", "lnk", "url", "scf", "reg", "inf", "chm", "xll", "gadget",
        // Disk images open as a drive whose files lose the warning; and Windows'
        // other run-on-open formats.
        "iso", "img", "vhd", "vhdx", "appref-ms", "settingcontent-ms", "library-ms", "search-ms", "diagcab", "msc", "ws",
        "wsc", "website", "cab",
    ];
    // Not Path::extension: for ".exe" that is None, but Windows runs it as an exe.
    name.rsplit_once('.').is_some_and(|(_, e)| BLOCKED.contains(&e.to_ascii_lowercase().as_str()))
}

fn ext_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        _ => return None,
    })
}

/// A safe Windows file name from what the browser reported: no folders or reserved
/// names, and an extension from the content type when the name doesn't have a usable one.
fn file_name(name: &str, mime: &str) -> String {
    // Keep only the last path part so a name can't leave the inbox folder.
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    // Direction overrides go: "evil\u{202E}gpj.exe" shows as "evilexe.jpg".
    let clean: String = base
        .chars()
        .filter(|&c| !crate::model::is_bidi_control(c))
        .map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']);
    let path = Path::new(clean);
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let mut stem = if stem.is_empty() || stem.chars().all(|c| c == '.') { "download".to_string() } else { stem };

    if let Some(known) = ext_for_mime(mime) {
        if ext.is_empty() {
            ext = known.to_string();
        } else if !ext.eq_ignore_ascii_case(known) && crate::media::kind_of(path) == crate::model::MediaKind::Other {
            // e.g. photo.php served as image/jpeg
            stem = format!("{stem}.{ext}");
            ext = known.to_string();
        }
    }
    // "CON .txt" is the device too: Windows ignores the spaces.
    let device = stem.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4 && (device.starts_with("COM") || device.starts_with("LPT")) && device.as_bytes()[3].is_ascii_digit());
    if reserved {
        stem.insert(0, '_');
    }
    // Cut to length, which can leave a dot or space at the end again.
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>().trim_end_matches(['.', ' ']).to_string();
    let (stem, ext) = (cut(&stem, 120), cut(&ext, 16));
    let stem = if stem.is_empty() { "download".to_string() } else { stem };
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

const EXTENSION_FILES: &[(&str, &[u8])] = &[
    ("manifest.json", include_bytes!("../extension/manifest.json")),
    ("background.js", include_bytes!("../extension/background.js")),
    ("popup.html", include_bytes!("../extension/popup.html")),
    ("popup.js", include_bytes!("../extension/popup.js")),
    ("icon48.png", include_bytes!("../extension/icon48.png")),
    ("icon128.png", include_bytes!("../extension/icon128.png")),
];

/// Writes the bundled extension where the browser can load it from.
pub fn write_extension() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("OrangeLink").join("browser-extension");
    fs::create_dir_all(&dir)?;
    for (name, bytes) in EXTENSION_FILES {
        fs::write(dir.join(name), bytes)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_extension_carries_the_apps_version() {
        let manifest: serde_json::Value = serde_json::from_slice(include_bytes!("../extension/manifest.json")).unwrap();
        assert_eq!(
            manifest["version"].as_str(),
            Some(env!("CARGO_PKG_VERSION")),
            "extension/manifest.json's version must match Cargo.toml's, so the extensions page shows which OrangeLink it came from"
        );
    }

    #[test]
    fn file_names_stay_safe() {
        assert_eq!(file_name("../../evil.png", "image/png"), "evil.png");
        assert_eq!(file_name(r"..\..\Windows\evil.exe", ""), "evil.exe");
        assert_eq!(file_name("a<b>:c?.jpg", ""), "a_b__c_.jpg");
        assert_eq!(file_name("", "image/webp"), "download.webp");
        assert_eq!(file_name("..", ""), "download");
        assert_eq!(file_name("CON.png", ""), "_CON.png");
        assert_eq!(file_name("com1", "image/png"), "_com1.png");
        assert_eq!(file_name("trailing. ", ""), "trailing");
        assert_eq!(file_name("photo", "image/jpeg; charset=x"), "photo.jpg");
        assert_eq!(file_name("photo.php", "image/jpeg"), "photo.php.jpg");
        assert_eq!(file_name("photo.jpg", "image/webp"), "photo.jpg");
        assert_eq!(file_name("圖片.png", ""), "圖片.png");
        assert!(file_name(&"x".repeat(500), "image/png").len() < 130);
    }

    #[test]
    fn executables_are_recognised() {
        for bad in ["a.exe", "A.EXE", "a.jpg.exe", "a.bat", "a.ps1", "a.msi", "a.lnk", "a.js", "a.iso", "a.IMG", "a.vhdx", "a.appref-ms", "a.settingcontent-ms", "a.diagcab", "a.msc"] {
            assert!(is_executable(bad), "{bad}");
        }
        for ok in ["a.jpg", "a.mp4", "a.zip", "a.pdf", "a.exe.jpg", "exe", "a"] {
            assert!(!is_executable(ok), "{ok}");
        }
        // Served as an image: the real extension is what Windows acts on.
        assert!(!is_executable(&file_name("photo.exe", "image/png")));
        assert_eq!(file_name("a.exe. ", ""), "a.exe");    }

    #[test]
    fn a_bare_extension_or_a_direction_override_cannot_sneak_an_executable_through() {
        for name in [".exe", ".BAT", "..exe"] {
            assert!(is_executable(&file_name(name, "")), "{name}");
        }
        let name = file_name("evil\u{202E}gpj.exe", "");
        assert_eq!(name, "evilgpj.exe", "no direction override left to disguise it");
        assert!(is_executable(&name));
    }

    #[test]
    fn query_decoding() {
        assert_eq!(query_param("a=1&name=%E5%9C%96%20x.png", "name").as_deref(), Some("圖 x.png"));
        assert_eq!(query_param("a=1", "b"), None);
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert!(is_web_url("HTTPS://x") && !is_web_url("--exec=calc") && !is_web_url("file:///c:/x"));
    }

    fn call(port: u16, method: &str, path: &str, headers: &str, body: &[u8]) -> (u16, String) {
        use std::io::Write;
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        s.write_all(body).unwrap();
        let mut raw = String::new();
        s.read_to_string(&mut raw).unwrap();
        let status = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, raw.split_once("\r\n\r\n").unwrap().1.to_string())
    }

    /// A body without a declared length, so the limit has to be found while copying.
    fn call_chunked(port: u16, path: &str, headers: &str, body: &[u8]) -> u16 {
        use std::io::Write;
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Connection: close\r\n\r\n").unwrap();
        for chunk in body.chunks(500) {
            // The server may stop reading once it is over the limit.
            if write!(s, "{:x}\r\n", chunk.len()).and_then(|_| s.write_all(chunk)).and_then(|_| s.write_all(b"\r\n")).is_err() {
                break;
            }
        }
        let _ = s.write_all(b"0\r\n\r\n");
        let mut raw = String::new();
        let _ = s.read_to_string(&mut raw);
        raw.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
    }

    #[test]
    fn receiver_checks_token_and_origin_and_saves_files() {
        let srv = WebServer::start(0, "secret".into(), egui::Context::default()).unwrap();
        let port = srv.port;
        let auth = "X-OrangeLink-Token: secret\r\n";
        assert_eq!(call(port, "GET", "/ping", "", b"").0, 401);
        assert_eq!(call(port, "GET", "/ping", "X-OrangeLink-Token: nope\r\n", b"").0, 401);
        let evil = format!("{auth}Origin: https://evil.example\r\n");
        assert_eq!(call(port, "POST", "/file?name=a.png", &evil, b"x").0, 403);
        let (status, body) = call(port, "GET", "/ping", auth, b"");
        assert_eq!(status, 200);
        let other = format!("{auth}Origin: chrome-extension://other\r\n");
        assert_eq!(call(port, "GET", "/ping", &other, b"").0, 200);
        srv.set_only(Some("chrome-extension://abc".into()));
        assert_eq!(call(port, "GET", "/ping", &other, b"").0, 403, "only the allowed extension");
        assert_eq!(call(port, "GET", "/ping", auth, b"").0, 403, "nor a program without an Origin");
        assert_eq!(call(port, "GET", "/ping", "X-OrangeLink-Token: nope\r\nOrigin: chrome-extension://other\r\n", b"").0, 401);
        let clients = srv.clients();
        assert!(clients.iter().any(|c| c.origin == "chrome-extension://other" && c.blocked && c.requests == 2), "{clients:?}");
        assert!(!clients.iter().any(|c| c.requests > 0 && c.origin.contains("nope")));
        srv.set_only(None);
        assert!(body.contains(&format!("\"max_bytes\":{}", DEFAULT_MAX_GB * GB)), "the extension checks sizes against it: {body}");
        let ext = format!("{auth}Origin: chrome-extension://abc\r\n");
        assert_eq!(call(port, "GET", "/ping", &ext, b"").0, 200);
        assert_eq!(call(port, "POST", "/ytdlp?url=--exec%3Dcalc", auth, b"").0, 400);
        assert_eq!(call(port, "POST", "/file?name=empty.png", auth, b"").0, 500);

        for name in ["setup.exe", "SETUP.EXE", "run.bat", "a.exe.%20.", "x%2Fy.ps1", "shortcut.lnk", "disk.iso", "app.appref-ms"] {
            let (status, body) = call(port, "POST", &format!("/file?name={name}"), auth, &[0u8; 300_000]);
            assert_eq!(status, 415, "{name}");
            assert!(body.contains("封鎖"), "{body}");
        }
        assert!(srv.rx.try_recv().is_err(), "blocked files are not imported");

        let q = "/file?name=..%2F..%2Fa.png&url=https%3A%2F%2Fexample.com%2Fp&type=image%2Fpng";
        assert_eq!(call(port, "POST", q, auth, b"hello").0, 200);
        match srv.rx.recv().unwrap() {
            WebMsg::File { path, url, folder } => {
                assert_eq!(folder, None);
                assert_eq!(url, "https://example.com/p");
                assert_eq!(path.file_name().unwrap(), "a.png");
                assert!(path.starts_with(inbox_dir()));
                assert_eq!(fs::read(&path).unwrap(), b"hello");
                // Marked as from the internet, and the import's copy keeps the mark.
                let zone = |p: &Path| {
                    let mut s = p.as_os_str().to_owned();
                    s.push(":Zone.Identifier");
                    fs::read_to_string(s).unwrap_or_default()
                };
                assert!(zone(&path).contains("ZoneId=3"), "{}", zone(&path));
                assert!(zone(&path).contains("ReferrerUrl=https://example.com/p"));
                let copy = std::env::temp_dir().join(format!("orangelink-motw-{}.png", crate::model::new_id()));
                fs::copy(&path, &copy).unwrap();
                assert!(zone(&copy).contains("ZoneId=3"), "fs::copy keeps the stream");
                let _ = fs::remove_file(&copy);
                remove_received(&path);
                assert!(!path.exists());
            }
            WebMsg::Ytdlp { .. } => panic!("expected a file"),
        }
        assert_eq!(call(port, "POST", "/ytdlp?url=https%3A%2F%2Fa.b%2Fc", auth, b"").0, 200);
        assert!(matches!(srv.rx.recv().unwrap(), WebMsg::Ytdlp { url } if url == "https://a.b/c"));

        assert_eq!(call(port, "GET", "/folders", "", b"").0, 401);
        srv.set_folders(vec![("f1".into(), "角色".into(), 0)]);
        let (status, body) = call(port, "GET", "/folders", auth, b"");
        assert_eq!(status, 200);
        assert!(body.contains("\"id\":\"f1\"") && body.contains("角色"), "{body}");
        assert_eq!(call(port, "POST", "/file?name=a.png&folder=gone", auth, &[0u8; 300_000]).0, 409);
        assert!(srv.rx.try_recv().is_err(), "a stale folder imports nothing");
        assert_eq!(call(port, "POST", "/file?name=a.png&folder=f1", auth, b"hi").0, 200);
        match srv.rx.recv().unwrap() {
            WebMsg::File { path, folder, .. } => {
                assert_eq!(folder.as_deref(), Some("f1"));
                remove_received(&path);
            }
            WebMsg::Ytdlp { .. } => panic!("expected a file"),
        }

        // Over the size limit: refused, nothing left in the inbox, at or under it still works.
        srv.set_max_bytes(1000);
        let inbox_entries = || fs::read_dir(inbox_dir()).map(|d| d.count()).unwrap_or(0);
        let before = inbox_entries();
        let (status, body) = call(port, "POST", "/file?name=big.png", auth, &[0u8; 1001]);
        assert_eq!(status, 413);
        assert!(body.contains("上限"), "{body}");
        let chunked = format!("{auth}Transfer-Encoding: chunked\r\n");
        assert_eq!(call_chunked(port, "/file?name=big.png", &chunked, &[0u8; 3000]), 413);
        assert_eq!(inbox_entries(), before, "no temp files left behind");
        assert!(srv.rx.try_recv().is_err());
        assert_eq!(call(port, "POST", "/file?name=ok.png", auth, &[1u8; 1000]).0, 200);
        match srv.rx.recv().unwrap() {
            WebMsg::File { path, .. } => remove_received(&path),
            WebMsg::Ytdlp { .. } => panic!("expected a file"),
        }

        // A new pairing code restarts the server on the same port straight away.
        drop(srv);
        let _srv = WebServer::start(port, "new".into(), egui::Context::default()).unwrap();
        assert_eq!(call(port, "GET", "/ping", auth, b"").0, 401);
        assert_eq!(call(port, "GET", "/ping", "X-OrangeLink-Token: new\r\n", b"").0, 200);
    }

    #[test]
    fn removes_only_inbox_files() {
        let dir = std::env::temp_dir().join(format!("ol-keep-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("keep.txt");
        fs::write(&f, "x").unwrap();
        remove_received(&f);
        assert!(f.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn web_url_rejects_control_chars_and_crlf() {
        assert!(is_web_url("https://example.com/a?b=1"));
        assert!(!is_web_url("https://example.com/a\r\nZoneId=0"));
        assert!(!is_web_url("https://example.com/a\nZoneId=0"));
        assert!(!is_web_url("https://example.com/\0bad"));
    }
}

/// Random requests' names and query strings: never a panic, and never a file
/// name that leaves the inbox or that Windows would treat specially.
#[cfg(test)]
mod fuzz_tests {
    use super::*;

    #[test]
    fn random_names_and_queries() {
        for s in crate::fuzz::strings_with(1, 20_000, &["%2F", "%5C", "%", "%E5%AD%97", "con", "COM1", ".exe", "..", "name="]) {
            let decoded = percent_decode(&s);
            if !s.contains('%') {
                assert_eq!(decoded, s);
            }
            let _ = query_param(&s, "name");
            for mime in ["", "image/png", "text/html; charset=utf-8"] {
                let name = file_name(&decoded, mime);
                assert!(!name.is_empty(), "{s:?}");
                assert!(!name.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)), "{s:?} -> {name:?}");
                assert!(!name.ends_with(['.', ' ']), "{s:?} -> {name:?}");
                let device = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                assert!(!matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "LPT1"), "{s:?} -> {name:?}");
            }
        }
    }
}
