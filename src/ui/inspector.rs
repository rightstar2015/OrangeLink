//! The right-hand inspector for one or several selected items.

use super::*;

/// Suggestions for the tag being typed: existing tags that start with it, then
/// those that contain it, the most used first in each; none the item has.
pub(super) fn tag_suggestions(all: &[(String, usize)], typed: &str, has: &[String], max: usize) -> Vec<String> {
    let q = typed.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<(bool, usize, &String)> = all
        .iter()
        .filter(|(t, _)| !has.contains(t))
        .filter_map(|(t, n)| {
            let l = t.to_lowercase();
            if l.starts_with(&q) {
                Some((false, *n, t))
            } else if l.contains(&q) {
                Some((true, *n, t))
            } else {
                None
            }
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then(a.2.cmp(b.2)));
    hits.into_iter().take(max).map(|(.., t)| t.clone()).collect()
}

/// The box for adding tags, with suggestions under it: ↑↓ pick one, Tab or
/// Enter takes it, Enter alone takes what was typed (commas split it). Returns
/// the tags to add.
pub(super) fn tag_box(ui: &mut Ui, input: &mut String, pick: &mut Option<usize>, all: &[(String, usize)], has: &[String], hint: &str, focus: bool) -> Vec<String> {
    let id = ui.make_persistent_id("tag-box");
    let suggestions = tag_suggestions(all, input, has, 8);
    if pick.is_some_and(|p| p >= suggestions.len()) {
        *pick = None;
    }
    if ui.memory(|m| m.has_focus(id)) && !suggestions.is_empty() {
        let last = suggestions.len() - 1;
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowDown) {
                *pick = Some(pick.map_or(0, |p| (p + 1).min(last)));
            }
            if i.consume_key(egui::Modifiers::NONE, Key::ArrowUp) {
                *pick = pick.and_then(|p| p.checked_sub(1));
            }
        });
    }
    let r = ui.add(TextEdit::singleline(input).id(id).hint_text(hint).desired_width(f32::INFINITY).lock_focus(!suggestions.is_empty()));
    if focus {
        r.request_focus();
        r.scroll_to_me(Some(Align::Center));
    }
    if r.changed() {
        *pick = None;
    }
    let tab = r.has_focus() && ui.input(|i| i.key_pressed(Key::Tab));
    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let mut out = Vec::new();
    let chosen = pick.or((tab && !suggestions.is_empty()).then_some(0));
    match chosen.filter(|_| tab || enter) {
        Some(p) => out.push(suggestions[p].clone()),
        None if enter => out.extend(input.split(['、', ',', '，']).map(str::trim).filter(|t| !t.is_empty()).map(String::from)),
        None => {}
    }
    if !out.is_empty() || enter {
        input.clear();
        *pick = None;
        r.request_focus();
    }
    if !suggestions.is_empty() && out.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for (n, t) in suggestions.iter().enumerate() {
                if ui.selectable_label(*pick == Some(n), RichText::new(t).small()).clicked() {
                    out.push(t.clone());
                    input.clear();
                    *pick = None;
                }
            }
        });
    }
    out
}

/// A related item for the inspector: id, name, kind, thumbnail.
type Related = (String, String, String, Option<(egui::TextureId, egui::Vec2)>);

/// A foldable inspector section's title; a fold is queued in `pending`.
/// `title` is a translation key, and in Chinese in the saved fold state
/// ("inspector:標籤"), so a section stays folded in every language.
/// Returns (open, ＋ clicked).
fn inspector_section(ui: &mut Ui, collapsed: &[String], title: &'static str, add: Option<&str>, pending: &mut Vec<Action>) -> (bool, bool) {
    let key = format!("inspector:{title}");
    let open = !collapsed.contains(&key);
    let (fold, added) = section_header(ui, crate::i18n::tr(title), open, add);
    if fold {
        pending.push(Action::ToggleSection(key));
    }
    (open, added)
}

