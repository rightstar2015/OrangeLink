//! Downloading from video sites with yt-dlp: finding/updating the tool, building
//! the command line, reading formats, running downloads and converting live chat.

use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        mpsc::{channel, Receiver},
        Arc, Mutex,
    },
    thread,
};

const EXE: &str = if cfg!(windows) { "yt-dlp.exe" } else { "yt-dlp" };
const RELEASE_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const LATEST_API: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
/// The hashes yt-dlp publishes with each release (`<sha256>  yt-dlp.exe` lines).
const SUMS_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";

fn command(program: &Path) -> Command {
    let mut cmd = crate::process::hidden(program);
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd
}

/// `%APPDATA%\OrangeLink\tools`, where OrangeLink keeps its own yt-dlp.
pub fn tools_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("OrangeLink").join("tools"))
}

/// Other places a yt-dlp/ffmpeg may already be: OrangeLink's tools folder, the
/// yt-dlp GUI's folder and winget's links folder.
pub fn extra_tool_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = tools_dir().into_iter().collect();
    if let Some(d) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(d).join("yt-dlp-gui"));
    }
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(d).join("Microsoft").join("WinGet").join("Links"));
    }
    dirs
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

pub fn find_ytdlp() -> Option<PathBuf> {
    extra_tool_dirs().into_iter().chain(path_dirs()).map(|d| d.join(EXE)).find(|p| p.is_file())
}

/// Deno, the JavaScript runtime yt-dlp needs for YouTube. Looked up in the same
/// places plus deno's own installer folder; passed to yt-dlp explicitly because
/// a runtime installed after OrangeLink started is not on our (inherited) PATH.
pub fn find_deno() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "deno.exe" } else { "deno" };
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(|h| PathBuf::from(h).join(".deno").join("bin"));
    path_dirs().into_iter().chain(extra_tool_dirs()).chain(home).map(|d| d.join(exe)).find(|p| p.is_file())
}

pub fn version(exe: &Path) -> Option<String> {
    let out = command(exe).arg("--version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

fn curl() -> Result<Command, String> {
    // yt-dlp.exe is about 20 MB.
    let mut cmd = crate::self_update::curl(200 * 1024 * 1024)?;
    cmd.env("PYTHONIOENCODING", "utf-8");
    Ok(cmd)
}

/// `name`'s hash in a `sha256sum`-style list.
fn hash_in_sums<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.trim().split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name && hash.len() == 64).then_some(hash)
    })
}

/// The newest release tag on GitHub (e.g. `2026.10.26`), if reachable.
pub fn latest_version() -> Option<String> {
    let out = curl().ok()?.args(["-sL", "--max-time", "8", "-H", "User-Agent: OrangeLink", LATEST_API]).output().ok()?;
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("tag_name")?.as_str().map(str::to_string)
}

/// Downloads the latest yt-dlp.exe into the tools folder.
pub fn install_latest() -> Result<PathBuf, String> {
    let dir = tools_dir().ok_or("找不到 APPDATA 資料夾")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (exe, part) = (dir.join(EXE), dir.join("yt-dlp.exe.part"));
    let out = curl()?
        .args(["-L", "--fail", "-sS", "--max-time", "300", "-o"])
        .arg(&part)
        .arg(RELEASE_URL)
        .output()
        .map_err(|e| format!("無法執行 curl：{e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // An exe that will be run: it must match the hash yt-dlp published for it.
    let sums = curl()?.args(["-L", "--fail", "-sS", "--max-time", "30", SUMS_URL]).output().ok().filter(|o| o.status.success());
    let want = sums.as_ref().and_then(|o| hash_in_sums(&String::from_utf8_lossy(&o.stdout), EXE).map(str::to_lowercase));
    let got = crate::self_update::sha256_file(&part).ok();
    match (want, got) {
        (Some(w), Some(g)) if w == g => {}
        (None, _) => {
            let _ = std::fs::remove_file(&part);
            return Err("讀不到 yt-dlp 公布的 SHA-256，無法確認下載的檔案，請稍後再試".into());
        }
        _ => {
            let _ = std::fs::remove_file(&part);
            return Err("下載的 yt-dlp.exe 和官方公布的 SHA-256 不符，已刪除".into());
        }
    }
    std::fs::rename(&part, &exe).map_err(|e| format!("無法取代 yt-dlp.exe（是否正在下載中？）：{e}"))?;
    version(&exe).ok_or("下載的 yt-dlp 無法執行")?;
    Ok(exe)
}

/// Result of the environment check run when the download window opens.
#[derive(Clone, Default)]
pub struct Check {
    pub ytdlp: Option<PathBuf>,
    pub version: Option<String>,
    pub latest: Option<String>,
    pub ffmpeg_ok: bool,
    pub deno: Option<PathBuf>,
}

impl Check {
    pub fn run(ffmpeg_dir: Option<&Path>) -> Self {
        let ytdlp = find_ytdlp();
        let version = ytdlp.as_deref().and_then(version);
        let ffmpeg_ok = ffmpeg_dir.is_some_and(|d| {
            ["ffmpeg", "ffprobe"].iter().all(|t| crate::media::tool(d, t).arg("-version").output().is_ok_and(|o| o.status.success()))
        });
        Check { ytdlp, version, latest: latest_version(), ffmpeg_ok, deno: find_deno() }
    }

    pub fn update_available(&self) -> bool {
        matches!((&self.version, &self.latest), (Some(v), Some(l)) if l.as_str() > v.as_str())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, Default)]
pub enum OutFormat {
    #[default]
    Mp4,
    Mp3,
    M4a,
}

impl OutFormat {
    pub const ALL: [OutFormat; 3] = [OutFormat::Mp4, OutFormat::Mp3, OutFormat::M4a];
    pub fn label(self) -> &'static str {
        match self {
            OutFormat::Mp4 => "mp4",
            OutFormat::Mp3 => "mp3",
            OutFormat::M4a => "m4a",
        }
    }
    pub fn note(self) -> &'static str {
        match self {
            OutFormat::Mp4 => "影片優先 avc1（H.264）＋ AAC，沒有時才用 VP9；合併需 FFmpeg",
            OutFormat::Mp3 => "只下載音訊並轉成 MP3（最高音質）；需 FFmpeg",
            OutFormat::M4a => "只下載音訊（AAC / M4A），通常不需轉檔",
        }
    }
}

