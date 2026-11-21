//! Speech to subtitles through FFmpeg's built-in whisper filter (FFmpeg 8 or newer
//! built with whisper.cpp). Models are downloaded once into the tools folder.

use crate::media::{parse_cues, Cue};
use eframe::egui;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
    time::Duration,
};

/// (name, size shown to the user)
pub const MODELS: [(&str, &str); 3] = [("base", "約 150 MB，較快"), ("small", "約 490 MB，建議"), ("medium", "約 1.5 GB，較準但較慢")];
/// (whisper code, label)
pub const LANGUAGES: [(&str, &str); 5] = [("auto", "自動偵測"), ("zh", "中文"), ("ja", "日文"), ("en", "英文"), ("ko", "韓文")];
/// Seconds of audio per transcription chunk: Whisper's own window. Shorter chunks cut
/// sentences apart and invite made-up lines (10 s lost the opening of a real video).
const QUEUE_SECS: u32 = 30;
/// Credits from Whisper's training data, which it tends to "hear" in music and silence.
const MADE_UP_LINES: [&str; 8] = ["字幕製作", "字幕制作", "字幕志願", "字幕志愿", "字幕提供", "不吝點贊", "不吝点赞", "Amara.org"];

fn command(program: &Path) -> Command {
    crate::process::hidden(program)
}

fn ffmpeg_exe(dir: &Path) -> PathBuf {
    dir.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" })
}

fn models_dir() -> Option<PathBuf> {
    crate::ytdlp::tools_dir().map(|d| d.join("whisper"))
}

fn model_file(name: &str) -> String {
    format!("ggml-{name}.bin")
}

pub fn has_model(name: &str) -> bool {
    models_dir().is_some_and(|d| d.join(model_file(name)).is_file())
}

/// Whether this FFmpeg build includes the whisper filter.
pub fn has_filter(ffmpeg_dir: &Path) -> bool {
    command(&ffmpeg_exe(ffmpeg_dir))
        .args(["-hide_banner", "-filters"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.split_whitespace().nth(1) == Some("whisper")))
}