impl App {
    pub fn inspector(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("inspector")
            .resizable(true)
            .default_width(300.0)
            .width_range(240.0..=520.0)
            .show(ctx, |ui| {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(8.0);
                    match self.selected.len() {
                        0 => self.inspector_empty(ui),
                        1 => self.inspector_single(ui),
                        _ => self.inspector_multi(ui),
                    }
                });
            });
    }

    pub(super) fn inspector_empty(&self, ui: &mut Ui) {
        let Some(lib) = &self.lib else { return };
        let stats = self.cached_sidebar_stats();
        ui.label(RichText::new(t!("資源庫")).strong());
        egui::Grid::new("lib-stats").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label(RichText::new(t!("項目")).weak());
            ui.label(stats.total.to_string());
            ui.end_row();
            ui.label(RichText::new(t!("總大小")).weak());
            ui.label(human_size(stats.total_size));
            ui.end_row();
            ui.label(RichText::new(t!("資料夾")).weak());
            ui.label(lib.meta.folders.len().to_string());
            ui.end_row();
            ui.label(RichText::new(t!("標籤")).weak());
            ui.label(stats.tag_counts.len().to_string());
            ui.end_row();
        });
        ui.add_space(16.0);
        ui.label(RichText::new(t!("快速鍵")).strong());
        self.brief_shortcuts(ui);
    }

    pub(super) fn inspector_single(&mut self, ui: &mut Ui) {
        let Some(id) = self.selected.iter().next().cloned() else { return };
        let thumb = self.preview_tex(&id).or_else(|| self.thumb_tex(&id));
        let progress = self.preview_progress(&id);
        let in_trash = self.view == View::Trash;
        let stats = self.cached_sidebar_stats();
        let all_tags = stats.tag_counts;
        let folders = stats.folder_flat;
        let (field_names, name) = match &self.lib {
            Some(lib) => (
                lib.meta.fields.clone(),
                lib.items.get(&id).map(|i| i.name.clone()).unwrap_or_default(),
            ),
            None => return,
        };
        // Related items with their thumbnails, before the library is borrowed for editing.
        let related: Vec<Related> = self
            .lib
            .as_ref()
            .map(|lib| lib.linked(&id).iter().map(|r| (r.id.clone(), r.name.clone(), r.kind.label().to_string(), self.thumb_tex(&r.id))).collect())
            .unwrap_or_default();
        let collapsed = self.config.collapsed_sections.clone();
        // Actions are collected locally: `self.lib` is mutably borrowed below.
        let mut pending: Vec<Action> = Vec::new();
        let focus_tag = std::mem::take(&mut self.focus_tag);

        if let Some((tex, size)) = thumb {
            let w = ui.available_width();
            let scale = (w / size.x).min(260.0 / size.y);
            ui.vertical_centered(|ui| {
                let r = ui.add(egui::Image::new((tex, size * scale)).sense(Sense::click()));
                if r.hovered() {
                    self.hover_now.set(Some(id.clone()));
                }
                paint_progress(ui.painter(), r.rect, progress);
                if r.on_hover_text(t!("點兩下預覽")).double_clicked() {
                    pending.push(Action::OpenViewer(id.clone()));
                }
            });
            ui.add_space(6.0);
        }

        let (basic_open, _) = inspector_section(ui, &collapsed, tk!("基本資訊"), None, &mut pending);
        if basic_open {
            // A click edits the name in place, with the same box as F2.
            let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let editing = self.renaming.borrow().as_ref().is_some_and(|e| e.is(&id, true));
            let strong = ui.visuals().strong_text_color();
            self.name_field(ui, rect.shrink2(vec2(2.0, 3.0)), &id, &name, 18.0, strong, true);
            if !editing {
                if r.hovered() {
                    ui.painter().rect_stroke(rect, 3.0, Stroke::new(1.0_f32, ui.visuals().widgets.hovered.bg_stroke.color));
                }
                if r.on_hover_text(t!("{name}\n點一下重新命名（F2）", name)).clicked() {
                    pending.push(Action::StartRename(id.clone(), true));
                }
            }
        }

        // The sidebar's ＋ opens the section to reach the box; keep focus_tag for next frame.
        if focus_tag && collapsed.iter().any(|k| k == "inspector:標籤") {
            self.focus_tag = true;
            pending.push(Action::ToggleSection("inspector:標籤".into()));
        }

        let tag_input = &mut self.tag_input;
        let tag_pick = &mut self.tag_pick;
        let Some(lib) = self.lib.as_mut() else { return };
        let Some(it) = lib.items.get_mut(&id) else { return };
        let mut before: Option<crate::model::Item> = None;
        let snapshot = |slot: &mut Option<crate::model::Item>, item: &crate::model::Item| {
            if slot.is_none() {
                *slot = Some(item.clone());
            }
        };

        if basic_open { ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for i in 1..=5u8 {
                let filled = it.rating >= i;
                let star = RichText::new(if filled { "★" } else { "☆" })
                    .size(20.0)
                    .color(if filled { star() } else { ui.visuals().weak_text_color() });
                if ui.add(egui::Label::new(star).sense(Sense::click())).clicked() {
                    snapshot(&mut before, it);
                    it.rating = if it.rating == i { 0 } else { i };
                }
            }
        }); }

        if inspector_section(ui, &collapsed, tk!("標籤"), None, &mut pending).0 {
            ui.horizontal_wrapped(|ui| {
                let mut remove = None;
                for t in &it.tags {
                    let (filter, x) = chip_parts(ui, t, true);
                    if filter {
                        pending.push(Action::ToggleTagFilter(t.clone()));
                    }
                    if x {
                        remove = Some(t.clone());
                    }
                }
                if let Some(t) = remove {
                    snapshot(&mut before, it);
                    it.tags.retain(|x| *x != t);
                }
            });
            for t in tag_box(ui, tag_input, tag_pick, &all_tags, &it.tags, t!("輸入標籤後按 Enter（可用逗號或頓號分隔）"), focus_tag) {
                if !it.tags.contains(&t) {
                    snapshot(&mut before, it);
                    it.tags.push(t);
                }
            }
        }

        if inspector_section(ui, &collapsed, tk!("資料夾"), None, &mut pending).0 {
        ui.horizontal_wrapped(|ui| {
            let mut remove = None;
            for fid in &it.folders {
                let name = folders.iter().find(|(id, _, _)| id == fid).map(|f| f.1.as_str()).unwrap_or("?");
                if chip(ui, &format!("📁 {name}")) {
                    remove = Some(fid.clone());
                }
            }
            if let Some(f) = remove {
                snapshot(&mut before, it);
                it.folders.retain(|x| *x != f);
            }
            ui.menu_button(t!("＋"), |ui| {
                if folders.is_empty() {
                    ui.label(RichText::new(t!("尚無資料夾")).weak());
                }
                for (fid, name, depth) in &folders {
                    let has = it.folders.contains(fid);
                    if ui.selectable_label(has, format!("{}📁 {name}", "    ".repeat(*depth))).clicked() {
                        snapshot(&mut before, it);
                        if has {
                            it.folders.retain(|x| x != fid);
                        } else {
                            it.folders.push(fid.clone());
                        }
                    }
                }
            });
        });
        }

        if inspector_section(ui, &collapsed, tk!("註解與網址"), None, &mut pending).0 {
        ui.label(RichText::new(t!("註解")).small().weak());
        let mut note_draft = it.annotation.clone();
        let note_changed = ui
            .scope(|ui| {
                ui.visuals_mut().extreme_bg_color = tokens().bg_elevated;
                ui.add(TextEdit::multiline(&mut note_draft).desired_rows(3).desired_width(f32::INFINITY)).changed()
            })
            .inner;
        if note_changed {
            snapshot(&mut before, it);
            it.annotation = note_draft;
        }

        ui.label(RichText::new(t!("來源網址")).small().weak());
        ui.horizontal(|ui| {
            let open_w = if it.url.is_empty() { 0.0 } else { 30.0 };
            let mut url_draft = it.url.clone();
            if ui
                .add(TextEdit::singleline(&mut url_draft).hint_text("https://").desired_width(ui.available_width() - open_w))
                .changed()
            {
                snapshot(&mut before, it);
                it.url = url_draft;
            }
            if !it.url.is_empty() && ui.small_button("↗").on_hover_text(t!("在瀏覽器開啟")).clicked() {
                let _ = open::that_detached(&it.url);
            }
        });
        }

        let (fields_open, add_field) = inspector_section(ui, &collapsed, tk!("自訂欄位"), Some(t!("新增一個所有素材共用的欄位")), &mut pending);
        if add_field {
            pending.push(Action::NewField);
        }
        if fields_open {
        if field_names.is_empty() {
            ui.label(RichText::new(t!("例如：授權、客戶、拍攝地點")).small().weak());
        }
        egui::Grid::new("fields").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for name in &field_names {
                let current = it.fields.get(name).cloned().unwrap_or_default();
                let label = ui.add(egui::Label::new(RichText::new(name).small()).sense(Sense::click()));
                label.on_hover_text(t!("右鍵：建立智慧資料夾、重新命名或刪除欄位")).context_menu(|ui| {
                    if !current.is_empty() && ui.button(t!("建立智慧資料夾：{name} = {current}", name, current)).clicked() {
                        pending.push(Action::FieldSmartFolder(name.clone(), current.clone()));
                        ui.close_menu();
                    }
                    if ui.button(t!("建立智慧資料夾：已填寫「{name}」", name)).clicked() {
                        pending.push(Action::FieldSmartFolder(name.clone(), String::new()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(t!("重新命名欄位…")).clicked() {
                        pending.push(Action::RenameField(name.clone()));
                        ui.close_menu();
                    }
                    if ui.button(t!("刪除欄位")).clicked() {
                        pending.push(Action::ConfirmDeleteField(name.clone()));
                        ui.close_menu();
                    }
                });
                let mut value = it.fields.get(name).cloned().unwrap_or_default();
                if ui.add(TextEdit::singleline(&mut value).desired_width(f32::INFINITY)).changed() {
                    snapshot(&mut before, it);
                    if value.trim().is_empty() {
                        it.fields.remove(name);
                    } else {
                        it.fields.insert(name.clone(), value);
                    }
                }
                ui.end_row();
            }
        });
        }

        if !related.is_empty() && inspector_section(ui, &collapsed, tk!("相關素材"), None, &mut pending).0 {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (rid, rname, kind, thumb) in &related {
                    let (rect, resp) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::click());
                    ui.painter().rect_filled(rect, ROUNDING, ui.visuals().extreme_bg_color);
                    if let Some((tex, size)) = *thumb {
                        ui.painter().image(tex, fit_rect(rect.shrink(2.0), size), UV, Color32::WHITE);
                    }
                    if resp.hovered() {
                        ui.painter().rect_stroke(rect, ROUNDING, Stroke::new(2.0_f32, accent()));
                    }
                    let resp = resp.on_hover_text(t!("{rname}（{kind}）\n點一下前往；右鍵解除關聯", rname, kind));
                    if resp.clicked() {
                        pending.push(Action::GoToItem(rid.clone()));
                    }
                    resp.context_menu(|ui| {
                        if ui.button(t!("解除關聯")).clicked() {
                            pending.push(Action::UnlinkItems(id.clone(), rid.clone()));
                            ui.close_menu();
                        }
                    });
                }
            });
        }

        if !it.palette.is_empty() && inspector_section(ui, &collapsed, tk!("色彩"), None, &mut pending).0 {
            ui.label(RichText::new(t!("點選以依顏色篩選")).small().weak());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for p in &it.palette {
                    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
                    ui.painter().rect_filled(rect, ROUNDING, rgb(p.rgb));
                    if resp.hovered() {
                        ui.painter().rect_stroke(rect, ROUNDING, Stroke::new(2.0_f32, tokens().text));
                    }
                    let hex = format!("#{:02X}{:02X}{:02X}", p.rgb[0], p.rgb[1], p.rgb[2]);
                    if resp.on_hover_text(format!("{hex}  {:.0}%", p.ratio * 100.0)).clicked() {
                        pending.push(Action::SetColorFilter(Some(p.rgb)));
                    }
                }
            });
        }

        let info_open = inspector_section(ui, &collapsed, tk!("檔案資訊"), None, &mut pending).0;
        if let Some(text) = it.text.as_ref().filter(|_| info_open) {
            ui.label(RichText::new(t!("內容預覽")).small().weak());
            let shown = if matches!(it.ext.as_str(), "srt" | "vtt" | "ass" | "ssa" | "lrc") {
                crate::media::subtitle_lines(text)
            } else {
                text.clone()
            };
            egui::Frame::none().fill(ui.visuals().extreme_bg_color).rounding(ROUNDING).inner_margin(6.0).show(ui, |ui| {
                ScrollArea::vertical().id_salt("text-preview").max_height(180.0).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(shown).small()).wrap());
                });
            });
        }

        if info_open {
        ui.add_space(4.0);
        egui::Grid::new("info").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let mut rows = vec![(t!("類型"), format!("{} {}", it.kind.icon(), it.kind.label()))];
            if it.width > 0 {
                rows.push((t!("尺寸"), format!("{} × {}", it.width, it.height)));
            }
            if let Some(d) = it.duration {
                rows.push((t!("長度"), fmt_duration(d)));
            }
            if let Some(fps) = it.fps {
                rows.push((t!("幀率"), format!("{fps:.3}").trim_end_matches('0').trim_end_matches('.').to_string() + " fps"));
            }
            if let Some(codec) = &it.codec {
                rows.push((t!("編碼"), codec.to_uppercase()));
            }
            if !it.clips.is_empty() {
                rows.push((t!("片段"), t!("{n} 個（在預覽中編輯）", n = it.clips.len())));
            }
            rows.extend([
                (t!("大小"), human_size(it.size)),
                (t!("格式"), it.ext.to_uppercase()),
                (t!("加入"), fmt_time(it.added_at)),
                (t!("使用"), match it.last_used {
                    Some(t) => t!("{use_count} 次，最近 {t}", use_count = it.use_count, t = fmt_time(t)),
                    None => t!("尚未使用").to_string(),
                }),
                (t!("修改"), fmt_time(it.modified_at)),
            ]);
            for (k, val) in rows {
                ui.label(RichText::new(k).small().weak());
                ui.label(RichText::new(val).small());
                ui.end_row();
            }
            ui.label(RichText::new(t!("位置")).small().weak());
            match &it.source {
                Some(src) => ui.add(egui::Label::new(RichText::new(t!("🔗 原位置\n{src}", src = src.display())).small()).wrap()),
                None => ui.label(RichText::new(t!("資源庫內（複本）")).small()),
            };
            ui.end_row();
        });
        }

        ui.add_space(10.0);
        let is_video = it.kind == MediaKind::Video;
        ui.horizontal_wrapped(|ui| {
            if is_video && ui.button(RichText::new(t!("✂ 剪輯工作台")).color(accent())).clicked() {
                pending.push(Action::OpenViewer(id.clone()));
            }
            if ui.button(t!("開啟")).clicked() {
                pending.push(Action::OpenExternal(id.clone()));
            }
            if ui.button(t!("檔案總管")).clicked() {
                pending.push(Action::ShowInExplorer(id.clone()));
            }
            if in_trash {
                if ui.button(t!("還原")).clicked() {
                    pending.push(Action::RestoreSelected);
                }
            } else if ui.button("🗑").on_hover_text(t!("移到垃圾桶")).clicked() {
                pending.push(Action::TrashSelected);
            }
        });

        if let Some(before) = before {
            lib.touch(&id);
            self.record_undo(vec![before], true);
        }
        for a in pending {
            self.act(a);
        }
    }

    pub(super) fn inspector_multi(&mut self, ui: &mut Ui) {
        let sum = self.multi_summary();
        let ids = &sum.ids;
        let stats = self.cached_sidebar_stats();

        ui.label(RichText::new(t!("已選取 {n} 個項目", n = ids.len())).heading());
        ui.label(RichText::new(human_size(sum.size)).weak());
        ui.add_space(6.0);

        // Stacked preview of the first few.
        let w = ui.available_width();
        let cell = ((w - 12.0) / 3.0).min(90.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
            for id in ids.iter().take(9) {
                let (rect, _) = ui.allocate_exact_size(vec2(cell, cell), Sense::hover());
                ui.painter().rect_filled(rect, ROUNDING, ui.visuals().extreme_bg_color);
                if let Some((tex, size)) = self.thumb_tex(id) {
                    ui.painter().image(tex, fit_rect(rect.shrink(2.0), size), UV, Color32::WHITE);
                }
            }
        });

        ui.add_space(8.0);
        let first_rating = sum.rating.unwrap_or(0);
        let uniform_rating = sum.rating.is_some();
        let rating_label = if uniform_rating {
            t!("評分（套用到全部）")
        } else {
            t!("評分（目前為混合評分，點選套用到全部）")
        };
        ui.label(RichText::new(rating_label).small().weak());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for i in 1..=5u8 {
                let filled = uniform_rating && first_rating >= i;
                let color = if filled {
                    star()
                } else if !uniform_rating {
                    star().gamma_multiply(0.45)
                } else {
                    ui.visuals().weak_text_color()
                };
                let glyph = if filled { "★" } else { "☆" };
                if ui.add(egui::Label::new(RichText::new(glyph).size(20.0).color(color)).sense(Sense::click())).clicked() {
                    self.act(Action::SetRating(ids.clone(), i));
                }
            }
            if ui.small_button(t!("清除")).clicked() {
                self.act(Action::SetRating(ids.clone(), 0));
            }
        });

        ui.add_space(8.0);
        ui.label(RichText::new(t!("共同標籤")).small().weak());
        let common = &sum.common_tags;
        ui.horizontal_wrapped(|ui| {
            if common.is_empty() {
                ui.label(RichText::new(t!("無")).small().weak());
            }
            for t in common {
                let (filter, x) = chip_parts(ui, t, true);
                if filter {
                    self.act(Action::ToggleTagFilter(t.clone()));
                }
                if x {
                    self.act(Action::RemoveTag(ids.clone(), t.clone()));
                }
            }
        });
        let folders = &stats.folder_flat;
        let all_tags = &stats.tag_counts;
        let focus = std::mem::take(&mut self.focus_tag);
        for t in tag_box(ui, &mut self.tag_input, &mut self.tag_pick, all_tags, common, t!("為全部新增標籤，按 Enter"), focus) {
            self.act(Action::AddTag(ids.clone(), t));
        }

        ui.add_space(8.0);
        let mut pending = Vec::new();
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("自訂欄位（套用到全部）")).small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(t!("＋ 欄位")).clicked() {
                    pending.push(Action::NewField);
                }
            });
        });
        egui::Grid::new("multi-fields").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for (name, shared) in &sum.fields {
                let hint = shared.clone().unwrap_or_else(|| t!("（多個不同的值）").to_string());
                ui.label(RichText::new(name).small()).context_menu(|ui| {
                    if ui.button(t!("清除這些項目的欄位值")).clicked() {
                        pending.push(Action::SetField(ids.clone(), name.clone(), String::new()));
                        ui.close_menu();
                    }
                });
                let draft = self.field_drafts.entry(name.clone()).or_default();
                ui.horizontal(|ui| {
                    let has_draft = !draft.trim().is_empty();
                    let w = (ui.available_width() - if has_draft { 28.0 } else { 0.0 }).max(60.0);
                    let r = ui.add(TextEdit::singleline(draft).hint_text(hint).desired_width(w));
                    let apply_clicked = has_draft && ui.small_button("✔").on_hover_text(t!("套用到選取的項目")).clicked();
                    let esc = ui.input(|i| i.key_pressed(Key::Escape));
                    if (apply_clicked || (r.lost_focus() && !esc)) && !draft.trim().is_empty() {
                        pending.push(Action::SetField(ids.clone(), name.clone(), std::mem::take(draft)));
                    } else if r.lost_focus() && esc {
                        draft.clear();
                    }
                });
                ui.end_row();
            }
        });
        for a in pending {
            self.act(a);
        }

        ui.add_space(8.0);
        let used_folders = &sum.folders;
        ui.horizontal_wrapped(|ui| {
            ui.menu_button(t!("📁 加入資料夾…"), |ui| {
                if folders.is_empty() {
                    ui.label(RichText::new(t!("尚無資料夾")).weak());
                }
                for (fid, name, depth) in folders {
                    if ui.button(format!("{}📁 {name}", "    ".repeat(*depth))).clicked() {
                        self.act(Action::AddToFolder(ids.clone(), fid.clone()));
                        ui.close_menu();
                    }
                }
            });
            if !used_folders.is_empty() {
                ui.menu_button(t!("📁 移出資料夾…"), |ui| {
                    for (fid, name, depth) in folders {
                        if used_folders.contains(fid) && ui.button(format!("{}📁 {name}", "    ".repeat(*depth))).clicked() {
                            self.act(Action::RemoveFromFolder(ids.clone(), fid.clone()));
                            ui.close_menu();
                        }
                    }
                });
            }
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button(t!("匯出…")).clicked() {
                self.act(Action::ExportSelected);
            }
            if ui.button(t!("複製檔案")).clicked() {
                self.act(Action::CopyFiles);
            }
            if ui.button(t!("批次重新命名…")).clicked() {
                self.act(Action::BatchRename(ids.clone()));
            }
            if self.view == View::Trash {
                if ui.button(t!("還原")).clicked() {
                    self.act(Action::RestoreSelected);
                }
                if ui.button(RichText::new(t!("永久刪除")).color(danger())).clicked() {
                    self.act(Action::ConfirmDeleteForever);
                }
            } else if ui.button(t!("🗑 移到垃圾桶")).clicked() {
                self.act(Action::TrashSelected);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::tag_suggestions;

    #[test]
    fn suggestions_put_prefixes_first_then_the_most_used() {
        let all: Vec<(String, usize)> = [("貓", 3), ("黑貓", 9), ("貓咪", 7), ("狗", 20), ("小貓", 1)].iter().map(|(t, n)| (t.to_string(), *n)).collect();
        assert_eq!(tag_suggestions(&all, "貓", &[], 8), ["貓咪", "貓", "黑貓", "小貓"]);
        // Tags the item has are left out; the count is capped.
        assert_eq!(tag_suggestions(&all, "貓", &["貓咪".to_string()], 2), ["貓", "黑貓"]);
        assert!(tag_suggestions(&all, "  ", &[], 8).is_empty());
        assert_eq!(tag_suggestions(&all, "Ｘ", &[], 8), Vec::<String>::new());
    }
}
