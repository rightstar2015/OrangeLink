//! File-type detection and ffmpeg helpers for non-image media.

use crate::model::MediaKind;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mkv", "avi", "webm", "m4v", "wmv", "flv", "mpg", "mpeg", "mts", "m2ts", "ts", "3gp", "mxf",
];
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "flac", "aac", "m4a", "ogg", "opus", "wma", "aiff", "aif"];
/// Image formats the `image` crate can't decode; ffmpeg may still make a thumbnail.
const EXTRA_IMAGE_EXTS: &[&str] = &["avif", "heic", "heif", "psd", "svg", "tga", "dds", "exr", "hdr", "jxl"];

/// The file types the 匯入檔案 dialog offers, the first one shown first.
/// Any file can be imported, so it starts with all of them; images alone
/// once hid every video and audio file.
pub fn import_dialog_filters() -> Vec<(&'static str, Vec<&'static str>)> {
    let images = crate::imaging::SUPPORTED_EXTS.iter().chain(EXTRA_IMAGE_EXTS).copied().collect();
    vec![
        (tk!("所有檔案"), vec!["*"]),
        (tk!("圖片"), images),
        (tk!("影片"), VIDEO_EXTS.to_vec()),
        (tk!("音訊"), AUDIO_EXTS.to_vec()),
        (tk!("字幕與文字"), vec!["srt", "vtt", "ass", "txt"]),
    ]
}

/// Files that are never worth importing.
pub fn is_junk(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return true };
    let lower = name.to_ascii_lowercase();
    name.starts_with('.')
        || name.starts_with("~$")
        || matches!(lower.as_str(), "desktop.ini" | "thumbs.db" | "ehthumbs.db")
        || lower.ends_with(".tmp")
        || lower.ends_with(".crdownload")
        || lower.ends_with(".part")
}

pub fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn kind_of(path: &Path) -> MediaKind {
    let ext = ext_of(path);
    let e = ext.as_str();
    if crate::imaging::SUPPORTED_EXTS.contains(&e) || EXTRA_IMAGE_EXTS.contains(&e) {
        MediaKind::Image
    } else if VIDEO_EXTS.contains(&e) {
        MediaKind::Video
    } else if AUDIO_EXTS.contains(&e) {
        MediaKind::Audio
    } else {
        MediaKind::Other
    }
}

/// Directory holding ffmpeg/ffprobe: the configured one, else the first match on PATH.
/// FFmpeg for a test. Without one the test is skipped, and says so; with
/// `ORANGELINK_REQUIRE_FFMPEG` set (CI installs FFmpeg) a missing FFmpeg fails it.
#[cfg(test)]
pub fn ffmpeg_for_test(test: &str) -> Option<PathBuf> {
    let found = find_ffmpeg_dir(None);
    if found.is_none() {
        assert!(std::env::var_os("ORANGELINK_REQUIRE_FFMPEG").is_none(), "{test}: FFmpeg is required but was not found");
        eprintln!("SKIPPED {test}: FFmpeg not found");
    }
    found
}

pub fn find_ffmpeg_dir(configured: Option<&Path>) -> Option<PathBuf> {
    let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    if let Some(dir) = configured {
        return dir.join(exe).is_file().then(|| dir.to_path_buf());
    }
    let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    path_dirs.into_iter().chain(crate::ytdlp::extra_tool_dirs()).find(|d| d.join(exe).is_file())
}

/// An ffmpeg/ffprobe command with no console window and no stdin
/// (a windowed app has no valid console handles to hand down).
pub fn tool(dir: &Path, name: &str) -> Command {
    let mut cmd = crate::process::hidden(dir.join(name));
    if name == "ffmpeg" {
        cmd.arg("-nostdin");
    }
    cmd
}

#[derive(Default)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration: Option<f64>,
    pub fps: Option<f64>,
    pub codec: Option<String>,
}

pub fn probe(ffmpeg_dir: &Path, path: &Path) -> Option<Probe> {
    let out = tool(ffmpeg_dir, "ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,width,height,avg_frame_rate:stream_disposition=attached_pic:format=duration"])
        .args(["-of", "json"])
        .arg(path)
        .output()
        .ok()?;
    parse_probe(&serde_json::from_slice(&out.stdout).ok()?)
}

