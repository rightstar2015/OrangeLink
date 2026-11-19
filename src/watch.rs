//! Watch folders through file system events: a new file is handed over for import once
//! it has stopped changing, so half-copied or still-downloading files are left alone.
//! The periodic rescan in the app stays as a fallback for missed events.

use crate::model::WatchFolder;
use eframe::egui;
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{channel, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

/// How long a path must stay unchanged before it is imported.
const QUIET: Duration = Duration::from_secs(2);

pub struct Watcher {
    _watcher: notify::RecommendedWatcher,
    pub folders: Vec<WatchFolder>,
    /// Settled paths, with whether their watch folder links instead of copying.
    pub rx: Receiver<(PathBuf, bool)>,
}

impl Watcher {
    /// Watches the folders that exist; None when the system refuses every one of them.
    pub fn start(folders: Vec<WatchFolder>, ctx: egui::Context) -> Option<Self> {
        let (event_tx, event_rx) = channel::<PathBuf>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                    for p in ev.paths {
                        let _ = event_tx.send(p);
                    }
                }
            }
        })
        .ok()?;
        let watched = folders.iter().filter(|w| watcher.watch(&w.path, RecursiveMode::Recursive).is_ok()).count();
        if watched == 0 {
            return None;
        }
        let (tx, rx) = channel();
        let roots = folders.clone();
        // Ends when the watcher is dropped, which drops the event sender.
        thread::spawn(move || {
            // path -> (last change seen, size at that time)
            let mut pending: HashMap<PathBuf, (Instant, Option<u64>)> = HashMap::new();
            loop {
                match event_rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(p) => {
                        if !crate::media::is_junk(&p) {
                            pending.insert(p, (Instant::now(), None));
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                let mut ready = Vec::new();
                pending.retain(|p, (since, size)| {
                    if since.elapsed() < QUIET {
                        return true;
                    }
                    let Ok(meta) = std::fs::metadata(p) else { return false }; // gone again
                    let now = (!meta.is_dir()).then_some(meta.len());
                    // A file must keep its size across two checks and be readable.
                    if meta.is_dir() || (*size == now && std::fs::File::open(p).is_ok()) {
                        ready.push(p.clone());
                        return false;
                    }
                    (*since, *size) = (Instant::now(), now);
                    true
                });
                for p in ready {
                    let link = roots.iter().filter(|w| p.starts_with(&w.path)).max_by_key(|w| w.path.as_os_str().len()).map(|w| w.link);
                    if let Some(link) = link {
                        if tx.send((p, link)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                }
            }
        });
        Some(Self { _watcher: watcher, folders, rx })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_file_is_handed_over_once_it_stops_growing() {
        let dir = std::env::temp_dir().join(format!("orangelink-watch-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let w = Watcher::start(vec![WatchFolder { path: dir.clone(), link: true, folder: None, tags: Vec::new() }], egui::Context::default()).unwrap();
        let start = Instant::now();
        let file = dir.join("new.png");
        let mut f = std::fs::File::create(&file).unwrap();
        // Still being written for a while: nothing yet.
        for _ in 0..6 {
            f.write_all(&[0u8; 1000]).unwrap();
            f.flush().unwrap();
            thread::sleep(Duration::from_millis(400));
        }
        assert!(w.rx.try_recv().is_err(), "not while it is still growing");
        drop(f);
        std::fs::write(dir.join("download.crdownload"), b"x").unwrap();
        let (path, link) = w.rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(start.elapsed() >= QUIET);
        assert_eq!((path.file_name().unwrap().to_str().unwrap(), link), ("new.png", true));
        assert!(w.rx.recv_timeout(Duration::from_secs(4)).is_err(), "once, and no partial downloads");
        drop(w);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
