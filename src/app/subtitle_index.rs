//! Makes the dialogue of older video and audio items searchable: after a library opens,
//! subtitles of items not indexed yet are read on a background thread.

use super::App;
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc,
    },
};

pub struct SubtitleIndex {
    rx: Receiver<(String, String)>,
    done: Arc<AtomicUsize>,
    total: usize,
    pub(super) cancel: Arc<AtomicBool>,
}

impl Drop for SubtitleIndex {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Reads the first candidate file of each job that holds subtitles.
fn index_worker(jobs: Vec<(String, Vec<PathBuf>)>, tx: Sender<(String, String)>, done: &AtomicUsize, cancel: &AtomicBool, ctx: &egui::Context) {
    for (id, candidates) in jobs {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some(text) = candidates.iter().filter(|p| p.is_file()).find_map(|p| crate::media::subtitle_search_text(p)) {
            if tx.send((id, text)).is_err() {
                return;
            }
        }
        done.fetch_add(1, Ordering::Relaxed);
        ctx.request_repaint();
    }
}

impl App {
    pub(super) fn start_subtitle_index(&mut self) {
        self.subtitle_index = None;
        let Some(lib) = &self.lib else { return };
        let jobs = lib.subtitle_jobs();
        if jobs.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        let (done, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let (d, c, ctx, total) = (done.clone(), cancel.clone(), self.egui_ctx.clone(), jobs.len());
        std::thread::spawn(move || index_worker(jobs, tx, &d, &c, &ctx));
        self.subtitle_index = Some(SubtitleIndex { rx, done, total, cancel });
    }

    pub(super) fn poll_subtitle_index(&mut self) {
        let Some(job) = &self.subtitle_index else { return };
        let mut found = Vec::new();
        let mut disconnected = false;
        loop {
            match job.rx.try_recv() {
                Ok(entry) => found.push(entry),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if let Some(lib) = &mut self.lib {
            for (id, text) in found {
                // Not through undo: this only fills in what search can see.
                if let Some(it) = lib.items.get_mut(&id).filter(|it| it.text.is_none()) {
                    it.text = Some(text);
                    lib.touch(&id);
                }
            }
        }
        if disconnected {
            self.subtitle_index = None;
        }
    }

    /// Indexing under way, for the status bar's jobs list.
    pub(super) fn subtitle_index_job(&self) -> Option<super::jobs::BgJob> {
        let job = self.subtitle_index.as_ref()?;
        let done = job.done.load(Ordering::Relaxed);
        let progress = (job.total > 0).then(|| done as f32 / job.total as f32);
        Some(super::jobs::BgJob::new(super::jobs::JobKind::SubtitleIndex, t!("建立台詞索引"), t!("{done}/{total}，讓舊素材的台詞也能搜尋", done, total = job.total), progress))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_usable_candidate_and_can_stop() {
        let dir = std::env::temp_dir().join(format!("orangelink-subidx-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.srt"), "1\n00:00:01,000 --> 00:00:02,000\n台詞一\n").unwrap();
        std::fs::write(dir.join("empty.srt"), "").unwrap();
        let jobs = vec![
            ("a".to_string(), vec![dir.join("missing.srt"), dir.join("empty.srt"), dir.join("a.srt")]),
            ("b".to_string(), vec![dir.join("nothing.vtt")]),
        ];
        let (tx, rx) = channel();
        let (done, cancel) = (AtomicUsize::new(0), AtomicBool::new(false));
        index_worker(jobs.clone(), tx, &done, &cancel, &egui::Context::default());
        assert_eq!(rx.try_iter().collect::<Vec<_>>(), vec![("a".to_string(), "台詞一".to_string())]);
        assert_eq!(done.load(Ordering::Relaxed), 2, "items without subtitles still count as done");

        let (tx, rx) = channel();
        cancel.store(true, Ordering::Relaxed);
        index_worker(jobs, tx, &AtomicUsize::new(0), &cancel, &egui::Context::default());
        assert!(rx.try_recv().is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