/// ffprobe's JSON (see `probe`) as a `Probe`.
fn parse_probe(json: &serde_json::Value) -> Option<Probe> {
    let streams = json.get("streams").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    // Cover art in an MP3 or M4A is a 1-frame "video" stream marked attached_pic:
    // it is not the file's video, and its codec and size aren't the file's.
    let cover = |s: &serde_json::Value| s.get("disposition").and_then(|d| d.get("attached_pic")).and_then(|v| v.as_u64()) == Some(1);
    let of_type = |t: &str| streams.iter().find(|s| s.get("codec_type").and_then(|v| v.as_str()) == Some(t) && !cover(s));
    let video = of_type("video");
    let main = video.or_else(|| of_type("audio"));
    let dim = |k: &str| video.and_then(|s| s.get(k)).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let fps = video
        .and_then(|s| s.get("avg_frame_rate"))
        .and_then(|v| v.as_str())
        .and_then(|r| {
            let (n, d) = r.split_once('/')?;
            let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
            (d > 0.0 && n > 0.0).then(|| n / d)
        });
    Some(Probe {
        width: dim("width"),
        height: dim("height"),
        duration: json
            .get("format")
            .and_then(|f| f.get("duration"))
            .and_then(|d| d.as_str())
            .and_then(|d| d.parse().ok()),
        fps,
        codec: main.and_then(|s| s.get("codec_name")).and_then(|v| v.as_str()).map(str::to_string),
    })
}

/// Renders an audio waveform as a PNG thumbnail.
pub fn waveform(ffmpeg_dir: &Path, path: &Path, out: &Path, side: u32) -> bool {
    let filter = format!("showwavespic=s={side}x{}:colors=0xFF8C32", side / 2);
    tool(ffmpeg_dir, "ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-filter_complex", &filter, "-frames:v", "1"])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && out.is_file()
}

const TEXT_EXTS: &[&str] = &["srt", "vtt", "ass", "ssa", "lrc", "txt", "md", "csv", "json", "xml"];

/// First ~64 KB of a text-like file (UTF-8 or UTF-16 with BOM), for preview and search.
pub fn text_excerpt(path: &Path) -> Option<String> {
    if !TEXT_EXTS.contains(&ext_of(path).as_str()) {
        return None;
    }
    read_text(path, 64 * 1024)
}

/// Reads up to `limit` bytes of a text file, honouring UTF-8/UTF-16 byte-order marks.
pub fn read_text(path: &Path, limit: u64) -> Option<String> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(limit).read_to_end(&mut buf).ok()?;
    let text = if let Some(rest) = buf.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = buf.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        decode_8bit(buf.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&buf))
    };
    Some(text)
}

/// UTF-8 when it is valid (a multi-byte character cut off by `limit` still counts),
/// otherwise Big5 (older Traditional Chinese subtitles) or GBK (Simplified), by
/// [`big5_gbk_votes`]; a tie goes to Big5.
fn decode_8bit(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => return s.to_string(),
        Err(e) if e.error_len().is_none() => return String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => {}
    }
    let (big5, gbk) = big5_gbk_votes(bytes);
    let (mut enc, other) = if gbk > big5 { (encoding_rs::GBK, encoding_rs::BIG5) } else { (encoding_rs::BIG5, encoding_rs::GBK) };
    // Bytes the likelier one can't read at all, but the other can.
    if enc.decode_without_bom_handling_and_without_replacement(bytes).is_none()
        && other.decode_without_bom_handling_and_without_replacement(bytes).is_some()
    {
        enc = other;
    }
    enc.decode_without_bom_handling(bytes).0.into_owned()
}