/// Selector for MP4: avc1 video with m4a audio first, then other H.264/AAC
/// combinations, with VP9 (vp09/vp9) only as a fallback.
pub const MP4_SELECTOR: &str = "bv*[vcodec^=avc1]+ba[ext=m4a]/bv*[vcodec^=avc1]+ba/b[ext=mp4][vcodec^=avc1]/\
                                bv*[vcodec^=vp09]+ba[ext=m4a]/bv*[vcodec^=vp9]+ba/bv*+ba/b";

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub url: String,
    pub playlist: bool,
    pub out_dir: PathBuf,
    /// Optional file name (without extension).
    pub name: String,
    /// (start, end) such as ("00:01:00", "00:02:30"); an empty end means "to the end".
    pub section: Option<(String, String)>,
    pub format: OutFormat,
    /// Advanced mode: exact format ids (video, audio); either may be None.
    pub custom: Option<(Option<String>, Option<String>)>,
    pub chat: bool,
    pub chat_only: bool,
    pub ffmpeg_dir: Option<PathBuf>,
    pub deno: Option<PathBuf>,
}

impl Request {
    pub fn label(&self) -> String {
        let what = if self.chat_only {
            "聊天室".to_string()
        } else if self.custom.is_some() {
            "自訂格式".to_string()
        } else {
            self.format.label().to_string()
        };
        format!("{}（{what}）", self.url)
    }
}

pub const PROGRESS_TAG: &str = "OLPROG";
pub const FILE_TAG: &str = "OLFILE";

/// Command-line arguments for a download (everything after the program name).
pub fn build_args(r: &Request) -> Vec<String> {
    let mut a: Vec<String> = [
        "--newline",
        "--progress",
        "--color",
        "never",
        "--encoding",
        "utf-8",
        "--progress-template",
        "download:OLPROG %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s %(progress.speed)s %(progress.eta)s",
        "--print",
        "after_move:OLFILE %(filepath)s",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&[if r.playlist { "--yes-playlist" } else { "--no-playlist" }]);
    push(&["-P", &r.out_dir.to_string_lossy()]);
    let name = r.name.trim().replace('%', "%%");
    let template = match (name.is_empty(), r.playlist) {
        (true, _) => "%(title)s [%(id)s].%(ext)s".to_string(),
        (false, true) => format!("{name} %(playlist_index)s.%(ext)s"),
        (false, false) => format!("{name}.%(ext)s"),
    };
    push(&["-o", &template]);
    if let Some(dir) = &r.ffmpeg_dir {
        push(&["--ffmpeg-location", &dir.to_string_lossy()]);
    }
    if let Some(deno) = &r.deno {
        push(&["--js-runtimes", &format!("deno:{}", deno.display())]);
    }
    if let Some((start, end)) = &r.section {
        let start = if start.trim().is_empty() { "0" } else { start.trim() };
        let end = if end.trim().is_empty() { "inf" } else { end.trim() };
        push(&["--download-sections", &format!("*{start}-{end}"), "--force-keyframes-at-cuts"]);
    }
    if r.chat || r.chat_only {
        push(&["--write-subs", "--sub-langs", "live_chat"]);
    }
    if r.chat_only {
        push(&["--skip-download"]);
    } else {
        match &r.custom {
            Some((v, au)) => {
                let sel = match (v, au) {
                    (Some(v), Some(au)) => format!("{v}+{au}"),
                    (Some(v), None) => v.clone(),
                    (None, Some(au)) => au.clone(),
                    (None, None) => "b".to_string(),
                };
                push(&["-f", &sel]);
                if v.is_some() && au.is_some() {
                    push(&["--merge-output-format", "mp4/mkv"]);
                }
            }
            None => match r.format {
                OutFormat::Mp4 => push(&["-f", MP4_SELECTOR, "--merge-output-format", "mp4"]),
                OutFormat::Mp3 => push(&["-f", "ba/b", "-x", "--audio-format", "mp3", "--audio-quality", "0"]),
                OutFormat::M4a => push(&["-f", "ba[ext=m4a]/ba/b", "-x", "--audio-format", "m4a"]),
            },
        }
    }
    a.push("--".to_string());
    a.push(r.url.trim().to_string());
    a
}

