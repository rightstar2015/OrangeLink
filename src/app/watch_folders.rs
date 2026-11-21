//! Starting imports, and watch folders: file system events plus the periodic rescan.

use super::*;

/// Watching and rescanning the library's watch folders.
pub struct WatchState {
    pub(super) last_scan: f64,
    pub(super) watcher: Option<crate::watch::Watcher>,
    /// Settled files from the watcher, waiting for the import queue to be free.
    pub(super) ready: Vec<(PathBuf, bool)>,
    /// Watch folders waiting for their periodic scan (taken from the end).
    pub(super) scan_queue: Vec<WatchFolder>,
}

impl Default for WatchState {
    fn default() -> Self {
        // The first scan is due at once.
        Self { last_scan: f64::NEG_INFINITY, watcher: None, ready: Vec::new(), scan_queue: Vec::new() }
    }
}

impl App {
    pub(super) fn start_import(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, link: bool, folder: Option<String>, tags: Vec<String>, silent: bool) {
        let Some(lib) = &self.lib else { return };
        if paths.is_empty() {
            return;
        }
        if self.import_running() {
            self.import_queue.push_back(ImportRequest { paths, link, folder, tags, silent });
            if !silent {
                self.toast(t!("已排在目前的匯入之後"));
            }
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let opts = ImportOptions {
            link,
            // A rule's folder may have been deleted since.
            folder: folder.filter(|f| lib.folder_name(f).is_some()),
            tags,
            // Watch scans must always skip known files, whatever the setting.
            skip_duplicates: self.config.skip_duplicates || silent,
            ffmpeg_dir: self.ffmpeg_dir.clone(),
            cancel: cancel.clone(),
        };
        let known = lib.known_keys(silent);
        let rx = importer::spawn(lib.root.clone(), paths, opts, known, ctx.clone());
        self.import = Some(ImportRun {
            rx,
            cancel,
            silent,
            progress: if silent { None } else { Some((0, 0)) },
            imported: 0,
            duplicates: 0,
            failed: 0,
            errors: Vec::new(),
            new: Vec::new(),
        });
    }

    /// Keeps the file system watcher in step with the library's watch folders, and
    /// imports the files it reports once nothing else is importing.
    pub(super) fn poll_watcher(&mut self, ctx: &egui::Context) {
        let want: Vec<WatchFolder> = self.lib.as_ref().map(|l| l.meta.watch_folders.iter().filter(|w| w.path.is_dir()).cloned().collect()).unwrap_or_default();
        // Only the paths and modes matter to the watcher, not the rules.
        let key = |v: &[WatchFolder]| v.iter().map(|w| (w.path.clone(), w.link)).collect::<Vec<_>>();
        if self.watch.watcher.as_ref().map(|w| key(&w.folders)) != Some(key(&want)) {
            self.watch.watcher = (!want.is_empty()).then(|| crate::watch::Watcher::start(want.clone(), ctx.clone())).flatten();
            self.watch.ready.clear();
        }
        if let Some(w) = &self.watch.watcher {
            self.watch.ready.extend(w.rx.try_iter());
        }
        if self.import_running() {
            return;
        }
        if !self.watch.ready.is_empty() {
            // One watch folder's files at a time, with its rule.
            let rule = |p: &PathBuf| want.iter().filter(|w| p.starts_with(&w.path)).max_by_key(|w| w.path.as_os_str().len()).cloned();
            let first = rule(&self.watch.ready[0].0);
            let (batch, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.watch.ready).into_iter().partition(|(p, _)| rule(p).map(|w| w.path) == first.as_ref().map(|w| w.path.clone()));
            self.watch.ready = rest;
            let mut paths: Vec<PathBuf> = batch.into_iter().map(|(p, _)| p).collect();
            paths.dedup();
            let Some(w) = first else { return };
            self.start_import(ctx, paths, w.link, w.folder, w.tags, true);
            return;
        }
        if let Some(w) = self.watch.scan_queue.pop() {
            self.start_import(ctx, vec![w.path], w.link, w.folder, w.tags, true);
        }
    }

    /// Imports new files from every watch folder, one folder after another.
    pub(super) fn scan_watch_folders(&mut self, _ctx: &egui::Context) {
        self.watch.last_scan = self.now;
        let Some(lib) = &self.lib else { return };
        for w in lib.meta.watch_folders.iter().filter(|w| w.path.is_dir()) {
            if !self.watch.scan_queue.iter().any(|q| q.path == w.path) {
                self.watch.scan_queue.insert(0, w.clone());
            }
        }
    }

    /// Before a large import: what was dropped, counted on a background thread.
    pub(super) fn preview_import(&mut self, paths: Vec<PathBuf>) {
        let Some(lib) = &self.lib else { return };
        let scan = Arc::new(std::sync::Mutex::new(crate::importer::ImportScan::default()));
        let (known, inputs, s, ctx) = (lib.known_keys(false), paths.clone(), scan.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || crate::importer::scan(inputs, known, s, ctx));
        let folder = match &self.view {
            View::Folder(f) if self.config.import_into_current_folder => Some(f.clone()),
            _ => None,
        };
        let link = self.config.import_mode == ImportMode::Link;
        self.dialog = Some(Dialog::ImportPreview { paths, link, folder, tags: String::new(), scan });
    }
}
