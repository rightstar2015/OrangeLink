//! Extensions in the app: the manifests of the folders the user added, runs
//! queued for items (one at a time: they are heavy), their progress in the
//! status bar, and their answers made into highlight candidates.

use super::{Action, App, Confirmed, Dialog, MediaTask};
use crate::extension::{self, Answer, Manifest};
use crate::model::MediaKind;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver},
        Arc, Mutex,
    },
};

/// An added folder and what was read from it.
pub struct Installed {
    pub dir: PathBuf,
    /// The manifest and its hash, or why it could not be read.
    pub manifest: Result<(Manifest, String), String>,
}

/// A run asked for: this action of the extension in `dir`, on this item.
#[derive(Clone, PartialEq)]
pub struct Task {
    pub dir: PathBuf,
    pub action: String,
    pub item: String,
}

pub struct ExtRun {
    pub task: Task,
    /// `<extension>：<action>` and the item's name, for the jobs list.
    pub label: String,
    pub item_name: String,
    rx: Receiver<Result<Option<Answer>, String>>,
    pub progress: Arc<Mutex<extension::Progress>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for ExtRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// The manifest's `check`, run from the settings.
pub struct CheckRun {
    pub dir: PathBuf,
    rx: Receiver<Result<(bool, String), String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for CheckRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Extensions {
    pub list: Vec<Installed>,
    queue: VecDeque<Task>,
    pub run: Option<ExtRun>,
    pub check: Option<CheckRun>,
    /// The last check's folder, whether it passed, and what it printed.
    pub check_result: Option<(PathBuf, bool, String)>,
}

/// Where an extension may keep its work for an item: next to the original
/// (`<name>_<extension id>`), else in the library's `extensions` folder.
pub fn workdir(original: Option<&std::path::Path>, library: &std::path::Path, ext_id: &str, item_id: &str) -> PathBuf {
    match original.and_then(|o| Some((o.parent()?, o.file_stem()?))) {
        Some((dir, stem)) => dir.join(format!("{}_{ext_id}", stem.to_string_lossy())),
        None => library.join("extensions").join(ext_id).join(item_id),
    }
}

/// The clips an answer becomes: candidates numbered in time order, their notes
/// saying which extension proposed them.
pub fn candidates(answer: &Answer, from: &str) -> Vec<crate::model::Clip> {
    answer
        .clips
        .iter()
        .enumerate()
        .map(|(n, p)| crate::model::Clip {
            id: crate::model::new_id(),
            name: format!("{}{:02}", super::highlights::CANDIDATE, n + 1),
            start: p.start,
            end: p.end,
            crop_x: p.crop_x,
            note: Some(match &p.note {
                Some(note) => format!("{from}：{note}"),
                None => from.to_string(),
            }),
            score: p.score,
            title: p.title.clone(),
            ..Default::default()
        })
        .collect()
}

/// The manifest of an extension trusted as it is now and turned on.
fn usable<'a>(config: &crate::config::Config, i: &'a Installed) -> Option<&'a Manifest> {
    let (m, hash) = i.manifest.as_ref().ok()?;
    let entry = config.extensions.iter().find(|e| e.dir == i.dir)?;
    (entry.enabled && entry.trusted.as_deref() == Some(hash.as_str())).then_some(m)
}

impl App {
    /// Reads the manifests of the added folders again.
    pub fn load_extensions(&mut self) {
        self.ext.list = self.config.extensions.iter().map(|e| Installed { dir: e.dir.clone(), manifest: extension::load(&e.dir) }).collect();
    }


    /// The actions offered for an item: (folder, action id, `extension：action`).
    pub fn extension_actions(&self, kind: MediaKind, duration: Option<f64>) -> Vec<(PathBuf, String, String)> {
        let mut out = Vec::new();
        for i in &self.ext.list {
            let Some(m) = usable(&self.config, i) else { continue };
            for a in m.actions.iter().filter(|a| a.applies(kind, duration)) {
                out.push((i.dir.clone(), a.id.clone(), format!("{}：{}", m.name, a.label)));
            }
        }
        out
    }

    /// Whether this item has a run under way or waiting.
    pub fn extension_busy(&self, id: &str) -> bool {
        self.ext.run.as_ref().is_some_and(|r| r.task.item == id) || self.ext.queue.iter().any(|t| t.item == id)
    }

