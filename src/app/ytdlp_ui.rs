//! The "download from URL" window: environment check, options, queue and log.

use super::App;
use crate::webclip::WebMsg;
use crate::ytdlp::{self, Check, Event, Job, OutFormat, ProbeInfo, Progress, Request};
use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit, Ui};
use std::{
    path::PathBuf,
    sync::mpsc::{channel, Receiver},
    time::SystemTime,
};



const MAX_LOG: usize = 3000;

struct Running {
    job: Job,
    req: Request,
    progress: Option<Progress>,
    files: Vec<PathBuf>,
    started: SystemTime,
    cancelled: bool,
}

pub struct YtState {
    pub open: bool,
    initialized: bool,
    check: Option<Check>,
    check_rx: Option<Receiver<Check>>,
    install_rx: Option<Receiver<Result<PathBuf, String>>>,
    url: String,
    playlist: bool,
    out_dir: String,
    name: String,
    ranged: bool,
    start: String,
    end: String,
    advanced: bool,
    format: OutFormat,
    probe: Option<ProbeInfo>,
    probe_rx: Option<Receiver<Result<ProbeInfo, String>>>,
    /// Advanced mode picks; None = leave that stream out.
    video_id: Option<String>,
    audio_id: Option<String>,
    chat: bool,
    chat_only: bool,
    chat_txt: bool,
    import: bool,
    link: bool,
    queue: Vec<Request>,
    queue_sel: Option<usize>,
    run_queue: bool,
    running: Option<Running>,
    log: Vec<String>,
    show_log: bool,
    last_files: Vec<PathBuf>,
    pub(super) pending_import: Vec<(Vec<PathBuf>, bool)>,
    status: (String, bool),
    /// Pages sent by the browser extension, waiting for the tool check.
    web_urls: Vec<String>,
}

impl Default for YtState {
    fn default() -> Self {
        Self {
            open: false,
            initialized: false,
            check: None,
            check_rx: None,
            install_rx: None,
            url: String::new(),
            playlist: false,
            out_dir: String::new(),
            name: String::new(),
            ranged: false,
            start: String::new(),
            end: String::new(),
            advanced: false,
            format: OutFormat::Mp4,
            probe: None,
            probe_rx: None,
            video_id: None,
            audio_id: None,
            chat: true,
            chat_only: false,
            chat_txt: true,
            import: true,
            link: true,
            queue: Vec::new(),
            queue_sel: None,
            run_queue: false,
            running: None,
            log: Vec::new(),
            show_log: true,
            last_files: Vec::new(),
            pending_import: Vec::new(),
            status: (String::new(), false),
            web_urls: Vec::new(),
        }
    }
}

impl YtState {
    fn log(&mut self, line: impl Into<String>) {
        let stamp = chrono::Local::now().format("%H:%M:%S");
        self.log.push(format!("[{stamp}] {}", line.into()));
        if self.log.len() > MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG);
        }
    }

    fn set_status(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        self.log(text.clone());
        self.status = (text, error);
    }

    fn busy(&self) -> bool {
        self.running.is_some()
    }
}

fn speed_text(p: &Progress) -> String {
    let mut parts = Vec::new();
    if let Some(f) = p.fraction {
        parts.push(format!("{:.1}%", f * 100.0));
    }
    if let Some(s) = p.speed {
        parts.push(format!("{}/s", ytdlp::human_mb(s as u64)));
    }
    if let Some(e) = p.eta {
        parts.push(format!("剩 {}", crate::media::fmt_duration(e)));
    }
    parts.join(" · ")
}

impl App {
    /// Opens the download window and checks yt-dlp / FFmpeg each time.
    pub(super) fn open_ytdlp(&mut self) {
        let yt = &mut self.yt;
        yt.open = true;
        if !yt.initialized {
            yt.initialized = true;
            yt.format = self.config.ytdlp_format;
            yt.import = self.config.ytdlp_import;
            yt.link = self.config.ytdlp_link;
            yt.chat = self.config.ytdlp_chat;
            yt.chat_txt = self.config.ytdlp_chat_txt;
            let default_dir = self.config.ytdlp_dir.clone().or_else(|| self.lib.as_ref().map(|l| l.root.join("downloads")));
            yt.out_dir = default_dir.map(|d| d.display().to_string()).unwrap_or_default();
        }
        self.start_tool_check();
    }

