//! A plain log file for bug reports: `%APPDATA%\OrangeLink\logs\orangelink-YYYY-MM-DD.log`,
//! one file a day, kept for 7 days. Lines never hold the user's home folder (it becomes
//! `~`) or the MCP token and pairing code.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

const KEEP_DAYS: usize = 7;

static SECRETS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static WRITE: Mutex<()> = Mutex::new(());

pub fn dir() -> Option<PathBuf> {
    // Tests log failures too (bad MCP arguments, missing FFmpeg): keep them out
    // of the user's real log.
    if cfg!(test) {
        return Some(std::env::temp_dir().join("orangelink-test-logs"));
    }
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("OrangeLink").join("logs"))
}

/// Values to blank out of every line, such as tokens.
pub fn set_secrets(secrets: impl IntoIterator<Item = String>) {
    let mut s = crate::lock(&SECRETS);
    *s = secrets.into_iter().filter(|v| v.len() >= 8).collect();
}

/// The home folder as `~`, and secrets as `***`.
fn scrub(text: &str) -> String {
    let mut out = text.to_string();
    for s in crate::lock(&SECRETS).iter() {
        out = out.replace(s.as_str(), "***");
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        out = replace_ignore_case(&out, &home.to_string_lossy(), "~");
    }
    out
}

/// Windows paths differ in case from one API to the next.
fn replace_ignore_case(text: &str, from: &str, to: &str) -> String {
    if from.is_empty() || !from.is_ascii() {
        return text.replace(from, to);
    }
    let (lower, needle) = (text.to_ascii_lowercase(), from.to_ascii_lowercase());
    let mut out = String::new();
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        out.push_str(&text[last..i]);
        out.push_str(to);
        last = i + from.len();
    }
    out.push_str(&text[last..]);
    out
}

fn write_line(dir: &Path, level: &str, area: &str, msg: &str) {
    let now = chrono::Local::now();
    let file = dir.join(format!("orangelink-{}.log", now.format("%Y-%m-%d")));
    let line = format!("{} [{level}] {area}: {}\n", now.format("%H:%M:%S"), scrub(msg).replace('\n', "\n    "));
    let _guard = crate::lock(&WRITE);
    let new_file = !file.exists();
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&file) {
        let _ = f.write_all(line.as_bytes());
    }
    if new_file {
        prune(dir);
    }
}

/// Keeps the newest `KEEP_DAYS` daily files.
fn prune(dir: &Path) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("orangelink-"))).collect())
        .unwrap_or_default();
    files.sort();
    for old in files.iter().rev().skip(KEEP_DAYS) {
        let _ = std::fs::remove_file(old);
    }
}

pub fn info(area: &str, msg: impl AsRef<str>) {
    if let Some(d) = dir() {
        write_line(&d, "INFO", area, msg.as_ref());
    }
}

pub fn error(area: &str, msg: impl AsRef<str>) {
    if let Some(d) = dir() {
        write_line(&d, "ERROR", area, msg.as_ref());
    }
}

/// A command line as it would be typed, for "this is what failed".
pub fn command_line(cmd: &Command) -> String {
    let quote = |s: &str| if s.contains([' ', '"']) || s.is_empty() { format!("\"{}\"", s.replace('"', "\\\"")) } else { s.to_string() };
    std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| quote(&a.to_string_lossy())).collect::<Vec<_>>().join(" ")
}

/// A program that failed: its command line and what it said.
pub fn command_failed(area: &str, cmd: &Command, detail: impl AsRef<str>) {
    error(area, format!("{}\n指令：{}", detail.as_ref().trim(), command_line(cmd)));
}

/// The last `n` lines across the newest log files, for pasting into a bug report.
pub fn tail(n: usize) -> String {
    let Some(d) = dir() else { return String::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&d).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    let mut lines: Vec<String> = Vec::new();
    for f in files.iter().rev() {
        let text = std::fs::read_to_string(f).unwrap_or_default();
        let mut these: Vec<String> = text.lines().map(str::to_string).collect();
        these.extend(lines);
        lines = these;
        if lines.len() >= n {
            break;
        }
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].join("\n")
}

/// Panics go to the log too, with where they happened.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error("panic", info.to_string());
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_hide_home_and_secrets_and_old_files_go() {
        let dir = std::env::temp_dir().join(format!("orangelink-log-{}", crate::model::new_id()));
        set_secrets(["df52bb127ba74508".to_string(), "short".to_string()]);
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| std::env::var("HOME").unwrap());
        write_line(&dir, "ERROR", "ffmpeg", &format!("{}\\Videos\\a.mp4 token df52bb127ba74508 short\nsecond", home.to_uppercase()));
        let file = std::fs::read_dir(&dir).unwrap().flatten().next().unwrap().path();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("[ERROR] ffmpeg: ~\\Videos\\a.mp4 token *** short\n    second"), "{text}");
        for day in 1..=9 {
            std::fs::write(dir.join(format!("orangelink-2000-01-0{day}.log")), "x").unwrap();
        }
        prune(&dir);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), KEEP_DAYS);
        assert!(file.exists(), "today's file is the newest");
        set_secrets([]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Needs no FFmpeg; tests log to a temporary folder (see `dir`).
    #[test]
    #[ignore]
    fn a_wrong_ffmpeg_folder_is_logged_with_its_command() {
        let bad = Path::new("C:/no-such-ffmpeg");
        let r = crate::signals::loudness(bad, Path::new("C:/clips/stream.mp4"), |_| true);
        assert!(r.is_err());
        println!("{}", tail(5));
        assert!(tail(5).contains("指令：C:/no-such-ffmpeg") && tail(5).contains("-i C:/clips/stream.mp4"));
    }

    #[test]
    fn command_lines_are_quoted() {
        let mut cmd = crate::process::hidden("ffmpeg");
        cmd.args(["-i", r"C:\My Videos\a.mp4", "-vf", "crop=608:ih:0:0"]);
        assert_eq!(command_line(&cmd), r#"ffmpeg -i "C:\My Videos\a.mp4" -vf crop=608:ih:0:0"#);
    }
}
