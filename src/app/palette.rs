//! Ctrl+K: one box to go to a folder, smart folder or tag, or to run a
//! command; with items selected, to file them into a folder or tag them.

use super::*;
use egui::{pos2, Align, Layout, RichText, ScrollArea, TextEdit};

#[derive(Default)]
pub struct Palette {
    pub query: String,
    /// The highlighted line.
    pub pick: usize,
    /// The box has been focused once.
    focused: bool,
    /// Where the box was drawn last frame; a click outside it closes it.
    pub rect: Option<egui::Rect>,
}

pub struct Entry {
    pub label: String,
    /// What the line does, shown dim on the right.
    pub what: &'static str,
    pub action: Action,
}

/// What the library offers the palette, gathered once per opening frame.
pub struct Sources<'a> {
    pub folders: &'a [(String, String, usize)],
    pub smart: Vec<(String, String)>,
    pub tags: &'a [(String, usize)],
    pub selected: Vec<String>,
    /// Extension actions for the item in the preview (else the selected ones):
    /// (label, folder, action), and the items they run on.
    pub extensions: Vec<(String, std::path::PathBuf, String)>,
    pub ext_targets: Vec<String>,
}

const MAX_SHOWN: usize = 40;

/// The lines for `query`, best first; with no query, the commands then the
/// folders in their order.
pub fn entries(query: &str, src: &Sources) -> Vec<Entry> {
    let mut all: Vec<Entry> = vec![
        Entry { label: t!("全部").into(), what: tk!("前往"), action: Action::SetView(View::All) },
        Entry { label: t!("未分類").into(), what: tk!("前往"), action: Action::SetView(View::Uncategorized) },
        Entry { label: t!("未標籤").into(), what: tk!("前往"), action: Action::SetView(View::Untagged) },
        Entry { label: t!("垃圾桶").into(), what: tk!("前往"), action: Action::SetView(View::Trash) },
        Entry { label: t!("匯入檔案…").into(), what: tk!("指令"), action: Action::ImportFiles },
        Entry { label: t!("匯入資料夾…").into(), what: tk!("指令"), action: Action::ImportFolder },
        Entry { label: t!("設定").into(), what: tk!("指令"), action: Action::OpenSettings },
    ];
    for (label, dir, action) in &src.extensions {
        all.push(Entry { label: format!("🧩 {label}"), what: tk!("擴充套件"), action: Action::RunExtension { dir: dir.clone(), action: action.clone(), ids: src.ext_targets.clone() } });
    }
    let filing = !src.selected.is_empty();
    for (id, name, _) in src.folders {
        all.push(Entry { label: format!("📁 {name}"), what: tk!("資料夾"), action: Action::SetView(View::Folder(id.clone())) });
        if filing {
            all.push(Entry { label: t!("把選取的加入「{name}」", name).to_string(), what: tk!("分類"), action: Action::AddToFolder(src.selected.clone(), id.clone()) });
        }
    }
    for (id, name) in &src.smart {
        all.push(Entry { label: format!("✨ {name}"), what: tk!("智慧資料夾"), action: Action::SetView(View::Smart(id.clone())) });
    }
    for (tag, _) in src.tags {
        all.push(Entry { label: format!("# {tag}"), what: tk!("標籤篩選"), action: Action::ToggleTagFilter(tag.clone()) });
        if filing {
            all.push(Entry { label: t!("為選取的加上標籤「{tag}」", tag).to_string(), what: tk!("分類"), action: Action::AddTag(src.selected.clone(), tag.clone()) });
        }
    }
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        // Filing lines only once something is typed: they would double the list.
        all.retain(|e| e.what != tk!("分類"));
        all.truncate(MAX_SHOWN);
        return all;
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(i32, usize, Entry)> = all
        .into_iter()
        .enumerate()
        .filter_map(|(i, e)| {
            let score = words.iter().map(|w| filter::fuzzy_score(w, &e.label)).sum::<Option<i32>>()?;
            Some((score, i, e))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(MAX_SHOWN).map(|(_, _, e)| e).collect()
}

impl App {
    pub(super) fn open_palette(&mut self) {
        self.palette = Some(Palette::default());
    }

    pub(super) fn palette_window(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.palette.take() else { return };
        let Some(lib) = &self.lib else { return };
        let smart: Vec<(String, String)> = lib.meta.smart_folders.iter().filter(|s| !s.group).map(|s| (s.id.clone(), s.name.clone())).collect();
        let mut selected: Vec<String> = self.selected.iter().cloned().collect();
        selected.sort();
        let ext_targets: Vec<String> = match &self.viewer {
            Some(v) => vec![v.id.clone()],
            None => selected.clone(),
        };
        // Offered when the first target fits; the run skips any other that does not.
        let extensions: Vec<(String, std::path::PathBuf, String)> = ext_targets
            .first()
            .and_then(|id| lib.items.get(id))
            .map(|it| self.extension_actions(it.kind, it.duration))
            .unwrap_or_default()
            .into_iter()
            .map(|(dir, action, label)| (label, dir, action))
            .collect();
        let (close, picked) = self.with_sidebar_stats(|stats| {
            let src = Sources { folders: &stats.folder_flat, smart, tags: &stats.tag_counts, selected, extensions, ext_targets };
            let found = entries(&p.query, &src);
            p.pick = p.pick.min(found.len().saturating_sub(1));
            let (up, down, enter, esc) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                )
            });
            if up {
                p.pick = p.pick.saturating_sub(1);
            }
            if down && p.pick + 1 < found.len() {
                p.pick += 1;
            }
            let mut picked = enter.then_some(p.pick).filter(|i| *i < found.len());
            let mut close = esc || picked.is_some();
            let screen = ctx.screen_rect();
            let area = egui::Area::new(egui::Id::new("command-palette"))
                .order(egui::Order::Foreground)
                .fixed_pos(pos2(screen.center().x - 260.0, screen.top() + 90.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).inner_margin(10.0).show(ui, |ui| {
                        ui.set_width(520.0);
                        let r = ui.add(TextEdit::singleline(&mut p.query).hint_text(t!("前往資料夾、標籤或執行指令…")).desired_width(f32::INFINITY));
                        if !p.focused {
                            r.request_focus();
                            p.focused = true;
                        }
                        if r.changed() {
                            p.pick = 0;
                        }
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                            if found.is_empty() {
                                ui.label(RichText::new(t!("沒有符合的項目")).weak());
                            }
                            for (i, e) in found.iter().enumerate() {
                                let row = ui.horizontal(|ui| {
                                    let label = ui.add(egui::SelectableLabel::new(i == p.pick, &e.label));
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        ui.label(RichText::new(crate::i18n::tr(e.what)).small().weak());
                                    });
                                    label
                                });
                                if i == p.pick && (up || down) {
                                    row.inner.scroll_to_me(None);
                                }
                                if row.inner.clicked() {
                                    picked = Some(i);
                                    close = true;
                                }
                            }
                        });
                        ui.label(RichText::new(t!("↑↓ 選擇　Enter 執行　Esc 關閉")).small().weak());
                    });
                });
            // A click outside closes it in `raw_input_hook` (which also eats that click);
            // this is where the hook finds the box.
            p.rect = Some(area.response.rect);
            (close, picked.and_then(|i| found.into_iter().nth(i)).map(|e| e.action))
        });
        if !close {
            self.palette = Some(p);
        }
        if let Some(a) = picked {
            self.act(a);
        }
    }
}