    /// Queues the action for these items (the ones it applies to).
    pub(super) fn run_extension(&mut self, dir: PathBuf, action: String, ids: Vec<String>) {
        let Some(lib) = &self.lib else { return };
        let Some(m) = self.ext.list.iter().find(|i| i.dir == dir).and_then(|i| usable(&self.config, i)) else {
            return self.toast(t!("這個擴充還沒有信任或已停用，請到設定 →「擴充套件」確認"));
        };
        let Some(a) = m.action(&action) else { return };
        let label = a.label.clone();
        let mut n = 0;
        for id in ids {
            let fits = lib.items.get(&id).is_some_and(|it| a.applies(it.kind, it.duration));
            let task = Task { dir: dir.clone(), action: action.clone(), item: id };
            if fits && !self.ext.queue.contains(&task) && self.ext.run.as_ref().is_none_or(|r| r.task != task) {
                self.ext.queue.push_back(task);
                n += 1;
            }
        }
        if n == 0 {
            return self.toast(t!("「{label}」不適用選取的項目（類型或長度不符）", label));
        }
        self.start_next_extension();
        self.toast(t!("「{label}」：{n} 個項目排入背景工作", label, n));
    }

    fn start_next_extension(&mut self) {
        if self.ext.run.is_some() {
            return;
        }
        let Some(task) = self.ext.queue.pop_front() else { return };
        let Some(lib) = &self.lib else { return };
        let (Some(it), Some(m)) = (lib.items.get(&task.item), self.ext.list.iter().find(|i| i.dir == task.dir).and_then(|i| usable(&self.config, i))) else {
            return self.start_next_extension();
        };
        let Some(a) = m.action(&task.action) else { return self.start_next_extension() };
        let Some(entry) = self.config.extensions.iter().find(|e| e.dir == task.dir) else { return self.start_next_extension() };
        let options = a.options.iter().map(|o| (o.id.clone(), entry.options.get(&format!("{}.{}", a.id, o.id)).copied().unwrap_or(o.default))).collect();
        let item = extension::RequestItem {
            id: it.id.clone(),
            name: it.name.clone(),
            kind: it.kind,
            path: lib.file_path(it),
            original: it.imported_from().cloned(),
            duration: it.duration,
            width: (it.width > 0).then_some(it.width),
            height: (it.height > 0).then_some(it.height),
            chat_file: it.chat_file(),
            subtitles: lib.find_subtitle(&it.id),
        };
        let (ext_id, root) = (m.id.clone(), lib.root.clone());
        let label = format!("{}：{}", m.name, a.label);
        let item_name = it.name.clone();
        let trusted = entry.trusted.clone().unwrap_or_default();
        let job = extension::Job {
            dir: task.dir.clone(),
            trusted,
            action: task.action.clone(),
            request: extension::Request { protocol: extension::PROTOCOL, action: task.action.clone(), options, workdir: PathBuf::new(), item },
        };
        let (tx, rx) = channel();
        let progress = Arc::new(Mutex::new(extension::Progress::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (p, c, ctx) = (progress.clone(), cancel.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let mut job = job;
            // Looked at here, off the UI thread: the original may be on a slow drive.
            job.request.item.original = job.request.item.original.filter(|o| o.is_file());
            job.request.workdir = workdir(job.request.item.original.as_deref(), &root, &ext_id, &job.request.item.id);
            let tick = ctx.clone();
            let watcher = std::thread::spawn({
                let c = c.clone();
                move || {
                    // Repaint now and then so the status bar shows the progress.
                    while !c.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        tick.request_repaint();
                    }
                }
            });
            let result = extension::run(&job, &c, &p);
            let _ = tx.send(result);
            c.store(true, Ordering::Relaxed);
            let _ = watcher.join();
            ctx.request_repaint();
        });
        self.ext.run = Some(ExtRun { task, label, item_name, rx, progress, cancel });
    }

    pub(super) fn poll_extension(&mut self) {
        let Some(run) = &self.ext.run else { return };
        let reply = super::state::poll(&run.rx);
        if matches!(reply, super::Reply::Pending) {
            return;
        }
        let Some(run) = self.ext.run.take() else { return };
        let (task, label, name) = (run.task.clone(), run.label.clone(), run.item_name.clone());
        drop(run);
        match reply {
            super::Reply::Pending => {}
            super::Reply::Lost => {
                crate::log::error("extension", format!("{label}「{name}」：{}", crate::i18n::tr(super::state::LOST)));
                self.toast_error(t!("「{name}」的「{label}」失敗：{state}", name, label, state = crate::i18n::tr(super::state::LOST)));
            }
            super::Reply::Done(Err(e)) => {
                crate::log::error("extension", format!("{label}「{name}」：{e}"));
                self.toast_error(t!("「{name}」的「{label}」失敗：{state}", name, label, state = e));
            }
            // Cancelled.
            super::Reply::Done(Ok(None)) => {}
            super::Reply::Done(Ok(Some(answer))) => self.apply_answer(&task, &label, &name, &answer),
        }
        self.start_next_extension();
    }

