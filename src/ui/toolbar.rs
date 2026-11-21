//! The toolbar: import and search on the left, one "篩選" popover in the middle,
//! sort, view and settings on the right, and a row of the conditions in effect.

use super::*;
use crate::grouping::GroupBy;
use crate::model::Item;
use std::collections::HashMap;

/// Colours offered in the filter popover: the common hues and the neutrals.
const SWATCHES: [[u8; 3]; 12] = [
    [230, 60, 60],
    [245, 140, 40],
    [240, 210, 50],
    [90, 190, 80],
    [60, 190, 190],
    [60, 120, 230],
    [140, 80, 210],
    [235, 110, 170],
    [140, 90, 50],
    [245, 245, 245],
    [128, 128, 128],
    [20, 20, 20],
];

/// Formats in the library, grouped by kind, with how many items have each.
pub type FormatGroups = Vec<(MediaKind, Vec<(String, usize)>)>;

pub fn format_groups<'a>(items: impl Iterator<Item = &'a Item>) -> FormatGroups {
    let mut counts: HashMap<(MediaKind, String), usize> = HashMap::new();
    for it in items.filter(|it| !it.deleted) {
        *counts.entry((it.kind, it.ext.clone())).or_default() += 1;
    }
    MediaKind::ALL
        .iter()
        .map(|k| {
            let mut exts: Vec<(String, usize)> = counts.iter().filter(|((kind, _), _)| kind == k).map(|((_, e), n)| (e.clone(), *n)).collect();
            exts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            (*k, exts)
        })
        .filter(|(_, exts)| !exts.is_empty())
        .collect()
}

/// Most recent searches kept.
const RECENT_SEARCHES: usize = 10;
const PINNED_SEARCHES: usize = 30;

impl App {
    /// Puts a search at the top of the recent list (pinned ones stay where they are).
    pub fn remember_search(&mut self, q: &str) {
        let q = q.trim();
        if q.is_empty() || self.config.pinned_searches.iter().any(|p| p == q) {
            return;
        }
        if self.config.recent_searches.first().is_some_and(|r| r == q) {
            return;
        }
        let recent = &mut self.config.recent_searches;
        // Typing on after a pause: the longer search replaces the last.
        if recent.first().is_some_and(|r| q.starts_with(r.as_str())) {
            recent.remove(0);
        }
        recent.retain(|r| r != q);
        recent.insert(0, q.to_string());
        recent.truncate(RECENT_SEARCHES);
        self.config.save();
    }

