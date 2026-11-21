//! File work that used to stop the window: copying the selected files out
//! ("匯出") and saving a video frame with FFmpeg. Both run on threads of their
//! own, show in the status bar's jobs and can be cancelled.

use super::*;
use std::{
    path::Path,
    process::Stdio,
    sync::mpsc::{channel, Sender},
    time::Duration,
};

/// FFmpeg gets this long to write one frame (a remote disk, a huge file).
const FRAME_TIMEOUT: Duration = Duration::from_secs(60);

/// The file work in flight.
#[derive(Default)]
pub struct FileJobs {
    pub copy: Option<FileCopy>,
    pub captures: Vec<Capture>,
}

/// Copying the selected items' files into a folder.
pub struct FileCopy {
    rx: Receiver<bool>,
    cancel: Arc<AtomicBool>,
    total: usize,
    ok: usize,
    failed: usize,
    dest: PathBuf,
}

/// A frame being written to `out`; it is imported and linked to `id` when done.
/// With `as_thumb`, it becomes `id`'s thumbnail instead, and the thread sends
/// the thumbnail's palette.
pub struct Capture {
    id: String,
    pos: f64,
    out: PathBuf,
    rx: Receiver<Result<Vec<crate::model::PaletteColor>, String>>,
    cancel: Arc<AtomicBool>,
    as_thumb: bool,
}

/// Copies each `(from, to)`, sending whether it worked; stops when `cancel` is set.
fn copy_files(files: Vec<(PathBuf, PathBuf)>, tx: Sender<bool>, cancel: Arc<AtomicBool>, ctx: egui::Context) {
    for (from, to) in files {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let ok = std::fs::copy(&from, &to).is_ok();
        if !ok {
            let _ = std::fs::remove_file(&to);
        }
        if tx.send(ok).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}

/// Makes the frame at `pos` of `input` the thumbnail at `thumb`, by way of
/// the full-size frame in `tmp` (removed after); returns the new palette.
fn frame_to_thumbnail(ffmpeg: &Path, input: &Path, pos: f64, tmp: &Path, thumb: &Path, cancel: &AtomicBool) -> Result<Vec<crate::model::PaletteColor>, String> {
    let made = write_frame(ffmpeg, input, pos, tmp, cancel).and_then(|()| crate::imaging::decode_file(tmp).map_err(|e| e.to_string()));
    let _ = std::fs::remove_file(tmp);
    let frame = made?;
    if cancel.load(Ordering::Relaxed) {
        return Err(t!("已取消").to_string());
    }
    crate::imaging::save_thumbnail(&frame, thumb).map_err(|e| e.to_string())?;
    Ok(crate::imaging::extract_palette(&crate::imaging::make_thumbnail(&frame)))
}

/// Writes the frame at `pos` of `input` to `out`. At or past the end, where
/// FFmpeg finds no frame and writes nothing, it takes the last frame instead.
fn write_frame(ffmpeg: &Path, input: &Path, pos: f64, out: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let first = run_frame(ffmpeg, &["-ss", &format!("{pos:.3}")], input, &["-frames:v", "1"], out, cancel);
    match first {
        Err(FrameError::Nothing) if pos > 0.0 => {
            // The last second, each frame over the one before: the file ends up
            // with the final frame.
            run_frame(ffmpeg, &["-sseof", "-1"], input, &[], out, cancel).map_err(|e| e.to_string())
        }
        r => r.map_err(|e| e.to_string()),
    }
}

enum FrameError {
    /// FFmpeg ran but wrote no picture (the time is past the end).
    Nothing,
    Failed(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FrameError::Nothing => f.write_str(t!("FFmpeg 在這個時間點找不到畫面")),
            FrameError::Failed(e) => f.write_str(e),
        }
    }
}

