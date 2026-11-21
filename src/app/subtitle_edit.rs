//! Proofreading a video's subtitles in the 字幕 tab: fixing words and times,
//! merging and splitting lines. Every change is written back to the subtitle
//! file at once (see `media::save_cues`), and can be taken back with 復原.

use super::App;
use crate::media::{edit_cues, save_cues, Cue, CueEdit};

/// How many subtitle changes 復原 can take back.
const UNDO_STEPS: usize = 50;

#[derive(Default)]
pub struct SubEdit {
    /// The lines picked, by index into the cues: from `anchor` to `last`.
    pub anchor: Option<usize>,
    pub last: Option<usize>,
    /// The text box of the one picked line, and the line it was filled from.
    pub draft: String,
    pub draft_for: Option<usize>,
    /// The cues before each change, newest last.
    pub undo: Vec<Vec<Cue>>,
    /// 尋找與取代 and 全部時間位移.
    pub find: String,
    pub replace: String,
    pub shift: f64,
    pub tools_open: bool,
}

impl SubEdit {
    /// The picked lines, first to last.
    pub fn picked(&self) -> Option<(usize, usize)> {
        let (a, b) = (self.anchor?, self.last.or(self.anchor)?);
        Some((a.min(b), a.max(b)))
    }

    /// Picks one line, or with `extend` the lines from the anchor to it.
    pub fn pick(&mut self, i: usize, extend: bool) {
        if extend && self.anchor.is_some() {
            self.last = Some(i);
        } else {
            self.anchor = Some(i);
            self.last = None;
        }
    }

    fn forget_picks(&mut self, len: usize) {
        if self.picked().is_none_or(|(_, b)| b >= len) {
            self.anchor = None;
            self.last = None;
        }
        self.draft_for = None;
    }
}

impl App {
    /// Applies `edit` to the shown subtitles and writes them to their file.
    pub(crate) fn edit_subtitles(&mut self, edit: CueEdit) {
        let Some(subs) = self.subs.as_mut() else { return };
        let Some(path) = subs.path.clone() else { return };
        let mut cues = subs.cues.clone();
        if !edit_cues(&mut cues, &edit) {
            return;
        }
        let before = std::mem::replace(&mut subs.cues, cues);
        subs.edit.undo.push(before);
        if subs.edit.undo.len() > UNDO_STEPS {
            subs.edit.undo.remove(0);
        }
        self.write_subtitles(&path);
    }

    /// Takes back the last subtitle change.
    pub(crate) fn undo_subtitles(&mut self) {
        let Some(subs) = self.subs.as_mut() else { return };
        let (Some(path), Some(before)) = (subs.path.clone(), subs.edit.undo.pop()) else { return };
        subs.cues = before;
        self.write_subtitles(&path);
    }

    fn write_subtitles(&mut self, path: &std::path::Path) {
        let Some(subs) = self.subs.as_mut() else { return };
        subs.matches = None;
        subs.edit.forget_picks(subs.cues.len());
        let id = subs.id.clone();
        match save_cues(path, &subs.cues) {
            Ok(false) => {}
            Ok(true) => {
                // A subtitle that is itself in the library: its size and hash
                // follow, so the library check doesn't call it changed.
                if let Some(lib) = &mut self.lib {
                    let own = lib.items.values().find(|it| !it.deleted && lib.file_path(it) == path).map(|it| it.id.clone());
                    if let Some(own) = own {
                        let (size, hash) = (std::fs::metadata(path).map(|m| m.len()), crate::imaging::quick_hash(path));
                        if let (Some(it), Ok(size), Ok(hash)) = (lib.items.get_mut(&own), size, hash) {
                            it.size = size;
                            it.hash = hash;
                            lib.touch(&own);
                        }
                    }
                }
                self.index_subtitles(&id, path);
            }
            Err(e) => {
                // The change stays on screen but not on disk: say so, and take it back.
                self.toast_error(t!("無法儲存字幕：{e}", e));
                if let Some(subs) = self.subs.as_mut() {
                    if let Some(before) = subs.edit.undo.pop() {
                        subs.cues = before;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_lines_one_or_a_run() {
        let mut e = SubEdit::default();
        assert_eq!(e.picked(), None);
        e.pick(5, true);
        assert_eq!(e.picked(), Some((5, 5)), "nothing to extend from: picks the one");
        e.pick(2, true);
        assert_eq!(e.picked(), Some((2, 5)), "back to an earlier line");
        e.pick(7, false);
        assert_eq!(e.picked(), Some((7, 7)));
        e.forget_picks(7);
        assert_eq!(e.picked(), None, "a pick past the last line is dropped");
    }
}
