//! Back and forward through the views visited: folder, tag, smart folder,
//! similarity results or search, with their filters, scroll position and
//! selection. A new view is recorded at once; a changed search or filter in
//! the same view once it has stayed put for a moment, so typing is one step.

use super::*;

/// What the grid shows.
#[derive(Clone, PartialEq)]
pub struct NavFilter {
    view: View,
    search: String,
    tags: Vec<String>,
    exts: Vec<String>,
    rating: u8,
    color: Option<[u8; 3]>,
}

struct NavEntry {
    filter: NavFilter,
    scroll: f32,
    selected: Vec<String>,
}

/// Seconds a changed search or filter has to stay before it is a step.
const SETTLE: f64 = 1.2;
const MAX_STEPS: usize = 50;
/// Larger selections are not remembered (copied every few frames otherwise).
const MAX_SELECTED: usize = 1000;

#[derive(Default)]
pub struct History {
    back: Vec<NavEntry>,
    forward: Vec<NavEntry>,
    /// The state shown now, as last recorded, with its scroll and selection.
    current: Option<NavFilter>,
    scroll: f32,
    selected: Vec<String>,
    changed_at: Option<f64>,
}

impl History {
    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

impl App {
    fn nav_filter(&self) -> NavFilter {
        NavFilter {
            view: self.view.clone(),
            search: self.search.trim().to_string(),
            tags: self.tag_filter.clone(),
            exts: self.ext_filter.clone(),
            rating: self.min_rating,
            color: self.color_filter,
        }
    }

    /// Called every frame: records the state being left when the view changes.
    pub(super) fn track_history(&mut self) {
        if self.lib.is_none() {
            return;
        }
        let live = self.nav_filter();
        let h = &mut self.history;
        let Some(current) = &h.current else {
            h.current = Some(live);
            return;
        };
        if *current == live {
            h.changed_at = None;
            h.scroll = self.grid_scroll;
            // The selection, cheaply: only when it changed size or now and then.
            let n = self.selected.len();
            if n <= MAX_SELECTED && (n != h.selected.len() || self.frame.is_multiple_of(15)) {
                h.selected = self.selected.iter().cloned().collect();
            } else if n > MAX_SELECTED {
                h.selected.clear();
            }
            return;
        }
        if current.view == live.view {
            let since = *h.changed_at.get_or_insert(self.now);
            if self.now - since < SETTLE {
                self.egui_ctx.request_repaint_after(std::time::Duration::from_secs_f64(SETTLE));
                return;
            }
        }
        let left = NavEntry { filter: current.clone(), scroll: h.scroll, selected: std::mem::take(&mut h.selected) };
        // An empty search on its way to a word is not worth a step back.
        let trivial = left.filter.view == live.view && left.filter.search.is_empty() && !live.search.is_empty() && h.back.last().is_some_and(|b| b.filter.view == live.view);
        if !trivial {
            h.back.push(left);
            if h.back.len() > MAX_STEPS {
                h.back.remove(0);
            }
        }
        h.forward.clear();
        let search = live.search.clone();
        h.current = Some(live);
        h.changed_at = None;
        h.scroll = 0.0;
        // A search that stayed put long enough to be a step is worth listing.
        self.remember_search(&search);
    }

    pub(super) fn go_back(&mut self) {
        if let Some(entry) = self.history.back.pop() {
            let here = self.history_here();
            self.history.forward.push(here);
            self.restore(entry);
        }
    }

    pub(super) fn go_forward(&mut self) {
        if let Some(entry) = self.history.forward.pop() {
            let here = self.history_here();
            self.history.back.push(here);
            self.restore(entry);
        }
    }

    fn history_here(&self) -> NavEntry {
        let selected = if self.selected.len() <= MAX_SELECTED { self.selected.iter().cloned().collect() } else { Vec::new() };
        NavEntry { filter: self.nav_filter(), scroll: self.grid_scroll, selected }
    }

    fn restore(&mut self, entry: NavEntry) {
        let f = entry.filter.clone();
        self.view = f.view.clone();
        self.search = f.search.clone();
        self.tag_filter = f.tags.clone();
        self.ext_filter = f.exts.clone();
        self.min_rating = f.rating;
        self.color_filter = f.color;
        let lib = self.lib.as_ref();
        self.selected = entry.selected.iter().filter(|id| lib.is_some_and(|l| l.items.contains_key(*id))).cloned().collect();
        self.anchor = entry.selected.last().cloned();
        self.caret = self.anchor.clone();
        // Through close_viewer: it stops the player, which would play on unseen.
        self.close_viewer(false);
        self.restore_scroll = Some(entry.scroll);
        let h = &mut self.history;
        h.current = Some(f);
        h.scroll = entry.scroll;
        h.selected = entry.selected;
        h.changed_at = None;
    }

    /// A new library starts with no history.
    pub(super) fn clear_history(&mut self) {
        self.history = History::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn going_back_from_a_preview_closes_it_with_its_player() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        let filter = app.nav_filter();
        app.history.back.push(NavEntry { filter, scroll: 0.0, selected: Vec::new() });
        app.viewer = Some(Viewer { id: "a".into(), zoom: 1.0, pan: egui::Vec2::ZERO });
        // What close_viewer clears along with the player: the I/O marks of the video.
        app.mark = (Some(3.0), Some(5.0));
        app.go_back();
        assert!(app.viewer.is_none());
        assert_eq!(app.mark, (None, None), "the preview was closed, not just hidden");
        assert!(app.player.is_none());
    }
}
