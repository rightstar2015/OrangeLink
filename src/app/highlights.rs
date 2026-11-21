//! Highlight finding for the item in the preview: chat activity from yt-dlp's chat
//! record, loudness from FFmpeg (cached as `loudness.json` in the item's folder), and
//! candidate clips at their peaks.

use super::{Action, App, MediaTask};
use crate::{library::item_dir, model::MediaKind, signals};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{channel, Receiver},
        Arc,
    },
};

/// 整場處理: items waiting and the one under way.
#[derive(Default)]
pub struct StreamJobs {
    pub(super) queue: std::collections::VecDeque<String>,
    pub(super) run: Option<StreamRun>,
}

/// Clip names given to candidates; finding again replaces clips still named like this.
pub const CANDIDATE: &str = "候選 ";

pub struct LoudnessJob {
    rx: Receiver<Result<Vec<f32>, String>>,
    /// Seconds analysed so far.
    pub progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Drop for LoudnessJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A chat record's weight curve and keyword hits, once read.
type ChatRead = Option<(Vec<f32>, signals::Hits)>;

#[derive(Default)]
pub struct Signals {
    pub id: String,
    /// Chat weight per second, once read, and which keywords it used when.
    pub chat: Option<Vec<f32>>,
    pub chat_hits: Option<signals::Hits>,
    pub chat_file: Option<PathBuf>,
    chat_rx: Option<Receiver<ChatRead>>,
    /// Loudest LUFS per second, once analysed.
    pub loud: Option<Vec<f32>>,
    pub loud_job: Option<LoudnessJob>,
    pub loud_error: Option<String>,
}

/// 整場處理 of one item on a background thread: the chat record read, the
/// loudness analysed (unless cached), and whether speech to text can run.
pub struct StreamRun {
    pub id: String,
    pub name: String,
    pub duration: Option<f64>,
    rx: Receiver<StreamResult>,
    /// Seconds of sound analysed so far.
    pub progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
}

impl Drop for StreamRun {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct StreamResult {
    chat: Option<(Vec<f32>, signals::Hits)>,
    /// The loudness curve; None without FFmpeg.
    loud: Result<Option<Vec<f32>>, String>,
    /// Analysed now (to be cached), not read from the cache.
    loud_new: bool,
    /// A Whisper model and FFmpeg's whisper filter are there.
    can_whisper: bool,
}

fn loudness_cache(root: &std::path::Path, id: &str) -> PathBuf {
    item_dir(root, id).join("loudness.json")
}

impl App {
    /// Reads what is already known about the item: the chat record and cached loudness.
    pub(super) fn load_signals(&mut self, id: &str) {
        self.signals = None;
        let Some(lib) = &self.lib else { return };
        let Some(it) = lib.items.get(id).filter(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio)) else { return };
        let mut s = Signals { id: id.to_string(), ..Default::default() };
        s.loud = std::fs::read(loudness_cache(&lib.root, id)).ok().and_then(|b| serde_json::from_slice(&b).ok());
        s.chat_file = it.imported_from().and_then(|p| signals::chat_file_for(p));
        if let Some(file) = s.chat_file.clone() {
            let (tx, rx) = channel();
            let (keywords, duration, ctx) = (self.config.chat_keywords.clone(), it.duration.unwrap_or(0.0), self.egui_ctx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(signals::read_chat(&file, &keywords, duration));
                ctx.request_repaint();
            });
            s.chat_rx = Some(rx);
        }
        self.signals = Some(s);
    }

