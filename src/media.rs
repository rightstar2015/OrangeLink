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

/// A change to a subtitle's lines, made in the 字幕 tab.
#[derive(Clone, Debug, PartialEq)]
pub enum CueEdit {
    /// The line's new text (an empty one deletes the line).
    Text(usize, String),
    /// A new start or end, kept only if the line stays longer than nothing.
    Start(usize, f64),
    End(usize, f64),
    /// The line and the next one become one line.
    MergeNext(usize),
    /// The line becomes two at this time (its middle when outside it).
    Split(usize, f64),
    Delete(Vec<usize>),
    /// Every line earlier (−) or later (+) by these seconds.
    Shift(f64),
    /// Every `find` in the text becomes the other text.
    Replace(String, String),
}

/// Applies `edit` to `cues`; false when it changes nothing (a bad index, a
/// time that would leave the line empty, a word that isn't there). Times are
/// never pulled into range: a time that doesn't fit is turned down.
pub fn edit_cues(cues: &mut Vec<Cue>, edit: &CueEdit) -> bool {
    let ok_time = |t: f64| t.is_finite() && t >= 0.0;
    match edit {
        CueEdit::Text(i, text) => {
            let text = text.trim().replace("\r\n", "\n");
            match cues.get_mut(*i) {
                Some(_) if text.is_empty() => {
                    cues.remove(*i);
                    true
                }
                Some(c) if c.text != text => {
                    c.text = text;
                    true
                }
                _ => false,
            }
        }
        CueEdit::Start(i, t) => match cues.get_mut(*i) {
            Some(c) if ok_time(*t) && *t < c.end && *t != c.start => {
                c.start = *t;
                cues.sort_by(|a, b| a.start.total_cmp(&b.start));
                true
            }
            _ => false,
        },
        CueEdit::End(i, t) => match cues.get_mut(*i) {
            Some(c) if ok_time(*t) && *t > c.start && *t != c.end => {
                c.end = *t;
                true
            }
            _ => false,
        },
        CueEdit::MergeNext(i) => {
            if *i + 1 >= cues.len() {
                return false;
            }
            let next = cues.remove(*i + 1);
            let c = &mut cues[*i];
            c.end = c.end.max(next.end);
            c.text = format!("{}\n{}", c.text, next.text);
            true
        }
        CueEdit::Split(i, t) => {
            let Some(c) = cues.get(*i) else { return false };
            let chars: Vec<char> = c.text.chars().collect();
            if chars.len() < 2 || c.end - c.start < 0.2 {
                return false;
            }
            let at = if *t > c.start + 0.1 && *t < c.end - 0.1 { *t } else { (c.start + c.end) / 2.0 };
            // The text breaks where the time does: at a line break near there
            // when the line has one, else at that share of its characters.
            let share = ((at - c.start) / (c.end - c.start) * chars.len() as f64).round() as usize;
            let breaks: Vec<usize> = chars.iter().enumerate().filter(|(_, ch)| **ch == '\n').map(|(n, _)| n).collect();
            let cut = breaks.iter().copied().min_by_key(|b| b.abs_diff(share)).unwrap_or(share.clamp(1, chars.len() - 1));
            let first: String = chars[..cut].iter().collect();
            let second: String = chars[cut..].iter().collect();
            let (first, second) = (first.trim().to_string(), second.trim().to_string());
            if first.is_empty() || second.is_empty() {
                return false;
            }
            let end = c.end;
            cues[*i].end = at;
            cues[*i].text = first;
            cues.insert(*i + 1, Cue { start: at, end, text: second });
            true
        }
        CueEdit::Delete(ids) => {
            let before = cues.len();
            let mut n = 0;
            cues.retain(|_| {
                n += 1;
                !ids.contains(&(n - 1))
            });
            cues.len() != before
        }
        CueEdit::Shift(d) => {
            if !d.is_finite() || *d == 0.0 || cues.is_empty() {
                return false;
            }
            for c in cues.iter_mut() {
                let len = c.end - c.start;
                c.start = (c.start + d).max(0.0);
                c.end = (c.end + d).max(c.start + len.min(0.01));
            }
            true
        }
        CueEdit::Replace(find, with) => {
            if find.is_empty() {
                return false;
            }
            let mut changed = false;
            for c in cues.iter_mut().filter(|c| c.text.contains(find.as_str())) {
                c.text = c.text.replace(find.as_str(), with);
                changed = true;
            }
            cues.retain(|c| !c.text.trim().is_empty());
            changed
        }
    }
}

