//! 「檢查資源庫」: leftovers of interrupted imports, items that name folders
//! that are gone, and files that changed since they were imported, each with a
//! fix. The files are compared on a thread of its own; it shows in the status
//! bar's jobs and can be cancelled.

use super::*;
use crate::library::{check_integrity, Integrity};
use std::sync::{atomic::AtomicUsize, mpsc::channel};

pub struct IntegrityRun {
    rx: Receiver<Integrity>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    total: usize,
    /// Found, waiting for another dialog to close.
    found: Option<Integrity>,
}

/// What the user picked in the dialog.
pub enum IntegrityFix {
    /// Leftover item folders to the Recycle Bin.
    RemoveLeftovers,
    /// Folder ids that no folder has any more.
    DropFolderRefs,
    /// Lists the changed items in the grid.
    ShowChanged,
    /// Takes the changed files as they are now (their hash is updated).
    AcceptChanged,
}

impl App {
    pub(super) fn start_integrity_check(&mut self) {
        if self.integrity.is_some() {
            return self.toast(t!("正在檢查資源庫"));
        }
        if self.import.is_some() {
            // An item being imported looks just like a leftover until it is done.
            return self.toast(t!("匯入進行中，請等匯入完成後再檢查"));
        }
        let Some(lib) = &self.lib else { return };
        let input = lib.integrity_input();
        let total = input.total();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let (c, d, ctx) = (cancel.clone(), done.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let found = check_integrity(input, &c, &d);
            if !c.load(Ordering::Relaxed) {
                let _ = tx.send(found);
            }
            ctx.request_repaint();
        });
        self.integrity = Some(IntegrityRun { rx, cancel, done, total, found: None });
    }

    pub(super) fn poll_integrity(&mut self) {
        let Some(run) = &mut self.integrity else { return };
        if run.found.is_none() {
            match state::poll(&run.rx) {
                Reply::Pending => return,
                Reply::Done(found) => run.found = Some(found),
                Reply::Lost => {
                    let cancelled = run.cancel.load(Ordering::Relaxed);
                    self.integrity = None;
                    if !cancelled {
                        self.toast_error(t!("檢查資源庫失敗：{state}", state = crate::i18n::tr(state::LOST)));
                    }
                    return;
                }
            }
        }
        if self.dialog.is_some() {
            return;
        }
        let Some(found) = self.integrity.take().and_then(|r| r.found) else { return };
        let refs = self.lib.as_ref().map_or(0, |l| l.missing_folder_refs());
        if found.is_clean() && refs == 0 {
            let note = match found.unreadable_files {
                0 => String::new(),
                n => t!("（{n} 個檔案無法讀取，沒有比對）", n),
            };
            return self.toast(t!("資源庫沒有發現問題{note}", note));
        }
        self.dialog = Some(Dialog::Integrity(found));
    }

    pub(super) fn integrity_job(&self) -> Option<jobs::BgJob> {
        let run = self.integrity.as_ref()?;
        if run.found.is_some() {
            return None;
        }
        let done = run.done.load(Ordering::Relaxed).min(run.total);
        Some(jobs::BgJob::new(jobs::JobKind::Integrity, t!("檢查資源庫"), format!("{done}/{}", run.total), Some(done as f32 / run.total.max(1) as f32)))
    }

    pub(super) fn cancel_integrity(&mut self) {
        if let Some(run) = self.integrity.take() {
            run.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Applies a fix from the dialog; what it fixed is taken out of `found`.
    pub(crate) fn apply_integrity_fix(&mut self, fix: IntegrityFix, found: &mut Integrity) {
        let Some(lib) = &mut self.lib else { return };
        match fix {
            IntegrityFix::RemoveLeftovers => {
                if self.import.is_some() {
                    return self.toast(t!("匯入進行中，請等匯入完成後再處理"));
                }
                let all = std::mem::take(&mut found.leftovers);
                let (removed, failed) = remove_leftovers(all, |id| lib.items.contains_key(id), |dir| {
                    trash::delete(dir).map_err(|e| {
                        crate::log::error("integrity", format!("{}: {e}", dir.display()));
                        e.to_string()
                    })
                });
                let n = failed.len();
                found.leftovers = failed;
                if n > 0 {
                    self.toast_error(t!("有 {n} 個資料夾無法移到資源回收筒（可能正被其他程式使用）", n));
                } else {
                    self.toast(t!("已把 {removed} 個殘留的資料夾移到資源回收筒", removed));
                }
            }
            IntegrityFix::DropFolderRefs => {
                let n = lib.drop_missing_folders();
                self.toast(t!("已整理 {n} 個素材的資料夾", n));
            }
            IntegrityFix::ShowChanged => {
                let ids = found.changed.iter().map(|(id, _)| id.clone()).collect();
                self.act(Action::SetView(View::Ids { label: t!("內容和匯入時不同").into(), ids, groups: Vec::new(), source: IdsSource::Plain }));
            }
            IntegrityFix::AcceptChanged => {
                let n = lib.accept_hashes(std::mem::take(&mut found.changed));
                self.toast(t!("已更新 {n} 個素材的比對資料", n));
            }
        }
    }
}

/// Moves leftover item folders out with `delete`. One that became an item
/// since the check (`is_item` knows its id, or it has metadata now) is left
/// alone. Returns how many went, and the ones `delete` failed on.
fn remove_leftovers(dirs: Vec<PathBuf>, is_item: impl Fn(&str) -> bool, mut delete: impl FnMut(&std::path::Path) -> Result<(), String>) -> (usize, Vec<PathBuf>) {
    let (mut removed, mut failed) = (0, Vec::new());
    for dir in dirs {
        let id = dir.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if is_item(&id) || dir.join(crate::library::META_FILE).exists() {
            continue;
        }
        match delete(&dir) {
            Ok(()) => removed += 1,
            Err(_) => failed.push(dir),
        }
    }
    (removed, failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftovers_that_became_items_stay() {
        let dir = std::env::temp_dir().join(format!("orangelink-leftovers-{}", crate::model::new_id()));
        let d = |id: &str| dir.join(format!("{id}.info"));
        for id in ["gone", "imported", "has-meta", "locked"] {
            std::fs::create_dir_all(d(id)).unwrap();
        }
        std::fs::write(d("has-meta").join(crate::library::META_FILE), "{}").unwrap();
        let mut asked = Vec::new();
        let (removed, failed) = remove_leftovers(
            vec![d("gone"), d("imported"), d("has-meta"), d("locked")],
            |id| id == "imported",
            |p| {
                asked.push(p.to_path_buf());
                if p.ends_with("locked.info") { Err("in use".into()) } else { Ok(()) }
            },
        );
        assert_eq!(removed, 1);
        assert_eq!(failed, [d("locked")], "kept to try again");
        assert_eq!(asked, [d("gone"), d("locked")], "an item now, or with metadata now: not touched");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