/// Two-byte characters in Big5's common block (常用字, A440–C67E) and in GB2312's
/// level-one block (B0A1–D7FE). Almost any GBK text is also valid Big5 and the
/// other way round, but each lands mostly in its own common block, so the larger
/// count is the file's encoding.
fn big5_gbk_votes(bytes: &[u8]) -> (usize, usize) {
    let (mut big5, mut gbk) = (0, 0);
    let mut i = 0;
    while i + 1 < bytes.len() {
        let (lead, trail) = (bytes[i], bytes[i + 1]);
        if lead < 0x81 {
            i += 1;
            continue;
        }
        if (0xA4..=0xC5).contains(&lead) || (lead == 0xC6 && trail <= 0x7E) {
            big5 += 1;
        }
        if (0xB0..=0xD7).contains(&lead) && trail >= 0xA1 {
            gbk += 1;
        }
        i += 2;
    }
    (big5, gbk)
}

/// One timed line of an SRT/VTT subtitle file.
#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// `01:02:03,456`, `01:02:03.456` or `02:03.456` in seconds.
fn parse_timestamp(s: &str) -> Option<f64> {
    let s = s.trim().replace(',', ".");
    let mut secs = 0.0;
    for part in s.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs)
}

/// Removes `<i>`-style markup and `{\an8}`-style override codes.
fn strip_markup(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' | '{' => depth += 1,
            '>' | '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Parses SRT or WebVTT cues, sorted by start time.
pub fn parse_cues(text: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((a, b)) = line.split_once("-->") else { continue };
        // VTT may put cue settings after the end time.
        let b = b.split_whitespace().next().unwrap_or("");
        let (Some(start), Some(end)) = (parse_timestamp(a), parse_timestamp(b)) else { continue };
        let mut body = Vec::new();
        while let Some(l) = lines.peek() {
            if l.trim().is_empty() {
                break;
            }
            body.push(strip_markup(lines.next().unwrap_or_default()));
        }
        let text = body.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
        if !text.is_empty() {
            cues.push(Cue { start, end: end.max(start), text });
        }
    }
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    cues
}

/// The dialogue of a subtitle file, for searching. Very long files are cut off.
pub fn subtitle_search_text(path: &Path) -> Option<String> {
    let cues = parse_cues(&read_text(path, 1 << 20)?);
    (!cues.is_empty()).then(|| cues.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join("\n"))
}

/// Subtitle text without cue numbers and timestamps, for display.
pub fn subtitle_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("-->") && l.parse::<u32>().is_err() && *l != "WEBVTT")
        .collect::<Vec<_>>()
        .join("\n")
}