    fn start_tool_check(&mut self) {
        if self.yt.check_rx.is_some() {
            return;
        }
        // Pick up an FFmpeg installed since startup (e.g. through winget).
        self.ffmpeg_dir = crate::media::find_ffmpeg_dir(self.config.ffmpeg_dir.as_deref());
        let ff = self.ffmpeg_dir.clone();
        let (tx, rx) = channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Check::run(ff.as_deref()));
            ctx.request_repaint();
        });
        self.yt.check_rx = Some(rx);
        self.yt.log("開始工作：檢查工具");
    }

    fn save_ytdlp_prefs(&mut self) {
        let yt = &self.yt;
        let c = &mut self.config;
        c.ytdlp_format = yt.format;
        c.ytdlp_import = yt.import;
        c.ytdlp_link = yt.link;
        c.ytdlp_chat = yt.chat;
        c.ytdlp_chat_txt = yt.chat_txt;
        c.ytdlp_dir = (!yt.out_dir.trim().is_empty()).then(|| PathBuf::from(yt.out_dir.trim()));
        c.save();
    }

    /// Logs what the tool check found and says what to do next.
    fn show_tool_check(&mut self, check: Check) {
        let yt = &mut self.yt;
        match &check.ytdlp {
            Some(p) => yt.log(format!("yt-dlp：{}（{}）", p.display(), check.version.as_deref().unwrap_or("無法執行"))),
            None => yt.log("找不到 yt-dlp"),
        }
        yt.log(format!("最新版本：{}", check.latest.as_deref().unwrap_or("無法檢查（網路？）")));
        match &self.ffmpeg_dir {
            Some(d) if check.ffmpeg_ok => yt.log(format!("FFmpeg / FFprobe：{}", d.display())),
            _ => yt.log("找不到可用的 FFmpeg / FFprobe（mp4 合併與 mp3 轉檔需要）"),
        }
        match &check.deno {
            Some(d) => yt.log(format!("JavaScript 執行環境（deno）：{}", d.display())),
            None => yt.log("找不到 deno：YouTube 需要它才能取得完整格式，建議按「安裝 deno」"),
        }
        let msg = if check.ytdlp.is_none() {
            ("尚未安裝 yt-dlp，請按「安裝 yt-dlp」".to_string(), true)
        } else if check.update_available() {
            (format!("yt-dlp 有新版本 {}，建議先更新", check.latest.as_deref().unwrap_or_default()), true)
        } else if check.deno.is_none() {
            ("工具檢查完成；未安裝 deno，YouTube 可能缺少部分格式".to_string(), true)
        } else {
            ("工具檢查完成。".to_string(), false)
        };
        yt.set_status(msg.0, msg.1);
        yt.check = Some(check);
    }

    pub(super) fn poll_ytdlp(&mut self, ctx: &egui::Context) {
        match super::state::take(&mut self.yt.check_rx).finished() {
            Some(Ok(check)) => self.show_tool_check(check),
            Some(Err(lost)) => self.yt.set_status(format!("工具檢查失敗：{lost}"), true),
            None => {}
        }
        if let Some(result) = super::state::take(&mut self.yt.install_rx).finished() {
            match result.map_err(str::to_string).and_then(|r| r) {
                Ok(path) => {
                    self.yt.set_status(format!("yt-dlp 已更新：{}", path.display()), false);
                    self.start_tool_check();
                }
                Err(e) => self.yt.set_status(format!("更新 yt-dlp 失敗：{e}"), true),
            }
        }
        if let Some(result) = super::state::take(&mut self.yt.probe_rx).finished() {
            let yt = &mut self.yt;
            match result.map_err(str::to_string).and_then(|r| r) {
                Ok(info) => {
                    let mut msg = format!("已取得：{}", info.title);
                    if let Some(d) = info.duration {
                        msg += &format!("（{}）", crate::media::fmt_duration(d));
                    }
                    if let Some(n) = info.entries {
                        msg += &format!("（播放清單 {n} 部）");
                    }
                    if info.is_live {
                        msg += "（直播中）";
                    }
                    yt.video_id = info.video_formats().first().map(|f| f.id.clone());
                    yt.audio_id = info.audio_formats().first().map(|f| f.id.clone());
                    yt.set_status(msg, false);
                    yt.probe = Some(info);
                }
                Err(e) => yt.set_status(format!("取得影片資訊失敗：{e}"), true),
            }
        }
        let mut finished = None;
        if let Some(run) = &mut self.yt.running {
            let mut lines = Vec::new();
            loop {
                let ev = match super::state::poll(&run.job.rx) {
                    super::Reply::Pending => break,
                    super::Reply::Done(ev) => ev,
                    // Ended without Done (a panic): finish this download as failed.
                    super::Reply::Lost => {
                        if finished.is_none() {
                            finished = Some(Err(super::state::LOST.to_string()));
                        }
                        break;
                    }
                };
                match ev {
                    Event::Log(l) if !l.trim().is_empty() => lines.push(l),
                    Event::Log(_) => {}
                    Event::Progress(p) => run.progress = Some(p),
                    Event::File(p) => {
                        lines.push(format!("完成檔案：{}", p.display()));
                        run.files.push(p);
                    }
                    Event::Done(r) => finished = Some(r),
                }
            }
            for l in lines {
                self.yt.log(l);
            }
        }
        if let Some(result) = finished {
            self.finish_download(result);
        }

        if !self.import_running() && !self.yt.pending_import.is_empty() {
            let (paths, link) = self.yt.pending_import.remove(0);
            self.apply(ctx, super::Action::StartImport { paths, link });
        }
        if self.yt.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn finish_download(&mut self, result: Result<(), String>) {
        let Some(run) = self.yt.running.take() else { return };
        let yt = &mut self.yt;
        let mut files = run.files;
        let mut extra = Vec::new();
        if run.req.chat || run.req.chat_only {
            for json in ytdlp::new_chat_files(&run.req.out_dir, run.started) {
                yt.log(format!("聊天室紀錄：{}", json.display()));
                if yt.chat_txt {
                    match ytdlp::chat_to_txt(&json) {
                        Ok((txt, n)) => {
                            yt.log(format!("已轉成文字檔（{n} 則）：{}", txt.display()));
                            extra.push(txt);
                        }
                        Err(e) => yt.log(format!("聊天室轉文字失敗：{e}")),
                    }
                }
            }
        }
        match (&result, run.cancelled) {
            (_, true) => yt.set_status("已取消工作（已下載的部分保留在下載資料夾）", true),
            (Ok(()), _) => {
                let n = files.len() + extra.len();
                yt.set_status(format!("下載完成：{n} 個檔案"), false);
            }
            (Err(e), _) => {
                crate::log::error("yt-dlp", format!("下載失敗：{e}"));
                yt.set_status(format!("下載失敗：{e}"), true)
            }
        }
        files.extend(extra);
        files.retain(|f| f.is_file());
        if !files.is_empty() && yt.import && !run.cancelled {
            yt.pending_import.push((files.clone(), yt.link));
            // Files from one download (the video, its chat as text) are related.
            let group = crate::model::new_id();
            for f in &files {
                if let Some(key) = crate::importer::path_key(f) {
                    self.links.web_sources.insert(key.clone(), super::WebSource { url: run.req.url.clone(), folder: None, temp: None });
                    if files.len() > 1 {
                        self.links.link_to.insert(key, super::LinkTo::Group(group.clone()));
                    }
                }
            }
        }
        if !files.is_empty() {
            yt.last_files = files;
        }
        if yt.run_queue && !yt.queue.is_empty() {
            let next = yt.queue.remove(0);
            yt.queue_sel = None;
            self.start_download(next);
        } else {
            yt.run_queue = false;
        }
    }

    /// Takes what the browser extension sent: files go to the import queue, pages to yt-dlp.
    pub(super) fn poll_webclip(&mut self) {
        if let Some(web) = self.web.running() {
            web.set_folders(self.lib.as_ref().map(|l| l.folder_flat()).unwrap_or_default());
            web.set_max_bytes(self.config.webclip_max_gb.max(1) * crate::webclip::GB);
            web.set_only(self.config.webclip_only_origin.clone());
        }
        if self.lib.is_none() {
            return; // kept in the channel until a library is open
        }
        let msgs: Vec<WebMsg> = self.web.running().map(|w| w.rx.try_iter().collect()).unwrap_or_default();
        for msg in msgs {
            match msg {
                WebMsg::File { path, url, folder } => {
                    if let Some(key) = crate::importer::path_key(&path) {
                        self.links.web_sources.insert(key, super::WebSource { url, folder, temp: Some(path.clone()) });
                    }
                    self.yt.pending_import.push((vec![path], false));
                }
                WebMsg::Ytdlp { url } => {
                    self.yt.web_urls.push(url);
                    self.open_ytdlp();
                }
            }
        }
        if !self.yt.web_urls.is_empty() && self.yt.check_rx.is_none() {
            self.start_web_downloads();
        }
        // Everything queued has been imported (or skipped as a duplicate).
        if !self.links.web_sources.is_empty() && !self.import_running() && self.yt.pending_import.is_empty() {
            for (_, source) in self.links.web_sources.drain() {
                if let Some(temp) = source.temp {
                    crate::webclip::remove_received(&temp);
                }
            }
        }
    }

    /// Downloads pages from the extension right away, ignoring leftovers in the form.
    fn start_web_downloads(&mut self) {
        let urls = std::mem::take(&mut self.yt.web_urls);
        let tools = self.yt.check.as_ref().filter(|c| c.ytdlp.is_some());
        let deno = tools.and_then(|c| c.deno.clone());
        if tools.is_none() {
            let yt = &mut self.yt;
            yt.url = urls.into_iter().next().unwrap_or_default();
            yt.set_status("尚未安裝 yt-dlp，請按「安裝 yt-dlp」後再開始下載", true);
            return;
        }
        for url in urls {
            let req = Request {
                url,
                playlist: false,
                out_dir: PathBuf::from(self.yt.out_dir.trim()),
                name: String::new(),
                section: None,
                format: self.yt.format,
                custom: None,
                chat: false,
                chat_only: false,
                ffmpeg_dir: self.ffmpeg_dir.clone(),
                deno: deno.clone(),
            };
            if self.yt.busy() {
                self.yt.log(format!("加入佇列：{}", req.label()));
                self.yt.queue.push(req);
                self.yt.run_queue = true;
            } else {
                self.start_download(req);
            }
        }
    }

    fn build_request(&self) -> Result<Request, String> {
        let yt = &self.yt;
        if yt.url.trim().is_empty() {
            return Err("請先輸入影片網址".into());
        }
        let out_dir = PathBuf::from(yt.out_dir.trim());
        if yt.out_dir.trim().is_empty() {
            return Err("請選擇下載位置".into());
        }
        let custom = (yt.advanced && !yt.chat_only).then(|| (yt.video_id.clone(), yt.audio_id.clone()));
        if let Some((None, None)) = custom {
            return Err("進階模式至少要選擇影片或音訊其中一個".into());
        }
        Ok(Request {
            url: yt.url.trim().to_string(),
            playlist: yt.playlist && !yt.advanced,
            out_dir,
            name: yt.name.clone(),
            section: yt.ranged.then(|| (yt.start.clone(), yt.end.clone())),
            format: yt.format,
            custom,
            chat: yt.chat,
            chat_only: yt.chat && yt.chat_only,
            ffmpeg_dir: self.ffmpeg_dir.clone(),
            deno: yt.check.as_ref().and_then(|c| c.deno.clone()),
        })
    }

    fn start_download(&mut self, req: Request) {
        let yt = &mut self.yt;
        let Some(exe) = yt.check.as_ref().and_then(|c| c.ytdlp.clone()) else {
            yt.set_status("找不到 yt-dlp，請先按「安裝 yt-dlp」", true);
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&req.out_dir) {
            yt.set_status(format!("無法建立下載資料夾：{e}"), true);
            return;
        }
        let args = ytdlp::build_args(&req);
        yt.log(format!("開始工作：{}", req.label()));
        yt.log(format!("指令：yt-dlp {}", args.join(" ")));
        match Job::spawn(&exe, &args, self.egui_ctx.clone()) {
            Ok(job) => {
                yt.status = ("下載中…".into(), false);
                yt.running = Some(Running { job, req, progress: None, files: Vec::new(), started: SystemTime::now(), cancelled: false });
            }
            Err(e) => {
                crate::log::error("yt-dlp", format!("無法啟動 yt-dlp：{e}"));
                yt.set_status(format!("無法啟動 yt-dlp：{e}"), true)
            }
        }
    }

    fn start_probe(&mut self) {
        let yt = &mut self.yt;
        let Some(exe) = yt.check.as_ref().and_then(|c| c.ytdlp.clone()) else {
            yt.set_status("找不到 yt-dlp，請先按「安裝 yt-dlp」", true);
            return;
        };
        if yt.url.trim().is_empty() {
            yt.set_status("請先輸入影片網址", true);
            return;
        }
        let (url, playlist) = (yt.url.clone(), yt.playlist && !yt.advanced);
        let deno = yt.check.as_ref().and_then(|c| c.deno.clone());
        let (tx, rx) = channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ytdlp::probe(&exe, &url, playlist, deno.as_deref()));
            ctx.request_repaint();
        });
        yt.probe_rx = Some(rx);
        yt.status = ("取得影片資訊中…".into(), false);
        yt.log("開始工作：取得影片資訊");
    }

    fn start_install(&mut self) {
        if self.yt.busy() {
            self.yt.set_status("下載進行中，請等完成後再更新 yt-dlp", true);
            return;
        }
        let (tx, rx) = channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ytdlp::install_latest());
            ctx.request_repaint();
        });
        self.yt.install_rx = Some(rx);
        self.yt.status = ("正在下載最新版 yt-dlp…".into(), false);
        self.yt.log("開始工作：更新 yt-dlp");
    }

    /// The download under way, for the status bar's jobs list.
    pub(super) fn ytdlp_job(&self) -> Option<super::jobs::BgJob> {
        let run = self.yt.running.as_ref()?;
        let mut detail = run.progress.as_ref().map(speed_text).unwrap_or_else(|| "準備中…".into());
        if !self.yt.queue.is_empty() {
            detail += &format!("（佇列還有 {} 個）", self.yt.queue.len());
        }
        let p = run.progress.as_ref();
        let mut job = super::jobs::BgJob::new(super::jobs::JobKind::Ytdlp, format!("下載 {}", run.req.label()), detail, p.and_then(|p| p.fraction));
        job.eta = p.and_then(|p| p.eta);
        job.has_window = true;
        Some(job)
    }

    /// Stops the download and the queue after it.
    pub(super) fn cancel_ytdlp(&mut self) {
        self.yt.run_queue = false;
        if let Some(run) = &mut self.yt.running {
            run.cancelled = true;
            run.job.cancel();
        }
    }

    pub(super) fn ytdlp_window(&mut self, ctx: &egui::Context) {
        if !self.yt.open {
            return;
        }
        let mut open = true;
        let mut actions: Vec<YtAction> = Vec::new();
        egui::Window::new("從網址下載（yt-dlp）")
            .open(&mut open)
            .default_size([640.0, 720.0])
            .min_width(520.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.ytdlp_body(ui, &mut actions));
            });
        self.yt.open = open;
        for a in actions {
            match a {
                YtAction::Check => self.start_tool_check(),
                YtAction::Install => self.start_install(),
                YtAction::Probe => self.start_probe(),
                YtAction::Download => match self.build_request() {
                    Ok(req) => {
                        self.save_ytdlp_prefs();
                        self.start_download(req);
                    }
                    Err(e) => self.yt.set_status(e, true),
                },
                YtAction::Enqueue => match self.build_request() {
                    Ok(req) => {
                        self.save_ytdlp_prefs();
                        self.yt.log(format!("加入佇列：{}", req.label()));
                        self.yt.queue.push(req);
                        self.yt.url.clear();
                        self.yt.probe = None;
                    }
                    Err(e) => self.yt.set_status(e, true),
                },
                YtAction::RunQueue => {
                    if !self.yt.queue.is_empty() && !self.yt.busy() {
                        self.yt.run_queue = true;
                        let next = self.yt.queue.remove(0);
                        self.start_download(next);
                    }
                }
                YtAction::Cancel => self.cancel_ytdlp(),
                YtAction::PickDir => {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.yt.out_dir = dir.display().to_string();
                        self.save_ytdlp_prefs();
                    }
                }
                YtAction::PickFfmpeg => {
                    if let Some(dir) = rfd::FileDialog::new().set_title("選擇含有 ffmpeg.exe 的資料夾").pick_folder() {
                        self.set_ffmpeg_dir(Some(dir));
                        self.start_tool_check();
                    }
                }
                YtAction::Winget(name, id) => match winget_install(name, id) {
                    Ok(()) => self.yt.set_status(format!("已開啟 {name} 安裝視窗，完成後按「檢查工具」"), false),
                    Err(e) => self.yt.set_status(e, true),
                },
                YtAction::SaveLog => {
                    if let Some(path) = rfd::FileDialog::new().set_file_name("yt-dlp-log.txt").save_file() {
                        let _ = std::fs::write(path, self.yt.log.join("\r\n"));
                    }
                }
            }
        }
    }

    fn ytdlp_body(&mut self, ui: &mut Ui, actions: &mut Vec<YtAction>) {
        let ffmpeg_dir = self.ffmpeg_dir.clone();
        let yt = &mut self.yt;
        let busy = yt.busy();

        // Environment line, like "yt-dlp：2026.10.26 | 最新：2026.10.26 | FFmpeg / FFprobe：已驗證".
        ui.horizontal_wrapped(|ui| {
            if yt.check_rx.is_some() {
                ui.spinner();
                ui.label("檢查工具中…");
            } else if let Some(c) = &yt.check {
                let ff = if c.ffmpeg_ok { "已驗證" } else { "未找到" };
                let js = if c.deno.is_some() { "已安裝" } else { "未安裝" };
                ui.label(format!(
                    "yt-dlp：{} | 最新：{} | FFmpeg / FFprobe：{ff} | deno：{js}",
                    c.version.as_deref().unwrap_or("未安裝"),
                    c.latest.as_deref().unwrap_or("無法檢查"),
                ));
                if c.ytdlp.is_none() {
                    if ui.button(RichText::new("安裝 yt-dlp").color(crate::ui::warning())).clicked() {
                        actions.push(YtAction::Install);
                    }
                } else if c.update_available() && ui.button(RichText::new("⬆ 更新到最新版").color(crate::ui::warning())).clicked() {
                    actions.push(YtAction::Install);
                }
            }
        });
        ui.add_space(4.0);

        ui.label("影片網址（支援 yt-dlp 可下載的網站）：");
        ui.add_enabled(
            !busy,
            TextEdit::singleline(&mut yt.url).hint_text("https://www.youtube.com/watch?v=...").desired_width(f32::INFINITY),
        );
        ui.add_enabled(!yt.advanced, egui::Checkbox::new(&mut yt.playlist, "下載播放清單（簡單模式；未勾選時只下載單支影片）"));

        ui.horizontal(|ui| {
            ui.label("下載位置：");
            let w = ui.available_width() - 100.0;
            ui.add(TextEdit::singleline(&mut yt.out_dir).desired_width(w.max(120.0)));
            if ui.button("選擇資料夾").clicked() {
                actions.push(YtAction::PickDir);
            }
        });
        ui.horizontal(|ui| {
            ui.label("檔案名稱：");
            ui.add(
                TextEdit::singleline(&mut yt.name)
                    .hint_text("可留空（使用影片標題）；播放清單會自動加上編號")
                    .desired_width(f32::INFINITY),
            );
        });

        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("下載範圍").color(crate::ui::info()));
            ui.horizontal(|ui| {
                ui.radio_value(&mut yt.ranged, false, "全片下載");
                ui.radio_value(&mut yt.ranged, true, "指定時間下載");
            });
            ui.add_enabled_ui(yt.ranged, |ui| {
                ui.horizontal(|ui| {
                    ui.label("開始：");
                    ui.add(TextEdit::singleline(&mut yt.start).hint_text("00:00:00").desired_width(120.0));
                    ui.label("結束：");
                    ui.add(TextEdit::singleline(&mut yt.end).hint_text("留空則到影片結尾").desired_width(140.0));
                });
            });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut yt.advanced, false, "簡單模式");
            ui.radio_value(&mut yt.advanced, true, "進階模式");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let probing = yt.probe_rx.is_some();
                if probing {
                    ui.spinner();
                }
                if ui.add_enabled(!probing, egui::Button::new("取得影片資訊 / 格式")).clicked() {
                    actions.push(YtAction::Probe);
                }
            });
        });

        if yt.advanced {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("進階格式").color(crate::ui::info()));
                match &yt.probe {
                    None => {
                        ui.label(RichText::new("請先按「取得影片資訊 / 格式」列出可下載的格式").weak());
                    }
                    Some(info) => {
                        let pick = |ui: &mut Ui, label: &str, id: &str, list: Vec<&ytdlp::FormatInfo>, none: &str, sel: &mut Option<String>| {
                            ui.horizontal(|ui| {
                                ui.label(label);
                                let current = sel
                                    .as_ref()
                                    .and_then(|s| list.iter().find(|f| f.id == *s))
                                    .map(|f| f.label())
                                    .unwrap_or_else(|| none.to_string());
                                egui::ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(current).show_ui(ui, |ui| {
                                    ui.selectable_value(sel, None, none);
                                    for f in list {
                                        ui.selectable_value(sel, Some(f.id.clone()), f.label());
                                    }
                                });
                            });
                        };
                        pick(ui, "影片：", "yt-video", info.video_formats(), "不下載影片", &mut yt.video_id);
                        pick(ui, "音訊：", "yt-audio", info.audio_formats(), "不下載音訊", &mut yt.audio_id);
                        ui.label(
                            RichText::new("同時選影片和音訊時會自動合併（mp4，無法合併時用 mkv）。avc1 相容性最好，VP9 / AV1 部分剪輯軟體無法讀取。")
                                .small()
                                .weak(),
                        );
                    }
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("下載格式：");
                egui::ComboBox::from_id_salt("yt-format").width(80.0).selected_text(yt.format.label()).show_ui(ui, |ui| {
                    for f in OutFormat::ALL {
                        ui.selectable_value(&mut yt.format, f, f.label());
                    }
                });
                ui.label(RichText::new(yt.format.note()).small());
            });
            let size = match &yt.probe {
                Some(info) => match info.estimate(yt.format) {
                    Some(b) if !yt.ranged => format!("預估大小：約 {}", ytdlp::human_mb(b)),
                    Some(_) => "指定時間下載：大小依範圍而定".to_string(),
                    None => "無法估計大小".to_string(),
                },
                None => "大小待取得影片資訊".to_string(),
            };
            ui.label(RichText::new(size).small().weak());
        }

        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("直播聊天室（YouTube 直播 / 首播）").color(crate::ui::info()));
            ui.checkbox(&mut yt.chat, "下載直播聊天室紀錄");
            ui.add_enabled(yt.chat, egui::Checkbox::new(&mut yt.chat_only, "只下載聊天室（不下載影片）"));
            ui.add_enabled(yt.chat, egui::Checkbox::new(&mut yt.chat_txt, "另存易讀文字檔（.txt，含時間、名稱、Super Chat 金額）"));
            ui.label(
                RichText::new("原始紀錄存為 .live_chat.json。直播進行中請使用「只下載聊天室」，會持續記錄到直播結束；中途按取消也會保留已錄到的留言。")
                    .small()
                    .weak(),
            );
        });

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut yt.import, "完成後匯入資源庫：");
            ui.add_enabled_ui(yt.import, |ui| {
                ui.radio_value(&mut yt.link, true, "保留在下載位置");
                ui.radio_value(&mut yt.link, false, "複製進資源庫");
            });
        });

        ui.add_space(6.0);
        let (status, error) = &yt.status;
        if !status.is_empty() {
            ui.label(RichText::new(status).color(if *error { crate::ui::warning() } else { crate::ui::good() }));
        }
        if let Some(run) = &yt.running {
            let fraction = run.progress.as_ref().and_then(|p| p.fraction).unwrap_or(0.0);
            let text = run.progress.as_ref().map(speed_text).unwrap_or_else(|| "準備中…".into());
            ui.add(egui::ProgressBar::new(fraction).text(text).animate(run.progress.is_none()));
        }
        ui.columns(3, |cols| {
            let big = |ui: &mut Ui, text: &str, fill: Color32, enabled: bool| {
                ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(fill).min_size([ui.available_width(), 34.0].into()))
                    .clicked()
            };
            if big(&mut cols[0], "下載影片", crate::ui::SOLID_GOOD, !busy) {
                actions.push(YtAction::Download);
            }
            if big(&mut cols[1], "加入佇列", crate::ui::SOLID_INFO, true) {
                actions.push(YtAction::Enqueue);
            }
            if big(&mut cols[2], "取消工作", crate::ui::SOLID_DANGER, busy) {
                actions.push(YtAction::Cancel);
            }
        });

        ui.add_space(4.0);
        ui.label(format!("下載佇列：{} 項", yt.queue.len()));
        if !yt.queue.is_empty() {
            egui::Frame::none().fill(ui.visuals().extreme_bg_color).inner_margin(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (i, r) in yt.queue.iter().enumerate() {
                    if ui.selectable_label(yt.queue_sel == Some(i), format!("{}. {}", i + 1, r.label())).clicked() {
                        yt.queue_sel = Some(i);
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!yt.queue.is_empty() && !busy, egui::Button::new("開始佇列")).clicked() {
                actions.push(YtAction::RunQueue);
            }
            if ui.add_enabled(yt.queue_sel.is_some(), egui::Button::new("移除選取項目")).clicked() {
                if let Some(i) = yt.queue_sel.take().filter(|i| *i < yt.queue.len()) {
                    yt.queue.remove(i);
                }
            }
        });

        ui.add_space(6.0);
        ui.columns(3, |cols| {
            let wide = |ui: &mut Ui, text: &str, enabled: bool| {
                ui.add_enabled(enabled, egui::Button::new(text).min_size([ui.available_width(), 28.0].into())).clicked()
            };
            let installing = yt.install_rx.is_some();
            let label = if yt.check.as_ref().is_some_and(|c| c.ytdlp.is_none()) { "安裝 yt-dlp" } else { "更新 yt-dlp" };
            if wide(&mut cols[0], label, !installing && !busy) {
                actions.push(YtAction::Install);
            }
            ui_install_buttons(&mut cols[1], actions);
            if wide(&mut cols[2], "檢查工具", yt.check_rx.is_none()) {
                actions.push(YtAction::Check);
            }
        });
        ui.horizontal(|ui| {
            ui.label("FFmpeg 資料夾：");
            let shown = ffmpeg_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "未找到（自動尋找系統 PATH 與常見位置）".into());
            ui.label(RichText::new(shown).small());
            if ui.small_button("選擇").clicked() {
                actions.push(YtAction::PickFfmpeg);
            }
        });
        ui.horizontal(|ui| {
            let last = yt.last_files.first().cloned();
            if ui.add_enabled(last.is_some(), egui::Button::new("開啟完成檔案")).clicked() {
                if let Some(f) = last {
                    let _ = open::that_detached(f);
                }
            }
            if ui.button("開啟下載資料夾").clicked() {
                let dir = PathBuf::from(yt.out_dir.trim());
                let _ = std::fs::create_dir_all(&dir);
                let _ = open::that_detached(dir);
            }
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut yt.show_log, "顯示工作紀錄");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("儲存紀錄").clicked() {
                    actions.push(YtAction::SaveLog);
                }
                if ui.button("複製紀錄").clicked() {
                    ui.ctx().copy_text(yt.log.join("\n"));
                }
            });
        });
        if yt.show_log {
            egui::Frame::none().fill(ui.visuals().extreme_bg_color).inner_margin(6.0).rounding(4.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ScrollArea::vertical().id_salt("yt-log").max_height(200.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in &yt.log {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
            });
        }
        if let Some(dir) = ytdlp::tools_dir() {
            ui.label(RichText::new(format!("工具儲存位置：{}", dir.display())).small().weak());
        }
    }
}

