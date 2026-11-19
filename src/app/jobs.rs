//! Everything running in the background, gathered for the status bar's one jobs
//! button: imports, downloads, speech to text, the subtitle index, loudness
//! analysis and slow thumbnails. Each module reports its own job; this file adds
//! the time left where the job cannot tell, and cancels by kind.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum JobKind {
    Import,
    Ytdlp,
    Whisper,
    SubtitleIndex,
    Loudness,
    Thumbnails,
    ClipExport,
    Stream,
    Update,
    FileCopy,
    Capture,
    Integrity,
}

impl JobKind {
    /// A stable name for the job's kind, as `get_status` reports it to AI assistants.
    pub fn key(self) -> &'static str {
        match self {
            JobKind::Import => "import",
            JobKind::Ytdlp => "download",
            JobKind::Whisper => "subtitles",
            JobKind::SubtitleIndex => "subtitle_index",
            JobKind::Loudness => "loudness",
            JobKind::Thumbnails => "thumbnails",
            JobKind::ClipExport => "clip_export",
            JobKind::Stream => "stream_processing",
            JobKind::Update => "update",
            JobKind::FileCopy => "file_export",
            JobKind::Capture => "frame_capture",
            JobKind::Integrity => "library_check",
        }
    }
}

pub struct BgJob {
    pub kind: JobKind,
    pub name: String,
    pub detail: String,
    /// 0..=1, when the job knows how far along it is.
    pub progress: Option<f32>,
    /// Seconds left, when it can be told.
    pub eta: Option<f64>,
    pub can_cancel: bool,
    /// The job has a window of its own to open.
    pub has_window: bool,
}

impl BgJob {
    pub fn new(kind: JobKind, name: impl Into<String>, detail: impl Into<String>, progress: Option<f32>) -> Self {
        Self { kind, name: name.into(), detail: detail.into(), progress, eta: None, can_cancel: true, has_window: false }
    }
}

/// Thumbnails waiting this long count as a job; shorter waits are just scrolling.
const THUMBS_AFTER: f64 = 1.5;
/// The estimate of the time left starts once a job has run this long.
const ETA_AFTER: f64 = 3.0;

/// Seconds left at the pace so far: `p0` at `t0`, `p` at `now`.
pub fn eta(t0: f64, p0: f32, now: f64, p: f32) -> Option<f64> {
    let (dt, dp) = (now - t0, (p - p0) as f64);
    (dt >= ETA_AFTER && dp > 0.01 && p < 1.0).then(|| dt / dp * (1.0 - p as f64))
}

impl App {
    /// The jobs running now, in the order the popover lists them.
    pub fn background_jobs(&mut self) -> Vec<BgJob> {
        let mut jobs = Vec::new();
        if let Some((done, total)) = self.import.as_ref().and_then(|r| r.progress) {
            let (detail, progress) = if total == 0 { ("掃描檔案中…".to_string(), None) } else { (format!("{done}/{total}"), Some(done as f32 / total as f32)) };
            jobs.push(BgJob::new(JobKind::Import, "匯入", detail, progress));
        }
        jobs.extend(self.ytdlp_job());
        jobs.extend(self.whisper_job());
        jobs.extend(self.subtitle_index_job());
        jobs.extend(self.loudness_job());
        jobs.extend(self.stream_job());
        jobs.extend(self.clip_export_job());
        jobs.extend(self.file_copy_job());
        jobs.extend(self.capture_job());
        jobs.extend(self.integrity_job());
        jobs.extend(self.update_job());
        let now = self.now;
        if self.thumbs.pending.is_empty() {
            self.thumbs.busy_since = None;
        } else {
            let since = *self.thumbs.busy_since.get_or_insert(now);
            if now - since >= THUMBS_AFTER {
                let mut job = BgJob::new(JobKind::Thumbnails, "產生縮圖", format!("還有 {} 個", self.thumbs.pending.len()), None);
                job.can_cancel = false;
                jobs.push(job);
            }
        }
        // The time left, from the pace since each job was first seen.
        self.job_clock.retain(|k, _| jobs.iter().any(|j| j.kind == *k));
        for job in &mut jobs {
            let Some(p) = job.progress else { continue };
            let clock = self.job_clock.entry(job.kind).or_insert((now, p));
            if p < clock.1 {
                // A new job of the same kind (the next download in the queue).
                *clock = (now, p);
            }
            if job.eta.is_none() {
                job.eta = eta(clock.0, clock.1, now, p);
            }
        }
        jobs
    }

    pub fn cancel_job(&mut self, kind: JobKind) {
        match kind {
            JobKind::Import => {
                if let Some(run) = &self.import {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
            JobKind::Ytdlp => self.cancel_ytdlp(),
            JobKind::Whisper => self.cancel_whisper(),
            JobKind::SubtitleIndex => self.subtitle_index = None,
            JobKind::Loudness => self.cancel_loudness(),
            JobKind::ClipExport => self.cancel_clip_queue(),
            JobKind::Stream => self.cancel_stream(),
            JobKind::Update => self.cancel_update(),
            JobKind::FileCopy | JobKind::Capture => self.cancel_file_jobs(kind),
            JobKind::Integrity => self.cancel_integrity(),
            JobKind::Thumbnails => {}
        }
    }

    pub fn show_job(&mut self, kind: JobKind) {
        match kind {
            JobKind::Ytdlp => self.yt.open = true,
            JobKind::Whisper => self.show_whisper(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::eta;

    #[test]
    fn time_left_follows_the_pace_once_there_is_one() {
        // Too early, or no progress yet: no guess.
        assert_eq!(eta(0.0, 0.0, 1.0, 0.1), None);
        assert_eq!(eta(0.0, 0.2, 10.0, 0.2), None);
        // 25% in 10 s: 30 s to go.
        assert!((eta(0.0, 0.0, 10.0, 0.25).unwrap() - 30.0).abs() < 1e-6);
        // Started later in the job: only the pace since counts.
        assert!((eta(5.0, 0.5, 15.0, 0.75).unwrap() - 10.0).abs() < 1e-6);
    }
}
