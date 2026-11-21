//! Extensions: programs on this computer that OrangeLink runs for an item and
//! whose answer it takes as highlight candidates. Each one is a folder with an
//! `orangelink-extension.json` manifest; nothing is downloaded or updated here.
//!
//! The manifest says what to run (an argument list, no shell). OrangeLink
//! writes the item into `request.json`, runs the program in the background
//! (the whole program tree is ended on cancel), shows the `@@ {json}` progress
//! lines it prints, and reads its `result.json`. Everything read from the
//! extension is checked here: a bad manifest is not loaded, a bad result is
//! refused whole, never clamped or half applied.
//!
//! A program runs only after the user trusted the manifest, and the manifest's
//! hash is checked again on every run: one changed since (a `git pull`) has to
//! be trusted again.

use crate::model::{is_bidi_control, MediaKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

pub const MANIFEST: &str = "orangelink-extension.json";
/// The manifest and result format this OrangeLink speaks.
pub const PROTOCOL: u32 = 1;

const MANIFEST_MAX_BYTES: u64 = 64 * 1024;
const RESULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const ID_MAX: usize = 40;
const LABEL_MAX: usize = 60;
const ARGS_MAX: usize = 32;
const ARG_MAX: usize = 1024;
const ACTIONS_MAX: usize = 16;
const OPTIONS_MAX: usize = 16;
pub const CLIPS_MAX: usize = 200;
pub const CLIP_NAME_MAX: usize = 80;
pub const CLIP_NOTE_MAX: usize = 2000;
/// Lines of the program's output kept for the log when it fails.
const TAIL_LINES: usize = 40;

/// Programs that may be named bare in `run` (found on PATH); anything else is
/// an `.exe` inside the extension's folder.
const INTERPRETERS: [&str; 4] = ["python", "python3", "py", "node"];
/// Whole arguments OrangeLink fills in; no other `{…}` is allowed.
const REQUEST: &str = "{request}";
const RESULT: &str = "{result}";

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub actions: Vec<ExtAction>,
    /// Checks the extension's own setup (installed packages and the like).
    pub check: Option<Vec<String>>,
    /// Opens the extension's own window or page, shown as a console the user can close.
    pub ui: Option<(String, Vec<String>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtAction {
    pub id: String,
    pub label: String,
    pub kinds: Vec<MediaKind>,
    pub min_duration: f64,
    pub run: Vec<String>,
    pub options: Vec<BoolOption>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoolOption {
    pub id: String,
    pub label: String,
    pub default: bool,
}

impl ExtAction {
    /// Whether the action can run on an item of this kind and length.
    pub fn applies(&self, kind: MediaKind, duration: Option<f64>) -> bool {
        self.kinds.contains(&kind) && (self.min_duration <= 0.0 || duration.is_some_and(|d| d >= self.min_duration))
    }
}

// The manifest as written; `Manifest::parse` checks it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    protocol: u32,
    id: String,
    name: String,
    #[serde(default)]
    version: String,
    actions: Vec<RawAction>,
    #[serde(default)]
    check: Option<Vec<String>>,
    #[serde(default)]
    ui: Option<RawUi>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    id: String,
    label: String,
    applies_to: RawApplies,
    run: Vec<String>,
    returns: String,
    #[serde(default)]
    options: Vec<RawOption>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawApplies {
    kinds: Vec<String>,
    #[serde(default)]
    min_duration_sec: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOption {
    id: String,
    label: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    default: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUi {
    label: String,
    run: Vec<String>,
}

/// Text shown in the interface: line breaks and tabs become spaces, other
/// control characters and direction overrides go, cut to `max` characters.
/// None when nothing is left.
pub fn clean_text(s: &str, max: usize) -> Option<String> {
    let s: String = s
        .chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if c.is_control() || is_bidi_control(c) => None,
            c => Some(c),
        })
        .collect();
    let s: String = s.trim().chars().take(max).collect();
    let s = s.trim_end();
    (!s.is_empty()).then(|| s.to_string())
}

/// Longer notes keep their line breaks.
fn clean_note(s: &str, max: usize) -> Option<String> {
    let s: String = s.chars().filter(|c| matches!(c, '\n' | '\t') || !(c.is_control() || is_bidi_control(*c))).take(max).collect();
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn check_id(what: &str, id: &str) -> Result<(), String> {
    let ok = !id.is_empty() && id.len() <= ID_MAX && !id.starts_with('-') && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(t!("{what}「{id}」只能用 a–z、0–9 和 -，最多 {ID_MAX} 個字元", what, id = clean_text(id, ID_MAX).unwrap_or_default(), ID_MAX))
    }
}

fn label(what: &str, s: &str) -> Result<String, String> {
    if s.chars().count() > LABEL_MAX {
        return Err(t!("{what}超過 {LABEL_MAX} 個字元", what, LABEL_MAX));
    }
    clean_text(s, LABEL_MAX).ok_or_else(|| t!("{what}是空的", what))
}

/// An argument list: the program first. `placeholders` are the `{…}` arguments
/// allowed here; each must be a whole argument.
fn check_args(what: &str, run: &[String], placeholders: &[&str]) -> Result<(), String> {
    if run.is_empty() || run.len() > ARGS_MAX {
        return Err(t!("{what}要有 1 到 {ARGS_MAX} 個參數", what, ARGS_MAX));
    }
    for a in run {
        if a.is_empty() || a.chars().count() > ARG_MAX || a.chars().any(|c| c.is_control() || is_bidi_control(c)) {
            return Err(t!("{what}有空的、太長或含控制字元的參數", what));
        }
        if (a.contains('{') || a.contains('}')) && !placeholders.contains(&a.as_str()) {
            return Err(t!("{what}的參數「{a}」不是可用的代入值（只能整個參數是 {list}）", what, a, list = placeholders.join("、")));
        }
    }
    for p in placeholders {
        if run.iter().filter(|a| a == p).count() > 1 {
            return Err(t!("{what}的 {p} 出現了不只一次", what, p));
        }
    }
    let program = &run[0];
    if INTERPRETERS.contains(&program.as_str()) {
        return Ok(());
    }
    let p = Path::new(program);
    let relative = p.is_relative() && p.components().all(|c| matches!(c, std::path::Component::Normal(_))) && !program.contains(':');
    let exe = p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !relative || !exe {
        return Err(t!("{what}的程式「{program}」要是 {list} 之一，或擴充資料夾裡的 .exe（相對路徑）", what, program, list = INTERPRETERS.join("、")));
    }
    Ok(())
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        // The protocol first, so a newer manifest says so instead of failing on a new field.
        let v: Value = serde_json::from_slice(bytes).map_err(|e| t!("{MANIFEST} 不是有效的 JSON：{e}", MANIFEST, e))?;
        match v.get("protocol").and_then(Value::as_u64) {
            Some(p) if p == PROTOCOL as u64 => {}
            Some(p) if p > PROTOCOL as u64 => return Err(t!("這個擴充需要較新版的 OrangeLink（擴充協定 {p}）", p)),
            _ => return Err(t!("{MANIFEST} 的 protocol 要是 {PROTOCOL}", MANIFEST, PROTOCOL)),
        }
        let raw: RawManifest = serde_json::from_value(v).map_err(|e| t!("{MANIFEST} 格式不對：{e}", MANIFEST, e))?;
        debug_assert_eq!(raw.protocol, PROTOCOL);
        check_id(t!("擴充 id"), &raw.id)?;
        let name = label(t!("名稱"), &raw.name)?;
        let version = clean_text(&raw.version, 20).unwrap_or_default();
        if raw.actions.is_empty() || raw.actions.len() > ACTIONS_MAX {
            return Err(t!("actions 要有 1 到 {ACTIONS_MAX} 個", ACTIONS_MAX));
        }
        let mut actions = Vec::new();
        for a in raw.actions {
            check_id(t!("動作 id"), &a.id)?;
            if actions.iter().any(|x: &ExtAction| x.id == a.id) {
                return Err(t!("動作 id「{id}」重複", id = a.id));
            }
            let what = t!("動作「{id}」", id = a.id);
            if a.returns != "clips" {
                return Err(t!("{what}的 returns 目前只支援 clips", what));
            }
            check_args(&what, &a.run, &[REQUEST, RESULT])?;
            if !a.run.iter().any(|x| x == REQUEST) || !a.run.iter().any(|x| x == RESULT) {
                return Err(t!("{what}的 run 要有 {REQUEST} 和 {RESULT}", what, REQUEST, RESULT));
            }
            let mut kinds = Vec::new();
            for k in &a.applies_to.kinds {
                kinds.push(match k.as_str() {
                    "video" => MediaKind::Video,
                    "audio" => MediaKind::Audio,
                    _ => return Err(t!("{what}的 kinds 只能是 video、audio", what)),
                });
            }
            if kinds.is_empty() {
                return Err(t!("{what}的 kinds 是空的", what));
            }
            let min = a.applies_to.min_duration_sec;
            if !(min.is_finite() && (0.0..=1e6).contains(&min)) {
                return Err(t!("{what}的 min_duration_sec 要在 0 到 1000000 之間", what));
            }
            if a.options.len() > OPTIONS_MAX {
                return Err(t!("{what}最多 {OPTIONS_MAX} 個選項", what, OPTIONS_MAX));
            }
            let mut options: Vec<BoolOption> = Vec::new();
            for o in a.options {
                check_id(t!("選項 id"), &o.id)?;
                if o.kind != "bool" || options.iter().any(|x| x.id == o.id) {
                    return Err(t!("{what}的選項「{id}」重複，或 type 不是 bool", what, id = o.id));
                }
                options.push(BoolOption { id: o.id, label: label(t!("選項名稱"), &o.label)?, default: o.default });
            }
            actions.push(ExtAction { id: a.id, label: label(t!("動作名稱"), &a.label)?, kinds, min_duration: min, run: a.run, options });
        }
        let check = match raw.check {
            Some(run) => {
                check_args("check", &run, &[])?;
                Some(run)
            }
            None => None,
        };
        let ui = match raw.ui {
            Some(u) => {
                check_args("ui", &u.run, &[])?;
                Some((label(t!("ui 名稱"), &u.label)?, u.run))
            }
            None => None,
        };
        Ok(Manifest { id: raw.id, name, version, actions, check, ui })
    }

    pub fn action(&self, id: &str) -> Option<&ExtAction> {
        self.actions.iter().find(|a| a.id == id)
    }
}

/// The manifest's bytes' SHA-256, to tell a trusted manifest from a changed one.
pub fn hash(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads and checks `<dir>/orangelink-extension.json`: the manifest and its hash.
pub fn load(dir: &Path) -> Result<(Manifest, String), String> {
    let path = dir.join(MANIFEST);
    let meta = std::fs::metadata(&path).map_err(|_| t!("資料夾裡沒有 {MANIFEST}", MANIFEST))?;
    if meta.len() > MANIFEST_MAX_BYTES {
        return Err(t!("{MANIFEST} 太大", MANIFEST));
    }
    let bytes = std::fs::read(&path).map_err(|e| t!("無法讀取 {MANIFEST}：{e}", MANIFEST, e))?;
    Ok((Manifest::parse(&bytes)?, hash(&bytes)))
}

/// The command for `run`, inside the extension's folder `dir`: placeholders
/// filled in, a program in the folder made absolute, and checked to really be
/// inside it (`..`, links and `\\?\` spellings included).
pub fn command(dir: &Path, run: &[String], fill: &[(&str, &Path)], visible: bool) -> Result<std::process::Command, String> {
    let make = |p: &std::ffi::OsStr| if visible { crate::process::visible(p) } else { crate::process::hidden(p) };
    let root = crate::importer::dunce_abs(dir).map_err(|e| t!("找不到擴充資料夾：{e}", e))?;
    let program = &run[0];
    let mut cmd = if INTERPRETERS.contains(&program.as_str()) {
        make(program.as_ref())
    } else {
        let exe = crate::importer::dunce_abs(&root.join(program)).map_err(|_| t!("找不到程式「{program}」", program))?;
        if !exe.starts_with(&root) || !exe.is_file() {
            return Err(t!("程式「{program}」不在擴充資料夾裡", program));
        }
        make(exe.as_os_str())
    };
    for a in &run[1..] {
        match fill.iter().find(|(k, _)| k == a) {
            Some((_, v)) => cmd.arg(v),
            None => cmd.arg(a),
        };
    }
    cmd.current_dir(&root).env("ORANGELINK_PROTOCOL", PROTOCOL.to_string()).env("PYTHONIOENCODING", "utf-8").env("PYTHONUTF8", "1");
    Ok(cmd)
}

/// What OrangeLink tells the extension about the item.
#[derive(Clone, Debug, Serialize)]
pub struct RequestItem {
    pub id: String,
    pub name: String,
    pub kind: MediaKind,
    /// The media file to read.
    pub path: PathBuf,
    /// Where it was imported from, when that file is still there.
    pub original: Option<PathBuf>,
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub chat_file: Option<PathBuf>,
    pub subtitles: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Request {
    pub protocol: u32,
    pub action: String,
    pub options: std::collections::BTreeMap<String, bool>,
    pub item: RequestItem,
    /// Where the extension may keep its work (analysis, renders): next to the
    /// original, else in the library's `extensions` folder. A suggestion.
    pub workdir: PathBuf,
}

/// A candidate the extension proposes, checked.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub start: f64,
    pub end: f64,
    pub title: Option<String>,
    pub note: Option<String>,
    pub score: Option<f32>,
    pub crop_x: Option<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResult {
    protocol: u32,
    clips: Vec<RawClip>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClip {
    start: f64,
    end: f64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    crop_x: Option<f64>,
}

/// What the extension answered: the candidates, and a line it wants shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub clips: Vec<Proposal>,
    pub message: Option<String>,
}

/// Checks `result.json` against the item's length (None: unknown). Any clip
/// out of range refuses the whole answer, naming the clip.
pub fn parse_result(bytes: &[u8], duration: Option<f64>) -> Result<Answer, String> {
    let raw: RawResult = serde_json::from_slice(bytes).map_err(|e| t!("result.json 格式不對：{e}", e))?;
    if raw.protocol != PROTOCOL {
        return Err(t!("result.json 的 protocol 要是 {PROTOCOL}", PROTOCOL));
    }
    if raw.clips.len() > CLIPS_MAX {
        return Err(t!("片段太多（{n} 個，最多 {CLIPS_MAX} 個）", n = raw.clips.len(), CLIPS_MAX));
    }
    let length = duration.filter(|d| d.is_finite() && *d > 0.0);
    let mut clips = Vec::new();
    for (n, c) in raw.clips.into_iter().enumerate() {
        let at = |why: &str| t!("第 {n} 個片段：{why}", n = n + 1, why);
        if !(c.start.is_finite() && c.end.is_finite() && c.start >= 0.0 && c.start < c.end) {
            return Err(at(t!("時間不對（要 0 ≤ start < end）")));
        }
        if let Some(len) = length {
            if c.end > len + 0.5 {
                return Err(at(&t!("超過影片長度（{end} 秒 > {len} 秒）", end = format!("{:.1}", c.end), len = format!("{len:.1}"))));
            }
        }
        let score = match c.score {
            Some(s) if !(s.is_finite() && s.abs() <= 1e6) => return Err(at(t!("score 不對"))),
            s => s.map(|s| s as f32),
        };
        let crop_x = match c.crop_x {
            Some(x) if !(x.is_finite() && (0.0..=1.0).contains(&x)) => return Err(at(t!("crop_x 要在 0 到 1 之間"))),
            x => x.map(|x| x as f32),
        };
        if c.name.as_ref().is_some_and(|s| s.chars().count() > CLIP_NAME_MAX) || c.note.as_ref().is_some_and(|s| s.chars().count() > CLIP_NOTE_MAX) {
            return Err(at(&t!("名稱超過 {CLIP_NAME_MAX} 字或註解超過 {CLIP_NOTE_MAX} 字", CLIP_NAME_MAX, CLIP_NOTE_MAX)));
        }
        // Up to half a second past the end is rounding; kept inside the media.
        let end = length.map_or(c.end, |len| c.end.min(len));
        if end <= c.start {
            return Err(at(t!("時間不對（要 0 ≤ start < end）")));
        }
        clips.push(Proposal {
            start: c.start,
            end,
            title: c.name.as_deref().and_then(|s| clean_text(s, CLIP_NAME_MAX)),
            note: c.note.as_deref().and_then(|s| clean_note(s, CLIP_NOTE_MAX)),
            score,
            crop_x,
        });
    }
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(Answer { clips, message: raw.message.as_deref().and_then(|s| clean_text(s, 200)) })
}

/// A progress line: `@@ {"progress": 0.4, "message": "…"}`, or the step lines
/// some tools already print (`{"step": "transcribe", "status": "start"}`).
/// None for any other line.
pub fn parse_progress(line: &str) -> Option<(Option<f32>, Option<String>)> {
    let v: Value = serde_json::from_str(line.strip_prefix("@@ ")?).ok()?;
    let v = v.as_object()?;
    let progress = v.get("progress").and_then(Value::as_f64).filter(|p| p.is_finite()).map(|p| p.clamp(0.0, 1.0) as f32);
    let message = match (v.get("message").and_then(Value::as_str), v.get("step").and_then(Value::as_str)) {
        (Some(m), _) => clean_text(m, 120),
        (None, Some(step)) => clean_text(&format!("{step} {}", v.get("status").and_then(Value::as_str).unwrap_or("")), 120),
        _ => None,
    };
    Some((progress, message))
}

/// Progress seen so far, shared with the status bar.
#[derive(Default)]
pub struct Progress {
    pub fraction: Option<f32>,
    pub message: Option<String>,
}

/// Everything a run needs, gathered on the UI thread.
pub struct Job {
    pub dir: PathBuf,
    /// The hash the user trusted; a manifest that differs now does not run.
    pub trusted: String,
    pub action: String,
    pub request: Request,
}

/// Runs the action to the end (or until `cancel`): checks the manifest is the
/// trusted one, writes the request, runs the program, reads its answer.
/// `Ok(None)` when cancelled.
pub fn run(job: &Job, cancel: &AtomicBool, progress: &std::sync::Arc<std::sync::Mutex<Progress>>) -> Result<Option<Answer>, String> {
    let manifest = trusted(&job.dir, &job.trusted)?;
    let action = manifest.action(&job.action).ok_or_else(|| t!("擴充沒有「{action}」這個動作了", action = job.action))?;
    let tmp = std::env::temp_dir().join(format!("orangelink-ext-{}", crate::model::new_id()));
    std::fs::create_dir_all(&tmp).map_err(|e| t!("無法建立暫存資料夾：{e}", e))?;
    let result = (|| {
        let request = tmp.join("request.json");
        let answer = tmp.join("result.json");
        let body = serde_json::to_vec_pretty(&job.request).map_err(|e| e.to_string())?;
        std::fs::write(&request, body).map_err(|e| t!("無法寫入 request.json：{e}", e))?;
        let mut cmd = command(&job.dir, &action.run, &[(REQUEST, &request), (RESULT, &answer)], false)?;
        let tail = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let (t, p) = (tail.clone(), progress.clone());
        let status = crate::process::tree_or_cancel(&mut cmd, cancel, move |line| {
            if let Some((fraction, message)) = parse_progress(line) {
                let mut p = crate::lock(&p);
                if fraction.is_some() {
                    p.fraction = fraction;
                }
                if message.is_some() {
                    p.message = message;
                }
            } else if let Some(line) = clean_text(line, 300) {
                let mut t = crate::lock(&t);
                if t.len() == TAIL_LINES {
                    t.pop_front();
                }
                t.push_back(line);
            }
        })
        .map_err(|e| t!("無法執行「{program}」：{e}", program = action.run[0], e))?;
        let Some(status) = status else { return Ok(None) };
        let tail: Vec<String> = crate::lock(&tail).iter().cloned().collect();
        if !status.success() {
            let last = tail.last().cloned().unwrap_or_default();
            crate::log::error("extension", format!("{} 結束代碼 {:?}：\n{}", manifest.id, status.code(), tail.join("\n")));
            return Err(t!("擴充結束代碼 {code}：{last}", code = status.code().map_or("?".to_string(), |c| c.to_string()), last));
        }
        let size = std::fs::metadata(&answer).map_err(|_| "擴充沒有寫出 result.json".to_string())?.len();
        if size > RESULT_MAX_BYTES {
            return Err("result.json 太大".to_string());
        }
        let bytes = std::fs::read(&answer).map_err(|e| t!("無法讀取 result.json：{e}", e))?;
        parse_result(&bytes, job.request.item.duration).map(Some)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The manifest in `dir`, read now, if it is still the one trusted (`hash`).
/// Checked right before anything runs: the file may have changed since it was read.
pub fn trusted(dir: &Path, hash: &str) -> Result<Manifest, String> {
    let (manifest, now) = load(dir)?;
    if now != hash {
        return Err(t!("擴充的設定檔改過了，請到設定 →「擴充套件」重新確認").to_string());
    }
    Ok(manifest)
}

/// Runs the trusted manifest's `check` and returns whether it passed and what it printed.
pub fn run_check(dir: &Path, hash: &str, cancel: &AtomicBool) -> Result<(bool, String), String> {
    let manifest = trusted(dir, hash)?;
    let Some(run) = manifest.check else { return Ok((true, String::new())) };
    let run = &run[..];
    let mut cmd = command(dir, run, &[], false)?;
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l = lines.clone();
    let status = crate::process::tree_or_cancel(&mut cmd, cancel, move |line| {
        let mut l = crate::lock(&l);
        if l.len() < 200 {
            l.extend(clean_text(line, 300));
        }
    })
    .map_err(|e| t!("無法執行「{program}」：{e}", program = run[0], e))?;
    let text = crate::lock(&lines).join("\n");
    Ok((status.is_some_and(|s| s.success()), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(action: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "protocol": 1, "id": "demo", "name": "Demo", "actions": [action] })).unwrap()
    }

    fn action() -> Value {
        serde_json::json!({ "id": "find", "label": "找精華", "applies_to": { "kinds": ["video"], "min_duration_sec": 600 },
                            "run": ["python", "adapter.py", "{request}", "{result}"], "returns": "clips",
                            "options": [{ "id": "ai", "label": "AI", "type": "bool", "default": true }] })
    }

    fn with(key: &str, v: Value) -> Vec<u8> {
        let mut a = action();
        a[key] = v;
        manifest(a)
    }

    #[test]
    fn a_good_manifest_is_read() {
        let m = Manifest::parse(&manifest(action())).unwrap();
        assert_eq!(m.id, "demo");
        let a = m.action("find").unwrap();
        assert_eq!(a.kinds, [MediaKind::Video]);
        assert_eq!(a.options[0], BoolOption { id: "ai".into(), label: "AI".into(), default: true });
        assert!(a.applies(MediaKind::Video, Some(3600.0)));
        assert!(!a.applies(MediaKind::Video, Some(60.0)) && !a.applies(MediaKind::Video, None) && !a.applies(MediaKind::Audio, Some(3600.0)));
    }

    #[test]
    fn manifests_that_could_run_something_else_are_refused() {
        for run in [
            serde_json::json!(["cmd", "/c", "{request}", "{result}"]),
            serde_json::json!(["C:\\Windows\\System32\\cmd.exe", "{request}", "{result}"]),
            serde_json::json!(["..\\other\\tool.exe", "{request}", "{result}"]),
            serde_json::json!(["bin/../../tool.exe", "{request}", "{result}"]),
            serde_json::json!(["\\\\?\\C:\\tool.exe", "{request}", "{result}"]),
            serde_json::json!(["run.bat", "{request}", "{result}"]),
            serde_json::json!(["python", "x.py --out={result}", "{request}"]),
            serde_json::json!(["python", "{request}", "{result}", "{result}"]),
            serde_json::json!(["python", "{workdir}", "{request}", "{result}"]),
            serde_json::json!(["python", "a\nb", "{request}", "{result}"]),
            serde_json::json!(["python", "x.py"]),
            serde_json::json!([]),
        ] {
            assert!(Manifest::parse(&with("run", run.clone())).is_err(), "{run}");
        }
        assert!(Manifest::parse(&with("run", serde_json::json!(["bin\\tool.exe", "{request}", "{result}"]))).is_ok());
    }

    #[test]
    fn manifests_with_wrong_types_or_values_are_errors_not_guesses() {
        assert!(Manifest::parse(&with("returns", "files".into())).is_err());
        assert!(Manifest::parse(&with("applies_to", serde_json::json!({ "kinds": ["image"] }))).is_err());
        assert!(Manifest::parse(&with("applies_to", serde_json::json!({ "kinds": ["video"], "min_duration_sec": -1 }))).is_err());
        assert!(Manifest::parse(&with("applies_to", serde_json::json!({ "kinds": ["video"], "min_duration_sec": "600" }))).is_err());
        assert!(Manifest::parse(&with("id", "Find Me".into())).is_err());
        assert!(Manifest::parse(&with("label", "\u{202E}\u{200F}".into())).is_err());
        assert!(Manifest::parse(&with("label", "長".repeat(61).into())).is_err());
        assert!(Manifest::parse(&with("unknown", 1.into())).is_err());
        assert!(Manifest::parse(&with("options", serde_json::json!([{ "id": "n", "label": "N", "type": "number" }]))).is_err());
        assert!(Manifest::parse(b"not json").is_err());
        assert!(Manifest::parse(br#"{"id":"demo"}"#).is_err());
        let newer = Manifest::parse(br#"{"protocol":2,"whatever":true}"#).unwrap_err();
        assert!(newer.contains("較新版"), "{newer}");
    }

    #[test]
    fn names_lose_control_and_direction_characters() {
        let m = Manifest::parse(&with("label", "找\u{202E}精\u{0007}華\n".into())).unwrap();
        assert_eq!(m.actions[0].label, "找精華");
    }

    #[test]
    fn a_result_is_checked_against_the_media() {
        let ok = br#"{"protocol":1,"clips":[{"start":120,"end":150.4,"name":"\u202e Boss","note":"chat x3","score":8.5,"crop_x":0.4},{"start":10,"end":20}]}"#;
        let a = parse_result(ok, Some(600.0)).unwrap();
        assert_eq!(a.clips.len(), 2);
        // In time order.
        assert_eq!((a.clips[0].start, a.clips[1].start), (10.0, 120.0));
        assert_eq!(a.clips[1].title.as_deref(), Some("Boss"));
        assert_eq!(a.clips[1].crop_x, Some(0.4));
        for bad in [
            r#"{"protocol":1,"clips":[{"start":590,"end":700}]}"#,
            r#"{"protocol":1,"clips":[{"start":30,"end":30}]}"#,
            r#"{"protocol":1,"clips":[{"start":-1,"end":30}]}"#,
            r#"{"protocol":1,"clips":[{"start":"10","end":30}]}"#,
            r#"{"protocol":1,"clips":[{"start":10,"end":30,"crop_x":1.5}]}"#,
            r#"{"protocol":1,"clips":[{"start":10,"end":30,"score":1e300}]}"#,
            r#"{"protocol":1,"clips":[{"in":10,"out":30}]}"#,
            r#"{"protocol":2,"clips":[]}"#,
            r#"{"clips":[]}"#,
            "",
        ] {
            assert!(parse_result(bad.as_bytes(), Some(600.0)).is_err(), "{bad}");
        }
        // Rounding past the end is kept inside it; an unknown length checks only the order.
        assert_eq!(parse_result(br#"{"protocol":1,"clips":[{"start":590,"end":600.3}]}"#, Some(600.0)).unwrap().clips[0].end, 600.0);
        assert!(parse_result(br#"{"protocol":1,"clips":[{"start":5000,"end":5010}]}"#, None).is_ok());
        let long = format!(r#"{{"protocol":1,"clips":[{{"start":1,"end":2,"name":"{}"}}]}}"#, "名".repeat(CLIP_NAME_MAX + 1));
        assert!(parse_result(long.as_bytes(), Some(600.0)).is_err());
        let many = format!(r#"{{"protocol":1,"clips":[{}]}}"#, vec![r#"{"start":1,"end":2}"#; CLIPS_MAX + 1].join(","));
        assert!(parse_result(many.as_bytes(), Some(600.0)).is_err());
    }

    #[test]
    fn progress_lines_are_read_and_other_lines_are_not() {
        assert_eq!(parse_progress(r#"@@ {"progress":0.25,"message":"轉錄中"}"#), Some((Some(0.25), Some("轉錄中".into()))));
        assert_eq!(parse_progress(r#"@@ {"step":"transcribe","status":"start"}"#), Some((None, Some("transcribe start".into()))));
        assert_eq!(parse_progress(r#"@@ {"progress":7}"#), Some((Some(1.0), None)));
        assert_eq!(parse_progress("loading model"), None);
        assert_eq!(parse_progress("@@ not json"), None);
        assert_eq!(parse_progress("@@ [1,2]"), None);
    }

    #[test]
    fn a_program_outside_the_extension_folder_is_not_run() {
        let root = std::env::temp_dir().join(format!("orangelink-ext-test-{}", crate::model::new_id()));
        let ext = root.join("ext");
        std::fs::create_dir_all(ext.join("bin")).unwrap();
        std::fs::write(root.join("outside.exe"), b"").unwrap();
        std::fs::write(ext.join("bin").join("tool.exe"), b"").unwrap();
        let run = |p: &str| command(&ext, &[p.to_string()], &[], false).map(|_| ());
        assert!(run("bin/tool.exe").is_ok());
        // `..` is refused by the manifest check; a link or a spelling that gets past
        // it must still resolve inside the folder.
        assert!(run("../outside.exe").is_err());
        assert!(run("bin/missing.exe").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("outside.exe"), ext.join("link.exe")).unwrap();
            assert!(run("link.exe").is_err());
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_changed_manifest_does_not_run() {
        let dir = std::env::temp_dir().join(format!("orangelink-ext-test-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), manifest(action())).unwrap();
        let (_, trusted) = load(&dir).unwrap();
        // Changed after it was trusted (say by a `git pull`).
        std::fs::write(dir.join(MANIFEST), with("run", serde_json::json!(["python", "other.py", "{request}", "{result}"]))).unwrap();
        let item = RequestItem { id: "i".into(), name: "n".into(), kind: MediaKind::Video, path: dir.join("v.mp4"), original: None, duration: Some(4000.0), width: None, height: None, chat_file: None, subtitles: None };
        let job = Job { dir: dir.clone(), trusted, action: "find".into(), request: Request { protocol: PROTOCOL, action: "find".into(), options: Default::default(), item, workdir: dir.clone() } };
        let err = run(&job, &AtomicBool::new(false), &Default::default()).unwrap_err();
        assert!(err.contains("改過"), "{err}");
        // Nor does its environment check.
        let err = run_check(&dir, &job.trusted, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("改過"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
