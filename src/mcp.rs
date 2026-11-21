//! A local MCP (Model Context Protocol) server so AI assistants can search and
//! organize the library. Speaks JSON-RPC over "Streamable HTTP" on 127.0.0.1;
//! `orangelink --mcp-stdio` bridges stdio-only clients to it.
//!
//! The HTTP thread checks each request's origin and token, then answers it on a
//! thread of its own, so a slow tool call doesn't hold up `initialize` or `ping`.
//! Tool calls are forwarded to the UI thread (which owns the library) and
//! answered there.

use eframe::egui;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Read, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    thread,
    time::Duration,
};

const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Requests answered at once; more are told to retry (503).
const MAX_IN_FLIGHT: usize = 8;

/// A tool call waiting for the UI thread.
pub struct McpCall {
    pub name: String,
    pub args: Value,
    pub reply: Sender<ToolOutput>,
}

pub struct ToolOutput {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: vec![json!({ "type": "text", "text": s.into() })], is_error: false }
    }

    pub fn json(v: Value) -> Self {
        Self::text(serde_json::to_string_pretty(&v).unwrap_or_default())
    }

    pub fn error(s: impl Into<String>) -> Self {
        Self { is_error: true, ..Self::text(s) }
    }
}

pub struct McpServer {
    server: Arc<tiny_http::Server>,
    pub rx: Receiver<McpCall>,
    pub port: u16,
    pub token: String,
}

/// Listens on `port` on this machine only. A server restarted with new settings
/// frees its port just after being dropped, so binding is tried for half a second.
pub(crate) fn bind_local(port: u16) -> Result<tiny_http::Server, String> {
    let mut bound = tiny_http::Server::http(("127.0.0.1", port));
    for _ in 0..10 {
        if bound.is_ok() {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(50));
        bound = tiny_http::Server::http(("127.0.0.1", port));
    }
    bound.map_err(|e| e.to_string())
}

impl McpServer {
    /// Every request must present `token` (see `authorized`); an empty token would let anything in.
    pub fn start(port: u16, token: String, ctx: egui::Context, tools: Value) -> Result<Self, String> {
        if token.is_empty() {
            return Err("缺少 MCP token".into());
        }
        let server = Arc::new(bind_local(port)?);
        // Port 0 lets the system pick one (the tests); report the one we got.
        let port = server.server_addr().to_ip().map_or(port, |a| a.port());
        let (tx, rx) = channel();
        let (s, t) = (server.clone(), token.clone());
        let tools = Arc::new(tools);
        let in_flight = Arc::new(AtomicUsize::new(0));
        thread::spawn(move || {
            for req in s.incoming_requests() {
                // Checked here, before a thread is started for the request.
                let Some(req) = admit(req, &t) else { continue };
                if in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let _ = req.respond(tiny_http::Response::from_string("busy").with_status_code(503).with_header(header("Retry-After", "1")));
                    continue;
                }
                let (tx, ctx, tools, in_flight) = (tx.clone(), ctx.clone(), tools.clone(), in_flight.clone());
                thread::spawn(move || {
                    struct Guard(Arc<AtomicUsize>);
                    impl Drop for Guard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _guard = Guard(in_flight);
                    serve(req, &tx, &ctx, &tools);
                });
            }
        });
        Ok(Self { server, rx, port, token })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

/// Largest JSON-RPC request body accepted on `/mcp` (16 MiB).
const MAX_BODY: usize = 16 << 20;

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

/// Compares without stopping at the first differing byte.
pub(crate) fn same_token(a: &str, b: &str) -> bool {
    let diff = a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    a.len() == b.len() && std::hint::black_box(diff) == 0
}

/// True when the request carries the token as `Authorization: Bearer <token>`. (Not in
/// the query string: URLs end up in logs and browser history.)
fn authorized(req: &tiny_http::Request, token: &str) -> bool {
    let bearer = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| {
            let (scheme, value) = h.value.as_str().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| value.trim().to_string())
        });
    bearer.is_some_and(|b| same_token(&b, token))
}

/// `localhost`, `127.0.0.1` or `[::1]`, with or without a port.
pub(crate) fn is_local_host(host_port: &str) -> bool {
    let host_port = host_port.trim();
    let host = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && !h.is_empty() => h,
        _ => host_port,
    };
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
}

/// An `Origin` of a page served from this computer.
fn is_local_origin(origin: &str) -> bool {
    let origin = origin.trim();
    let rest = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://"));
    rest.map(|r| r.split('/').next().unwrap_or("")).is_some_and(is_local_host)
}