#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub fraction: Option<f32>,
    pub speed: Option<f64>,
    pub eta: Option<f64>,
}

/// Parses a line written by our `--progress-template`.
pub fn parse_progress(line: &str) -> Option<Progress> {
    let rest = line.trim().strip_prefix(PROGRESS_TAG)?;
    let n: Vec<Option<f64>> = rest.split_whitespace().map(|s| s.parse().ok()).collect();
    let get = |i: usize| n.get(i).copied().flatten();
    let total = get(1).or(get(2)).filter(|t| *t > 0.0);
    Some(Progress {
        fraction: get(0).zip(total).map(|(d, t)| (d / t).clamp(0.0, 1.0) as f32),
        speed: get(3),
        eta: get(4),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormatInfo {
    pub id: String,
    pub ext: String,
    pub height: Option<u32>,
    pub vcodec: String,
    pub acodec: String,
    pub abr: Option<f64>,
    /// Bytes, exact or estimated from the bitrate.
    pub size: Option<u64>,
    pub protocol: String,
}

impl FormatInfo {
    pub fn has_video(&self) -> bool {
        !self.vcodec.is_empty() && self.vcodec != "none"
    }
    pub fn has_audio(&self) -> bool {
        !self.acodec.is_empty() && self.acodec != "none"
    }
    pub fn label(&self) -> String {
        let size = self.size.map(human_mb).unwrap_or_else(|| "大小未知".to_string());
        if self.has_video() {
            let res = self.height.map(|h| format!("{h}p")).unwrap_or_else(|| "?".into());
            format!("{res} | {} | {} | {size} | ID {}", self.ext, self.vcodec, self.id)
        } else {
            let abr = self.abr.map(|a| format!("{a:.0}k")).unwrap_or_else(|| "?".into());
            format!("{abr} | {} | {} | {size} | ID {}", self.ext, self.acodec, self.id)
        }
    }
}

pub fn human_mb(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProbeInfo {
    pub title: String,
    pub duration: Option<f64>,
    pub is_live: bool,
    /// Number of videos when the URL is a playlist.
    pub entries: Option<usize>,
    pub formats: Vec<FormatInfo>,
}

impl ProbeInfo {
    pub fn video_formats(&self) -> Vec<&FormatInfo> {
        let mut v: Vec<&FormatInfo> = self.formats.iter().filter(|f| f.has_video()).collect();
        // yt-dlp prefers direct https over HLS (m3u8) at the same quality.
        let hls = |f: &FormatInfo| f.protocol.contains("m3u8");
        v.sort_by(|a, b| {
            b.height.cmp(&a.height).then(codec_rank(&a.vcodec).cmp(&codec_rank(&b.vcodec))).then(hls(a).cmp(&hls(b)))
        });
        v
    }
    pub fn audio_formats(&self) -> Vec<&FormatInfo> {
        let mut v: Vec<&FormatInfo> = self.formats.iter().filter(|f| f.has_audio() && !f.has_video()).collect();
        v.sort_by(|a, b| (b.ext == "m4a").cmp(&(a.ext == "m4a")).then(b.abr.unwrap_or(0.0).total_cmp(&a.abr.unwrap_or(0.0))));
        v
    }

    /// Best guess of what the simple-mode selector picks, and its size.
    pub fn estimate(&self, format: OutFormat) -> Option<u64> {
        let audio = self.audio_formats().into_iter().next();
        match format {
            OutFormat::Mp4 => {
                let video = self.video_formats().into_iter().next()?;
                Some(video.size? + audio.and_then(|a| a.size).unwrap_or(0))
            }
            OutFormat::Mp3 | OutFormat::M4a => audio?.size,
        }
    }
}

/// avc1 first, then vp09/vp9, then everything else (av01…), matching the MP4 preference.
fn codec_rank(vcodec: &str) -> u8 {
    if vcodec.starts_with("avc1") {
        0
    } else if vcodec.starts_with("vp09") || vcodec.starts_with("vp9") {
        1
    } else {
        2
    }
}

/// Parses `yt-dlp -J` output.
pub fn parse_probe(json: &str) -> Result<ProbeInfo, String> {
    let v: Value = serde_json::from_str(json.trim_start_matches('\u{feff}')).map_err(|e| format!("無法解讀影片資訊：{e}"))?;
    let duration = v.get("duration").and_then(Value::as_f64);
    let s = |f: &Value, k: &str| f.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let formats = v
        .get("formats")
        .and_then(Value::as_array)
        .map(|fs| {
            fs.iter()
                .filter(|f| s(f, "ext") != "mhtml")
                .map(|f| {
                    let tbr = f.get("tbr").and_then(Value::as_f64);
                    let size = f
                        .get("filesize")
                        .and_then(Value::as_u64)
                        .or_else(|| f.get("filesize_approx").and_then(Value::as_u64))
                        .or_else(|| tbr.zip(duration).map(|(t, d)| (t * 125.0 * d) as u64));
                    FormatInfo {
                        id: s(f, "format_id"),
                        ext: s(f, "ext"),
                        height: f.get("height").and_then(Value::as_u64).map(|h| h as u32),
                        vcodec: s(f, "vcodec"),
                        acodec: s(f, "acodec"),
                        abr: f.get("abr").and_then(Value::as_f64).filter(|a| *a > 0.0),
                        size,
                        protocol: s(f, "protocol"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ProbeInfo {
        title: s(&v, "title"),
        duration,
        is_live: v.get("is_live").and_then(Value::as_bool).unwrap_or(false),
        entries: v.get("entries").and_then(Value::as_array).map(Vec::len),
        formats,
    })
}

/// Runs `yt-dlp -J` (flat for playlists, so it stays quick).
pub fn probe(exe: &Path, url: &str, playlist: bool, deno: Option<&Path>) -> Result<ProbeInfo, String> {
    let mut cmd = command(exe);
    cmd.args(["-J", "--encoding", "utf-8"]);
    if let Some(deno) = deno {
        cmd.arg("--js-runtimes").arg(format!("deno:{}", deno.display()));
    }
    cmd.arg(if playlist { "--flat-playlist" } else { "--no-playlist" });
    let out = cmd.arg("--").arg(url.trim()).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(last_error(&String::from_utf8_lossy(&out.stderr)));
    }
    parse_probe(&String::from_utf8_lossy(&out.stdout))
}

fn last_error(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .or_else(|| stderr.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("yt-dlp 執行失敗")
        .trim()
        .to_string()
}

pub enum Event {
    Log(String),
    Progress(Progress),
    File(PathBuf),
    /// Finished; Err holds the last error line.
    Done(Result<(), String>),
}

pub struct Job {
    pub rx: Receiver<Event>,
    pub pid: u32,
    /// Set, under its lock, once the program has ended and its handle is closed;
    /// until then Windows cannot give `pid` to another process.
    exited: Arc<Mutex<bool>>,
}

impl Job {
    pub fn spawn(exe: &Path, args: &[String], ctx: eframe::egui::Context) -> std::io::Result<Job> {
        let mut child = command(exe).args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let pid = child.id();
        let exited = Arc::new(Mutex::new(false));
        let (tx, rx) = channel();
        let out = child.stdout.take().expect("piped");
        let err = child.stderr.take().expect("piped");
        let (tx_out, ctx_out) = (tx.clone(), ctx.clone());
        let reader = thread::spawn(move || {
            let mut r = BufReader::new(out);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                let event = if let Some(p) = parse_progress(&line) {
                    Event::Progress(p)
                } else if let Some(path) = line.trim().strip_prefix(FILE_TAG) {
                    Event::File(PathBuf::from(path.trim()))
                } else {
                    Event::Log(line)
                };
                let _ = tx_out.send(event);
                ctx_out.request_repaint();
            }
        });
        let (tx_err, ctx_err) = (tx.clone(), ctx.clone());
        let err_reader = thread::spawn(move || {
            let mut last = None;
            let mut r = BufReader::new(err);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                if line.contains("ERROR") {
                    last = Some(line.trim().to_string());
                }
                let _ = tx_err.send(Event::Log(line));
                ctx_err.request_repaint();
            }
            last
        });
        let ended = exited.clone();
        thread::spawn(move || {
            let status = child.wait();
            // The handle closes (freeing the pid for reuse) only with the flag set,
            // so a cancel either sees it or kills while the pid is still this one.
            {
                let mut ended = crate::lock(&ended);
                *ended = true;
                drop(child);
            }
            let _ = reader.join();
            let last_error = err_reader.join().ok().flatten();
            let result = match status {
                Ok(s) if s.success() => Ok(()),
                Ok(_) => Err(last_error.unwrap_or_else(|| "yt-dlp 執行失敗".to_string())),
                Err(e) => Err(e.to_string()),
            };
            let _ = tx.send(Event::Done(result));
            ctx.request_repaint();
        });
        Ok(Job { rx, pid, exited })
    }

    /// Stops yt-dlp and the processes it started (the exe is a launcher, ffmpeg runs below it).
    /// Runs in the background: taskkill can take a moment, and the UI must not wait.
    pub fn cancel(&self) {
        let (pid, exited) = (self.pid, self.exited.clone());
        thread::spawn(move || kill_if_running(pid, &exited));
    }
}

/// Kills the process tree of `pid` unless it has ended; whether it tried.
fn kill_if_running(pid: u32, exited: &Mutex<bool>) -> bool {
    let ended = crate::lock(exited);
    if *ended {
        return false;
    }
    #[cfg(windows)]
    {
        // From System32, not whatever taskkill.exe comes first on PATH.
        let Ok(taskkill) = crate::self_update::system_tool("taskkill") else { return false };
        let _ = command(&taskkill).args(["/T", "/F", "/PID", &pid.to_string()]).output();
    }
    #[cfg(not(windows))]
    {
        let _ = command(Path::new("kill")).arg(pid.to_string()).output();
    }
    true
}

/// Live-chat JSON files written into `dir` at or after `since`.
pub fn new_chat_files(dir: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            // A cancelled recording stays as .part, with everything recorded so far.
            let name = p.to_string_lossy();
            name.ends_with(".live_chat.json") || name.ends_with(".live_chat.json.part")
        })
        .filter(|p| p.metadata().and_then(|m| m.modified()).is_ok_and(|t| t >= since))
        .collect()
}

fn text_of(v: &Value) -> String {
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        return s.to_string();
    }
    v.get("runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .map(|r| match r.get("text").and_then(Value::as_str) {
                    Some(t) => t.to_string(),
                    None => r
                        .pointer("/emoji/shortcuts/0")
                        .or_else(|| r.pointer("/emoji/emojiId"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn fmt_offset(ms: i64) -> String {
    let s = (ms.max(0) / 1000) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// One chat message from yt-dlp's `.live_chat.json`.
pub struct ChatMessage {
    /// Position in the video, when the record has one.
    pub offset_ms: Option<i64>,
    /// As shown in the chat (`1:23`), else from `offset_ms`.
    pub time: String,
    pub author: String,
    pub text: String,
    /// Super Chat amount.
    pub paid: Option<String>,
    /// Super Chat, sticker, membership or gifted memberships.
    pub special: bool,
}

/// One readable line per chat message: `[1:23] 名稱：訊息`, with Super Chat amounts.
pub fn chat_lines(json_lines: &str) -> Vec<String> {
    chat_messages(json_lines)
        .into_iter()
        .map(|m| {
            let mut line = format!("[{}] {}：{}", m.time, m.author, m.text);
            if let Some(amount) = m.paid {
                line += &format!("〔Super Chat {amount}〕");
            }
            line
        })
        .collect()
}

/// Every chat message once, in recording order.
pub fn chat_messages(json_lines: &str) -> Vec<ChatMessage> {
    const KINDS: [&str; 5] = [
        "liveChatTextMessageRenderer",
        "liveChatPaidMessageRenderer",
        "liveChatPaidStickerRenderer",
        "liveChatMembershipItemRenderer",
        "liveChatSponsorshipsGiftPurchaseAnnouncementRenderer",
    ];
    fn walk<'a>(v: &'a Value, out: &mut Vec<(&'a str, &'a Value)>) {
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if KINDS.contains(&k.as_str()) {
                        out.push((k.as_str(), child));
                    } else if k != "addLiveChatTickerItemAction" {
                        // The ticker repeats a paid message already in the chat itself.
                        walk(child, out);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut messages = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in json_lines.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(raw) else { continue };
        let offset = v
            .pointer("/replayChatItemAction/videoOffsetTimeMsec")
            .and_then(|o| o.as_str().and_then(|s| s.parse().ok()).or_else(|| o.as_i64()));
        let mut found = Vec::new();
        walk(&v, &mut found);
        for (kind, r) in found {
            if let Some(id) = r.get("id").and_then(Value::as_str) {
                if !seen.insert(id.to_string()) {
                    continue;
                }
            }
            let time = r
                .pointer("/timestampText/simpleText")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| offset.map(fmt_offset))
                .unwrap_or_default();
            let gift = r.pointer("/header/liveChatSponsorshipsHeaderRenderer");
            let author = gift.unwrap_or(r).get("authorName").map(text_of).unwrap_or_default();
            let mut msg = r.get("message").map(text_of).unwrap_or_default();
            match kind {
                "liveChatMembershipItemRenderer" => {
                    let sub = r.get("headerSubtext").map(text_of).unwrap_or_default();
                    msg = [sub, msg].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                }
                "liveChatPaidStickerRenderer" => msg = "（貼圖）".to_string(),
                _ => {}
            }
            if let Some(text) = gift.and_then(|g| g.get("primaryText")) {
                msg = text_of(text);
            }
            let paid = r.get("purchaseAmountText").map(text_of).filter(|s| !s.is_empty());
            let special = kind != "liveChatTextMessageRenderer";
            messages.push(ChatMessage { offset_ms: offset, time, author, text: msg, paid, special });
        }
    }
    messages
}

/// Writes `<name>.live_chat.txt` next to the JSON; returns its path and message count.
pub fn chat_to_txt(json: &Path) -> Result<(PathBuf, usize), String> {
    let text = std::fs::read_to_string(json).map_err(|e| e.to_string())?;
    let lines = chat_lines(&text);
    let name = json.to_string_lossy();
    let out = PathBuf::from(name.trim_end_matches(".part").trim_end_matches(".json").to_string() + ".txt");
    std::fs::write(&out, lines.join("\r\n")).map_err(|e| e.to_string())?;
    Ok((out, lines.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn cancelling_a_download_that_already_ended_kills_nothing() {
        let args = vec!["/c".to_string(), "exit".to_string(), "0".to_string()];
        let job = Job::spawn(Path::new("cmd"), &args, eframe::egui::Context::default()).unwrap();
        while !matches!(job.rx.recv().unwrap(), Event::Done(_)) {}
        // Its pid may belong to another program by now.
        assert!(!kill_if_running(job.pid, &job.exited), "taskkill ran on an ended program's pid");
    }

    #[test]
    fn a_running_download_is_killed_on_cancel() {
        // u32::MAX is no process; the point is that a kill is tried while it runs.
        assert!(kill_if_running(u32::MAX, &Mutex::new(false)));
    }

    #[test]
    fn finds_the_exe_in_the_published_hashes() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  yt-dlp
{b}  yt-dlp.exe
short  yt-dlp_x86.exe
");
        assert_eq!(hash_in_sums(&sums, "yt-dlp.exe"), Some(b.as_str()));
        assert_eq!(hash_in_sums(&sums, "yt-dlp"), Some(a.as_str()));
        assert_eq!(hash_in_sums(&sums, "yt-dlp_x86.exe"), None, "not a hash");
        assert_eq!(hash_in_sums(&format!("{a} *yt-dlp.exe"), "yt-dlp.exe"), Some(a.as_str()), "binary-mode marker");
    }

    fn req() -> Request {
        Request {
            url: "https://www.youtube.com/watch?v=abc".into(),
            playlist: false,
            out_dir: PathBuf::from(r"E:\dl"),
            name: String::new(),
            section: None,
            format: OutFormat::Mp4,
            custom: None,
            chat: false,
            chat_only: false,
            ffmpeg_dir: Some(PathBuf::from(r"C:\ffmpeg\bin")),
            deno: None,
        }
    }

    fn has(args: &[String], pair: &[&str]) -> bool {
        args.windows(pair.len()).any(|w| w.iter().zip(pair).all(|(a, b)| a == b))
    }

    #[test]
    fn args_for_each_format() {
        let a = build_args(&req());
        assert!(has(&a, &["-f", MP4_SELECTOR]) && has(&a, &["--merge-output-format", "mp4"]));
        assert!(has(&a, &["--no-playlist"]) && has(&a, &["--ffmpeg-location", r"C:\ffmpeg\bin"]));
        assert!(has(&a, &["-o", "%(title)s [%(id)s].%(ext)s"]));
        assert_eq!(a[a.len() - 2..], ["--".to_string(), "https://www.youtube.com/watch?v=abc".to_string()]);
        assert!(MP4_SELECTOR.find("avc1").unwrap() < MP4_SELECTOR.find("vp09").unwrap());

        let mp3 = build_args(&Request { format: OutFormat::Mp3, ..req() });
        assert!(has(&mp3, &["-x", "--audio-format", "mp3"]));
        let m4a = build_args(&Request { format: OutFormat::M4a, name: "訪談".into(), ..req() });
        assert!(has(&m4a, &["--audio-format", "m4a"]) && has(&m4a, &["-o", "訪談.%(ext)s"]));

        let cut = build_args(&Request { section: Some(("00:01:00".into(), String::new())), playlist: true, ..req() });
        assert!(has(&cut, &["--download-sections", "*00:01:00-inf", "--force-keyframes-at-cuts"]));
        assert!(has(&cut, &["--yes-playlist"]));

        let custom = build_args(&Request { custom: Some((Some("137".into()), Some("140".into()))), ..req() });
        assert!(has(&custom, &["-f", "137+140"]) && has(&custom, &["--merge-output-format", "mp4/mkv"]));

        let js = build_args(&Request { deno: Some(PathBuf::from(r"C:\deno\deno.exe")), ..req() });
        assert!(has(&js, &["--js-runtimes", r"deno:C:\deno\deno.exe"]));

        let chat = build_args(&Request { chat_only: true, ..req() });
        assert!(has(&chat, &["--write-subs", "--sub-langs", "live_chat", "--skip-download"]));
        assert!(!chat.iter().any(|x| x == "-f"));
        assert_eq!(chat[chat.len() - 2..], ["--".to_string(), "https://www.youtube.com/watch?v=abc".to_string()]);

        let pct = build_args(&Request { name: "100% 精華".into(), ..req() });
        assert!(has(&pct, &["-o", "100%% 精華.%(ext)s"]));
    }

    /// Real network run with a video you may download:
    /// `$env:ORANGELINK_TEST_VIDEO_URL = "<url>"; cargo test real_download -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_download() {
        let Ok(url) = std::env::var("ORANGELINK_TEST_VIDEO_URL") else {
            println!("SKIPPED: set ORANGELINK_TEST_VIDEO_URL to a video you may download");
            return;
        };
        let url = url.as_str();
        let ff = crate::media::find_ffmpeg_dir(None);
        let check = Check::run(ff.as_deref());
        println!("yt-dlp {:?} {:?} latest {:?} ffmpeg {}", check.ytdlp, check.version, check.latest, check.ffmpeg_ok);
        let exe = check.ytdlp.expect("yt-dlp installed");
        let dir = std::env::temp_dir().join(format!("orangelink-yt-{}", crate::model::new_id()));
        let info = probe(&exe, url, false, check.deno.as_deref()).unwrap();
        println!("{} {:?}s, {} formats, mp4 estimate {:?}", info.title, info.duration, info.formats.len(), info.estimate(OutFormat::Mp4));

        let r = Request {
            url: url.into(),
            out_dir: dir.clone(),
            name: "clip".into(),
            section: Some(("00:00:10".into(), "00:00:15".into())),
            ffmpeg_dir: ff.clone(),
            deno: check.deno.clone(),
            ..req()
        };
        let job = Job::spawn(&exe, &build_args(&r), eframe::egui::Context::default()).unwrap();
        let (mut files, mut progress, mut result) = (Vec::new(), 0, None);
        for ev in job.rx.iter() {
            match ev {
                Event::File(p) => files.push(p),
                Event::Progress(_) => progress += 1,
                Event::Log(l) => println!("  {l}"),
                Event::Done(r) => {
                    result = Some(r);
                    break;
                }
            }
        }
        assert_eq!(result, Some(Ok(())));
        assert!(progress > 0, "progress lines parsed");
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.is_file() && f.extension().is_some_and(|e| e == "mp4"), "{f:?}");
        let p = crate::media::probe(ff.as_deref().unwrap(), f).unwrap();
        println!("{f:?}: {:?}s {:?}", p.duration, p.codec);
        assert_eq!(p.codec.as_deref(), Some("h264"));
        assert!((p.duration.unwrap() - 5.0).abs() < 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_lines() {
        let p = parse_progress("OLPROG 5242880 10485760 NA 1048576.5 5").unwrap();
        assert_eq!(p.fraction, Some(0.5));
        assert_eq!((p.speed, p.eta), (Some(1048576.5), Some(5.0)));
        let est = parse_progress("OLPROG 100 NA 400 NA NA").unwrap();
        assert_eq!(est.fraction, Some(0.25));
        assert!(parse_progress("[download] Destination: x.mp4").is_none());
    }

    #[test]
    fn probe_and_estimate() {
        let json = r#"{"title":"T","duration":100,"formats":[
            {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a.40.2","abr":129.5,"filesize":1600000},
            {"format_id":"251","ext":"webm","vcodec":"none","acodec":"opus","abr":140},
            {"format_id":"248","ext":"webm","height":1080,"vcodec":"vp9","acodec":"none","filesize":9000000},
            {"format_id":"137","ext":"mp4","height":1080,"vcodec":"avc1.640028","acodec":"none","filesize":8000000},
            {"format_id":"270","ext":"mp4","height":1080,"vcodec":"avc1.640028","acodec":"none","tbr":800,"protocol":"m3u8_native"},
            {"format_id":"sb0","ext":"mhtml","vcodec":"none","acodec":"none"}]}"#;
        let p = parse_probe(json).unwrap();
        assert_eq!(p.formats.len(), 5);
        let videos = p.video_formats();
        assert!(videos[0].id == "137" && videos[1].id == "270" && videos[2].vcodec == "vp9");
        assert_eq!(p.formats.iter().find(|f| f.id == "270").unwrap().size, Some(10_000_000));
        assert_eq!(p.audio_formats()[0].id, "140");
        assert!(p.estimate(OutFormat::Mp4).unwrap() >= 9_600_000);
        assert_eq!(p.estimate(OutFormat::M4a), Some(1_600_000));
        assert!(videos[0].label().starts_with("1080p | mp4 | avc1"));
    }

    #[test]
    fn chat_matches_real_youtube_structure() {
        let text = r#"{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatPaidMessageRenderer":{"id":"p1","message":{"runs":[{"text":"thanks"}]},"authorName":{"simpleText":"@a"},"purchaseAmountText":{"simpleText":"$20.00"},"timestampText":{"simpleText":"-9:52"}}}}}]}}
{"replayChatItemAction":{"actions":[{"addLiveChatTickerItemAction":{"item":{"liveChatTickerPaidMessageItemRenderer":{"showItemEndpoint":{"showLiveChatItemEndpoint":{"renderer":{"liveChatPaidMessageRenderer":{"id":"p1","message":{"runs":[{"text":"thanks"}]},"authorName":{"simpleText":"@a"},"purchaseAmountText":{"simpleText":"$20.00"},"timestampText":{"simpleText":"-9:52"}}}}}}}}}]}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatPaidMessageRenderer":{"id":"p1","message":{"runs":[{"text":"thanks"}]},"authorName":{"simpleText":"@a"},"purchaseAmountText":{"simpleText":"$20.00"},"timestampText":{"simpleText":"-9:52"}}}}}]}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatPaidStickerRenderer":{"id":"s1","authorName":{"simpleText":"@b"},"timestampText":{"simpleText":"3:25"},"purchaseAmountText":{"simpleText":"ISK 224"}}}}}]}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatSponsorshipsGiftPurchaseAnnouncementRenderer":{"id":"g1","header":{"liveChatSponsorshipsHeaderRenderer":{"authorName":{"simpleText":"@c"},"primaryText":{"runs":[{"text":"Sent "},{"text":"10"},{"text":" gift memberships"}]}}}}}}}],"videoOffsetTimeMsec":"214000"}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatSponsorshipsGiftRedemptionAnnouncementRenderer":{"id":"r1","authorName":{"simpleText":"@d"},"message":{"runs":[{"text":"received a gift membership"}]}}}}}]}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatPlaceholderItemRenderer":{"id":"x"}}}}]}}"#;
        assert_eq!(
            chat_lines(text),
            vec!["[-9:52] @a：thanks〔Super Chat $20.00〕", "[3:25] @b：（貼圖）〔Super Chat ISK 224〕", "[3:34] @c：Sent 10 gift memberships"]
        );
    }

    #[test]
    fn chat_to_readable_lines() {
        let text = r#"{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatTextMessageRenderer":{"message":{"runs":[{"text":"晚安 "},{"emoji":{"emojiId":"x","shortcuts":[":wave:"]}}]},"authorName":{"simpleText":"小明"},"timestampText":{"simpleText":"1:05"}}}}}],"videoOffsetTimeMsec":"65000"}}
{"replayChatItemAction":{"actions":[{"addChatItemAction":{"item":{"liveChatPaidMessageRenderer":{"message":{"runs":[{"text":"加油"}]},"authorName":{"simpleText":"阿華"},"purchaseAmountText":{"simpleText":"NT$150.00"}}}}}],"videoOffsetTimeMsec":"3725000"}}
not json"#;
        assert_eq!(chat_lines(text), vec!["[1:05] 小明：晚安 :wave:", "[1:02:05] 阿華：加油〔Super Chat NT$150.00〕"]);
    }
}

#[cfg(test)]
mod real_chat {
    #[test]
    #[ignore]
    fn dump() {
        let path = std::env::var("OL_CHAT").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let t = std::time::Instant::now();
        let lines = super::chat_lines(&text);
        println!("{} lines in {:?}", lines.len(), t.elapsed());
        std::fs::write(format!("{path}.out.txt"), lines.join("\n")).unwrap();
    }
}