enum YtAction {
    Check,
    Install,
    Probe,
    Download,
    Enqueue,
    RunQueue,
    Cancel,
    PickDir,
    PickFfmpeg,
    /// (display name, winget package id)
    Winget(&'static str, &'static str),
    SaveLog,
}

/// Installs a package with winget in a console of its own, where its prompts show.
pub(super) fn winget_install(name: &str, id: &str) -> Result<(), String> {
    let title = format!("安裝 {name}");
    crate::process::visible("cmd")
        .args(["/c", "start", &title, "cmd", "/k", &format!("winget install --id {id} -e")])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("無法執行 winget：{e}"))
}

fn ui_install_buttons(ui: &mut Ui, actions: &mut Vec<YtAction>) {
    ui.horizontal(|ui| {
        let w = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
        if ui.add(egui::Button::new("安裝 FFmpeg").min_size([w, 28.0].into())).on_hover_text("以 winget 安裝 Gyan.FFmpeg").clicked() {
            actions.push(YtAction::Winget("FFmpeg", "Gyan.FFmpeg"));
        }
        if ui.add(egui::Button::new("安裝 deno").min_size([w, 28.0].into())).on_hover_text("以 winget 安裝 DenoLand.Deno（YouTube 需要的 JavaScript 執行環境）").clicked() {
            actions.push(YtAction::Winget("deno", "DenoLand.Deno"));
        }
    });
}