    pub(super) fn poll_signals(&mut self) {
        let Some(s) = &mut self.signals else { return };
        if let Some(chat) = s.chat_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            s.chat_rx = None;
            (s.chat, s.chat_hits) = chat.map_or((None, None), |(c, h)| (Some(c), Some(h)));
        }
        let Some(job) = &s.loud_job else { return };
        let result = match super::state::poll(&job.rx) {
            super::Reply::Pending => return,
            super::Reply::Done(result) => result,
            super::Reply::Lost => Err(super::state::LOST.to_string()),
        };
        s.loud_job = None;
        match result {
            Ok(curve) => {
                if let Some(lib) = &self.lib {
                    let _ = crate::library::write_json(&loudness_cache(&lib.root, &s.id), &curve);
                }
                s.loud = Some(curve);
            }
            Err(e) => s.loud_error = (e != "已取消").then_some(e),
        }
    }

    /// Runs the item menu's 影音 task once the preview has the item's curves: the
    /// chat record read, for finding candidates.
    pub(super) fn run_pending_media(&mut self) {
        let Some((id, task)) = self.pending_media.clone() else { return };
        if self.viewer.as_ref().is_none_or(|v| v.id != id) && self.viewer_loading.is_none() {
            self.pending_media = None; // the preview moved on
            return;
        }
        let Some(s) = self.signals.as_ref().filter(|s| s.id == id) else { return };
        match task {
            MediaTask::Loudness => {
                if s.loud_job.is_none() {
                    self.analyze_loudness();
                }
            }
            MediaTask::Candidates if s.chat_rx.is_some() => return,
            MediaTask::Candidates => self.find_candidates(&id),
            MediaTask::Open => {}
        }
        self.pending_media = None;
    }

    /// Loudness analysis under way, for the status bar's jobs list.
    pub(super) fn loudness_job(&self) -> Option<super::jobs::BgJob> {
        let s = self.signals.as_ref()?;
        let job = s.loud_job.as_ref()?;
        let it = self.lib.as_ref().and_then(|l| l.items.get(&s.id));
        let secs = job.progress.load(Ordering::Relaxed) as f64;
        let progress = it.and_then(|i| i.duration).filter(|d| *d > 0.0).map(|d| (secs / d).clamp(0.0, 1.0) as f32);
        let name = it.map_or_else(String::new, |i| i.name.clone());
        Some(super::jobs::BgJob::new(super::jobs::JobKind::Loudness, format!("分析音量：{name}"), format!("已讀 {}", crate::media::fmt_duration(secs)), progress))
    }

    pub(super) fn cancel_loudness(&mut self) {
        if let Some(s) = self.signals.as_mut() {
            s.loud_job = None;
        }
    }

    pub fn analyze_loudness(&mut self) {
        let (Some(lib), Some(ff)) = (&self.lib, self.ffmpeg_dir.clone()) else {
            return self.toast("需要 FFmpeg 才能分析音量");
        };
        let Some(s) = self.signals.as_mut() else { return };
        let Some(it) = lib.items.get(&s.id) else { return };
        let input = lib.file_path(it);
        let (tx, rx) = channel();
        let (progress, cancel) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        let (p, c, ctx) = (progress.clone(), cancel.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let result = signals::loudness(&ff, &input, |t| {
                p.store(t as u64, Ordering::Relaxed);
                ctx.request_repaint();
                !c.load(Ordering::Relaxed)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        s.loud_error = None;
        s.loud_job = Some(LoudnessJob { rx, progress, cancel });
    }

    /// Clips around the strongest moments of the item in the preview.
    pub(super) fn find_candidates(&mut self, id: &str) {
        let Some(s) = self.signals.as_ref().filter(|s| s.id == id) else { return };
        let (chat, hits, loud) = (s.chat.clone(), s.chat_hits.clone(), s.loud.clone());
        match self.make_candidates(id, chat.as_deref(), hits.as_ref(), loud.as_deref()) {
            0 => self.toast("沒有找到明顯的高峰；可以先分析音量，或確認有聊天室紀錄"),
            n => self.toast(format!("已建立 {n} 個候選片段；不要的刪掉，要的改個名字就會保留")),
        }
    }

    /// Clips around the strongest moments, replacing earlier candidates not
    /// renamed since. Returns how many were made.
    fn make_candidates(&mut self, id: &str, chat: Option<&[f32]>, hits: Option<&signals::Hits>, loud: Option<&[f32]>) -> usize {
        let Some(it) = self.lib.as_ref().and_then(|l| l.items.get(id)) else { return 0 };
        let c = &self.config;
        let (before, after) = (c.highlight_before.max(0.0) as f64, c.highlight_after.max(0.0) as f64);
        let end = it.duration.unwrap_or(f64::MAX);
        let gap = (before + after).max(10.0) as usize;
        let moments = signals::peaks(chat, loud, gap, c.highlight_max.max(1));
        let segments = signals::segments(&moments, before, after, c.highlight_merge.max(0.0) as f64, end);
        let clips: Vec<crate::model::Clip> = segments
            .iter()
            .enumerate()
            .map(|(n, s)| crate::model::Clip {
                id: crate::model::new_id(),
                name: format!("{CANDIDATE}{:02}", n + 1),
                start: s.start,
                end: s.end,
                note: signals::describe(chat, hits, loud, s.start, s.end).map(|d| if s.peaks > 1 { format!("{d}（{} 個高峰合併）", s.peaks) } else { d }),
                score: Some((s.score * 10.0).round() / 10.0),
                ..Default::default()
            })
            .collect();
        if clips.is_empty() {
            return 0;
        }
        let n = clips.len();
        self.edit_items(&[id.to_string()], |it| {
            it.clips.retain(|c| !is_candidate(&c.name));
            it.clips.extend(clips.iter().cloned());
            it.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        });
        n
    }

    /// Queues 整場處理 for these items (video and audio only).
    pub(super) fn process_streams(&mut self, ids: Vec<String>) {
        let Some(lib) = &self.lib else { return };
        let media: Vec<String> = ids.into_iter().filter(|id| lib.items.get(id).is_some_and(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio))).collect();
        if media.is_empty() {
            return self.toast("只有影片和音訊可以整場處理");
        }
        let n = media.len();
        for id in media {
            if !self.stream.queue.contains(&id) && self.stream.run.as_ref().is_none_or(|r| r.id != id) {
                self.stream.queue.push_back(id);
            }
        }
        self.start_next_stream();
        self.toast(format!("整場處理：{n} 個項目排入背景工作（音量 → 精華候選 → 字幕）"));
    }

    fn start_next_stream(&mut self) {
        if self.stream.run.is_some() {
            return;
        }
        let Some(lib) = &self.lib else { return };
        let Some(id) = self.stream.queue.pop_front() else { return };
        let Some(it) = lib.items.get(&id) else { return self.start_next_stream() };
        let input = lib.file_path(it);
        let chat_file = it.imported_from().and_then(|p| signals::chat_file_for(p));
        let cached: Option<Vec<f32>> = std::fs::read(loudness_cache(&lib.root, &id)).ok().and_then(|b| serde_json::from_slice(&b).ok());
        let (keywords, duration) = (self.config.chat_keywords.clone(), it.duration);
        let (ff, model, ctx) = (self.ffmpeg_dir.clone(), self.config.whisper_model.clone(), self.egui_ctx.clone());
        let (tx, rx) = channel();
        let (progress, cancel) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
        let (p, c) = (progress.clone(), cancel.clone());
        std::thread::spawn(move || {
            let chat = chat_file.and_then(|f| signals::read_chat(&f, &keywords, duration.unwrap_or(0.0)));
            let (loud, loud_new) = match (cached, &ff) {
                (Some(curve), _) => (Ok(Some(curve)), false),
                (None, Some(ff)) => {
                    let r = signals::loudness(ff, &input, |t| {
                        p.store(t as u64, Ordering::Relaxed);
                        ctx.request_repaint();
                        !c.load(Ordering::Relaxed)
                    });
                    (r.map(Some), true)
                }
                (None, None) => (Ok(None), false),
            };
            let can_whisper = !c.load(Ordering::Relaxed) && ff.as_deref().is_some_and(|ff| crate::whisper::has_model(&model) && crate::whisper::has_filter(ff));
            let _ = tx.send(StreamResult { chat, loud, loud_new, can_whisper });
            ctx.request_repaint();
        });
        self.stream.run = Some(StreamRun { id, name: it.name.clone(), duration, rx, progress, cancel });
    }

    pub(super) fn poll_stream(&mut self) {
        let Some(run) = &self.stream.run else { return };
        let result = match super::state::poll(&run.rx) {
            super::Reply::Pending => return,
            super::Reply::Done(result) => result,
            super::Reply::Lost => {
                // Analysis panicked: report it and go on with the queue.
                let name = run.name.clone();
                self.stream.run = None;
                crate::log::error("highlights", format!("整場處理「{name}」：{}", super::state::LOST));
                self.toast_error(format!("「{name}」整場處理失敗：{}", super::state::LOST));
                return self.start_next_stream();
            }
        };
        let Some(run) = self.stream.run.take() else { return };
        let (id, name) = (run.id.clone(), run.name.clone());
        drop(run);
        let loud = match result.loud {
            Ok(l) => l,
            Err(e) => {
                if e != "已取消" {
                    crate::log::error("highlights", format!("整場處理「{name}」：{e}"));
                    self.toast(format!("「{name}」分析音量失敗：{e}"));
                }
                return self.start_next_stream();
            }
        };
        if let (true, Some(curve), Some(lib)) = (result.loud_new, &loud, &self.lib) {
            let _ = crate::library::write_json(&loudness_cache(&lib.root, &id), curve);
        }
        if let Some(s) = self.signals.as_mut().filter(|s| s.id == id) {
            if s.loud.is_none() {
                s.loud = loud.clone();
            }
            if s.chat.is_none() {
                (s.chat, s.chat_hits) = result.chat.clone().map_or((None, None), |(c, h)| (Some(c), Some(h)));
            }
        }
        let (chat, hits) = result.chat.as_ref().map_or((None, None), |(c, h)| (Some(c.as_slice()), Some(h)));
        let n = self.make_candidates(&id, chat, hits, loud.as_deref());
        let has_sub = self.lib.as_ref().is_some_and(|l| l.find_subtitle(&id).is_some());
        let subtitles = if has_sub {
            ""
        } else if result.can_whisper {
            self.queue_whisper(&id);
            "，字幕已排入轉錄"
        } else {
            "，沒有字幕（需要 Whisper 模型，在「產生字幕」視窗下載）"
        };
        let found = if n > 0 { format!("{n} 個精華候選") } else { "沒有找到明顯的高峰".to_string() };
        let chat = if result.chat.is_none() { "（沒有聊天室紀錄，只依音量）" } else { "" };
        self.toast_with(format!("「{name}」：{found}{chat}{subtitles}"), "查看", Action::Media(id, MediaTask::Open));
        self.start_next_stream();
    }

    /// 整場處理 under way, for the status bar's jobs list.
    pub(super) fn stream_job(&self) -> Option<super::jobs::BgJob> {
        let run = self.stream.run.as_ref()?;
        let secs = run.progress.load(Ordering::Relaxed) as f64;
        let progress = run.duration.filter(|d| *d > 0.0).map(|d| (secs / d).clamp(0.0, 1.0) as f32);
        let left = self.stream.queue.len();
        let mut detail = format!("分析音量 {}", crate::media::fmt_duration(secs));
        if left > 0 {
            detail += &format!("（還有 {left} 個）");
        }
        Some(super::jobs::BgJob::new(super::jobs::JobKind::Stream, format!("整場處理：{}", run.name), detail, progress))
    }

    pub(super) fn cancel_stream(&mut self) {
        self.stream.queue.clear();
        self.stream.run = None;
    }
}

/// The name a kept candidate gets: `候選 03` becomes `精華 03`.
pub fn kept_name(name: &str) -> String {
    match name.strip_prefix(CANDIDATE) {
        Some(n) if is_candidate(name) => format!("精華 {n}"),
        _ => name.to_string(),
    }
}

/// `候選 01` and the like, as named by `find_candidates`.
pub fn is_candidate(name: &str) -> bool {
    name.strip_prefix(CANDIDATE).is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The dedicated ⚡ 精華 tab in the video studio side panel.
pub fn candidates_ui(app: &mut App, ui: &mut egui::Ui, id: &str) {
    use egui::{RichText, Stroke};
    let Some(s) = app.signals.as_ref().filter(|s| s.id == id) else {
        ui.label(RichText::new("此素材不支援精華分析").weak());
        return;
    };
    let mut analyze = false;
    let mut cancel = false;
    let mut open_settings = false;
    let t = crate::ui::tokens();
    egui::Frame::none()
        .fill(t.bg_elevated)
        .stroke(Stroke::new(1.0_f32, t.separator))
        .rounding(crate::ui::ROUNDING)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("直播精華自動偵測").strong());
            ui.add_space(6.0);
            let (chat_text, chat_ok) = match (&s.chat_file, &s.chat, &s.chat_rx) {
                (None, ..) => ("找不到同名 .live_chat.json（下載時勾選聊天室並與影片同資料夾）".to_string(), false),
                (Some(_), _, Some(_)) => ("聊天室：讀取中…".to_string(), false),
                (Some(_), Some(c), _) => (format!("✓ 聊天室已載入（約 {} 分鐘）", c.len() / 60), true),
                (Some(_), None, None) => ("聊天室紀錄沒有可用的時間資訊".to_string(), false),
            };
            ui.label(RichText::new(chat_text).small().color(if chat_ok { crate::ui::good() } else { t.text_muted }));
            ui.add_space(4.0);
            ui.horizontal(|ui| match (&s.loud, &s.loud_job) {
                (_, Some(job)) => {
                    ui.spinner();
                    ui.label(RichText::new(format!("分析音量中… {}", crate::media::fmt_duration(job.progress.load(Ordering::Relaxed) as f64))).small());
                    cancel = ui.small_button("取消").clicked();
                }
                (Some(_), None) => {
                    ui.label(RichText::new("✓ 音量軌已分析").small().color(crate::ui::good()));
                    analyze = ui.small_button("重新分析").clicked();
                }
                (None, None) => {
                    ui.label(RichText::new("音量軌：未分析").small().weak());
                    analyze = ui.small_button("分析音量").on_hover_text("用 FFmpeg 讀完整段聲音，長直播約需幾分鐘；結果會保存").clicked();
                }
            });
            if let Some(e) = &s.loud_error {
                ui.add_space(4.0);
                ui.label(RichText::new(e).small().color(crate::ui::danger()));
            }
            ui.add_space(8.0);
            let ready = s.chat.is_some() || s.loud.is_some();
            let btn = egui::Button::new(RichText::new("⚡ 自動找精華候選").strong().color(if ready { egui::Color32::WHITE } else { t.text_muted }))
                .fill(if ready { t.selection_bg } else { t.bg_control });
            let find_resp = ui
                .add_enabled(ready, btn)
                .on_hover_text("在聊天熱度和音量的高峰前後建立片段；前後秒數與加權關鍵字可在設定調整");
            if find_resp.clicked() {
                app.act(Action::FindCandidates(id.to_string()));
            }
            ui.add_space(4.0);
            if ui.link(RichText::new("調整精華參數與關鍵字…").small()).clicked() {
                open_settings = true;
            }
        });
    ui.add_space(8.0);
    ui.label(
        RichText::new("提示：分析完成後可在時間軸上看到橘色聊天熱度與藍色音量曲線，點一下即可跳轉；按 I／O 標記並按 Enter 加入片段，最後從上方一鍵匯出 Premiere XML。")
            .small()
            .weak(),
    );
    if analyze {
        app.analyze_loudness();
    }
    if cancel {
        if let Some(s) = &mut app.signals {
            s.loud_job = None;
        }
    }
    if open_settings {
        app.act(Action::OpenSettingsAt(crate::config::SettingsPage::Highlights));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names() {
        assert!(is_candidate("候選 01") && is_candidate("候選 12"));
        assert!(!is_candidate("候選 01 爆笑") && !is_candidate("候選 ") && !is_candidate("開場"));
    }

    #[test]
    fn a_kept_candidate_is_no_longer_one() {
        assert_eq!(kept_name("候選 03"), "精華 03");
        assert!(!is_candidate(&kept_name("候選 03")));
        // Anything else keeps its name.
        assert_eq!(kept_name("開場"), "開場");
        assert_eq!(kept_name("候選 01 爆笑"), "候選 01 爆笑");
    }
}