    /// The answer's clips replace the item's unkept candidates; one undo step.
    fn apply_answer(&mut self, task: &Task, label: &str, name: &str, answer: &Answer) {
        if !self.lib.as_ref().is_some_and(|l| l.items.contains_key(&task.item)) {
            return;
        }
        let message = answer.message.as_ref().map_or(String::new(), |m| format!("（{m}）"));
        if answer.clips.is_empty() {
            return self.toast(t!("「{name}」：「{label}」沒有找到候選{message}", name, label, message));
        }
        let clips = candidates(answer, label);
        let n = clips.len();
        self.edit_items(std::slice::from_ref(&task.item), |it| {
            it.clips.retain(|c| !super::highlights::is_candidate(&c.name));
            it.clips.extend(clips.iter().cloned());
            it.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        });
        self.toast_with(t!("「{name}」：「{label}」建立了 {n} 個精華候選{message}", name, label, n, message), t!("查看"), Action::Media(task.item.clone(), MediaTask::Open));
    }

    /// The run under way, for the status bar's jobs list.
    pub(super) fn extension_job(&self) -> Option<super::jobs::BgJob> {
        let run = self.ext.run.as_ref()?;
        let p = crate::lock(&run.progress);
        let mut detail = p.message.clone().unwrap_or_else(|| t!("執行中…").to_string());
        let left = self.ext.queue.len();
        if left > 0 {
            detail += &t!("（還有 {left} 個）", left);
        }
        Some(super::jobs::BgJob::new(super::jobs::JobKind::Extension, format!("{}：{}", run.label, run.item_name), detail, p.fraction))
    }

    /// Stops the run under way (its program tree is ended) and drops the queue.
    pub(super) fn cancel_extensions(&mut self) {
        self.ext.queue.clear();
        self.ext.run = None;
    }

    /// Adds the extension in a folder the user picked, then asks to trust it.
    pub(super) fn add_extension(&mut self, dir: PathBuf) {
        let dir = crate::importer::dunce_abs(&dir).unwrap_or(dir);
        if self.config.extensions.iter().any(|e| e.dir == dir) {
            return self.toast(t!("這個資料夾已經加入了"));
        }
        let (m, _) = match extension::load(&dir) {
            Ok(x) => x,
            Err(e) => return self.toast_error(t!("無法加入擴充：{e}", e)),
        };
        if self.ext.list.iter().any(|i| i.manifest.as_ref().is_ok_and(|(o, _)| o.id == m.id)) {
            return self.toast_error(t!("已經有 id 是「{id}」的擴充", id = m.id));
        }
        self.config.extensions.push(crate::config::ExtensionEntry { dir: dir.clone(), trusted: None, enabled: true, options: Default::default() });
        self.config.save();
        self.load_extensions();
        self.ask_trust_extension(&dir);
    }

    /// Shows what the extension will run and asks to trust it.
    pub fn ask_trust_extension(&mut self, dir: &std::path::Path) {
        let Some(i) = self.ext.list.iter().find(|i| i.dir == dir) else { return };
        let Ok((m, hash)) = &i.manifest else { return };
        let mut lines = vec![t!("「{name}」{version}\n資料夾：{dir}", name = m.name, version = m.version, dir = dir.display())];
        for a in &m.actions {
            lines.push(t!("・{label}：{cmd}", label = a.label, cmd = a.run.join(" ")));
        }
        if let Some(run) = &m.check {
            lines.push(t!("・檢查環境：{cmd}", cmd = run.join(" ")));
        }
        if let Some((label, run)) = &m.ui {
            lines.push(t!("・{label}：{cmd}", label, cmd = run.join(" ")));
        }
        let message = t!("要信任這個擴充嗎？\n\n{list}\n\n信任後，你在選單按下時 OrangeLink 會用你的帳號執行上面的程式。它們能讀寫這台電腦上的檔案，請只加入你自己寫的或信任的擴充。它讀得到的項目資料：影片檔、聊天室紀錄、字幕的路徑；它的結果只會變成精華候選，不會直接改資源庫。", list = lines.join("\n"));
        self.dialog = Some(Dialog::Confirm { message, on_yes: Confirmed::TrustExtension(dir.to_path_buf(), hash.clone()) });
    }