/// Runs FFmpeg once to write a still to `out`, giving up after `FRAME_TIMEOUT`
/// or when cancelled.
fn run_frame(ffmpeg: &Path, before: &[&str], input: &Path, after: &[&str], out: &Path, cancel: &AtomicBool) -> Result<(), FrameError> {
    let _ = std::fs::remove_file(out);
    let mut child = media::tool(ffmpeg, "ffmpeg")
        .args(["-v", "error", "-y"])
        .args(before)
        .arg("-i")
        .arg(input)
        .args(after)
        .args(["-update", "1"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| FrameError::Failed(e.to_string()))?;
    // Read stderr on the side so a chatty FFmpeg can't fill the pipe and stall.
    let mut stderr = child.stderr.take();
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = std::io::Read::read_to_string(e, &mut s);
        }
        s
    });
    let start = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| FrameError::Failed(e.to_string()))? {
            break Some(status);
        }
        if cancel.load(Ordering::Relaxed) || start.elapsed() > FRAME_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let err = err_reader.join().unwrap_or_default();
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    match status {
        Some(s) if s.success() && size > 0 => Ok(()),
        _ => {
            let _ = std::fs::remove_file(out);
            if status.is_none() {
                return Err(FrameError::Failed(if cancel.load(Ordering::Relaxed) { t!("已取消").into() } else { t!("FFmpeg 超過 {n} 秒沒有完成", n = FRAME_TIMEOUT.as_secs()) }));
            }
            match err.lines().rev().find(|l| !l.trim().is_empty()) {
                Some(line) => Err(FrameError::Failed(line.trim().to_string())),
                None => Err(FrameError::Nothing),
            }
        }
    }
}

impl App {
    /// Asks for a folder, then copies the selected items' files there in the background.
    pub(super) fn export_selected(&mut self) {
        let ids = self.selected_ids();
        if ids.is_empty() {
            return;
        }
        if self.files.copy.is_some() {
            self.toast(t!("正在匯出，請等目前的匯出完成"));
            return;
        }
        let Some(dest) = rfd::FileDialog::new().set_title(t!("匯出到資料夾")).pick_folder() else { return };
        let Some(lib) = &self.lib else { return };
        // Names are chosen here, so two items of the same name get two files.
        let mut taken = HashSet::new();
        let files: Vec<(PathBuf, PathBuf)> = ids
            .iter()
            .filter_map(|id| lib.items.get(id))
            .map(|it| {
                let mut target = unique_path(&dest.join(format!("{}.{}", sanitize_file_name(&it.name), it.ext)));
                let mut n = 2;
                while !taken.insert(target.clone()) {
                    target = unique_path(&dest.join(format!("{} ({n}).{}", sanitize_file_name(&it.name), it.ext)));
                    n += 1;
                }
                (lib.file_path(it), target)
            })
            .collect();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let total = files.len();
        let (c, ctx) = (cancel.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || copy_files(files, tx, c, ctx));
        self.files.copy = Some(FileCopy { rx, cancel, total, ok: 0, failed: 0, dest });
    }