/// Grabs one frame (or embedded cover art, for audio) as a PNG thumbnail.
pub fn thumbnail(ffmpeg_dir: &Path, path: &Path, out: &Path, duration: Option<f64>, side: u32) -> bool {
    let seek = duration.map(|d| (d * 0.1).min(5.0)).unwrap_or(0.0);
    let scale = format!("scale='min({side},iw)':'min({side},ih)':force_original_aspect_ratio=decrease");
    tool(ffmpeg_dir, "ffmpeg")
        .args(["-v", "error", "-y", "-ss", &format!("{seek:.2}"), "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-vf", &scale])
        .arg(out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        && out.is_file()
}

pub fn fmt_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `1:02.5`-style time with tenths, for clip marks.
pub fn fmt_precise(secs: f64) -> String {
    let t = (secs.max(0.0) * 10.0).round() as u64;
    let (s, tenth) = (t / 10, t % 10);
    if s >= 3600 {
        format!("{}:{:02}:{:02}.{tenth}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}.{tenth}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_dialog_offers_videos_audio_and_subtitles_not_only_images() {
        let filters = import_dialog_filters();
        assert_eq!(filters[0].1, vec!["*"], "the first, shown filter lets any file through");
        let offered = |ext: &str| filters.iter().any(|(_, exts)| exts.contains(&ext));
        for ext in ["png", "mp4", "mkv", "mp3", "m4a", "srt", "vtt"] {
            assert!(offered(ext), "{ext} has a filter");
        }
    }

    #[test]
    fn cover_art_in_an_audio_file_is_not_taken_for_its_video() {
        let json = serde_json::json!({
            "streams": [
                { "codec_type": "audio", "codec_name": "mp3" },
                { "codec_type": "video", "codec_name": "mjpeg", "width": 600, "height": 600, "avg_frame_rate": "0/0", "disposition": { "attached_pic": 1 } }
            ],
            "format": { "duration": "215.5" }
        });
        let p = parse_probe(&json).unwrap();
        assert_eq!((p.width, p.height, p.codec.as_deref()), (0, 0, Some("mp3")));
        assert_eq!(p.duration, Some(215.5));

        let json = serde_json::json!({ "streams": [
            { "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "avg_frame_rate": "30/1", "disposition": { "attached_pic": 0 } },
            { "codec_type": "audio", "codec_name": "aac" }
        ] });
        let p = parse_probe(&json).unwrap();
        assert_eq!((p.width, p.height, p.codec.as_deref(), p.fps), (1920, 1080, Some("h264"), Some(30.0)));
    }

    #[test]
    fn parses_srt_and_vtt() {
        let srt = "1\r\n00:00:01,500 --> 00:00:03,000\r\n<i>Hello</i>\r\nworld\r\n\r\n2\r\n00:01:00,000 --> 00:01:02,250\r\n{\\an8}第二句\r\n";
        let cues = parse_cues(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start: 1.5, end: 3.0, text: "Hello\nworld".into() });
        assert_eq!(cues[1].text, "第二句");
        assert!((cues[1].end - 62.25).abs() < 1e-9);

        let vtt = "WEBVTT\n\n00:05.000 --> 00:06.000 align:start\nshort form\n";
        let cues = parse_cues(vtt);
        assert_eq!(cues, vec![Cue { start: 5.0, end: 6.0, text: "short form".into() }]);
    }

    #[test]
    fn decodes_big5_and_truncated_utf8() {
        let line = "大家好，歡迎收看";
        let (big5, _, _) = encoding_rs::BIG5.encode(line);
        assert_eq!(decode_8bit(&big5), line);
        // A UTF-8 file cut in the middle of a character is still UTF-8.
        let utf8 = line.as_bytes();
        assert!(decode_8bit(&utf8[..utf8.len() - 1]).starts_with("大家好，歡迎收"));
    }

    /// GBK bytes are nearly always valid Big5 too, so trying Big5 first turned
    /// Simplified subtitles into nonsense.
    #[test]
    fn tells_gbk_from_big5() {
        let srt = |lines: &[&str]| -> String {
            lines.iter().enumerate().map(|(n, l)| format!("{}\n00:00:0{n},000 --> 00:00:0{n},900\n{l}\n\n", n + 1)).collect()
        };
        let simplified = srt(&["大家晚安", "今天来玩恐怖游戏", "谢谢大家的支持，我们下次见", "这个真的太可怕了吧"]);
        let traditional = srt(&["大家晚安", "今天來玩恐怖遊戲", "謝謝大家的支持，我們下次見", "這個真的太可怕了吧"]);
        let (gbk, _, bad) = encoding_rs::GBK.encode(&simplified);
        assert!(!bad);
        assert_eq!(decode_8bit(&gbk), simplified);
        let (big5, _, bad) = encoding_rs::BIG5.encode(&traditional);
        assert!(!bad);
        assert_eq!(decode_8bit(&big5), traditional);
        // A single short line still comes out right.
        assert_eq!(decode_8bit(&encoding_rs::GBK.encode("大家晚安").0), "大家晚安");
        // Cut off in the middle of a character: the rest is still read.
        assert!(decode_8bit(&gbk[..gbk.len() - 3]).contains("谢谢大家的支持"));
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    /// Damaged or odd subtitle files: never a panic, and only real times.
    #[test]
    fn random_subtitle_text() {
        let pieces = &["-->", " --> ", "00:00:01,000", "01:02.5", "99:99:99.999", "\n\n", "1\n", "WEBVTT\n", "::", ",,"];
        for s in crate::fuzz::strings_with(2, 20_000, pieces) {
            for c in parse_cues(&s) {
                assert!(c.start.is_finite() && c.end.is_finite() && c.start >= 0.0, "{s:?}: {} {}", c.start, c.end);
            }
        }
    }
}