/// Answers requests from other sites or without the token at once; the rest are
/// handed back to be served.
fn admit(req: tiny_http::Request, token: &str) -> Option<tiny_http::Request> {
    use tiny_http::Response;
    // Reject browser pages on other sites (DNS rebinding): only local origins may call us.
    // The host is compared whole: `http://localhost.evil.example` is not local.
    let header_value = |name: &'static str| req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string());
    let origin_ok = header_value("Origin").is_none_or(|o| is_local_origin(&o));
    let host_ok = header_value("Host").is_none_or(|h| is_local_host(&h));
    if !origin_ok || !host_ok {
        let _ = req.respond(Response::from_string("forbidden origin").with_status_code(403));
        return None;
    }
    // Any other local program can reach 127.0.0.1 without an Origin, so the token is what keeps them out.
    if !authorized(&req, token) {
        let _ = req.respond(
            Response::from_string("unauthorized: 需要 MCP token（設定 → AI 整理）")
                .with_status_code(401)
                .with_header(header("WWW-Authenticate", "Bearer")),
        );
        return None;
    }
    Some(req)
}

fn serve(mut req: tiny_http::Request, tx: &Sender<McpCall>, ctx: &egui::Context, tools: &Value) {
    use tiny_http::{Method, Response};
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path != "/mcp" && path != "/" {
        let _ = req.respond(Response::from_string("not found").with_status_code(404));
        return;
    }
    if *req.method() != Method::Post {
        // No server-initiated stream; clients fall back to plain request/response.
        let _ = req.respond(Response::from_string("").with_status_code(405).with_header(header("Allow", "POST")));
        return;
    }
    if req.body_length().is_some_and(|n| n > MAX_BODY) {
        let _ = req.respond(Response::from_string("body too large").with_status_code(413).with_header(header("Connection", "close")));
        return;
    }
    let mut raw = Vec::new();
    if std::io::Read::take(req.as_reader(), (MAX_BODY + 1) as u64).read_to_end(&mut raw).is_err() {
        let _ = req.respond(Response::from_string("bad body").with_status_code(400));
        return;
    }
    if raw.len() > MAX_BODY {
        let _ = req.respond(Response::from_string("body too large").with_status_code(413).with_header(header("Connection", "close")));
        return;
    }
    let Ok(body) = String::from_utf8(raw) else {
        let _ = req.respond(Response::from_string("bad body").with_status_code(400));
        return;
    };
    let reply = match serde_json::from_str::<Value>(&body) {
        Ok(Value::Array(batch)) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| dispatch(m, tx, ctx, tools)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        Ok(msg) => dispatch(&msg, tx, ctx, tools),
        Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": e.to_string() } })),
    };
    let _ = match reply {
        Some(v) => req.respond(
            Response::from_string(v.to_string()).with_header(header("Content-Type", "application/json")),
        ),
        // Only notifications/responses were sent.
        None => req.respond(Response::from_string("").with_status_code(202)),
    };
}

/// Handles one JSON-RPC message; returns None for notifications.
fn dispatch(msg: &Value, tx: &Sender<McpCall>, ctx: &egui::Context, tools: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "OrangeLink", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "OrangeLink 是使用者的個人素材庫（圖片、影片、音訊等）。先用 library_info 了解資源庫，\
                    用 search_items 找素材，get_thumbnail 可以看圖片內容以便判斷與加標籤。整理時優先用 add_tags、\
                    add_to_folder、set_rating；找到素材後可用 select_items 在 OrangeLink 視窗中為使用者選取。",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools })),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let (reply, rx) = channel();
            let out = if tx.send(McpCall { name, args, reply }).is_ok() {
                ctx.request_repaint();
                rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| {
                    ToolOutput::error("OrangeLink 沒有回應（視窗最小化時可能暫停處理，請把視窗還原後再試）")
                })
            } else {
                ToolOutput::error("OrangeLink 正在關閉")
            };
            Ok(json!({ "content": out.content, "isError": out.is_error }))
        }
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
}

/// Minimal HTTP/1.1 POST to the running app; returns (status, body).
fn post(port: u16, token: &str, body: &str) -> std::io::Result<(u16, String)> {
    request(port, "/mcp", &format!("Authorization: Bearer {token}\r\n"), body)
}

fn request(port: u16, path: &str, extra_headers: &str, body: &str) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(60)))?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = head.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    Ok((status, rest.to_string()))
}