    pub(super) fn poll_file_copy(&mut self) {
        let Some(job) = &mut self.files.copy else { return };
        loop {
            match job.rx.try_recv() {
                Ok(true) => job.ok += 1,
                Ok(false) => job.failed += 1,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        let job = self.files.copy.take().expect("checked");
        let skipped = job.total - job.ok - job.failed;
        let mut msg = t!("已匯出 {job} 個檔案", job = job.ok);
        if skipped > 0 {
            msg += &t!("，取消了 {skipped} 個", skipped);
        }
        if job.failed > 0 {
            msg += &t!("，{failed} 個失敗", failed = job.failed);
            self.toast_error(msg);
        } else {
            self.toast_with(msg, t!("開啟資料夾"), Action::OpenPath(job.dest));
        }
    }

    pub(super) fn file_copy_job(&self) -> Option<jobs::BgJob> {
        let job = self.files.copy.as_ref()?;
        let done = job.ok + job.failed;
        Some(jobs::BgJob::new(jobs::JobKind::FileCopy, t!("匯出檔案"), format!("{done}/{}", job.total), Some(done as f32 / job.total.max(1) as f32)))
    }

    /// Starts saving the video frame at `pos` as a PNG in `captures/`; it is
    /// imported when written. Returns where it will be.
    pub(crate) fn capture_frame(&mut self, id: &str, pos: f64) -> Result<PathBuf, String> {
        let result = self.start_capture(id, pos);
        if let Err(e) = &result {
            self.toast(t!("擷取畫面失敗：{e}", e));
        }
        result
    }

    /// Starts making the video frame at `pos` the item's thumbnail.
    pub(crate) fn frame_as_thumbnail(&mut self, id: &str, pos: f64) {
        if let Err(e) = self.start_thumb_capture(id, pos) {
            self.toast(t!("無法設定縮圖：{e}", e));
        }
    }

    fn start_thumb_capture(&mut self, id: &str, pos: f64) -> Result<(), String> {
        let lib = self.lib.as_ref().ok_or(t!("沒有開啟資源庫"))?;
        let ff = self.ffmpeg_dir.clone().ok_or(t!("找不到 FFmpeg"))?;
        let it = lib.items.get(id).ok_or(t!("找不到項目"))?;
        let (input, thumb) = (lib.file_path(it), lib.thumb_path(id));
        let out = std::env::temp_dir().join(format!("orangelink-frame-{}.png", crate::model::new_id()));
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (o, c, ctx) = (out.clone(), cancel.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(frame_to_thumbnail(&ff, &input, pos, &o, &thumb, &c));
            ctx.request_repaint();
        });
        self.files.captures.push(Capture { id: id.to_string(), pos, out, rx, cancel, as_thumb: true });
        Ok(())
    }

    fn start_capture(&mut self, id: &str, pos: f64) -> Result<PathBuf, String> {
        let lib = self.lib.as_ref().ok_or(t!("沒有開啟資源庫"))?;
        let ff = self.ffmpeg_dir.clone().ok_or(t!("找不到 FFmpeg"))?;
        let it = lib.items.get(id).ok_or(t!("找不到項目"))?;
        let dir = lib.root.join("captures");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let stamp = crate::media::fmt_duration(pos).replace(':', "-");
        let mut out = unique_path(&dir.join(format!("{}_{stamp}.png", sanitize_file_name(&it.name))));
        // Two captures of the same moment before either is written.
        let mut n = 2;
        while self.files.captures.iter().any(|c| c.out == out) {
            out = unique_path(&dir.join(format!("{}_{stamp} ({n}).png", sanitize_file_name(&it.name))));
            n += 1;
        }
        let input = lib.file_path(it);
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let (o, c, ctx) = (out.clone(), cancel.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(write_frame(&ff, &input, pos, &o, &c).map(|()| Vec::new()));
            ctx.request_repaint();
        });
        self.files.captures.push(Capture { id: id.to_string(), pos, out: out.clone(), rx, cancel, as_thumb: false });
        Ok(out)
    }

    pub(super) fn poll_captures(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.files.captures.len() {
            let result = match state::poll(&self.files.captures[i].rx) {
                Reply::Pending => {
                    i += 1;
                    continue;
                }
                Reply::Done(r) => r,
                Reply::Lost => Err(state::LOST.to_string()),
            };
            let cap = self.files.captures.remove(i);
            match result {
                Ok(palette) if cap.as_thumb => {
                    self.thumbs.tex.remove(&cap.id);
                    self.thumbs.failed.remove(&cap.id);
                    if let Some(it) = self.lib.as_mut().and_then(|l| l.items.get_mut(&cap.id)) {
                        it.palette = palette;
                    }
                    if let Some(lib) = self.lib.as_mut() {
                        lib.touch(&cap.id);
                    }
                    self.toast(t!("已把 {pos} 的畫面設為縮圖", pos = crate::media::fmt_duration(cap.pos)));
                }
                Ok(_) => {
                    if let Some(key) = importer::path_key(&cap.out) {
                        self.links.link_to.insert(key, LinkTo::Item(cap.id.clone()));
                    }
                    let msg = t!("已擷取畫面 {pos}，匯入中", pos = crate::media::fmt_duration(cap.pos));
                    self.toast_with(msg, t!("在檔案總管中顯示"), Action::RevealPath(cap.out.clone()));
                    self.start_import(ctx, vec![cap.out], false, None, Vec::new(), false);
                }
                // Cancelled from the jobs list: not a failure.
                Err(_) if cap.cancel.load(Ordering::Relaxed) => self.toast(t!("已取消擷取畫面")),
                Err(e) => {
                    crate::log::error("capture", format!("{} 的 {}：{e}", cap.out.display(), crate::media::fmt_precise(cap.pos)));
                    self.toast(t!("擷取畫面失敗：{e}", e));
                }
            }
        }
    }

    pub(super) fn capture_job(&self) -> Option<jobs::BgJob> {
        let n = self.files.captures.len();
        (n > 0).then(|| jobs::BgJob::new(jobs::JobKind::Capture, t!("擷取畫面"), if n > 1 { t!("{n} 張", n) } else { t!("FFmpeg 處理中").to_string() }, None))
    }