/// A click that lands away from the open palette closes it, like Esc, and is
/// removed from `events` along with its release, so it does not also press
/// whatever is underneath. `swallow` carries "the release is still to come"
/// between frames. Returns whether the palette closes.
pub fn eat_outside_click(events: &mut Vec<egui::Event>, palette: Option<egui::Rect>, swallow: &mut bool) -> bool {
    let mut open = palette;
    let mut closed = false;
    events.retain(|e| {
        let egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, .. } = e else { return true };
        if *pressed && open.is_some_and(|r| !r.contains(*pos)) {
            open = None;
            closed = true;
            *swallow = true;
            return false;
        }
        if !*pressed && *swallow {
            *swallow = false;
            return false;
        }
        true
    });
    closed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicking_outside_the_palette_closes_it_and_the_click_goes_no_further() {
        let r = egui::Rect::from_min_size(pos2(100.0, 100.0), egui::vec2(520.0, 300.0));
        let click = |p: egui::Pos2, pressed| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut swallow = false;
        let mut ev = vec![click(pos2(10.0, 10.0), true)];
        assert!(eat_outside_click(&mut ev, Some(r), &mut swallow));
        assert!(ev.is_empty(), "the press must not reach the button underneath");
        // The release comes a frame later, with the palette already gone.
        let mut ev = vec![click(pos2(10.0, 10.0), false)];
        assert!(!eat_outside_click(&mut ev, None, &mut swallow));
        assert!(ev.is_empty(), "and neither must the release");
        assert!(!swallow);
        // Later clicks are the app's again.
        let mut ev = vec![click(pos2(10.0, 10.0), true), click(pos2(10.0, 10.0), false)];
        assert!(!eat_outside_click(&mut ev, None, &mut swallow));
        assert_eq!(ev.len(), 2);
        // Inside the box stays open and untouched.
        let mut ev = vec![click(pos2(200.0, 200.0), true)];
        assert!(!eat_outside_click(&mut ev, Some(r), &mut swallow));
        assert_eq!(ev.len(), 1);
        assert!(!eat_outside_click(&mut Vec::new(), Some(r), &mut swallow), "no click, no close");
    }

    fn src<'a>(folders: &'a [(String, String, usize)], tags: &'a [(String, usize)], selected: &[&str]) -> Sources<'a> {
        Sources { folders, smart: vec![("s1".into(), "待處理的直播".into())], tags, selected: selected.iter().map(|s| s.to_string()).collect(), extensions: Vec::new(), ext_targets: Vec::new() }
    }

    #[test]
    fn ctrl_k_finds_folders_tags_and_commands_by_a_few_letters() {
        let folders = vec![("f1".into(), "Opening Draft".into(), 0), ("f2".into(), "表情包".into(), 1)];
        let tags = vec![("boss".to_string(), 3)];
        let s = src(&folders, &tags, &[]);
        let first = |q: &str| entries(q, &s).into_iter().next().map(|e| e.action);
        assert!(matches!(first("opd"), Some(Action::SetView(View::Folder(id))) if id == "f1"));
        assert!(matches!(first("表情"), Some(Action::SetView(View::Folder(id))) if id == "f2"));
        assert!(matches!(first("boss"), Some(Action::ToggleTagFilter(t)) if t == "boss"));
        assert!(matches!(first("直播"), Some(Action::SetView(View::Smart(id))) if id == "s1"));
        assert!(entries("zzzz", &s).is_empty());
        assert!(entries("", &s).iter().all(|e| e.what != tk!("分類")));
    }

    #[test]
    fn with_items_selected_ctrl_k_files_them() {
        let folders = vec![("f1".into(), "表情包".into(), 0)];
        let tags = vec![("boss".to_string(), 3)];
        let s = src(&folders, &tags, &["a", "b"]);
        let found = entries("表情包", &s);
        assert!(found.iter().any(|e| matches!(&e.action, Action::AddToFolder(ids, f) if ids == &["a", "b"] && f == "f1")));
        let found = entries("boss", &s);
        assert!(found.iter().any(|e| matches!(&e.action, Action::AddTag(ids, t) if ids.len() == 2 && t == "boss")));
        assert!(entries("", &s).iter().all(|e| !matches!(e.action, Action::AddToFolder(..))), "not before something is typed");
    }

    #[test]
    fn ctrl_k_runs_an_extension_on_the_item_in_view() {
        let mut s = src(&[], &[], &[]);
        s.extensions = vec![("Demo：找精華".into(), "D:/ext".into(), "find".into())];
        s.ext_targets = vec!["i1".into()];
        let found = entries("找精華", &s);
        assert!(matches!(&found[0].action, Action::RunExtension { action, ids, .. } if action == "find" && ids == &["i1"]));
    }
}
