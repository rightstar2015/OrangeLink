//! The speech-to-subtitles window: picks a model and language, then works through a queue.

use super::App;
use crate::{
    library::item_dir,
    model::MediaKind,
    whisper::{self, Event, Job, LANGUAGES, MODELS},
};
use eframe::egui::{self, RichText};
use std::sync::mpsc::{channel, Receiver};



struct Running {
    id: String,
    name: String,
    duration: Option<f64>,
    position: f64,
    job: Job,
}

#[derive(Default)]
pub struct WhisperState {
    pub(super) open: bool,
    /// Items waiting, including those not started yet.
    queue: Vec<String>,
    running: Option<Running>,
    filter_rx: Option<Receiver<bool>>,
    filter_ok: Option<bool>,
    download: Option<whisper::Download>,
    status: (String, bool),
    done: usize,
}

impl App {
    /// Opens the window for these items; files that aren't video or audio are left out.
    pub(super) fn open_whisper(&mut self, ids: Vec<String>) {
        let Some(lib) = &self.lib else { return };
        let media: Vec<String> = ids.into_iter().filter(|id| lib.items.get(id).is_some_and(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio))).collect();
        if media.is_empty() {
            return self.toast("只有影片和音訊可以產生字幕");
        }
        let w = &mut self.whisper;
        w.open = true;
        w.status = (String::new(), false);
        w.done = 0;
        for id in media {
            if !w.queue.contains(&id) && w.running.as_ref().is_none_or(|r| r.id != id) {
                w.queue.push(id);
            }
        }
        self.check_whisper_filter();
    }

    fn check_whisper_filter(&mut self) {
        self.whisper.filter_ok = None;
        self.ffmpeg_dir = crate::media::find_ffmpeg_dir(self.config.ffmpeg_dir.as_deref());
        let Some(dir) = self.ffmpeg_dir.clone() else {
            self.whisper.filter_ok = Some(false);
            return;
        };
        let (tx, rx) = channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(whisper::has_filter(&dir));
            ctx.request_repaint();
        });
        self.whisper.filter_rx = Some(rx);
    }

    pub(super) fn poll_whisper(&mut self) {
        if let Some(ok) = super::state::take(&mut self.whisper.filter_rx).finished() {
            self.whisper.filter_ok = Some(ok.unwrap_or(false));
        }
        if let Some(result) = self.whisper.download.as_ref().and_then(|d| super::state::poll(&d.rx).finished()) {
            self.whisper.download = None;
            self.whisper.status = match result.map_err(str::to_string).and_then(|r| r) {
                Ok(()) => ("模型下載完成".into(), false),
                Err(e) if e == whisper::CANCELLED => ("已取消下載模型".into(), false),
                Err(e) => (format!("模型下載失敗：{e}"), true),
            };
        }
        let mut finished = None;
        if let Some(run) = &mut self.whisper.running {
            loop {
                match super::state::poll(&run.job.rx) {
                    super::Reply::Pending => break,
                    super::Reply::Done(Event::Position(p)) => run.position = p,
                    super::Reply::Done(Event::Done(r)) => finished = Some(r),
                    // Ended without Done (a panic): fail this item, go on with the queue.
                    super::Reply::Lost => {
                        if finished.is_none() {
                            finished = Some(Err(super::state::LOST.to_string()));
                        }
                        break;
                    }
                }
            }
        }
        if let Some(result) = finished {
            self.finish_whisper(result);
            self.start_next_whisper();
        }
    }

    fn output_path(&self, id: &str) -> Option<std::path::PathBuf> {
        let lib = self.lib.as_ref()?;
        let it = lib.items.get(id)?;
        let stem = std::path::Path::new(&it.file_name).file_stem()?.to_string_lossy().to_string();
        Some(item_dir(&lib.root, id).join(format!("{stem}.whisper.srt")))
    }

    fn finish_whisper(&mut self, result: Result<(), String>) {
        let Some(run) = self.whisper.running.take() else { return };
        match result {
            Ok(()) => {
                if let Some(path) = self.output_path(&run.id) {
                    self.edit_items(std::slice::from_ref(&run.id), |it| it.subtitle = Some(path.clone()));
                    self.index_subtitles(&run.id, &path);
                }
                if self.subs.as_ref().is_some_and(|s| s.id == run.id) {
                    self.load_subtitles(&run.id);
                }
                self.whisper.done += 1;
                let more = !self.whisper.queue.is_empty();
                if !more {
                    let n = self.whisper.done;
                    self.toast(format!("字幕產生完成：{n} 個素材"));
                    self.whisper.status = (format!("完成 {n} 個素材，預覽時可在右側點台詞跳轉"), false);
                }
            }
            Err(e) => {
                if e != "已取消" {
                    self.toast(format!("「{}」字幕產生失敗", run.name));
                }
                self.whisper.status = (format!("「{}」：{e}", run.name), true);
            }
        }
    }

    fn start_next_whisper(&mut self) {
        if self.whisper.running.is_some() {
            return;
        }
        // Items deleted since they were queued are dropped here: one left at the
        // front would stop everything behind it, since nothing else starts the queue.
        while let Some(id) = self.whisper.queue.first() {
            if self.output_path(id).is_some() {
                break;
            }
            self.whisper.queue.remove(0);
        }
        if self.whisper.queue.is_empty() {
            return;
        }
        let model = self.config.whisper_model.clone();
        let (Some(ff), true) = (self.ffmpeg_dir.clone(), whisper::has_model(&model)) else { return };
        let id = self.whisper.queue.remove(0);
        let (Some(lib), Some(out)) = (&self.lib, self.output_path(&id)) else { return };
        let Some(it) = lib.items.get(&id) else { return };
        let job = Job::spawn(&ff, &lib.file_path(it), &out, &model, &self.config.whisper_language, self.egui_ctx.clone());
        self.whisper.running = Some(Running { name: it.name.clone(), duration: it.duration, position: 0.0, id, job });
    }

    fn fraction(run: &Running) -> f32 {
        run.duration.filter(|d| *d > 0.0).map_or(0.0, |d| (run.position / d).clamp(0.0, 1.0) as f32)
    }

    /// The transcription under way, for the status bar's jobs list.
    pub(super) fn whisper_job(&self) -> Option<super::jobs::BgJob> {
        let run = self.whisper.running.as_ref()?;
        let left = self.whisper.queue.len();
        let detail = if left > 0 { format!("{:.0}%（還有 {left} 個）", Self::fraction(run) * 100.0) } else { format!("{:.0}%", Self::fraction(run) * 100.0) };
        let mut job = super::jobs::BgJob::new(super::jobs::JobKind::Whisper, format!("產生字幕：{}", run.name), detail, Some(Self::fraction(run)));
        job.has_window = true;
        Some(job)
    }

    /// The model download under way, for the status bar's jobs list.
    pub(super) fn model_download_job(&self) -> Option<super::jobs::BgJob> {
        let d = self.whisper.download.as_ref()?;
        let mb = |b: u64| b as f64 / 1_048_576.0;
        let got = d.got.load(std::sync::atomic::Ordering::Relaxed);
        let detail = match d.total.load(std::sync::atomic::Ordering::Relaxed) {
            0 => format!("{:.0} MB", mb(got)),
            total => format!("{:.0} / {:.0} MB", mb(got), mb(total)),
        };
        let mut job = super::jobs::BgJob::new(super::jobs::JobKind::ModelDownload, format!("下載語音模型：{}", self.config.whisper_model), detail, d.progress());
        job.has_window = true;
        Some(job)
    }

    /// Stops the model download; curl is ended and the partial file removed.
    pub(super) fn cancel_model_download(&mut self) {
        if let Some(d) = &self.whisper.download {
            d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Stops the transcription and drops the items waiting after it.
    pub(super) fn cancel_whisper(&mut self) {
        self.whisper.queue.clear();
        if let Some(run) = &self.whisper.running {
            run.job.cancel();
        }
    }

    /// Adds an item to the queue and starts it when nothing is running, without
    /// opening the window (整場處理).
    pub(super) fn queue_whisper(&mut self, id: &str) {
        let w = &mut self.whisper;
        if !w.queue.iter().any(|q| q == id) && w.running.as_ref().is_none_or(|r| r.id != id) {
            w.queue.push(id.to_string());
        }
        self.start_next_whisper();
    }

    /// Stops everything for good: the result of the running job is dropped.
    pub(super) fn drop_whisper(&mut self) {
        self.cancel_whisper();
        self.whisper.running = None;
    }

    pub(super) fn show_whisper(&mut self) {
        self.whisper.open = true;
    }

    pub(super) fn whisper_window(&mut self, ctx: &egui::Context) {
        if !self.whisper.open {
            return;
        }
        let mut open = true;
        let (mut start, mut download, mut recheck) = (false, false, false);
        let c = &mut self.config;
        let w = &mut self.whisper;
        egui::Window::new("產生字幕（語音轉文字）").open(&mut open).resizable(false).collapsible(false).default_width(460.0).show(ctx, |ui| {
            ui.label(format!("等待處理：{} 個素材", w.queue.len()));
            ui.horizontal(|ui| {
                ui.label("語言：");
                let shown = LANGUAGES.iter().find(|l| l.0 == c.whisper_language).map_or("自動偵測", |l| l.1);
                egui::ComboBox::from_id_salt("whisper-lang").selected_text(shown).show_ui(ui, |ui| {
                    for (code, label) in LANGUAGES {
                        ui.selectable_value(&mut c.whisper_language, code.to_string(), label);
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("模型：");
                let shown = MODELS.iter().find(|m| m.0 == c.whisper_model).map_or(c.whisper_model.clone(), |m| format!("{}（{}）", m.0, m.1));
                egui::ComboBox::from_id_salt("whisper-model").width(260.0).selected_text(shown).show_ui(ui, |ui| {
                    for (name, size) in MODELS {
                        let have = if whisper::has_model(name) { " ✔" } else { "" };
                        ui.selectable_value(&mut c.whisper_model, name.to_string(), format!("{name}（{size}）{have}"));
                    }
                });
            });
            let have_model = whisper::has_model(&c.whisper_model);
            match w.filter_ok {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("檢查 FFmpeg…");
                    });
                }
                Some(false) => {
                    ui.label(RichText::new("目前的 FFmpeg 不支援語音轉文字。需要 FFmpeg 8 以上的完整版（可在「從網址下載」視窗安裝或更新 FFmpeg）。").color(crate::ui::warning()));
                    if ui.button("重新檢查").clicked() {
                        recheck = true;
                    }
                }
                Some(true) if !have_model => {
                    ui.label(RichText::new("第一次使用這個模型需要下載，來源為 Hugging Face（whisper.cpp）。").small());
                    ui.horizontal(|ui| match &w.download {
                        None => {
                            if ui.button("下載模型").clicked() {
                                download = true;
                            }
                        }
                        Some(d) => {
                            match d.progress() {
                                Some(p) => ui.add(egui::ProgressBar::new(p).show_percentage().desired_width(160.0)),
                                None => ui.spinner(),
                            };
                            if ui.button("取消下載").clicked() {
                                d.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            ui.label(RichText::new("可以先關閉這個視窗").small().weak());
                        }
                    });
                }
                Some(true) => {}
            }
            if let Some(run) = &w.running {
                ui.separator();
                ui.label(format!("處理中：{}", run.name));
                ui.add(egui::ProgressBar::new(Self::fraction(run)).show_percentage());
            }
            ui.separator();
            ui.horizontal(|ui| {
                let can = w.filter_ok == Some(true) && have_model && w.running.is_none() && !w.queue.is_empty();
                if ui.add_enabled(can, egui::Button::new("開始")).clicked() {
                    start = true;
                }
                if ui.add_enabled(w.running.is_some() || !w.queue.is_empty(), egui::Button::new("取消")).clicked() {
                    w.queue.clear();
                    if let Some(run) = &w.running {
                        run.job.cancel();
                    }
                }
            });
            let (status, error) = &w.status;
            if !status.is_empty() {
                ui.label(RichText::new(status).color(if *error { crate::ui::warning() } else { crate::ui::good() }));
            }
            ui.label(RichText::new("在本機轉錄，影音不會上傳。中文會自動轉成繁體（台灣用語），台詞可以直接搜尋；建議產生後再校對。").small().weak());
        });
        self.whisper.open = open;
        if recheck {
            self.check_whisper_filter();
        }
        if download {
            self.whisper.download = Some(whisper::Download::start(self.config.whisper_model.clone(), self.egui_ctx.clone()));
            self.whisper.status = (String::new(), false);
        }
        if start {
            self.config.save();
            self.start_next_whisper();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_download_whose_thread_died_shows_as_failed() {
        let mut app = App::with_config(&egui::Context::default(), crate::config::Config::default());
        let (tx, rx) = std::sync::mpsc::channel();
        app.whisper.download = Some(whisper::Download { rx, cancel: Default::default(), got: Default::default(), total: Default::default() });
        app.whisper.status = ("正在下載模型…".into(), false);
        drop(tx); // the download thread panicked
        app.poll_whisper();
        assert!(app.whisper.download.is_none(), "no longer downloading");
        assert!(app.whisper.status.1 && app.whisper.status.0.contains("失敗"), "{:?}", app.whisper.status);
    }

    #[test]
    fn a_model_download_is_a_job_that_can_be_cancelled() {
        let mut app = App::with_config(&egui::Context::default(), crate::config::Config::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let d = whisper::Download { rx, cancel: Default::default(), got: Default::default(), total: Default::default() };
        d.total.store(200 << 20, std::sync::atomic::Ordering::Relaxed);
        d.got.store(50 << 20, std::sync::atomic::Ordering::Relaxed);
        let cancel = d.cancel.clone();
        app.whisper.download = Some(d);
        let jobs = app.background_jobs();
        let job = jobs.iter().find(|j| j.kind == super::super::jobs::JobKind::ModelDownload).expect("in the jobs list");
        assert_eq!(job.progress, Some(0.25));
        assert!(job.can_cancel && job.detail.contains("50 / 200 MB"), "{}", job.detail);
        app.cancel_job(super::super::jobs::JobKind::ModelDownload);
        assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
        tx.send(Err(whisper::CANCELLED.to_string())).unwrap();
        app.poll_whisper();
        assert!(app.whisper.download.is_none() && !app.whisper.status.1, "{:?}", app.whisper.status);
    }

    #[test]
    fn an_item_deleted_while_queued_does_not_hold_up_the_queue() {
        let mut app = App::with_config(&egui::Context::default(), crate::config::Config::default());
        let root = std::env::temp_dir().join(format!("orangelink-whisper-{}", crate::model::new_id()));
        app.lib = Some(crate::library::Library::create(&root).unwrap());
        app.whisper.queue = vec!["deleted".into(), "deleted too".into()];
        app.start_next_whisper();
        assert!(app.whisper.queue.is_empty(), "{:?}", app.whisper.queue);
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }
}