/// `orangelink --mcp-stdio`: newline-delimited JSON-RPC on stdin/stdout,
/// forwarded to the HTTP server of the running OrangeLink window.
pub fn run_stdio_proxy(port: u16, token: &str) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        // Some hosts (e.g. .NET pipes) prefix the stream with a BOM.
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() {
            continue;
        }
        // Busy (too many requests at once): the server asks to retry after a second.
        let mut result = post(port, token, line);
        for _ in 0..2 {
            if !matches!(result, Ok((503, _))) {
                break;
            }
            std::thread::sleep(Duration::from_secs(1));
            result = post(port, token, line);
        }
        if let Some(out) = stdio_answer(line, result.map_err(|_| ())) {
            let _ = writeln!(stdout, "{}", out.trim());
            let _ = stdout.flush();
        }
    }
}

/// What the stdio bridge writes back for one request line, given the HTTP
/// status and body (Err: no connection). Every request (a line with an `id`)
/// gets an answer, so the AI client never waits forever; notifications get none
/// unless the server sent one.
fn stdio_answer(line: &str, result: Result<(u16, String), ()>) -> Option<String> {
    let failure = |message: String| {
        let id = serde_json::from_str::<Value>(line).ok().and_then(|v| v.get("id").cloned());
        id.map(|id| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }).to_string())
    };
    match result {
        Ok((200, body)) if !body.trim().is_empty() => Some(body),
        // Fine for a notification (no id: nothing is written); a request needs an answer.
        Ok((200 | 202 | 204, _)) => failure("OrangeLink 沒有回應內容".into()),
        Ok((401, _)) => failure("OrangeLink 拒絕了連線：token 不符。請開啟 OrangeLink 的設定 → AI 整理，確認已啟用 MCP 伺服器".into()),
        Ok((403, _)) => failure("OrangeLink 拒絕了這個請求（來源不符）".into()),
        Ok((503, _)) => failure("OrangeLink 正在處理太多請求，請稍後再試".into()),
        Ok((code, _)) => failure(format!("OrangeLink 回應了錯誤（HTTP {code}）")),
        Err(()) => failure("連不到 OrangeLink：請先開啟 OrangeLink，並在設定中啟用 MCP 伺服器".into()),
    }
}

/// The Claude Code CLI when it isn't on PATH: its installer puts it in `~/.local/bin`
/// without always adding that to PATH. None means plain `claude` will do.
pub fn find_claude() -> Option<std::path::PathBuf> {
    let on_path = std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| ["claude.exe", "claude.cmd", "claude"].iter().any(|n| d.join(n).is_file())));
    if on_path {
        return None;
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(home).join(".local").join("bin").join("claude.exe")).filter(|p| p.is_file())
}