pub fn download_model(name: &str) -> Result<(), String> {
    let dir = models_dir().ok_or("找不到 APPDATA 資料夾")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = model_file(name);
    let part = dir.join(format!("{file}.part"));
    let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}");
    // The largest model is about 3 GB.
    let curl = || crate::self_update::curl(8 * 1024 * 1024 * 1024);
    let out = curl()?
        .args(["-L", "--fail", "-sS", "--max-time", "3600", "-o"])
        .arg(&part)
        .arg(url)
        .output()
        .map_err(|e| format!("無法執行 curl：{e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    // Hugging Face lists each model's SHA-256 (its LFS id): a cut-off or damaged
    // download is caught here, not when the transcription fails.
    let listing = curl()?.args(["-L", "--fail", "-sS", "--max-time", "30", MODELS_API]).output().ok().filter(|o| o.status.success());
    match listing.and_then(|o| lfs_hash(&o.stdout, &file)) {
        Some(want) => {
            let got = crate::self_update::sha256_file(&part).map_err(|e| e.to_string())?;
            if !got.eq_ignore_ascii_case(&want) {
                let _ = std::fs::remove_file(&part);
                return Err(format!("下載的 {file} 和 Hugging Face 公布的 SHA-256 不符，已刪除，請重試"));
            }
        }
        None => {
            let _ = std::fs::remove_file(&part);
            crate::log::error("whisper", format!("讀不到 {file} 公布的 SHA-256，已拒絕使用未驗證的檔案"));
            return Err(format!("無法取得 Hugging Face 公布的 {file} SHA-256，基於安全已刪除暫存檔，請稍後重試"));
        }
    }
    std::fs::rename(&part, dir.join(file)).map_err(|e| e.to_string())
}

const MODELS_API: &str = "https://huggingface.co/api/models/ggerganov/whisper.cpp/tree/main";

/// `file`'s SHA-256 in the Hugging Face tree listing (`[{"path": …, "lfs": {"oid": …}}]`).
fn lfs_hash(listing: &[u8], file: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(listing).ok()?;
    let entry = v.as_array()?.iter().find(|e| e.get("path").and_then(|p| p.as_str()) == Some(file))?;
    let oid = entry.get("lfs")?.get("oid")?.as_str()?;
    (oid.len() == 64 && oid.chars().all(|c| c.is_ascii_hexdigit())).then(|| oid.to_lowercase())
}

pub enum Event {
    /// Seconds of media processed so far.
    Position(f64),
    Done(Result<(), String>),
}

pub struct Job {
    pub rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Transcribes `input` into the SRT file `out`, on a background thread.
    pub fn spawn(ffmpeg_dir: &Path, input: &Path, out: &Path, model: &str, language: &str, ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (exe, input, out) = (ffmpeg_exe(ffmpeg_dir), input.to_path_buf(), out.to_path_buf());
        let (model, language, stop) = (model.to_string(), language.to_string(), cancel.clone());
        std::thread::spawn(move || {
            let result = run(&exe, &input, &out, &model, &language, &stop, &tx, &ctx);
            let _ = tx.send(Event::Done(result));
            ctx.request_repaint();
        });
        Self { rx, cancel }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(exe: &Path, input: &Path, out: &Path, model: &str, language: &str, stop: &AtomicBool, tx: &Sender<Event>, ctx: &egui::Context) -> Result<(), String> {
    let dir = models_dir().ok_or("找不到 APPDATA 資料夾")?;
    // Run inside the model folder: Windows paths would need escaping twice in a filter.
    let temp = format!("job-{}.srt", crate::model::new_id());
    let filter = format!("whisper=model={}:language={language}:queue={QUEUE_SECS}:format=srt:destination={temp}", model_file(model));
    let mut child = command(exe)
        .current_dir(&dir)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(input)
        .args(["-vn", "-af", &filter, "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("無法啟動 FFmpeg：{e}"))?;
    let stderr = child.stderr.take().ok_or("無法讀取 FFmpeg 輸出")?;
    let (progress, ctx2) = (tx.clone(), ctx.clone());
    let reader = std::thread::spawn(move || read_progress(stderr, &progress, &ctx2));
    let status = loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let log = reader.join().unwrap_or_default();
    let temp = dir.join(temp);
    let result = if stop.load(Ordering::Relaxed) {
        Err("已取消".to_string())
    } else if !status.success() {
        crate::log::error("whisper", format!("FFmpeg 結束代碼 {:?}，輸入 {}\n{log}", status.code(), input.display()));
        Err(log)
    } else {
        finish(&temp, out, language)
    };
    let _ = std::fs::remove_file(&temp);
    result
}

/// Whisper mixes Simplified and Traditional. Japanese shares many characters, so an
/// auto-detected transcript with any kana is left as it is.
fn is_chinese(language: &str, cues: &[Cue]) -> bool {
    let kana = |c: char| matches!(c, '\u{3040}'..='\u{30FF}');
    language == "zh" || (language == "auto" && !cues.iter().any(|c| c.text.chars().any(kana)))
}

fn to_traditional(cues: &mut [Cue]) {
    let Ok(cc) = OpenCC::from_config(BuiltinConfig::S2twp) else { return };
    for c in cues {
        c.text = cc.convert(&c.text);
    }
}

fn finish(temp: &Path, out: &Path, language: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(temp).map_err(|e| format!("沒有產生字幕：{e}"))?;
    let mut cues = parse_cues(&text);
    cues.retain(|c| !MADE_UP_LINES.iter().any(|line| c.text.contains(line)));
    if cues.is_empty() {
        return Err("沒有辨識到語音".to_string());
    }
    if is_chinese(language, &cues) {
        to_traditional(&mut cues);
    }
    std::fs::write(out, to_srt(&cues)).map_err(|e| format!("無法寫入字幕：{e}"))
}

/// Reads FFmpeg's stderr, reporting `time=`; returns the last lines for error messages.
fn read_progress(mut stderr: impl Read, tx: &Sender<Event>, ctx: &egui::Context) -> String {
    let (mut buf, mut pending, mut tail) = ([0u8; 4096], String::new(), Vec::<String>::new());
    while let Ok(n) = stderr.read(&mut buf) {
        if n == 0 {
            break;
        }
        pending.push_str(&String::from_utf8_lossy(&buf[..n]));
        // Progress lines end in \r, everything else in \n.
        while let Some(end) = pending.find(['\r', '\n']) {
            let line: String = pending.drain(..=end).collect();
            match parse_time(&line) {
                Some(pos) => {
                    let _ = tx.send(Event::Position(pos));
                    ctx.request_repaint();
                }
                None if !line.trim().is_empty() => {
                    tail.push(line.trim().to_string());
                    if tail.len() > 3 {
                        tail.remove(0);
                    }
                }
                None => {}
            }
        }
    }
    tail.join("\n")
}

fn parse_time(line: &str) -> Option<f64> {
    let value = line.split("time=").nth(1)?.split_whitespace().next()?;
    let mut secs = 0.0;
    for part in value.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs)
}

/// Numbered from 1 with no overlaps (the filter's cues can run into the next one).
fn to_srt(cues: &[Cue]) -> String {
    let stamp = |t: f64| {
        let ms = (t.max(0.0) * 1000.0).round() as u64;
        format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
    };
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        let end = cues.get(i + 1).map_or(c.end, |next| c.end.min(next.start)).max(c.start);
        out += &format!("{}\r\n{} --> {}\r\n{}\r\n\r\n", i + 1, stamp(c.start), stamp(end), c.text);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_model_hashes_from_the_listing() {
        let h = "7E".repeat(32);
        let listing = format!(r#"[{{"path":"README.md","oid":"bb07"}},{{"path":"ggml-base.bin","lfs":{{"oid":"{h}","size":1}}}}]"#);
        assert_eq!(lfs_hash(listing.as_bytes(), "ggml-base.bin"), Some(h.to_lowercase()));
        assert_eq!(lfs_hash(listing.as_bytes(), "README.md"), None, "not stored in LFS");
        assert_eq!(lfs_hash(b"<html>", "ggml-base.bin"), None);
    }

    #[test]
    fn chinese_becomes_traditional_and_japanese_stays() {
        let cue = |text: &str| Cue { start: 0.0, end: 1.0, text: text.into() };
        let mut cues = vec![cue("大家好，欢迎观看这个视频"), cue("這段已經是繁體，皇后也不會變"), cue("OK 123")];
        assert!(is_chinese("auto", &cues));
        to_traditional(&mut cues);
        assert_eq!(cues[0].text, "大家好，歡迎觀看這個影片");
        assert_eq!(cues[1].text, "這段已經是繁體，皇后也不會變");
        assert_eq!(cues[2].text, "OK 123");
        assert!(!is_chinese("auto", &[cue("気になるアニメ")]));
        assert!(!is_chinese("ja", &[cue("気")]));
        assert!(!is_chinese("en", &[cue("hello")]));
        assert!(is_chinese("zh", &[cue("气")]));
    }

    #[test]
    fn made_up_credits_are_dropped() {
        let dir = std::env::temp_dir().join(format!("ol-whisper-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (temp, out) = (dir.join("raw.srt"), dir.join("out.srt"));
        std::fs::write(&temp, "0\n00:00:00,000 --> 00:00:10,000\n(字幕製作:貝爾)\n\n1\n00:00:10,000 --> 00:00:11,000\n不能拉我啊\n").unwrap();
        finish(&temp, &out, "zh").unwrap();
        let srt = std::fs::read_to_string(&out).unwrap();
        assert!(srt.starts_with("1\r\n00:00:10,000") && !srt.contains("貝爾"), "{srt}");
        std::fs::write(&temp, "0\n00:00:00,000 --> 00:00:10,000\n字幕提供\n").unwrap();
        assert_eq!(finish(&temp, &out, "zh"), Err("沒有辨識到語音".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn srt_is_renumbered_without_overlaps() {
        let raw = "0\n00:00:00,000 --> 00:00:03,240\n大家好\n\n1\n00:00:02,971 --> 00:00:04,971\n歡迎\n\n2\n00:01:02,000 --> 00:01:03,500\n再見\n";
        let srt = to_srt(&parse_cues(raw));
        assert!(srt.starts_with("1\r\n00:00:00,000 --> 00:00:02,971\r\n大家好"), "{srt}");
        assert!(srt.contains("3\r\n00:01:02,000 --> 00:01:03,500\r\n再見"), "{srt}");
        assert_eq!(parse_cues(&srt).len(), 3);
    }

    /// Needs FFmpeg with whisper, APPDATA holding the base model, and OL_SPEECH.
    #[test]
    #[ignore]
    fn cancels_and_transcribes() {
        let input = PathBuf::from(std::env::var("OL_SPEECH").unwrap());
        let ff = crate::media::find_ffmpeg_dir(None).unwrap();
        assert!(has_filter(&ff));
        let out = std::env::temp_dir().join(format!("ol-{}.srt", crate::model::new_id()));
        let done = |job: &Job| loop {
            if let Event::Done(r) = job.rx.recv().unwrap() {
                break r;
            }
        };
        let job = Job::spawn(&ff, &input, &out, "base", "zh", egui::Context::default());
        std::thread::sleep(Duration::from_millis(300));
        job.cancel();
        assert_eq!(done(&job), Err("已取消".to_string()));
        assert!(!out.exists());
        let job = Job::spawn(&ff, &input, &out, "base", "zh", egui::Context::default());
        assert_eq!(done(&job), Ok(()));
        let srt = std::fs::read_to_string(&out).unwrap();
        println!("{srt}");
        assert!(srt.contains(" --> "));
        std::fs::remove_file(&out).unwrap();
        let bad = Job::spawn(&ff, &input.with_extension("missing"), &out, "base", "zh", egui::Context::default());
        assert!(done(&bad).is_err());
        let leftovers = std::fs::read_dir(models_dir().unwrap()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("job-")).count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn reads_ffmpeg_progress() {
        assert_eq!(parse_time("size=N/A time=00:01:02.50 bitrate=N/A speed=2.7x"), Some(62.5));
        assert_eq!(parse_time("time=N/A bitrate=N/A"), None);
        assert_eq!(parse_time("[whisper] run transcription"), None);
    }
}