/// `cues` as a subtitle file: WebVTT when `vtt`, else SRT. Windows line ends.
pub fn cues_to_file(cues: &[Cue], vtt: bool) -> String {
    let stamp = |t: f64| {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        let sep = if vtt { '.' } else { ',' };
        format!("{:02}:{:02}:{:02}{sep}{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
    };
    let mut out = String::new();
    if vtt {
        out += "WEBVTT\r\n\r\n";
    }
    for (n, c) in cues.iter().enumerate() {
        if !vtt {
            out += &format!("{}\r\n", n + 1);
        }
        out += &format!("{} --> {}\r\n{}\r\n\r\n", stamp(c.start), stamp(c.end), c.text.replace('\n', "\r\n"));
    }
    out
}

/// Writes edited `cues` over the subtitle file at `path`, as UTF-8: to a
/// temporary file, to the disk, then in its place. Before the first change the
/// file as it was is kept beside it as `<name>.orig.<ext>`. False when the file
/// already holds these lines.
pub fn save_cues(path: &Path, cues: &[Cue]) -> std::io::Result<bool> {
    use std::io::Write;
    let ext = ext_of(path);
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend(cues_to_file(cues, ext == "vtt").into_bytes());
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let orig = path.with_file_name(format!("{stem}.orig.{ext}"));
    if path.is_file() && !orig.exists() {
        std::fs::copy(path, &orig)?;
    }
    let tmp = path.with_file_name(format!("{stem}.{ext}.orangelink-tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(true)
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
/// A frame of one flat colour (a black fade-in, a "starting soon" card) is
/// passed over for one later on; when every try is flat, the busiest is kept.
pub fn thumbnail(ffmpeg_dir: &Path, path: &Path, out: &Path, duration: Option<f64>, side: u32) -> bool {
    let mut best: Option<(f32, PathBuf)> = None;
    let mut tries = Vec::new();
    for (i, seek) in thumbnail_seeks(duration).into_iter().enumerate() {
        // `_thumbnail.png.try1.png`: still a name the library keeps for itself.
        let shot = if i == 0 {
            out.to_path_buf()
        } else {
            let mut name = out.as_os_str().to_owned();
            name.push(format!(".try{i}.png"));
            PathBuf::from(name)
        };
        if !grab_frame(ffmpeg_dir, path, &shot, seek, side) {
            // No picture at all (audio without cover art): a later time won't have one either.
            if i == 0 {
                return false;
            }
            continue;
        }
        tries.push(shot.clone());
        // A frame `image` can't read is kept as it is.
        let spread = crate::imaging::decode_file(&shot).map_or(f32::MAX, |img| frame_spread(&img));
        if best.as_ref().is_none_or(|(s, _)| spread > *s) {
            best = Some((spread, shot));
        }
        if spread >= PLAIN_SPREAD {
            break;
        }
    }
    let Some((_, best)) = best else { return false };
    let kept = best == out || std::fs::rename(&best, out).is_ok();
    for t in tries.iter().filter(|t| t.as_path() != out) {
        let _ = std::fs::remove_file(t);
    }
    kept && out.is_file()
}

fn grab_frame(ffmpeg_dir: &Path, path: &Path, out: &Path, seek: f64, side: u32) -> bool {
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

/// Where a video's thumbnail is looked for, in order: early on (10%, at most
/// 5 s in), then a quarter and half way through.
fn thumbnail_seeks(duration: Option<f64>) -> Vec<f64> {
    match duration.filter(|d| d.is_finite() && *d > 0.0) {
        Some(d) => vec![(d * 0.1).min(5.0), d * 0.25, d * 0.5],
        None => vec![0.0],
    }
}

/// How far a frame's brightness varies (standard deviation, 0–255) under which
/// it counts as one flat colour.
const PLAIN_SPREAD: f32 = 8.0;

/// The standard deviation of a frame's brightness, on a 32×32 copy.
fn frame_spread(img: &image::DynamicImage) -> f32 {
    let small = img.thumbnail(32, 32).to_luma8();
    let n = small.pixels().len().max(1) as f32;
    let mean = small.pixels().map(|p| p.0[0] as f32).sum::<f32>() / n;
    (small.pixels().map(|p| (p.0[0] as f32 - mean).powi(2)).sum::<f32>() / n).sqrt()
}

/// A video's clips as YouTube chapters (`0:00 開頭`, `12:34 名稱`…), by start
/// time, with a first one at 0:00 when no clip starts there. Also whether
/// YouTube will take them: at least 3, each at least 10 s long.
pub fn youtube_chapters(clips: &[(f64, String)], intro: &str) -> (String, bool) {
    let mut marks: Vec<(u64, String)> = clips.iter().filter(|(t, _)| t.is_finite() && *t >= 0.0).map(|(t, name)| (t.floor() as u64, name.replace(['\n', '\r'], " ").trim().to_string())).collect();
    marks.sort_by_key(|m| m.0);
    marks.dedup_by_key(|m| m.0);
    if marks.first().is_none_or(|m| m.0 > 0) {
        marks.insert(0, (0, intro.to_string()));
    }
    let stamp = |s: u64| if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) };
    let text = marks.iter().map(|(s, name)| format!("{} {name}", stamp(*s))).collect::<Vec<_>>().join("\n");
    let long_enough = marks.windows(2).all(|w| w[1].0 - w[0].0 >= 10);
    (text, marks.len() >= 3 && long_enough)
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

    fn cue(start: f64, end: f64, text: &str) -> Cue {
        Cue { start, end, text: text.into() }
    }

    /// A stream that opens on a black screen got a black thumbnail: 860
    /// of them in a grid can't be told apart.
    #[test]
    fn flat_frames_are_told_from_pictures() {
        let black = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(64, 36, image::Rgb([0, 0, 0])));
        let card = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, _| image::Rgb([200, 120, 40 + (x % 3) as u8])));
        let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(64, 36, |x, y| image::Rgb([(x * 4) as u8, (y * 7) as u8, 90])));
        assert!(frame_spread(&black) < PLAIN_SPREAD);
        assert!(frame_spread(&card) < PLAIN_SPREAD, "a flat card with a little noise");
        assert!(frame_spread(&picture) >= PLAIN_SPREAD);
        assert_eq!(thumbnail_seeks(Some(9.0 * 3600.0)), vec![5.0, 8100.0, 16200.0]);
        assert_eq!(thumbnail_seeks(Some(20.0)), vec![2.0, 5.0, 10.0]);
        assert_eq!(thumbnail_seeks(None), vec![0.0]);
        assert_eq!(thumbnail_seeks(Some(f64::NAN)), vec![0.0]);
    }

    #[test]
    fn a_video_thumbnail_passes_over_a_black_opening() {
        let Some(ff) = ffmpeg_for_test("a_video_thumbnail_passes_over_a_black_opening") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-thumb-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("v.mp4");
        // 4 s of black, then 16 s of test pattern: 10% (2 s) is black, 25% (5 s) isn't.
        let ok = crate::process::hidden(ff.join("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=black:size=320x180:rate=10:duration=4", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=10:duration=16"])
            .args(["-filter_complex", "[0:v][1:v]concat=n=2:v=1[v]", "-map", "[v]", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let out = dir.join("thumb.png");
        assert!(thumbnail(&ff, &video, &out, Some(20.0), 256));
        let img = crate::imaging::decode_file(&out).unwrap();
        assert!(frame_spread(&img) >= PLAIN_SPREAD, "the thumbnail is the pattern, not the black");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "the other tries are removed: {left:?}");
        let silent = dir.join("a.wav");
        assert!(crate::process::hidden(ff.join("ffmpeg")).args(["-v", "error", "-y", "-f", "lavfi", "-i", "anullsrc=d=2"]).arg(&silent).status().unwrap().success());
        assert!(!thumbnail(&ff, &silent, &dir.join("a.png"), Some(2.0), 256), "audio without a cover has no frame");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clips_become_youtube_chapters_from_zero() {
        let c = |t: f64, n: &str| (t, n.to_string());
        let (text, ok) = youtube_chapters(&[c(754.9, "BOSS 戰"), c(65.0, "開場聊天"), c(3725.0, "結尾\n抽獎")], "開頭");
        assert_eq!(text, "0:00 開頭\n1:05 開場聊天\n12:34 BOSS 戰\n1:02:05 結尾 抽獎");
        assert!(ok);
        let (text, ok) = youtube_chapters(&[c(0.0, "A"), c(5.0, "B"), c(30.0, "C")], "開頭");
        assert_eq!(text, "0:00 A\n0:05 B\n0:30 C", "a clip at 0:00 is the first chapter");
        assert!(!ok, "a chapter under 10 s: YouTube ignores them all");
        assert!(!youtube_chapters(&[c(100.0, "A")], "開頭").1, "fewer than 3");
    }

    #[test]
    fn subtitle_edits_change_lines_and_turn_down_times_that_dont_fit() {
        let mut cues = vec![cue(1.0, 3.0, "大家好"), cue(4.0, 6.0, "今天玩這個\n新遊戲"), cue(7.0, 9.0, "草")];
        assert!(edit_cues(&mut cues, &CueEdit::Text(0, " 大家晚安 ".into())));
        assert_eq!(cues[0].text, "大家晚安");
        assert!(!edit_cues(&mut cues, &CueEdit::Start(0, 3.5)), "past its end: turned down, not pulled in");
        assert!(!edit_cues(&mut cues, &CueEdit::Start(0, -1.0)));
        assert!(!edit_cues(&mut cues, &CueEdit::End(9, 5.0)), "no such line");
        assert!(edit_cues(&mut cues, &CueEdit::End(2, 99_999.0)), "past the video's end is kept as written");
        assert_eq!(cues[2].end, 99_999.0);
        assert!(edit_cues(&mut cues, &CueEdit::Split(1, 5.0)));
        assert_eq!((cues[1].text.as_str(), cues[1].end, cues[2].text.as_str(), cues[2].start), ("今天玩這個", 5.0, "新遊戲", 5.0), "split at the line break");
        assert!(edit_cues(&mut cues, &CueEdit::MergeNext(1)));
        assert_eq!((cues[1].text.as_str(), cues[1].end), ("今天玩這個\n新遊戲", 6.0));
        assert!(edit_cues(&mut cues, &CueEdit::Replace("遊戲".into(), "遊戲機".into())));
        assert!(!edit_cues(&mut cues, &CueEdit::Replace(String::new(), "x".into())), "an empty word replaces nothing");
        assert!(edit_cues(&mut cues, &CueEdit::Shift(-1.5)));
        assert_eq!((cues[0].start, cues[0].end), (0.0, 1.5), "kept at 0, not before it");
        assert!(edit_cues(&mut cues, &CueEdit::Text(2, "  ".into())), "an emptied line goes");
        assert_eq!(cues.len(), 2);
        assert!(edit_cues(&mut cues, &CueEdit::Delete(vec![0])));
        assert_eq!(cues.len(), 1);
    }

    #[test]
    fn splitting_a_line_without_breaks_cuts_between_characters() {
        // Multi-byte text cut at a byte offset once panicked the UI thread.
        let mut cues = vec![cue(0.0, 4.0, "#aébbb中文字")];
        assert!(edit_cues(&mut cues, &CueEdit::Split(0, 2.0)));
        assert_eq!(format!("{}{}", cues[0].text, cues[1].text), "#aébbb中文字");
        let mut short = vec![cue(0.0, 1.0, "嗯")];
        assert!(!edit_cues(&mut short, &CueEdit::Split(0, 0.5)), "one character can't be split");
    }

    #[test]
    fn edited_subtitles_are_saved_as_utf8_and_the_gbk_original_is_kept() {
        let dir = std::env::temp_dir().join(format!("orangelink-subsave-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("直播.srt");
        let (gbk, _, _) = encoding_rs::GBK.encode("1\r\n00:00:01,000 --> 00:00:02,500\r\n简体字幕测试\r\n");
        std::fs::write(&path, &gbk).unwrap();
        let mut cues = parse_cues(&read_text(&path, 1 << 20).unwrap());
        assert_eq!(cues[0].text, "简体字幕测试");
        assert!(edit_cues(&mut cues, &CueEdit::Text(0, "改好的字幕".into())));
        assert!(save_cues(&path, &cues).unwrap());
        assert_eq!(std::fs::read(dir.join("直播.orig.srt")).unwrap(), gbk.to_vec(), "the original, byte for byte");
        let back = parse_cues(&read_text(&path, 1 << 20).unwrap());
        assert_eq!(back, vec![cue(1.0, 2.5, "改好的字幕")]);
        assert!(!save_cues(&path, &back).unwrap(), "unchanged: not written");
        let vtt = dir.join("a.vtt");
        save_cues(&vtt, &back).unwrap();
        assert!(read_text(&vtt, 1 << 20).unwrap().starts_with("WEBVTT"));
        assert_eq!(parse_cues(&read_text(&vtt, 1 << 20).unwrap()), back);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
