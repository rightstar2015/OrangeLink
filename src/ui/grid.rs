//! The item grid in its three layouts (grid, justified, list), selection and item menus.

use super::*;
use crate::grouping::{self, Row};
use cell::{paint_group_header, GROUP_HEADER_H};

/// A row of the justified view: (visible index, aspect ratio) of its cells, its
/// height, and the group whose header it is instead.
type JustifiedRow = (Vec<(usize, f32)>, f32, Option<usize>);

impl App {
    /// Above similarity and duplicate results: the count, the threshold, search
    /// again, and per group keep one or merge into one.
    fn results_bar(&mut self, ui: &mut Ui, label: &str, source: &crate::app::IdsSource, grouped: bool) {
        use crate::app::IdsSource;
        ui.horizontal_wrapped(|ui| {
            let text = if grouped { format!("{label}（{} 組、{} 個）", self.groups.len(), self.visible.len()) } else { format!("{label}（{} 個）", self.visible.len()) };
            ui.label(RichText::new(text).strong().color(accent()));
            if let IdsSource::Similar(target) = source {
                let (value, default) = if target.is_some() { (&mut self.similar_near, crate::similarity::NEAR) } else { (&mut self.similar_dupes, crate::similarity::DUPES) };
                ui.label(RichText::new("差異上限").small());
                let r = ui
                    .add(egui::Slider::new(value, 0..=24).show_value(true))
                    .on_hover_text(format!("縮圖指紋 64 位元中最多幾位不同。越小越嚴格；預設 {default}。放開後重新比對"));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.act(Action::RerunResults);
                }
            }
            if !matches!(source, IdsSource::Plain) && ui.small_button("重新比對").clicked() {
                self.act(Action::RerunResults);
            }
            if ui.small_button("✖ 返回全部").clicked() {
                self.act(Action::SetView(View::All));
            }
        });
        if !grouped || self.groups.is_empty() {
            return;
        }
        let picked = self.result_groups().iter().filter(|g| g.iter().any(|id| self.selected.contains(id))).count();
        ui.horizontal_wrapped(|ui| {
            let hint = if picked == 0 { "每組第一個是建議保留的（評分最高、最早加入）；點一下改選要保留的那個（多組時 Ctrl＋點）".to_string() } else { format!("已在 {picked} 組選了要保留的") };
            ui.label(RichText::new(hint).small().weak());
            let tip_keep = "選取的那個留下，同一組的其他移到垃圾桶（可復原）";
            let tip_merge = "其他項目的標籤、資料夾、註解、片段、自訂欄位和使用次數併到選取的那個，再把它們移到垃圾桶（可復原）";
            if ui.add_enabled(picked > 0, egui::Button::new("保留選取的")).on_hover_text(tip_keep).clicked() {
                self.act(Action::ResolveDuplicates { merge: false, only_selected: true });
            }
            if ui.add_enabled(picked > 0, egui::Button::new("合併到選取的")).on_hover_text(tip_merge).clicked() {
                self.act(Action::ResolveDuplicates { merge: true, only_selected: true });
            }
            ui.menu_button(format!("全部 {} 組 ▼", self.groups.len()), |ui| {
                ui.label(RichText::new("有選取的組留選取的，其他組留第一個").small().weak());
                if ui.button("每組保留一個，其他移到垃圾桶").clicked() {
                    self.act(Action::ResolveDuplicates { merge: false, only_selected: false });
                    ui.close_menu();
                }
                if ui.button("每組合併成一個").on_hover_text(tip_merge).clicked() {
                    self.act(Action::ResolveDuplicates { merge: true, only_selected: false });
                    ui.close_menu();
                }
            });
        });
    }

    pub fn grid_ui(&mut self, ui: &mut Ui) {
        let panel_rect = ui.max_rect();
        if let (View::Ids { label, source, groups, .. }, false) = (&self.view, self.searching_everywhere()) {
            let (label, source, grouped) = (label.clone(), source.clone(), !groups.is_empty());
            self.results_bar(ui, &label, &source, grouped);
            ui.add_space(4.0);
        }
        if self.searching_everywhere() {
            ui.label(RichText::new(format!("在全部素材中搜尋（{} 個）", self.visible.len())).small().color(accent()));
            ui.add_space(4.0);
        }
        if self.ffmpeg_dir.is_none() && !self.config.ffmpeg_hint_dismissed {
            self.ffmpeg_bar(ui);
        }
        if self.visible.is_empty() {
            return self.empty_state(ui);
        }
        match self.config.view_mode {
            ViewMode::Justified => self.justified_ui(ui),
            ViewMode::List => self.list_ui(ui),
            ViewMode::Grid => self.tiles_ui(ui),
        }
        if self.selected.len() >= 2 && self.viewer.is_none() {
            self.multi_action_bar(ui.ctx(), panel_rect);
        }
    }

    /// Floating pill bar at the bottom-center of the grid when multiple items are selected.
    fn multi_action_bar(&mut self, ctx: &egui::Context, panel_rect: Rect) {
        let n = self.selected.len();
        let ids = self.selected_ids();
        let in_trash = self.view == View::Trash;
        let t = tokens();
        let pos = pos2(panel_rect.center().x, panel_rect.bottom() - 16.0);
        egui::Area::new(egui::Id::new("multi-action-bar"))
            .order(egui::Order::Foreground)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos)
            .show(ctx, |ui| {
                egui::Frame::none()
                    .fill(t.bg_elevated)
                    .stroke(Stroke::new(1.0_f32, accent().gamma_multiply(0.8)))
                    .rounding(18.0)
                    .inner_margin(egui::Margin::symmetric(14.0, 6.0))
                    .shadow(ui.visuals().popup_shadow)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new(format!("已選取 {n} 項")).strong().color(accent()));
                            ui.separator();
                            ui.menu_button("★ 評分", |ui| {
                                for r in (1..=5u8).rev() {
                                    if ui.button(RichText::new("★".repeat(r as usize)).color(star())).clicked() {
                                        self.act(Action::SetRating(ids.clone(), r));
                                        ui.close_menu();
                                    }
                                }
                                ui.separator();
                                if ui.button("清除評分").clicked() {
                                    self.act(Action::SetRating(ids.clone(), 0));
                                    ui.close_menu();
                                }
                            });
                            if ui.button("🏷 標籤").on_hover_text("聚焦右側標籤輸入框").clicked() {
                                self.act(Action::FocusTagInput);
                            }
                            if ui.button("F2 改名").on_hover_text("批次重新命名").clicked() {
                                self.act(Action::BatchRename(ids.clone()));
                            }
                            if in_trash {
                                if ui.button("還原").clicked() {
                                    self.act(Action::RestoreSelected);
                                }
                                if ui.button(RichText::new("永久刪除").color(danger())).clicked() {
                                    self.act(Action::ConfirmDeleteForever);
                                }
                            } else if ui.button(RichText::new("🗑 刪除").color(danger())).on_hover_text("移到垃圾桶 (Delete)").clicked() {
                                self.act(Action::TrashSelected);
                            }
                            if ui.small_button("✖").on_hover_text("取消選取 (Esc)").clicked() {
                                self.act(Action::ClearSelection);
                            }
                        });
                    });
            });
    }

    fn tiles_ui(&mut self, ui: &mut Ui) {
        let gap = 12.0;
        let label_h = 40.0;
        let avail = ui.available_width() - 12.0;
        let target = self.config.thumb_size;
        let cols = (((avail + gap) / (target + gap)).floor() as usize).max(1);
        let cell_w = ((avail + gap) / cols as f32 - gap).max(40.0);
        let cell_h = cell_w + label_h;
        self.grid_cols = cols;
        // Each group starts on a row of its own under its header.
        let rows = grouping::rows(&self.groups, self.visible.len(), cols);
        let mut tops = Vec::with_capacity(rows.len());
        let mut y = 4.0;
        for row in &rows {
            tops.push(y);
            y += match row {
                Row::Header(_) => GROUP_HEADER_H,
                Row::Items { .. } => cell_h + gap,
            };
        }
        let total_h = y;

        let mut scroll = ScrollArea::vertical().id_salt("grid").auto_shrink([false, false]).drag_to_scroll(false);
        if let Some(offset) = self.glide(ui, total_h) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        if let Some((idx, frac, at)) = self.zoom_anchor.take() {
            if let Some(r) = rows.iter().position(|row| matches!(row, Row::Items { first, end } if (*first..*end).contains(&idx))) {
                scroll = scroll.vertical_scroll_offset((tops[r] + frac * (cell_h + gap) - at).max(0.0));
            }
        }
        if let Some(offset) = self.restore_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        if let Some(id) = self.scroll_to.take() {
            if let Some(idx) = self.visible.iter().position(|x| *x == id) {
                let r = rows.iter().position(|row| matches!(row, Row::Items { first, end } if (*first..*end).contains(&idx))).unwrap_or(0);
                // The group's header comes into view with its first row.
                let top = if r > 0 && matches!(rows[r - 1], Row::Header(_)) { tops[r - 1] } else { tops[r] };
                let bottom = tops[r] + cell_h;
                if top < self.grid_scroll {
                    scroll = scroll.vertical_scroll_offset(top);
                } else if bottom > self.grid_scroll + self.grid_view_h {
                    scroll = scroll.vertical_scroll_offset(bottom - self.grid_view_h + gap);
                }
            }
        }

        // Clicking empty space clears the selection; dragging it draws a selection box.
        // This must be registered before the cells: egui gives input to the widget
        // registered last (topmost), so presses on a cell still go to the cell.
        let bg = self.grid_background(ui);
        if let Some(offset) = self.marquee_autoscroll(ui, bg.rect) {
            scroll = scroll.vertical_scroll_offset(offset);
        }

        ui.spacing_mut().item_spacing = vec2(gap, gap);
        let lib = self.lib.as_ref().unwrap();
        let out = scroll.show_viewport(ui, |ui, viewport| {
            ui.set_height(total_h);
            let origin = ui.min_rect().min;
            self.cell_origin.set(origin);
            for (r, row) in rows.iter().enumerate() {
                let top = tops[r];
                let h = if matches!(row, Row::Header(_)) { GROUP_HEADER_H } else { cell_h };
                if top + h < viewport.min.y || top > viewport.max.y {
                    continue;
                }
                match *row {
                    Row::Header(g) => paint_group_header(ui, Rect::from_min_size(origin + vec2(4.0, top), vec2(avail, GROUP_HEADER_H)), &self.groups[g]),
                    Row::Items { first, end } => {
                        let rect = Rect::from_min_size(origin + vec2(4.0, top), vec2(avail + gap, cell_h));
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Min)), |ui| {
                            for id in &self.visible[first..end] {
                                if let Some(it) = lib.items.get(id) {
                                    self.grid_cell(ui, it, vec2(cell_w, cell_h));
                                }
                            }
                        });
                    }
                }
            }
        });
        self.grid_scroll = out.state.offset.y;
        self.grid_view_h = out.inner_rect.height();
        if let Some(p) = self.wheel_zoom(ui, out.inner_rect) {
            // The row under the pointer (a header counts as the row after it), and the column.
            let y = p.y + self.grid_scroll;
            let r = tops.iter().rposition(|t| *t <= y).unwrap_or(0);
            let r = (r..rows.len()).find(|r| matches!(rows[*r], Row::Items { .. }));
            if let Some(r) = r {
                let Row::Items { first, end } = rows[r] else { unreachable!() };
                let col = ((p.x - 4.0) / (cell_w + gap)).max(0.0) as usize;
                let frac = ((y - tops[r]) / (cell_h + gap)).clamp(0.0, 1.0);
                self.zoom_anchor = Some(((first + col).min(end - 1), frac, p.y));
            }
        }
        self.marquee_update(ui, &bg);
    }

    /// Ctrl+wheel over the thumbnails (`area`) resizes them, as the slider does.
    /// Returns the pointer's position in `area` when the size changed.
    pub(super) fn wheel_zoom(&mut self, ui: &Ui, area: Rect) -> Option<egui::Vec2> {
        if self.viewer.is_some() {
            return None;
        }
        let (zoom, pos) = ui.input(|i| (i.zoom_delta(), i.pointer.hover_pos()));
        let pos = pos.filter(|p| area.contains(*p) && ui.ctx().layer_id_at(*p) == Some(ui.layer_id()))?;
        if zoom == 1.0 {
            return None;
        }
        let range = crate::config::THUMB_SIZES;
        let before = self.config.thumb_size;
        self.config.thumb_size = (before * zoom).clamp(*range.start(), *range.end());
        if self.config.thumb_size == before {
            return None;
        }
        ui.ctx().request_repaint();
        Some(pos - area.min)
    }

    /// A bar over the grid while FFmpeg is missing, until closed.
    fn ffmpeg_bar(&mut self, ui: &mut Ui) {
        egui::Frame::none().fill(warning().gamma_multiply(0.15)).rounding(ROUNDING).inner_margin(egui::Margin::symmetric(10.0, 6.0)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let what = if cfg!(windows) && self.config.play_engine != crate::config::PlayEngine::Ffmpeg {
                    "影片縮圖、片段輸出和 Windows 無法播放的格式需要 FFmpeg"
                } else {
                    "影片縮圖和播放需要 FFmpeg"
                };
                ui.label(RichText::new(what).color(warning()));
                if ui.small_button("安裝").on_hover_text("以 winget 安裝 Gyan.FFmpeg").clicked() {
                    self.act(Action::InstallFfmpeg);
                }
                if ui.small_button("指定資料夾…").clicked() {
                    self.act(Action::OpenSettingsAt(crate::config::SettingsPage::Tools));
                }
                if ui.small_button("✖").on_hover_text("不再顯示").clicked() {
                    self.config.ffmpeg_hint_dismissed = true;
                    self.config.save();
                }
            });
        });
        ui.add_space(4.0);
    }

    /// Nothing to show: the trash is empty, the library is empty (ways to fill
    /// it), or the conditions leave nothing (what they are, and a way out).
    fn empty_state(&mut self, ui: &mut Ui) {
        let empty_library = self.lib.as_ref().is_some_and(|l| l.items.values().all(|i| i.deleted));
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            if self.view == View::Trash {
                ui.label(RichText::new("垃圾桶是空的").size(16.0).weak());
            } else if empty_library {
                ui.label(RichText::new("資源庫還是空的").size(18.0).strong());
                ui.add_space(14.0);
                let size = [168.0, 38.0];
                ui.horizontal_wrapped(|ui| {
                    let w = size[0] * 3.0 + ui.spacing().item_spacing.x * 2.0;
                    if ui.available_width() > w {
                        ui.add_space((ui.available_width() - w) / 2.0);
                    }
                    if ui.add_sized(size, egui::Button::new("📄 匯入檔案…")).clicked() {
                        self.act(Action::ImportFiles);
                    }
                    if ui.add_sized(size, egui::Button::new("📁 匯入資料夾…")).clicked() {
                        self.act(Action::ImportFolder);
                    }
                    if ui.add_sized(size, egui::Button::new("⬇ 從網址下載…")).clicked() {
                        self.act(Action::OpenYtdlp);
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("也可以直接把檔案或資料夾拖進視窗，或用 Ctrl+V 貼上").weak());
                if ui.link("安裝瀏覽器擴充功能，在網頁上按右鍵就能加入素材").clicked() {
                    self.act(Action::OpenSettingsAt(crate::config::SettingsPage::Integration));
                }
            } else {
                ui.label(RichText::new("沒有符合條件的項目").size(16.0).weak());
                let conditions = self.condition_summary();
                if !conditions.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("目前的條件：{}", conditions.join("、"))).small().weak());
                }
                ui.add_space(10.0);
                let had_filters = !self.search.trim().is_empty() || !self.tag_filter.is_empty() || self.filter_count() > 0;
                let btn_label = if had_filters { "清除篩選" } else { "回到全部素材" };
                if ui.button(btn_label).clicked() {
                    self.clear_filters();
                    self.search.clear();
                    if self.config.search_all {
                        self.config.search_all = false;
                        self.config.save();
                    }
                    if !had_filters && self.view != View::All {
                        self.act(Action::SetView(View::All));
                    }
                }
            }
        });
    }

    /// The conditions narrowing the grid, in words.
    fn condition_summary(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.search.trim().is_empty() {
            out.push(format!("搜尋「{}」", self.search.trim()));
        }
        out.extend(self.tag_filter.iter().map(|t| format!("標籤 #{t}")));
        if !self.ext_filter.is_empty() {
            out.push(format!("格式 {}", self.ext_filter.iter().map(|e| e.to_uppercase()).collect::<Vec<_>>().join("／")));
        }
        if self.min_rating > 0 {
            out.push(format!("★{} 以上", self.min_rating));
        }
        if self.color_filter.is_some() {
            out.push("顏色".into());
        }
        match &self.view {
            View::All => {}
            View::Uncategorized => out.push("未分類".into()),
            View::Untagged => out.push("未標籤".into()),
            View::Kind(k) => out.push(format!("類型 {}", k.label())),
            _ => out.push("目前選的資料夾或分類".into()),
        }
        out
    }

    /// The empty area behind the cells: click clears the selection, drag selects a box.
    pub(super) fn grid_background(&mut self, ui: &mut Ui) -> egui::Response {
        if self.marquee.is_none() {
            self.cell_rects.borrow_mut().clear();
        }
        let bg = ui.interact(ui.available_rect_before_wrap(), ui.id().with("grid-bg"), Sense::click_and_drag());
        if bg.clicked() {
            self.act(Action::ClearSelection);
        }
        bg
    }

    /// Remembers where a cell is (in content coordinates) for drag-box selection.
    pub(super) fn record_cell(&self, id: &str, rect: Rect) {
        self.cell_rects.borrow_mut().insert(id.to_string(), rect.translate(-self.cell_origin.get().to_vec2()));
    }

    /// Scrolls while a selection box is dragged past the top or bottom edge.
    pub(super) fn marquee_autoscroll(&self, ui: &Ui, area: Rect) -> Option<f32> {
        self.marquee.as_ref()?;
        let y = ui.input(|i| i.pointer.interact_pos())?.y;
        let edge = 24.0;
        let delta = if y < area.top() + edge {
            -(area.top() + edge - y)
        } else if y > area.bottom() - edge {
            y - (area.bottom() - edge)
        } else {
            return None;
        };
        ui.ctx().request_repaint();
        Some((self.grid_scroll + delta.clamp(-60.0, 60.0) * 0.5).max(0.0))
    }

    /// Starts, updates and draws the selection box; call after the cells are drawn.
    pub(super) fn marquee_update(&mut self, ui: &Ui, bg: &egui::Response) {
        let origin = self.cell_origin.get().to_vec2();
        if bg.drag_started() {
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                let mods = ui.input(|i| i.modifiers);
                let base = if mods.command || mods.shift { self.selected.clone() } else { std::collections::HashSet::new() };
                self.marquee = Some((p - origin, base));
            }
        }
        let Some((start, base)) = &self.marquee else { return };
        if !ui.input(|i| i.pointer.primary_down()) {
            self.marquee = None;
            return;
        }
        let Some(now) = ui.input(|i| i.pointer.interact_pos()) else { return };
        let area = Rect::from_two_pos(*start, now - origin);
        let mut selected = base.clone();
        selected.extend(self.cell_rects.borrow().iter().filter(|(_, r)| r.intersects(area)).map(|(id, _)| id.clone()));
        if selected != self.selected {
            self.anchor_to_first(&selected);
            self.selected = selected;
        }
        let screen = area.translate(origin);
        let painter = ui.painter().with_clip_rect(bg.rect);
        painter.rect_filled(screen, 2.0, accent().gamma_multiply(0.12));
        painter.rect_stroke(screen, 2.0, Stroke::new(1.0_f32, accent()));
    }

    pub(super) fn grid_cell(&self, ui: &mut Ui, it: &crate::model::Item, size: egui::Vec2) {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        self.record_cell(&it.id, rect);
        let resp = ui.interact(rect, egui::Id::new(("item", &it.id)), Sense::click_and_drag());
        let selected = self.selected.contains(&it.id);
        let img_rect = Rect::from_min_size(rect.min, vec2(size.x, size.x));
        let v = ui.visuals();
        let painter = ui.painter();

        painter.rect_filled(img_rect, 6.0, if resp.hovered() { v.widgets.hovered.bg_fill } else { v.extreme_bg_color });
        let previewing = self.preview_tex(&it.id);
        match previewing.or_else(|| self.thumb_tex(&it.id)) {
            Some((tex, tex_size)) => {
                painter.image(tex, fit_rect(img_rect.shrink(6.0), tex_size), UV, Color32::WHITE);
            }
            None if !self.thumbs.failed.contains(&it.id) => {
                painter.text(img_rect.center(), Align2::CENTER_CENTER, "…", FontId::proportional(22.0), v.weak_text_color());
            }
            None => {
                // No thumbnail possible: show the file type instead.
                let icon_size = (size.x * 0.3).clamp(24.0, 72.0);
                painter.text(img_rect.center(), Align2::CENTER_CENTER, it.kind.icon(), FontId::proportional(icon_size), v.weak_text_color());
            }
        }
        paint_progress(painter, img_rect, self.preview_progress(&it.id));
        self.paint_cell_overlay(ui, img_rect, 6.0, it, &resp, previewing.is_some());

        let label_rect = Rect::from_min_max(pos2(rect.min.x + 2.0, img_rect.max.y + 4.0), rect.max);
        let (text_color, weak_color) = (ui.visuals().text_color(), ui.visuals().weak_text_color());
        let name_rect = Rect::from_min_max(label_rect.min, pos2(label_rect.max.x - 2.0, label_rect.min.y + 18.0));
        let color = if selected { accent() } else { text_color };
        self.item_name(ui, name_rect, it, 13.0, color);
        let detail = if size.x < 130.0 {
            human_size(it.size)
        } else if it.width > 0 {
            let mut s = format!("{} × {}  ·  {}", it.width, it.height, human_size(it.size));
            if size.x >= 185.0 {
                if let Some(fps) = it.fps {
                    s += &format!("  ·  {fps:.0}fps");
                }
            }
            s
        } else {
            human_size(it.size)
        };
        let detail_rect = Rect::from_min_max(label_rect.min + vec2(0.0, 18.0), label_rect.max);
        cell::paint_name(ui, detail_rect, &detail, 11.0, weak_color);

        self.item_interactions(ui, &resp, it);
    }

    /// Click / double-click / right-click / drag behaviour shared by all layouts.
    pub(super) fn item_interactions(&self, ui: &Ui, resp: &egui::Response, it: &crate::model::Item) {
        let selected = self.selected.contains(&it.id);
        if resp.hovered() {
            self.hover_now.set(Some(it.id.clone()));
        }
        if resp.clicked() {
            self.act(Action::Click(it.id.clone(), ui.input(|i| i.modifiers)));
        }
        if resp.double_clicked() {
            self.act(Action::Activate(it.id.clone()));
        }
        if resp.secondary_clicked() && !selected {
            self.act(Action::Click(it.id.clone(), egui::Modifiers::NONE));
        }
        if resp.drag_started() {
            let ids = if selected { self.selected_ids() } else { vec![it.id.clone()] };
            egui::DragAndDrop::set_payload(ui.ctx(), DragIds(ids));
        }
        let id = it.id.clone();
        resp.context_menu(|ui| self.item_menu(ui, &id, selected));
    }

    /// Justified rows: every row has the same height and images keep their aspect ratio.
    pub(super) fn justified_ui(&mut self, ui: &mut Ui) {
        let Some(lib) = &self.lib else { return };
        let gap = 8.0;
        let label_h = 20.0;
        // Not below a thumbnail's width: a very narrow window (or the side panels
        // taking it all) made it negative, and every row a negative height.
        let width = (ui.available_width() - 12.0).max(40.0);
        let target_h = self.config.thumb_size;
        let aspect = |id: &String| {
            lib.items
                .get(id)
                .filter(|i| i.width > 0 && i.height > 0)
                .map(|i| (i.width as f32 / i.height as f32).clamp(0.3, 4.0))
                .unwrap_or(1.0)
        };
        // Greedy row packing, a group at a time; each group's last row keeps the
        // target height. A header is a row with no cells and the group's index.
        let mut rows: Vec<JustifiedRow> = Vec::new();
        let whole = [grouping::Group { label: "".into(), start: 0, len: self.visible.len() }];
        let runs: &[grouping::Group] = if self.groups.is_empty() { &whole } else { &self.groups };
        for (g, run) in runs.iter().enumerate() {
            if !self.groups.is_empty() {
                rows.push((Vec::new(), 0.0, Some(g)));
            }
            let mut row: Vec<(usize, f32)> = Vec::new();
            let mut row_aspect = 0.0;
            for i in run.range() {
                let a = aspect(&self.visible[i]);
                row.push((i, a));
                row_aspect += a;
                let natural_w = row_aspect * target_h + gap * (row.len() - 1) as f32;
                if natural_w >= width {
                    let h = (width - gap * (row.len() - 1) as f32) / row_aspect;
                    rows.push((std::mem::take(&mut row), h, None));
                    row_aspect = 0.0;
                }
            }
            if !row.is_empty() {
                rows.push((row, target_h, None));
            }
        }
        let row_h = |(_, h, header): &JustifiedRow| if header.is_some() { GROUP_HEADER_H } else { h + label_h + gap };
        let mut tops = Vec::with_capacity(rows.len());
        let mut y = 4.0;
        for row in &rows {
            tops.push(y);
            y += row_h(row);
        }
        let total_h = y;

        let mut scroll = ScrollArea::vertical().id_salt("justified").auto_shrink([false, false]).drag_to_scroll(false);
        if let Some(offset) = self.glide(ui, total_h) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        if let Some((idx, frac, at)) = self.zoom_anchor.take() {
            if let Some(r) = rows.iter().position(|(cells, ..)| cells.iter().any(|(i, _)| *i == idx)) {
                scroll = scroll.vertical_scroll_offset((tops[r] + frac * row_h(&rows[r]) - at).max(0.0));
            }
        }
        if let Some(offset) = self.restore_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        if let Some(id) = self.scroll_to.take() {
            if let Some(idx) = self.visible.iter().position(|x| *x == id) {
                if let Some(r) = rows.iter().position(|(cells, ..)| cells.iter().any(|(i, _)| *i == idx)) {
                    let top = if r > 0 && rows[r - 1].2.is_some() { tops[r - 1] } else { tops[r] };
                    let bottom = tops[r] + rows[r].1 + label_h;
                    if top < self.grid_scroll {
                        scroll = scroll.vertical_scroll_offset(top);
                    } else if bottom > self.grid_scroll + self.grid_view_h {
                        scroll = scroll.vertical_scroll_offset(bottom - self.grid_view_h + gap);
                    }
                }
            }
        }
        let bg = self.grid_background(ui);
        if let Some(offset) = self.marquee_autoscroll(ui, bg.rect) {
            scroll = scroll.vertical_scroll_offset(offset);
        }
        let Some(lib) = &self.lib else { return };
        let out = scroll.show_viewport(ui, |ui, viewport| {
            ui.set_height(total_h);
            let origin = ui.min_rect().min;
            self.cell_origin.set(origin);
            for (r, (cells, h, header)) in rows.iter().enumerate() {
                let top = tops[r];
                if top + row_h(&rows[r]) < viewport.min.y || top > viewport.max.y {
                    continue;
                }
                if let Some(g) = header {
                    paint_group_header(ui, Rect::from_min_size(origin + vec2(4.0, top), vec2(width, GROUP_HEADER_H)), &self.groups[*g]);
                    continue;
                }
                let mut x = 4.0;
                for (idx, a) in cells {
                    let w = a * h;
                    let rect = Rect::from_min_size(origin + vec2(x, top), vec2(w, h + label_h));
                    x += w + gap;
                    let Some(it) = lib.items.get(&self.visible[*idx]) else { continue };
                    self.justified_cell(ui, rect, label_h, it);
                }
            }
        });
        self.grid_scroll = out.state.offset.y;
        self.grid_view_h = out.inner_rect.height();
        self.grid_cols = rows.iter().find(|r| r.2.is_none()).map(|(c, ..)| c.len()).unwrap_or(1);
        if let Some(p) = self.wheel_zoom(ui, out.inner_rect) {
            let y = p.y + self.grid_scroll;
            let r = tops.iter().rposition(|t| *t <= y).unwrap_or(0);
            if let Some(r) = (r..rows.len()).find(|r| rows[*r].2.is_none()) {
                let (cells, h, _) = &rows[r];
                // The cell the pointer is over, or the row's last one.
                let mut x = 4.0;
                let hit = cells.iter().find(|(_, a)| {
                    x += a * h + gap;
                    p.x < x
                });
                if let Some((idx, _)) = hit.or(cells.last()) {
                    let frac = ((y - tops[r]) / row_h(&rows[r])).clamp(0.0, 1.0);
                    self.zoom_anchor = Some((*idx, frac, p.y));
                }
            }
        }
        self.marquee_update(ui, &bg);
    }

    pub(super) fn justified_cell(&self, ui: &mut Ui, rect: Rect, label_h: f32, it: &crate::model::Item) {
        self.record_cell(&it.id, rect);
        let resp = ui.interact(rect, egui::Id::new(("item", &it.id)), Sense::click_and_drag());
        let selected = self.selected.contains(&it.id);
        let img_rect = Rect::from_min_max(rect.min, rect.max - vec2(0.0, label_h));
        let v = ui.visuals();
        let painter = ui.painter();
        painter.rect_filled(img_rect, ROUNDING, v.extreme_bg_color);
        let previewing = self.preview_tex(&it.id);
        match previewing.or_else(|| self.thumb_tex(&it.id)) {
            Some((tex, size)) => {
                painter.image(tex, fit_rect(img_rect, size), UV, Color32::WHITE);
                paint_progress(painter, img_rect, self.preview_progress(&it.id));
            }
            None => {
                let glyph = if self.thumbs.failed.contains(&it.id) { it.kind.icon() } else { "…" };
                painter.text(img_rect.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(24.0), v.weak_text_color());
            }
        }
        let previewing = previewing.is_some();
        self.paint_cell_overlay(ui, img_rect, 4.0, it, &resp, previewing);
        let label = Rect::from_min_max(pos2(rect.min.x + 2.0, img_rect.max.y + 2.0), rect.max);
        let text_color = ui.visuals().text_color();
        let color = if selected { accent() } else { text_color };
        self.item_name(ui, label, it, 12.0, color);
        self.item_interactions(ui, &resp, it);
    }

    /// Detailed list: one row per item with sortable columns.
    pub(super) fn list_ui(&mut self, ui: &mut Ui) {
        if self.lib.is_none() {
            return;
        }
        let row_h = 34.0;
        // (title, width, sort key)
        let cols: [(&str, f32, Option<SortKey>); 7] = [
            ("名稱", 0.0, Some(SortKey::Name)),
            ("類型", 70.0, None),
            ("尺寸", 110.0, Some(SortKey::Dimensions)),
            ("長度", 70.0, None),
            ("大小", 90.0, Some(SortKey::FileSize)),
            ("評分", 80.0, Some(SortKey::Rating)),
            ("加入時間", 130.0, Some(SortKey::Added)),
        ];
        let fixed: f32 = cols.iter().map(|c| c.1).sum();
        let name_w = (ui.available_width() - fixed - 52.0).max(160.0);
        let total_w = 44.0 + name_w + fixed;
        let width_of = |i: usize| if i == 0 { name_w } else { cols[i].1 };

        ScrollArea::horizontal().id_salt("list-h").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_min_width(total_w);
            ui.horizontal(|ui| {
                ui.add_space(44.0);
                for (i, (title, _, key)) in cols.iter().enumerate() {
                    let arrow = match key {
                        Some(k) if *k == self.sort => if self.sort_desc { " ⬇" } else { " ⬆" },
                        _ => "",
                    };
                    let sense = if key.is_some() { Sense::click() } else { Sense::hover() };
                    let (rect, r) = ui.allocate_exact_size(vec2(width_of(i), 20.0), sense);
                    let color = if r.hovered() && key.is_some() { accent() } else { ui.visuals().strong_text_color() };
                    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, format!("{title}{arrow}"), FontId::proportional(12.0), color);
                    if let (Some(k), true) = (key, r.clicked()) {
                        if self.sort == *k {
                            self.sort_desc = !self.sort_desc;
                        } else {
                            self.sort = *k;
                        }
                    }
                }
            });
            ui.separator();

            // One item a row, with a header row before each group.
            let rows = grouping::rows(&self.groups, self.visible.len(), 1);
            let mut scroll = ScrollArea::vertical().id_salt("list").auto_shrink([false, false]).drag_to_scroll(false);
            if let Some(offset) = self.glide(ui, rows.len() as f32 * row_h) {
                scroll = scroll.vertical_scroll_offset(offset);
            }
            if let Some(offset) = self.restore_scroll.take() {
                scroll = scroll.vertical_scroll_offset(offset);
            }
            if let Some(id) = self.scroll_to.take() {
                if let Some(idx) = self.visible.iter().position(|x| *x == id) {
                    let r = rows.iter().position(|row| *row == Row::Items { first: idx, end: idx + 1 }).unwrap_or(idx);
                    let top = r as f32 * row_h;
                    if top < self.grid_scroll {
                        scroll = scroll.vertical_scroll_offset(top);
                    } else if top + row_h > self.grid_scroll + self.grid_view_h {
                        scroll = scroll.vertical_scroll_offset(top + row_h - self.grid_view_h);
                    }
                }
            }
            let bg = self.grid_background(ui);
            if let Some(offset) = self.marquee_autoscroll(ui, bg.rect) {
                scroll = scroll.vertical_scroll_offset(offset);
            }
            let Some(lib) = &self.lib else { return };
            ui.spacing_mut().item_spacing.y = 0.0;
            let out = scroll.show_rows(ui, row_h, rows.len(), |ui, range| {
                self.cell_origin.set(ui.max_rect().min - vec2(0.0, range.start as f32 * row_h));
                for r in range {
                    let idx = match rows[r] {
                        Row::Items { first, .. } => first,
                        Row::Header(g) => {
                            let (rect, _) = ui.allocate_exact_size(vec2(total_w, row_h), Sense::hover());
                            paint_group_header(ui, rect, &self.groups[g]);
                            continue;
                        }
                    };
                    let Some(it) = lib.items.get(&self.visible[idx]) else { continue };
                    let (rect, _) = ui.allocate_exact_size(vec2(total_w, row_h), Sense::hover());
                    self.record_cell(&it.id, rect);
                    // Clicks anywhere on the row; dragging the thumbnail/name drags the files out,
                    // dragging the other columns falls through to the background's selection box.
                    let row = ui.interact(rect, egui::Id::new(("item", &it.id)), Sense::click());
                    let grab_rect = Rect::from_min_size(rect.min, vec2(44.0 + name_w, row_h));
                    // One response for both, so clicks on the thumbnail and name reach the row
                    // (`item_interactions` starts the drag out from it).
                    let (resp, _) = hits::with_handle(ui, row, grab_rect, egui::Id::new(("item-grab", &it.id)));
                    let selected = self.selected.contains(&it.id);
                    let missing = lib.missing.contains(&it.id);
                    let v = ui.visuals();
                    let painter = ui.painter();
                    if selected {
                        painter.rect_filled(rect, 3.0, tokens().accent_soft);
                        let bar = Rect::from_min_max(rect.min + vec2(0.0, 4.0), pos2(rect.min.x + 2.5, rect.max.y - 4.0));
                        painter.rect_filled(bar, 1.5, accent());
                    } else if resp.hovered() {
                        painter.rect_filled(rect, 3.0, v.widgets.hovered.weak_bg_fill);
                    } else if idx % 2 == 1 {
                        painter.rect_filled(rect, 0.0, v.faint_bg_color);
                    }
                    let thumb_rect = Rect::from_min_size(rect.min + vec2(6.0, 2.0), vec2(30.0, 30.0));
                    match self.thumb_tex(&it.id) {
                        Some((tex, size)) => {
                            painter.image(tex, fit_rect(thumb_rect, size), UV, Color32::WHITE);
                        }
                        None => {
                            painter.text(thumb_rect.center(), Align2::CENTER_CENTER, it.kind.icon(), FontId::proportional(16.0), v.weak_text_color());
                        }
                    }
                    let name_text = if missing {
                        format!("⚠ {}", it.name)
                    } else if it.source.is_some() {
                        format!("🔗 {}", it.name)
                    } else {
                        it.name.clone()
                    };
                    let cells = [
                        name_text,
                        it.kind.label().to_string(),
                        if it.width > 0 { format!("{} × {}", it.width, it.height) } else { String::new() },
                        it.duration.map(fmt_duration).unwrap_or_default(),
                        human_size(it.size),
                        "★".repeat(it.rating as usize),
                        fmt_time(it.added_at),
                    ];
                    let mut x = rect.min.x + 44.0;
                    for (i, text) in cells.iter().enumerate() {
                        let w = width_of(i);
                        let cell = Rect::from_min_size(pos2(x, rect.min.y), vec2(w - 8.0, row_h));
                        let color = if i == 5 {
                            star()
                        } else if i == 0 && missing {
                            danger()
                        } else if i == 0 {
                            if selected { accent() } else { v.text_color() }
                        } else {
                            v.weak_text_color()
                        };
                        painter.with_clip_rect(cell.intersect(ui.clip_rect())).text(cell.left_center(), Align2::LEFT_CENTER, text, FontId::proportional(13.0), color);
                        x += w;
                    }
                    if self.renaming.borrow().as_ref().is_some_and(|e| e.is(&it.id, false)) {
                        let name_cell = Rect::from_min_size(pos2(rect.min.x + 44.0, rect.center().y - 9.0), vec2(name_w - 8.0, 18.0));
                        let color = ui.visuals().text_color();
                        self.item_name(ui, name_cell, it, 13.0, color);
                    }
                    self.item_interactions(ui, &resp, it);
                }
            });
            self.grid_scroll = out.state.offset.y;
            self.grid_view_h = out.inner_rect.height();
            self.grid_cols = 1;
            self.marquee_update(ui, &bg);
        });
    }

    pub(super) fn item_menu(&self, ui: &mut Ui, id: &str, selected: bool) {
        let Some(lib) = &self.lib else { return };
        let targets = if selected { self.selected_ids() } else { vec![id.to_string()] };
        let n = targets.len();
        let item = lib.items.get(id);
        let trash = self.view == View::Trash;
        if n > 1 {
            ui.label(RichText::new(format!("{n} 個項目")).weak());
            ui.separator();
        }
        let only_this = |r: egui::Response| if n > 1 { r.on_hover_text("只開啟按右鍵的這一個") } else { r };
        if only_this(menu_item(ui, "在預覽中開啟", "Enter")).clicked() {
            self.act(Action::OpenViewer(id.to_string()));
            ui.close_menu();
        }
        if only_this(ui.button("在檔案總管中顯示")).clicked() {
            self.act(Action::ShowInExplorer(id.to_string()));
            ui.close_menu();
        }
        if menu_item(ui, "複製檔案", "Ctrl+C").clicked() {
            self.act(Action::CopyFiles);
            ui.close_menu();
        }
        let label = if n > 1 { format!("複製 {n} 個檔案路徑") } else { "複製檔案路徑".to_string() };
        let r = ui.button(label);
        if (if n > 1 { r.on_hover_text("一行一個") } else { r }).clicked() {
            self.act(Action::CopyPaths(targets.clone()));
            ui.close_menu();
        }
        ui.menu_button("開啟方式", |ui| {
            if only_this(ui.button("預設程式")).clicked() {
                self.act(Action::OpenExternal(id.to_string()));
                ui.close_menu();
            }
            for (i, app) in self.config.external_apps.iter().enumerate() {
                if ui.button(&app.name).clicked() {
                    self.act(Action::OpenWith(i, targets.clone()));
                    ui.close_menu();
                }
            }
            if ui.button("其他程式…").on_hover_text("在設定中新增程式").clicked() {
                self.act(Action::OpenSettings);
                ui.close_menu();
            }
            if cfg!(windows) {
                ui.separator();
                if ui.button("Windows 選單…").on_hover_text("檔案總管的右鍵選單：7-Zip、傳送到、內容等").clicked() {
                    self.act(Action::ShellMenu(targets.clone()));
                    ui.close_menu();
                }
            }
        });
        ui.separator();
        ui.menu_button("加入資料夾", |ui| {
            if let View::Folder(f) = &self.view {
                if ui.button("從此資料夾移除").clicked() {
                    self.act(Action::RemoveFromFolder(targets.clone(), f.clone()));
                    ui.close_menu();
                }
                ui.separator();
            }
            let folders = lib.folder_flat();
            if folders.is_empty() {
                ui.label(RichText::new("尚無資料夾").weak());
            }
            for (fid, name, depth) in folders {
                if ui.button(format!("{}📁 {name}", "    ".repeat(depth))).clicked() {
                    self.act(Action::AddToFolder(targets.clone(), fid));
                    ui.close_menu();
                }
            }
            ui.separator();
            if ui.button("＋ 新增資料夾…").clicked() {
                self.act(Action::NewFolder(None));
                ui.close_menu();
            }
        });
        ui.menu_button("評分", |ui| {
            for r in (0..=5u8).rev() {
                let label = if r == 0 { "清除評分".to_string() } else { "★".repeat(r as usize) };
                if menu_item(ui, label, &r.to_string()).clicked() {
                    self.act(Action::SetRating(targets.clone(), r));
                    ui.close_menu();
                }
            }
        });
        if n > 1 {
            if menu_item(ui, "批次重新命名…", "F2").clicked() {
                self.act(Action::BatchRename(targets.clone()));
                ui.close_menu();
            }
        } else if menu_item(ui, "重新命名…", "F2").clicked() {
            self.act(Action::StartRename(id.to_string(), false));
            ui.close_menu();
        }
        ui.menu_button("整理", |ui| {
            if ui.button("尋找相似的素材").clicked() {
                self.act(Action::FindSimilar(id.to_string()));
                ui.close_menu();
            }
            if targets.len() > 1 && ui.button(format!("把這 {} 個互相關聯", targets.len())).on_hover_text("在右側面板的「相關素材」互相列出").clicked() {
                self.act(Action::LinkItems(targets.clone()));
                ui.close_menu();
            }
            if let Some(it) = item.filter(|_| !lib.meta.fields.is_empty()) {
                ui.menu_button("依自訂欄位建立智慧資料夾", |ui| {
                    for name in &lib.meta.fields {
                        if let Some(v) = it.fields.get(name).filter(|v| !v.trim().is_empty()) {
                            if ui.button(format!("{name} = {v}")).clicked() {
                                self.act(Action::FieldSmartFolder(name.clone(), v.clone()));
                                ui.close_menu();
                            }
                        }
                        if ui.button(format!("已填寫「{name}」")).clicked() {
                            self.act(Action::FieldSmartFolder(name.clone(), String::new()));
                            ui.close_menu();
                        }
                    }
                });
            }
            if ui.button("重新產生縮圖").clicked() {
                self.act(Action::RegenerateThumbnails(targets.clone()));
                ui.close_menu();
            }
            let relink = lib.missing.contains(id) || item.is_some_and(|i| i.source.is_some());
            if relink && ui.button("重新指定檔案位置…").clicked() {
                self.act(Action::RelinkItem(id.to_string()));
                ui.close_menu();
            }
        });
        let is_media = item.is_some_and(|it| matches!(it.kind, MediaKind::Video | MediaKind::Audio));
        if is_media && !trash {
            ui.menu_button("影音", |ui| {
                if ui.button("產生字幕（語音轉文字）…").clicked() {
                    self.act(Action::OpenWhisper(targets.clone()));
                    ui.close_menu();
                }
                if ui
                    .button("整場處理（音量 → 精華候選 → 字幕）")
                    .on_hover_text("在背景依序分析音量、讀聊天室紀錄、建立精華候選，沒有字幕的排入語音轉文字；可多選，進度在狀態列的背景工作")
                    .clicked()
                {
                    self.act(Action::ProcessStreams(targets.clone()));
                    ui.close_menu();
                }
                let hint = "在預覽中開啟這個項目並開始；結果在「片段」分頁";
                if ui.button("分析音量").on_hover_text(hint).clicked() {
                    self.act(Action::Media(id.to_string(), MediaTask::Loudness));
                    ui.close_menu();
                }
                if ui.button("自動找精華候選").on_hover_text(hint).clicked() {
                    self.act(Action::Media(id.to_string(), MediaTask::Candidates));
                    ui.close_menu();
                }
            });
        }
        ui.menu_button("匯出", |ui| {
            if ui.button("匯出檔案…").clicked() {
                self.act(Action::ExportSelected);
                ui.close_menu();
            }
            let label = if n > 1 { format!("Premiere 用 XML（{n} 個）…") } else { "Premiere 用 XML…".to_string() };
            if ui.button(label).clicked() {
                self.act(Action::ExportPremiereXmlFor(targets.clone()));
                ui.close_menu();
            }
        });
        ui.separator();
        if trash {
            if ui.button("還原").clicked() {
                self.act(Action::RestoreSelected);
                ui.close_menu();
            }
            if menu_item(ui, RichText::new("永久刪除").color(danger()), "Delete").clicked() {
                self.act(Action::ConfirmDeleteForever);
                ui.close_menu();
            }
        } else if menu_item(ui, "移到垃圾桶", "Delete").clicked() {
            self.act(Action::TrashSelected);
            ui.close_menu();
        }
    }
}

/// A menu entry with its keyboard shortcut on the right.
fn menu_item(ui: &mut Ui, label: impl Into<egui::WidgetText>, shortcut: &str) -> egui::Response {
    ui.add(egui::Button::new(label).shortcut_text(RichText::new(shortcut).weak()))
}