    pub(super) fn cancel_file_jobs(&mut self, kind: jobs::JobKind) {
        match kind {
            jobs::JobKind::FileCopy => {
                if let Some(job) = &self.files.copy {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
            jobs::JobKind::Capture => {
                for c in &self.files.captures {
                    c.cancel.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_cancelled_capture_is_not_reported_as_failed() {
        let ctx = egui::Context::default();
        let mut app = App::with_config(&ctx, crate::config::Config::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        app.files.captures.push(Capture { id: "v".into(), pos: 1.0, out: std::env::temp_dir().join("never.png"), rx, cancel, as_thumb: false });
        app.cancel_file_jobs(jobs::JobKind::Capture);
        tx.send(Err("已取消".into())).unwrap();
        app.poll_captures(&ctx);
        assert!(app.files.captures.is_empty());
        let msgs: Vec<&str> = app.toast.list.iter().map(|t| t.msg.as_str()).collect();
        assert!(msgs.iter().all(|m| !m.contains("失敗")), "{msgs:?}");
    }

    /// A time at or past the end still gives a picture: the last frame.
    #[test]
    fn a_frame_past_the_end_is_the_last_one() {
        let Some(ff) = crate::media::ffmpeg_for_test("a_frame_past_the_end_is_the_last_one") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-frame-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let made = media::tool(&ff, "ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=3", "-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success());
        let no = AtomicBool::new(false);
        for pos in [1.0, 3.0, 99999.0] {
            let out = dir.join(format!("{pos}.png"));
            write_frame(&ff, &src, pos, &out, &no).unwrap_or_else(|e| panic!("{pos}: {e}"));
            assert!(fs::metadata(&out).unwrap().len() > 0, "{pos}");
        }
        assert!(write_frame(&ff, &dir.join("missing.mp4"), 1.0, &dir.join("x.png"), &no).is_err());
        assert!(!dir.join("x.png").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// 設為縮圖 replaces the item's thumbnail with the frame shown, and
    /// leaves no full-size frame behind.
    #[test]
    fn the_frame_shown_becomes_the_thumbnail() {
        let Some(ff) = crate::media::ffmpeg_for_test("the_frame_shown_becomes_the_thumbnail") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-thumbframe-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let made = media::tool(&ff, "ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:duration=3", "-c:v", "libx264", "-preset", "ultrafast"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success());
        let (tmp, thumb) = (dir.join("frame.png"), dir.join("_thumbnail.png"));
        fs::write(&thumb, b"old").unwrap();
        let palette = frame_to_thumbnail(&ff, &src, 1.5, &tmp, &thumb, &AtomicBool::new(false)).unwrap();
        assert!(!palette.is_empty());
        assert!(!tmp.exists(), "the full-size frame is removed");
        let img = crate::imaging::decode_file(&thumb).unwrap();
        assert!(img.width() <= crate::imaging::THUMB_SIDE && img.width() > 0, "{}", img.width());
        assert!(frame_to_thumbnail(&ff, &dir.join("missing.mp4"), 1.0, &tmp, &thumb, &AtomicBool::new(false)).is_err());
        assert!(crate::imaging::decode_file(&thumb).is_ok(), "a failed try keeps the thumbnail it had");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copies_until_cancelled() {
        let dir = std::env::temp_dir().join(format!("orangelink-copy-{}", crate::model::new_id()));
        fs::create_dir_all(dir.join("out")).unwrap();
        let files: Vec<(PathBuf, PathBuf)> = (0..3)
            .map(|i| {
                let from = dir.join(format!("{i}.bin"));
                fs::write(&from, [i as u8; 10]).unwrap();
                (from, dir.join("out").join(format!("{i}.bin")))
            })
            .chain([(dir.join("missing.bin"), dir.join("out").join("missing.bin"))])
            .collect();
        let (tx, rx) = channel();
        copy_files(files.clone(), tx, Arc::new(AtomicBool::new(false)), egui::Context::default());
        let sent: Vec<bool> = rx.try_iter().collect();
        assert_eq!(sent, [true, true, true, false]);
        assert_eq!(fs::read(dir.join("out").join("2.bin")).unwrap(), [2u8; 10]);
        assert!(!dir.join("out").join("missing.bin").exists());

        let (tx, rx) = channel();
        copy_files(files, tx, Arc::new(AtomicBool::new(true)), egui::Context::default());
        assert_eq!(rx.try_iter().count(), 0, "cancelled before the first file");
        fs::remove_dir_all(&dir).unwrap();
    }
}