    pub(super) fn trust_extension(&mut self, dir: PathBuf, hash: String) {
        if let Some(e) = self.config.extensions.iter_mut().find(|e| e.dir == dir) {
            e.trusted = Some(hash);
            e.enabled = true;
            self.config.save();
            self.toast(t!("已信任擴充；在影片的右鍵選單「影音」或「⚡ 精華」分頁使用"));
        }
    }

    pub(super) fn remove_extension(&mut self, dir: &std::path::Path) {
        if self.ext.run.as_ref().is_some_and(|r| r.task.dir == dir) {
            self.ext.run = None;
        }
        self.ext.queue.retain(|t| t.dir != dir);
        self.config.extensions.retain(|e| e.dir != dir);
        self.config.save();
        self.load_extensions();
    }

    /// Runs the manifest's `check` in the background; the settings show the answer.
    pub(super) fn check_extension(&mut self, dir: PathBuf) {
        // Only a trusted extension's check runs: it is the extension's own code too.
        if !self.ext.list.iter().find(|i| i.dir == dir).is_some_and(|i| usable(&self.config, i).is_some()) {
            return;
        }
        let Some(hash) = self.config.extensions.iter().find(|e| e.dir == dir).and_then(|e| e.trusted.clone()) else { return };
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (c, d, ctx) = (cancel.clone(), dir.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(extension::run_check(&d, &hash, &c));
            ctx.request_repaint();
        });
        self.ext.check_result = None;
        self.ext.check = Some(CheckRun { dir, rx, cancel });
    }

    pub(super) fn poll_extension_check(&mut self) {
        let Some(check) = &self.ext.check else { return };
        let result = match super::state::poll(&check.rx) {
            super::Reply::Pending => return,
            super::Reply::Done(r) => r,
            super::Reply::Lost => Err(crate::i18n::tr(super::state::LOST).to_string()),
        };
        let dir = check.dir.clone();
        self.ext.check = None;
        self.ext.check_result = Some(match result {
            Ok((ok, text)) => (dir, ok, text),
            Err(e) => (dir, false, e),
        });
    }

    /// Opens the extension's own window (a console the user closes to stop it).
    pub(super) fn open_extension_ui(&mut self, dir: PathBuf) {
        if !self.ext.list.iter().find(|i| i.dir == dir).is_some_and(|i| usable(&self.config, i).is_some()) {
            return;
        }
        let Some(hash) = self.config.extensions.iter().find(|e| e.dir == dir).and_then(|e| e.trusted.clone()) else { return };
        let started = extension::trusted(&dir, &hash).and_then(|m| {
            let (_, run) = m.ui.ok_or_else(|| t!("這個擴充沒有介面").to_string())?;
            extension::command(&dir, &run, &[], true)?.spawn().map(|_| ()).map_err(|e| e.to_string())
        });
        match started {
            Ok(()) => {}
            Err(e) => self.toast_error(t!("無法開啟：{e}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn work_goes_next_to_the_original_or_into_the_library() {
        let lib = Path::new("L");
        assert_eq!(workdir(Some(Path::new("D/20260101/直播.mp4")), lib, "demo", "i1"), Path::new("D/20260101/直播_demo"));
        assert_eq!(workdir(None, lib, "demo", "i1"), Path::new("L/extensions/demo/i1"));
    }

    #[test]
    fn an_answer_becomes_numbered_candidates_that_say_where_they_came_from() {
        let answer = Answer {
            clips: vec![
                extension::Proposal { start: 10.0, end: 20.0, title: Some("開場".into()), note: Some("聊天 ×3".into()), score: Some(2.0), crop_x: None },
                extension::Proposal { start: 30.0, end: 40.0, title: None, note: None, score: None, crop_x: Some(0.3) },
            ],
            message: None,
        };
        let clips = candidates(&answer, "Demo：找精華");
        assert_eq!(clips.len(), 2);
        assert!(clips.iter().all(|c| super::super::highlights::is_candidate(&c.name)));
        assert_eq!((clips[0].name.as_str(), clips[0].title.as_deref(), clips[0].note.as_deref()), ("候選 01", Some("開場"), Some("Demo：找精華：聊天 ×3")));
        assert_eq!((clips[1].name.as_str(), clips[1].crop_x, clips[1].note.as_deref()), ("候選 02", Some(0.3), Some("Demo：找精華")));
    }
}