    /// Below the search box while it has focus: pinned and recent searches,
    /// those containing what is typed when it is not one of them.
    fn search_suggestions(&mut self, ui: &mut Ui, search: &egui::Response) {
        let id = ui.make_persistent_id("search-suggestions");
        let has_any = !self.config.recent_searches.is_empty() || !self.config.pinned_searches.is_empty();
        if has_any && (search.gained_focus() || search.clicked() || (search.changed() && search.has_focus())) {
            ui.memory_mut(|m| m.open_popup(id));
        }
        if !ui.memory(|m| m.is_popup_open(id)) {
            return;
        }
        let typed = self.search.trim().to_lowercase();
        let wanted = |s: &String| typed.is_empty() || s.to_lowercase().contains(&typed) && s.to_lowercase() != typed;
        let pinned: Vec<String> = self.config.pinned_searches.iter().filter(|s| wanted(s)).cloned().collect();
        let recent: Vec<String> = self.config.recent_searches.iter().filter(|s| wanted(s)).cloned().collect();
        if pinned.is_empty() && recent.is_empty() {
            // Closed, not left open with nothing drawn: an open popup takes Esc and
            // the next click. Typing opens it again.
            ui.memory_mut(|m| m.close_popup());
            return;
        }
        enum Do {
            Use(String),
            Pin(String),
            Unpin(String),
            Forget(String),
            Save(String),
        }
        let mut action = None;
        egui::popup::popup_below_widget(ui, id, search, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(search.rect.width().max(260.0));
            let row = |ui: &mut Ui, q: &String, pinned: bool, action: &mut Option<Do>| {
                ui.horizontal(|ui| {
                    let star = if pinned { "★" } else { "☆" };
                    let tip = if pinned { t!("取消釘選") } else { t!("釘選，一直留在清單上") };
                    if ui.add(egui::Button::new(star).frame(false)).on_hover_text(tip).clicked() {
                        *action = Some(if pinned { Do::Unpin(q.clone()) } else { Do::Pin(q.clone()) });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pinned {
                            if ui.small_button("✨").on_hover_text(t!("存成智慧資料夾")).clicked() {
                                *action = Some(Do::Save(q.clone()));
                            }
                        } else if ui.small_button("✖").on_hover_text(t!("從最近搜尋移除")).clicked() {
                            *action = Some(Do::Forget(q.clone()));
                        }
                        let w = ui.available_width();
                        let r = ui.add_sized([w, 18.0], egui::Button::new(RichText::new(q)).frame(false).truncate());
                        if r.clicked() {
                            *action = Some(Do::Use(q.clone()));
                        }
                    });
                });
            };
            if !pinned.is_empty() {
                ui.label(RichText::new(t!("釘選的搜尋")).small().weak());
                for q in &pinned {
                    row(ui, q, true, &mut action);
                }
            }
            if !recent.is_empty() {
                if !pinned.is_empty() {
                    ui.separator();
                }
                ui.label(RichText::new(t!("最近搜尋")).small().weak());
                for q in &recent {
                    row(ui, q, false, &mut action);
                }
            }
        });
        let Some(action) = action else { return };
        let c = &mut self.config;
        match action {
            Do::Use(q) => {
                self.search = q.clone();
                ui.memory_mut(|m| m.close_popup());
                self.remember_search(&q);
                return;
            }
            Do::Pin(q) => {
                c.recent_searches.retain(|r| *r != q);
                if !c.pinned_searches.contains(&q) {
                    c.pinned_searches.push(q);
                    c.pinned_searches.truncate(PINNED_SEARCHES);
                }
            }
            Do::Unpin(q) => {
                c.pinned_searches.retain(|p| *p != q);
                c.recent_searches.insert(0, q);
                c.recent_searches.truncate(RECENT_SEARCHES);
            }
            Do::Forget(q) => c.recent_searches.retain(|r| *r != q),
            Do::Save(q) => {
                self.search = q;
                ui.memory_mut(|m| m.close_popup());
                self.act(Action::NewSmartIn { parent: None, group: false });
                return;
            }
        }
        self.config.save();
        // Keep the list open after pinning or removing.
        ui.memory_mut(|m| m.open_popup(id));
    }

    /// Conditions set in the filter popover (formats, rating, colour).
    pub fn filter_count(&self) -> usize {
        usize::from(!self.ext_filter.is_empty()) + usize::from(self.min_rating > 0) + usize::from(self.color_filter.is_some())
    }

    pub fn clear_filters(&mut self) {
        self.tag_filter.clear();
        self.ext_filter.clear();
        self.min_rating = 0;
        self.color_filter = None;
    }

    /// The formats list, rebuilt only when the library changes.
    fn cached_formats(&self) -> FormatGroups {
        let Some(lib) = &self.lib else { return Vec::new() };
        let mut cache = self.format_cache.borrow_mut();
        if cache.0 != Some(lib.revision) {
            *cache = (Some(lib.revision), format_groups(lib.items.values()));
        }
        cache.1.clone()
    }

    pub(super) fn toolbar_left(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        if ui.add_enabled(self.history.can_go_back(), egui::Button::new("←").frame(false)).on_hover_text(t!("上一個檢視（Alt + ←、滑鼠側鍵）")).clicked() {
            self.act(Action::NavBack);
        }
        if ui.add_enabled(self.history.can_go_forward(), egui::Button::new("→").frame(false)).on_hover_text(t!("下一個檢視（Alt + →）")).clicked() {
            self.act(Action::NavForward);
        }
        ui.separator();
        if ui
            .add_enabled(self.can_undo(), egui::Button::new("↩").frame(false))
            .on_hover_text(self.undo_tip())
            .on_disabled_hover_text(t!("沒有可以復原的操作（Ctrl+Z）"))
            .clicked()
        {
            self.act(Action::Undo);
        }
        if ui
            .add_enabled(self.can_redo(), egui::Button::new("↪").frame(false))
            .on_hover_text(self.redo_tip())
            .on_disabled_hover_text(t!("沒有可以重做的操作（Ctrl+Y）"))
            .clicked()
        {
            self.act(Action::Redo);
        }
        ui.separator();
        ui.menu_button(RichText::new(t!("＋ 匯入")).strong(), |ui| {
            if ui.button(t!("匯入檔案…")).clicked() {
                self.act(Action::ImportFiles);
                ui.close_menu();
            }
            if ui.button(t!("匯入資料夾…")).clicked() {
                self.act(Action::ImportFolder);
                ui.close_menu();
            }
            if ui.button(t!("從網址下載（yt-dlp）…")).clicked() {
                self.act(Action::OpenYtdlp);
                ui.close_menu();
            }
            ui.separator();
            ui.label(RichText::new(t!("也可以直接把檔案拖進視窗")).small().weak());
        });

        let search = ui.add(
            TextEdit::singleline(&mut self.search)
                .hint_text(t!("🔍 搜尋名稱、標籤、字幕…（如 ext:png rating:4）"))
                // Grows with the window, leaving room for the filter and the right side.
                .desired_width((ui.available_width() - 620.0).clamp(150.0, 400.0)),
        );
        let search = if search.has_focus() {
            search
        } else {
            search.on_hover_ui(|ui| {
                ui.label(RichText::new(t!("搜尋語法")).strong());
                super::shortcuts::search_syntax_table(ui);
            })
        };
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) || std::mem::take(&mut self.focus_search) {
            search.request_focus();
            // Select what is there, so typing starts a new search.
            if let Some(mut state) = egui::TextEdit::load_state(ctx, search.id) {
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(self.search.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ctx, search.id);
            }
        }
        self.search_suggestions(ui, &search);
        if search.lost_focus() {
            let q = self.search.clone();
            self.remember_search(&q);
            if self.summoned && ctx.input(|i| i.key_pressed(Key::Enter)) {
                self.copy_search_result(ctx);
            } else if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.send_back(ctx);
            }
        }
        if !self.search.is_empty() && ui.small_button("✖").on_hover_text(t!("清除搜尋")).clicked() {
            self.search.clear();
        }
        let syntax_btn = ui
            .add(egui::Button::new(RichText::new("?").small().color(if self.syntax_open { accent() } else { tokens().text_muted })))
            .on_hover_text(t!("搜尋語法速查（點擊快速套用）"));
        if syntax_btn.clicked() {
            self.syntax_open = !self.syntax_open;
            if self.syntax_open {
                self.filter_open = false;
            }
        }
        if self.syntax_open {
            self.syntax_popover(ctx, syntax_btn.rect);
        }
        if ui.selectable_label(self.config.search_all, t!("搜尋全部")).on_hover_text(t!("開啟：搜尋時不限目前選的資料夾和標籤\n關閉：只搜尋目前選的範圍")).clicked() {
            self.config.search_all = !self.config.search_all;
            self.config.save();
        }

        ui.separator();
        let n = self.filter_count();
        let label = if n == 0 { t!("篩選 ▼").to_string() } else { t!("篩選 ({n}) ▼", n) };
        let text = if n == 0 { RichText::new(label) } else { RichText::new(label).color(accent()) };
        let button = ui.button(text).on_hover_text(t!("格式、評分、顏色"));
        if button.clicked() {
            self.filter_open = !self.filter_open;
            if self.filter_open {
                self.syntax_open = false;
            }
        }
        if self.filter_open {
            self.filter_popover(ctx, button.rect);
        }
    }

    /// Clickable search-syntax quick reference below the `?` button.
    fn syntax_popover(&mut self, ctx: &egui::Context, anchor: Rect) {
        let mut insert: Option<&'static str> = None;
        let area = egui::Area::new(egui::Id::new("syntax-popover"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom() + vec2(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(RichText::new(t!("常用搜尋語法（點一下加入搜尋欄）")).strong());
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for token in ["ext:mp4", "ext:png", "rating:4", "dur>10m", "w>=1920", t!("-草稿"), t!("貓 OR 狗")] {
                            if ui.add(egui::Button::new(RichText::new(token).small().monospace().color(accent()))).clicked() {
                                insert = Some(token);
                            }
                        }
                    });
                    ui.separator();
                    super::shortcuts::search_syntax_table(ui);
                });
            });
        if let Some(tok) = insert {
            if !self.search.is_empty() && !self.search.ends_with(' ') {
                self.search.push(' ');
            }
            self.search.push_str(tok);
            self.syntax_open = false;
            return;
        }
        let clicked_out = ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p) || anchor.contains(p));
        let escaped = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if clicked_out || escaped {
            self.syntax_open = false;
        }
    }

    /// The popover under "篩選": formats grouped by kind, rating, colour, save.
    fn filter_popover(&mut self, ctx: &egui::Context, anchor: Rect) {
        let groups = self.cached_formats();
        let area = egui::Area::new(egui::Id::new("filter-popover"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor.left_bottom() + vec2(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(440.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t!("格式")).strong());
                        if !self.ext_filter.is_empty() && ui.small_button(t!("清除")).clicked() {
                            self.ext_filter.clear();
                        }
                    });
                    if groups.is_empty() {
                        ui.label(RichText::new(t!("資源庫是空的")).weak());
                    }
                    for (kind, exts) in &groups {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(format!("{} {}", kind.icon(), kind.label())).weak());
                            for (ext, count) in exts {
                                let mut on = self.ext_filter.contains(ext);
                                if ui.toggle_value(&mut on, ext.to_uppercase()).on_hover_text(t!("{count} 個", count)).changed() {
                                    if on {
                                        self.ext_filter.push(ext.clone());
                                        self.ext_filter.sort();
                                    } else {
                                        self.ext_filter.retain(|e| e != ext);
                                    }
                                }
                            }
                        });
                    }
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t!("評分")).strong());
                        for r in 0..=5u8 {
                            let text = if r == 0 { t!("不限").to_string() } else { format!("★{r}+") };
                            ui.selectable_value(&mut self.min_rating, r, text);
                        }
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label(RichText::new(t!("顏色")).strong());
                        if self.color_filter.is_some() && ui.small_button(t!("清除")).clicked() {
                            self.color_filter = None;
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        for c in SWATCHES {
                            let chosen = self.color_filter == Some(c);
                            if swatch(ui, c, chosen).clicked() {
                                self.color_filter = if chosen { None } else { Some(c) };
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        let draft = self.color_draft.get_or_insert(self.color_filter.unwrap_or([230, 60, 60]));
                        ui.label(t!("自訂"));
                        egui::color_picker::color_edit_button_srgb(ui, draft);
                        if ui.button(t!("套用")).clicked() {
                            self.color_filter = Some(*draft);
                        }
                        let picked = self.lib.as_ref().and_then(|lib| {
                            let id = self.selected_ids().into_iter().next()?;
                            let it = lib.items.get(&id)?;
                            it.palette.iter().max_by(|a, b| a.ratio.total_cmp(&b.ratio)).map(|p| p.rgb)
                        });
                        if ui
                            .add_enabled(picked.is_some(), egui::Button::new(t!("從選取的素材取色")))
                            .on_hover_text(t!("用選取素材的主要顏色篩選"))
                            .on_disabled_hover_text(t!("先選取一個素材"))
                            .clicked()
                        {
                            self.color_filter = picked;
                            self.color_draft = picked;
                        }
                    });
                    ui.separator();

                    ui.horizontal(|ui| {
                        let any = !self.current_filter().is_empty();
                        if ui.add_enabled(any, egui::Button::new(t!("✨ 儲存成智慧資料夾"))).on_hover_text(t!("把目前的搜尋與篩選存成智慧資料夾")).clicked() {
                            self.act(Action::SaveSmartFolder);
                            self.filter_open = false;
                        }
                        if self.filter_count() > 0 && ui.button(t!("全部清除")).clicked() {
                            self.ext_filter.clear();
                            self.min_rating = 0;
                            self.color_filter = None;
                        }
                    });
                });
            });
        // Closes on a click elsewhere or Esc; the colour picker's own popup counts as inside.
        let picker_open = ctx.memory(|m| m.any_popup_open());
        let clicked_out = ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p) || anchor.contains(p))
            && !picker_open;
        let escaped = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if clicked_out || escaped {
            self.filter_open = false;
        }
    }

    /// Styled filter chips bar for every condition in effect, each with ✖, then "全部清除".
    pub(super) fn condition_row(&mut self, ui: &mut Ui) {
        // Only while it changes the results: a search is typed.
        let everywhere = self.searching_everywhere();
        if self.tag_filter.is_empty() && self.filter_count() == 0 && !everywhere {
            return;
        }
        ui.add_space(2.0);
        egui::Frame::none()
            .fill(tokens().bg_faint)
            .rounding(ROUNDING)
            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t!("篩選條件：")).small().weak());
                    let mut remove_tag = None;
                    for t in &self.tag_filter {
                        if accent_chip(ui, &format!("#{t}")) {
                            remove_tag = Some(t.clone());
                        }
                    }
                    if let Some(t) = remove_tag {
                        self.act(Action::ToggleTagFilter(t));
                    }
                    let mut remove_ext = None;
                    for e in &self.ext_filter {
                        if accent_chip(ui, &format!("ext:{}", e.to_lowercase())) {
                            remove_ext = Some(e.clone());
                        }
                    }
                    if let Some(e) = remove_ext {
                        self.ext_filter.retain(|x| *x != e);
                    }
                    if self.min_rating > 0 && accent_chip(ui, &t!("★{min_rating} 以上", min_rating = self.min_rating)) {
                        self.min_rating = 0;
                    }
                    if let Some(c) = self.color_filter {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        ui.painter().rect_filled(r, 3.0, Color32::from_rgb(c[0], c[1], c[2]));
                        // An edge, so white reads on the light theme and black on the dark.
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(1.0_f32, tokens().outline));
                        if accent_chip(ui, t!("顏色")) {
                            self.color_filter = None;
                        }
                    }
                    if everywhere && accent_chip(ui, t!("搜尋全部")) {
                        self.config.search_all = false;
                        self.config.save();
                    }
                    if (!self.tag_filter.is_empty() || self.filter_count() > 0) && ui.small_button(t!("全部清除")).clicked() {
                        self.clear_filters();
                    }
                });
            });
    }

    /// Right to left: settings, thumbnail size, view mode, sort.
    pub(super) fn toolbar_right(&mut self, ui: &mut Ui) {
        if ui.button("⚙").on_hover_text(t!("設定")).clicked() {
            self.act(Action::OpenSettings);
        }
        ui.separator();
        // Greyed out rather than gone in the list, so the view buttons beside it
        // stay where they are when switching.
        ui.spacing_mut().slider_width = 90.0;
        let list = self.config.view_mode == ViewMode::List;
        let slider = egui::Slider::new(&mut self.config.thumb_size, crate::config::THUMB_SIZES).show_value(false);
        ui.add_enabled(!list, slider)
            .on_hover_text(t!("縮圖大小（Ctrl+滾輪）"))
            .on_disabled_hover_text(t!("清單檢視沒有縮圖大小"));
        for (mode, label, tip) in [
            (ViewMode::List, t!("清單"), t!("詳細清單，可點欄位排序")),
            (ViewMode::Justified, t!("比例"), t!("依圖片比例排列")),
            (ViewMode::Grid, t!("格狀"), t!("固定大小的格子")),
        ] {
            if ui.selectable_label(self.config.view_mode == mode, label).on_hover_text(tip).clicked() {
                self.config.view_mode = mode;
            }
        }
        ui.separator();
        let arrow = if self.sort_desc { "⬇" } else { "⬆" };
        let grouped = match self.config.group_by {
            GroupBy::None => String::new(),
            by => t!("・依{label}分組", label = by.label()),
        };
        ui.menu_button(format!("{} {arrow}{grouped}", self.sort.label()), |ui| {
            for key in SortKey::ALL {
                if ui.selectable_label(self.sort == key, key.label()).clicked() {
                    self.sort = key;
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.selectable_label(!self.sort_desc, t!("⬆ 遞增")).clicked() {
                self.sort_desc = false;
                ui.close_menu();
            }
            if ui.selectable_label(self.sort_desc, t!("⬇ 遞減")).clicked() {
                self.sort_desc = true;
                ui.close_menu();
            }
            ui.separator();
            ui.label(RichText::new(t!("分組")).weak());
            for by in GroupBy::ALL {
                if ui.selectable_label(self.config.group_by == by, by.label()).clicked() {
                    self.config.group_by = by;
                    ui.close_menu();
                }
            }
        })
        .response
        .on_hover_text(t!("排序與分組"));
        if self.view == View::Trash {
            ui.separator();
            if ui.button(t!("🗑 清空垃圾桶")).clicked() {
                self.act(Action::ConfirmEmptyTrash);
            }
        }
    }
}

fn swatch(ui: &mut Ui, c: [u8; 3], chosen: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(r.shrink(2.0), 4.0, Color32::from_rgb(c[0], c[1], c[2]));
    let stroke = if chosen {
        Stroke::new(2.0_f32, accent())
    } else if resp.hovered() {
        Stroke::new(1.0_f32, tokens().text_muted)
    } else {
        Stroke::new(1.0_f32, tokens().outline)
    };
    painter.rect_stroke(r.shrink(1.0), 5.0, stroke);
    resp.on_hover_text(t!("依這個顏色篩選"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_suggestions_with_nothing_to_show_are_closed_not_hidden() {
        let ctx = egui::Context::default();
        let mut app = crate::app::App::with_config(&ctx, crate::config::Config::default());
        app.config.recent_searches = vec!["貓".into()];
        app.search = "狗".into();
        let mut text = String::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let resp = ui.text_edit_singleline(&mut text);
                let id = ui.make_persistent_id("search-suggestions");
                ui.memory_mut(|m| m.open_popup(id));
                app.search_suggestions(ui, &resp);
            });
        });
        assert!(!ctx.memory(|m| m.any_popup_open()), "an empty popup is still open");
    }

    #[test]
    fn formats_are_grouped_by_kind_most_used_first() {
        let item = |id: &str, ext: &str, kind: &str, deleted: bool| -> Item {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "file_name": format!("{id}.{ext}"), "ext": ext, "kind": kind, "size": 1,
                "width": 1, "height": 1, "added_at": 0, "modified_at": 0, "deleted": deleted
            }))
            .unwrap()
        };
        let items = [
            item("a", "png", "image", false),
            item("b", "jpg", "image", false),
            item("c", "jpg", "image", false),
            item("d", "mp4", "video", false),
            item("e", "mkv", "video", true),
        ];
        let groups = format_groups(items.iter());
        assert_eq!(
            groups,
            vec![
                (MediaKind::Image, vec![("jpg".to_string(), 2), ("png".to_string(), 1)]),
                (MediaKind::Video, vec![("mp4".to_string(), 1)]),
            ],
            "the trash does not count"
        );
    }
}
