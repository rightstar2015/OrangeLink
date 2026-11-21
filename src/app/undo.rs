//! Undo and redo: item states before each change, and the library structure
//! (folders, smart folders, tag groups, fields) when that changed too.

use super::*;

/// The undo and redo stacks.
#[derive(Default)]
pub(super) struct Edits {
    pub(super) undo: Vec<UndoStep>,
    pub(super) redo: Vec<UndoStep>,
    /// Numbers the steps, so a toast's 復原 undoes only its own change.
    pub(super) seq: u64,
    /// Library structure as of `meta_rev`, to notice structural edits made anywhere.
    pub(super) meta_seen: Option<(u64, crate::model::LibraryMeta)>,
}

/// One undoable change: item states before it, and the library structure
/// (folders, smart folders, tag groups, fields) if that changed too.
pub(super) struct UndoStep {
    /// Tells this step apart, so a toast's 復原 undoes only its own change.
    pub(super) seq: u64,
    pub(super) time: f64,
    pub(super) items: Vec<Item>,
    pub(super) meta: Option<crate::model::LibraryMeta>,
}

impl App {
    /// Pushes an undo step. With `coalesce`, rapid edits of the same items
    /// (typing in the inspector) are merged into one step.
    pub fn record_undo(&mut self, before: Vec<Item>, coalesce: bool) {
        if before.is_empty() {
            return;
        }
        let ids: Vec<&String> = before.iter().map(|i| &i.id).collect();
        let merge = coalesce
            && self.edits.undo.last().is_some_and(|s| {
                s.meta.is_none()
                    && self.now - s.time < 1.5
                    && s.items.len() == ids.len()
                    && s.items.iter().zip(&ids).all(|(a, b)| a.id == **b)
            });
        if merge {
            self.edits.undo.last_mut().expect("checked").time = self.now;
        } else {
            self.push_undo(UndoStep { seq: 0, time: self.now, items: before, meta: None });
        }
    }

    pub(super) fn push_undo(&mut self, mut step: UndoStep) {
        self.edits.seq += 1;
        step.seq = self.edits.seq;
        self.edits.undo.push(step);
        if self.edits.undo.len() > 200 {
            self.edits.undo.remove(0);
        }
        self.edits.redo.clear();
    }

    pub fn can_undo(&self) -> bool {
        !self.edits.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.edits.redo.is_empty()
    }

    fn step_summary(step: &UndoStep) -> String {
        match (step.meta.is_some(), step.items.len()) {
            (true, 0) => "資料夾／分類結構".to_string(),
            (true, n) => format!("資料夾／分類結構與 {n} 個項目"),
            (false, n) => format!("{n} 個項目"),
        }
    }

    pub fn undo_tip(&self) -> String {
        match self.edits.undo.last() {
            Some(s) => format!("復原：{}（Ctrl+Z）", Self::step_summary(s)),
            None => "復原（Ctrl+Z）".to_string(),
        }
    }

    pub fn redo_tip(&self) -> String {
        match self.edits.redo.last() {
            Some(s) => format!("重做：{}（Ctrl+Y）", Self::step_summary(s)),
            None => "重做（Ctrl+Y）".to_string(),
        }
    }

    /// Records structural edits (folders, smart folders, tag groups, fields) made
    /// since the last frame, whichever code path made them. Runs once per frame.
    pub(super) fn track_structure(&mut self) {
        let Some(lib) = &mut self.lib else {
            self.edits.meta_seen = None;
            return;
        };
        let captured = std::mem::take(&mut lib.captured);
        match &self.edits.meta_seen {
            Some((rev, _)) if *rev == lib.meta_rev => {
                if !captured.is_empty() {
                    self.push_undo(UndoStep { seq: 0, time: self.now, items: captured, meta: None });
                }
            }
            Some((_, before)) => {
                let before = before.clone();
                let changed = !before.same_structure(&lib.meta);
                self.edits.meta_seen = Some((lib.meta_rev, lib.meta.clone()));
                if changed {
                    self.push_undo(UndoStep { seq: 0, time: self.now, items: captured, meta: Some(before) });
                } else if !captured.is_empty() {
                    self.push_undo(UndoStep { seq: 0, time: self.now, items: captured, meta: None });
                }
            }
            None => self.edits.meta_seen = Some((lib.meta_rev, lib.meta.clone())),
        }
    }

    /// Swaps the stored item states with the current ones (undo or redo).
    pub(super) fn restore_items(&mut self, redo: bool) {
        // A structural edit made earlier this frame becomes its own step first,
        // so undo neither swallows it nor has it clear the redo just made.
        self.track_structure();
        let step = if redo { self.edits.redo.pop() } else { self.edits.undo.pop() };
        let Some(step) = step else {
            self.toast(if redo { "沒有可以重做的操作" } else { "沒有可以復原的操作" });
            return;
        };
        let Some(lib) = &mut self.lib else { return };
        let mut current = Vec::new();
        for item in step.items {
            let id = item.id.clone();
            if let Some(slot) = lib.items.get_mut(&id) {
                current.push(std::mem::replace(slot, item));
                lib.touch(&id);
            }
        }
        let n = current.len();
        let structural = step.meta.is_some();
        let meta = step.meta.map(|m| {
            let old = std::mem::replace(&mut lib.meta, m);
            lib.touch_meta();
            old
        });
        // The restored structure is the new baseline, not a fresh edit.
        self.edits.meta_seen = Some((lib.meta_rev, lib.meta.clone()));
        let still_exists = match &self.view {
            View::Folder(id) => lib.meta.folders.iter().any(|f| f.id == *id),
            View::Smart(id) => lib.meta.smart_folders.iter().any(|f| f.id == *id),
            _ => true,
        };
        if !still_exists {
            self.view = View::All;
        }
        // An item that went back to the trash (or out of it) is no longer in
        // view; leaving it selected would let the next batch edit change it.
        let in_trash = matches!(self.view, View::Trash);
        self.selected.retain(|id| lib.items.get(id).is_some_and(|it| it.deleted == in_trash));
        self.edits.seq += 1;
        let back = UndoStep { seq: self.edits.seq, time: f64::NEG_INFINITY, items: current, meta };
        if redo {
            self.edits.undo.push(back);
        } else {
            self.edits.redo.push(back);
        }
        let what = match (structural, n) {
            (true, 0) => "資料夾／分類結構".to_string(),
            (true, n) => format!("資料夾／分類結構與 {n} 個項目"),
            (false, n) => format!("{n} 個項目"),
        };
        self.toast(format!("{}（{what}）", if redo { "已重做" } else { "已復原" }));
    }
}