/// The command that registers OrangeLink with Claude Code, for every project (user scope).
pub fn claude_add_command(claude: Option<&std::path::Path>, url: &str, token: &str) -> String {
    let exe = claude.map_or("claude".to_string(), |p| format!("& \"{}\"", p.display()));
    format!("{exe} mcp add --scope user --transport http orangelink {url} --header \"Authorization: Bearer {token}\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restarted_server_gets_its_port_back_when_the_old_one_lets_go_late() {
        let old = McpServer::start(0, "secret".into(), egui::Context::default(), json!([])).unwrap();
        let port = old.port;
        // The old server goes a moment after the new one starts, as on a settings change.
        let going = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(150));
            drop(old);
        });
        let new = McpServer::start(port, "secret2".into(), egui::Context::default(), json!([]));
        going.join().unwrap();
        assert_eq!(new.map(|s| s.port).ok(), Some(port));
    }

    /// Every request gets an answer, busy or refused; notifications don't.
    #[test]
    fn the_stdio_bridge_answers_every_request() {
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
        let note = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(stdio_answer(req, Ok((200, "{\"ok\":1}".into()))).as_deref(), Some("{\"ok\":1}"));
        for result in [Ok((503, "busy".to_string())), Ok((403, String::new())), Ok((401, String::new())), Ok((500, String::new())), Ok((200, String::new())), Err(())] {
            let out = stdio_answer(req, result.clone()).unwrap_or_else(|| panic!("{result:?}"));
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["id"], 7);
            assert!(v["error"]["message"].as_str().is_some_and(|m| !m.is_empty()), "{out}");
        }
        assert!(stdio_answer(req, Ok((503, "busy".into()))).unwrap().contains("太多請求"));
        assert_eq!(stdio_answer(note, Ok((202, String::new()))), None);
        assert_eq!(stdio_answer(note, Ok((503, "busy".into()))), None);
    }

    #[test]
    fn claude_command_uses_full_path_when_needed() {
        let url = "http://127.0.0.1:47821/mcp";
        assert_eq!(
            claude_add_command(None, url, "t1"),
            "claude mcp add --scope user --transport http orangelink http://127.0.0.1:47821/mcp --header \"Authorization: Bearer t1\""
        );
        let cmd = claude_add_command(Some(std::path::Path::new(r"C:\Users\A B\.local\bin\claude.exe")), url, "t1");
        assert!(cmd.starts_with(r#"& "C:\Users\A B\.local\bin\claude.exe" mcp add --scope user"#), "{cmd}");
    }

    #[test]
    fn requests_need_the_token() {
        assert!(McpServer::start(0, String::new(), egui::Context::default(), json!([])).is_err(), "an empty token is refused");
        let srv = McpServer::start(0, "secret".into(), egui::Context::default(), json!([])).unwrap();
        let port = srv.port;
        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;

        assert_eq!(request(port, "/mcp", "", list).unwrap().0, 401, "no token");
        assert_eq!(request(port, "/mcp", "Authorization: Bearer nope\r\n", list).unwrap().0, 401, "wrong token");
        assert_eq!(request(port, "/mcp", "Authorization: Bearer secre\r\n", list).unwrap().0, 401, "prefix of the token");
        assert_eq!(request(port, "/mcp", "Authorization: secret\r\n", list).unwrap().0, 401, "missing scheme");
        assert_eq!(request(port, "/mcp?token=nope", "", list).unwrap().0, 401, "wrong query token");
        let evil = "Authorization: Bearer secret\r\nOrigin: https://evil.example\r\n";
        assert_eq!(request(port, "/mcp", evil, list).unwrap().0, 403, "a foreign origin is still refused");
        let lookalike = "Authorization: Bearer secret\r\nOrigin: http://localhost.evil.example\r\n";
        assert_eq!(request(port, "/mcp", lookalike, list).unwrap().0, 403, "a host that only starts with localhost");
        let local_page = "Authorization: Bearer secret\r\nOrigin: http://localhost:3000\r\n";
        assert_eq!(request(port, "/mcp", local_page, list).unwrap().0, 200, "a local page");

        let (status, body) = post(port, "secret", list).unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("\"tools\""), "{body}");
        assert_eq!(request(port, "/mcp", "authorization: bearer secret\r\n", list).unwrap().0, 200, "header and scheme are case-insensitive");
        assert_eq!(request(port, "/mcp?token=secret", "", list).unwrap().0, 401, "a token in the query is not accepted");
    }

    #[test]
    fn a_slow_tool_call_does_not_hold_up_other_requests() {
        // Nothing answers tool calls here, so this one waits.
        let srv = McpServer::start(0, "secret".into(), egui::Context::default(), json!([])).unwrap();
        let port = srv.port;
        std::thread::spawn(move || {
            let _ = post(port, "secret", r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"library_info"}}"#);
        });
        let call = srv.rx.recv_timeout(Duration::from_secs(5)).expect("the call reached the UI side");
        assert_eq!(call.name, "library_info");
        let start = std::time::Instant::now();
        let (status, body) = post(port, "secret", r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#).unwrap();
        assert_eq!(status, 200, "{body}");
        assert!(start.elapsed() < Duration::from_secs(5), "ping waited for the tool call");
        let _ = call.reply.send(ToolOutput::text("done"));
    }

    #[test]
    fn local_hosts_and_origins() {
        for h in ["localhost", "LOCALHOST:8080", "127.0.0.1", "127.0.0.1:47821", "[::1]", "[::1]:80"] {
            assert!(is_local_host(h), "{h}");
        }
        for h in ["localhost.evil.example", "127.0.0.1.evil.example", "evil.example", "", ":80", "127.0.0.2"] {
            assert!(!is_local_host(h), "{h}");
        }
        assert!(is_local_origin("http://localhost:3000"));
        assert!(is_local_origin("https://127.0.0.1"));
        assert!(!is_local_origin("http://localhost.evil.example"));
        assert!(!is_local_origin("http://127.0.0.1.evil.example:80/x"));
        assert!(!is_local_origin("chrome-extension://abc"));
        assert!(!is_local_origin("null"));
    }

    #[test]
    fn token_comparison() {
        assert!(same_token("abc", "abc"));
        assert!(!same_token("abc", "abd"));
        assert!(!same_token("abc", "abcd"));
        assert!(!same_token("", "a"));
    }
}
