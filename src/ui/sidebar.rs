//! The left sidebar: library views, folders, smart folders and tags.

use super::*;
use std::collections::HashMap;

/// Counts and flat trees derived from the library, cached per `lib.revision`
/// so the sidebar and inspector don't rescan every item on every frame.
#[derive(Clone, Default)]
pub struct SidebarStats {
    pub total: usize,
    pub uncategorized: usize,
    pub untagged: usize,
    pub trash: usize,
    pub total_size: u64,
    pub by_kind: [usize; 4],
    pub folder_counts: HashMap<String, usize>,
    pub tag_counts: Vec<(String, usize)>,
    pub folder_flat: Vec<(String, String, usize)>,
}

impl SidebarStats {
    pub fn compute(lib: &Library) -> Self {
        let mut s = Self {
            folder_counts: lib.folder_counts(),
            tag_counts: lib.tag_counts(),
            folder_flat: lib.folder_flat(),
            ..Default::default()
        };
        for it in lib.items.values() {
            if it.deleted {
                s.trash += 1;
            } else {
                s.total += 1;
                s.total_size += it.size;
                s.uncategorized += usize::from(it.folders.is_empty());
                s.untagged += usize::from(it.tags.is_empty());
                let idx = MediaKind::ALL.iter().position(|k| *k == it.kind).unwrap_or(3);
                s.by_kind[idx] += 1;
            }
        }
        s
    }
}

impl App {
    /// Sidebar & inspector stats, rebuilt only when `lib.revision` advances.
    pub(super) fn cached_sidebar_stats(&self) -> SidebarStats {
        let Some(lib) = &self.lib else { return SidebarStats::default() };
        let mut cache = self.sidebar_cache.borrow_mut();
        if cache.0 != Some(lib.revision) {
            *cache = (Some(lib.revision), SidebarStats::compute(lib));
        }
        cache.1.clone()
    }

    pub fn sidebar(&mut self, ctx: &egui::Context) {
        let stats = self.cached_sidebar_stats();
        egui::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(230.0)
            .width_range(180.0..=420.0)
            .show(ctx, |ui| {
                let lib = self.lib.as_ref().unwrap();
                ui.add_space(6.0);
                // The library's name opens what can be done with the library; the
                // help menu sits at the bottom.
                let lib_btn = ui
                    .menu_button(RichText::new(format!("{}   ", lib.name())).heading().strong(), |ui| {
                    if ui.button("開啟其他資源庫…").clicked() {
                        self.act(Action::OpenLibraryDialog);
                        ui.close_menu();
                    }
                    if ui.button("新建資源庫…").clicked() {
                        self.act(Action::CreateLibraryDialog);
                        ui.close_menu();
                    }
                    let others: Vec<_> = self.config.recent.iter().filter(|p| **p != lib.root).collect();
                    if !others.is_empty() {
                        ui.menu_button("最近使用", |ui| {
                            for p in others {
                                if ui.button(p.display().to_string()).clicked() {
                                    self.act(Action::OpenLibrary(p.clone()));
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    ui.separator();
                    if ui.button("監看資料夾…").clicked() {
                        self.act(Action::WatchFoldersDialog);
                        ui.close_menu();
                    }
                    if ui.button("檢查遺失的檔案").clicked() {
                        self.act(Action::RefreshMissing);
                        ui.close_menu();
                    }
                    if ui.button("重新連結遺失的檔案…").clicked() {
                        self.act(Action::RelinkInFolder);
                        ui.close_menu();
                    }
                    if ui.button("找出完全相同的檔案").on_hover_text("內容一模一樣的檔案（例如改過名字的同一支影片）。大檔案會再逐位元組比對確認，不會誤判").clicked() {
                        self.act(Action::FindExactDuplicates);
                        ui.close_menu();
                    }
                    if ui.button("找出可能重複的素材").on_hover_text("縮圖看起來很像的素材（縮放、重新存檔過的圖片等）").clicked() {
                        self.act(Action::FindDuplicates);
                        ui.close_menu();
                    }
                    if ui
                        .button("檢查資源庫")
                        .on_hover_text("找出中斷的匯入留下的資料夾、指向已刪除資料夾的素材，以及檔案內容和匯入時不同的素材，並可一鍵處理")
                        .clicked()
                    {
                        self.act(Action::CheckIntegrity);
                        ui.close_menu();
                    }
                    if ui.button("從備份還原…").clicked() {
                        self.act(Action::BackupsDialog);
                        ui.close_menu();
                    }
                    if ui.button("加入預設的智慧資料夾").clicked() {
                        self.act(Action::RestoreDefaultSmart);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("匯出 Premiere 用 XML…（實驗性）").clicked() {
                        self.act(Action::ExportPremiereXml);
                        ui.close_menu();
                    }
                    if ui.button("開啟資源庫資料夾").clicked() {
                        let _ = open::that_detached(&lib.root);
                        ui.close_menu();
                    }
                    })
                    .response;
                paint_arrow(ui.painter(), lib_btn.rect.right_center() - vec2(10.0, 0.0), Arrow::Open, ui.visuals().weak_text_color());
                lib_btn.on_hover_text(format!("資源庫位置：{}\n點一下開啟資源庫選單", lib.root.display()));
                ui.separator();

                let total = stats.total;
                let uncategorized = stats.uncategorized;
                let untagged = stats.untagged;
                let trash = stats.trash;
                let by_kind = stats.by_kind;
                let folder_counts = &stats.folder_counts;
                let tags = &stats.tag_counts;
                // Edited through a local so the closure below only needs `&self`.
                let mut tag_search = std::mem::take(&mut self.tag_search);
                let mut open_ai_log = false;

                egui::TopBottomPanel::bottom("sidebar-help").show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.menu_button("❓ 說明", |ui| {
                            if ui.add(egui::Button::new("快速鍵一覽").shortcut_text("? / F1")).clicked() {
                                self.act(Action::ShowShortcuts);
                                ui.close_menu();
                            }
                            if ui.button(format!("查看新版本（目前 {}）", env!("CARGO_PKG_VERSION"))).clicked() {
                                let _ = open::that_detached(crate::update::releases_url());
                                ui.close_menu();
                            }
                            if ui.button("回報問題…").on_hover_text("在瀏覽器開啟 GitHub 回報頁面，需要 GitHub 帳號").clicked() {
                                let _ = open::that_detached(crate::update::report_url());
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("開啟記錄資料夾").clicked() {
                                if let Some(d) = crate::log::dir() {
                                    let _ = std::fs::create_dir_all(&d);
                                    let _ = open::that_detached(d);
                                }
                                ui.close_menu();
                            }
                            if ui.button("複製最近的記錄").on_hover_text("複製最後 200 行，可以貼到回報裡；不會自動上傳").clicked() {
                                ui.ctx().copy_text(crate::log::tail(200));
                                ui.close_menu();
                            }
                            ui.separator();
                            ui.label(RichText::new(format!("OrangeLink {}", env!("CARGO_PKG_VERSION"))).small().weak());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if self.mcp.is_running() {
                                if ui
                                    .add(egui::Button::new(RichText::new("● MCP").small().color(good())).frame(false))
                                    .on_hover_text("本機 MCP 伺服器執行中；按一下查看 AI 操作紀錄")
                                    .clicked()
                                {
                                    open_ai_log = true;
                                }
                            } else if ui
                                .add(egui::Button::new(RichText::new("○ MCP").small().weak()).frame(false))
                                .on_hover_text("AI 助理整合（MCP）尚未啟用；按一下開啟整合設定")
                                .clicked()
                            {
                                self.act(Action::OpenSettingsAt(crate::config::SettingsPage::Integration));
                            }
                        });
                    });
                });

                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (view, label, n) in [
                        (View::All, "🖼  全部", total),
                        (View::Uncategorized, "📥  未分類", uncategorized),
                        (View::Untagged, "🏷  未標籤", untagged),
                        (View::Trash, "🗑  垃圾桶", trash),
                    ] {
                        let r = nav_row(ui, self.view == view, 0.0, label, Some(n));
                        if r.clicked() {
                            self.act(Action::SetView(view));
                        }
                    }

                    let open = |name: &str| self.section_open(&format!("sidebar:{name}"));
                    let fold = |name: &str| Action::ToggleSection(format!("sidebar:{name}"));
                    let (toggle, _) = section_header(ui, "類型", open("類型"), None);
                    if toggle {
                        self.act(fold("類型"));
                    }
                    for (kind, n) in MediaKind::ALL.into_iter().zip(by_kind).filter(|_| open("類型")) {
                        let view = View::Kind(kind);
                        let label = format!("{}  {}", kind.icon(), kind.label());
                        if nav_row(ui, self.view == view, 0.0, &label, Some(n)).clicked() {
                            self.act(Action::SetView(view));
                        }
                    }

                    let (toggle, add) = section_header(ui, "智慧資料夾", open("智慧資料夾"), Some("把目前的篩選條件儲存為智慧資料夾"));
                    if toggle {
                        self.act(fold("智慧資料夾"));
                    }
                    if add {
                        self.act(Action::SaveSmartFolder);
                    }
                    if open("智慧資料夾") {
                        if lib.meta.smart_folders.is_empty() {
                            ui.label(RichText::new("設定篩選條件後按 ＋ 儲存").small().weak());
                        }
                        self.smart_tree(ui, lib, None, 0);
                    }
                    if egui::DragAndDrop::has_payload_of_type::<DragSmart>(ui.ctx()) {
                        let r = nav_row(ui, false, 0.0, "↑ 拖到這裡移到最上層", None);
                        if let Some(p) = r.dnd_release_payload::<DragSmart>() {
                            self.act(Action::MoveSmart(p.0.clone(), None));
                        }
                    }

                    let (toggle, add) = section_header(ui, "資料夾", open("資料夾"), Some("新增資料夾"));
                    if toggle {
                        self.act(fold("資料夾"));
                    }
                    if add {
                        self.act(Action::NewFolder(None));
                    }
                    if open("資料夾") {
                        if lib.meta.folders.is_empty() {
                            ui.label(RichText::new("尚無資料夾").small().weak());
                        }
                        self.folder_tree(ui, lib, None, 0, folder_counts);
                    }
                    // Drop zone for moving a folder back to the top level.
                    if egui::DragAndDrop::has_payload_of_type::<DragFolder>(ui.ctx()) {
                        let r = nav_row(ui, false, 0.0, "↑ 拖到這裡移到最上層", None);
                        if let Some(p) = r.dnd_release_payload::<DragFolder>() {
                            self.act(Action::MoveFolder(p.0.clone(), None));
                        }
                    }

                    // ＋ only when there is something selected to tag.
                    let add_tag = (!self.selected.is_empty()).then_some("為選取的素材加上標籤");
                    let (toggle, add) = section_header(ui, "標籤", open("標籤"), add_tag);
                    if toggle {
                        self.act(fold("標籤"));
                    }
                    if add {
                        self.act(Action::FocusTagInput);
                    }
                    if !open("標籤") {
                        return;
                    }
                    if tags.len() > 8 {
                        ui.add(TextEdit::singleline(&mut tag_search).hint_text("篩選標籤").desired_width(f32::INFINITY));
                    }
                    let q = tag_search.to_lowercase();
                    let shown = |t: &String| q.is_empty() || t.to_lowercase().contains(&q);
                    let counts: std::collections::HashMap<&String, usize> = tags.iter().map(|(t, n)| (t, *n)).collect();
                    let grouped: std::collections::HashSet<&String> = lib.meta.tag_groups.iter().flat_map(|g| g.tags.iter()).collect();
                    for g in &lib.meta.tag_groups {
                        let n: usize = g.tags.iter().filter_map(|t| counts.get(t)).sum();
                        let arrow = if g.collapsed { Arrow::Closed } else { Arrow::Open };
                        let r = tree_row(ui, false, 0.0, arrow, &g.name, Some(n), None, Sense::click());
                        if r.clicked() {
                            self.act(Action::ToggleTagGroup(g.id.clone()));
                        }
                        r.context_menu(|ui| {
                            if ui.button("重新命名群組…").clicked() {
                                self.act(Action::RenameTagGroup(g.id.clone()));
                                ui.close_menu();
                            }
                            if ui.button("刪除群組（標籤保留）").clicked() {
                                self.act(Action::DeleteTagGroup(g.id.clone()));
                                ui.close_menu();
                            }
                        });
                        if !g.collapsed || !q.is_empty() {
                            for tag in g.tags.iter().filter(|t| shown(t) && counts.contains_key(t)) {
                                self.tag_row(ui, lib, tag, counts[tag], 28.0);
                            }
                        }
                    }
                    for (tag, n) in tags.iter().filter(|(t, _)| shown(t) && !grouped.contains(t)) {
                        self.tag_row(ui, lib, tag, *n, 0.0);
                    }
                    if !tags.is_empty() {
                        ui.horizontal(|ui| {
                            if ui.small_button("＋ 標籤群組").clicked() {
                                self.act(Action::NewTagGroup(None));
                            }
                        });
                    }
                });
                self.tag_search = tag_search;
                if open_ai_log {
                    self.show_ai_log = true;
                }
            });
    }

    pub(super) fn tag_row(&self, ui: &mut Ui, lib: &Library, tag: &String, n: usize, indent: f32) {
        let sel = self.tag_filter.contains(tag);
        let r = nav_row(ui, sel, indent, &format!("# {tag}"), Some(n));
        if r.clicked() {
            self.act(Action::ToggleTagFilter(tag.clone()));
        }
        if let Some(p) = r.dnd_release_payload::<DragIds>() {
            self.act(Action::AddTag(p.0.clone(), tag.clone()));
        }
        r.context_menu(|ui| {
            if ui.button("重新命名標籤…").clicked() {
                self.act(Action::RenameTag(tag.clone()));
                ui.close_menu();
            }
            ui.menu_button("移到群組", |ui| {
                for g in &lib.meta.tag_groups {
                    if ui.button(&g.name).clicked() {
                        self.act(Action::SetTagGroup(tag.clone(), Some(g.id.clone())));
                        ui.close_menu();
                    }
                }
                if lib.meta.tag_groups.iter().any(|g| g.tags.contains(tag)) && ui.button("移出群組").clicked() {
                    self.act(Action::SetTagGroup(tag.clone(), None));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("＋ 新群組…").clicked() {
                    self.act(Action::NewTagGroup(Some(tag.clone())));
                    ui.close_menu();
                }
            });
            if ui.button("刪除標籤").clicked() {
                self.act(Action::ConfirmDeleteTag(tag.clone()));
                ui.close_menu();
            }
        });
    }

    pub(super) fn smart_tree(&self, ui: &mut Ui, lib: &Library, parent: Option<&str>, depth: usize) {
        for s in lib.child_smart_folders(parent) {
            let indent = depth as f32 * 14.0;
            let view = View::Smart(s.id.clone());
            let (arrow, count) = match (s.group, s.collapsed) {
                (true, true) => (Arrow::Closed, None),
                (true, false) => (Arrow::Open, None),
                (false, _) => (Arrow::Leaf, self.smart_counts.1.get(&s.id).copied()),
            };
            let r = tree_row(ui, self.view == view, indent, arrow, &s.name, count, None, Sense::click_and_drag());
            if r.clicked() {
                if s.group {
                    self.act(Action::ToggleSmart(s.id.clone()));
                } else {
                    self.act(Action::SetView(view));
                }
            }
            if r.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), DragSmart(s.id.clone()));
            }
            if let Some(p) = r.dnd_release_payload::<DragSmart>() {
                if p.0 != s.id {
                    // Dropping on a group nests inside it; on a smart folder, next to it.
                    let target = if s.group { Some(s.id.clone()) } else { s.parent.clone() };
                    self.act(Action::MoveSmart(p.0.clone(), target));
                }
            }
            r.context_menu(|ui| {
                if s.group {
                    if ui.button("在此儲存目前的篩選條件…").clicked() {
                        self.act(Action::NewSmartIn { parent: Some(s.id.clone()), group: false });
                        ui.close_menu();
                    }
                    if ui.button("新增子群組…").clicked() {
                        self.act(Action::NewSmartIn { parent: Some(s.id.clone()), group: true });
                        ui.close_menu();
                    }
                } else {
                    if ui.button("編輯條件…").clicked() {
                        self.act(Action::EditSmartFilter(s.id.clone()));
                        ui.close_menu();
                    }
                    if ui.button("以目前的篩選條件更新").clicked() {
                        self.act(Action::UpdateSmartFilter(s.id.clone()));
                        ui.close_menu();
                    }
                }
                if ui.button("重新命名…").clicked() {
                    self.act(Action::RenameSmartFolder(s.id.clone()));
                    ui.close_menu();
                }
                if s.parent.is_some() && ui.button("移到最上層").clicked() {
                    self.act(Action::MoveSmart(s.id.clone(), None));
                    ui.close_menu();
                }
                let del = if s.group { "刪除群組（含裡面的智慧資料夾）" } else { "刪除智慧資料夾" };
                if ui.button(del).clicked() {
                    self.act(Action::DeleteSmartFolder(s.id.clone()));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("新增群組…").clicked() {
                    self.act(Action::NewSmartIn { parent: None, group: true });
                    ui.close_menu();
                }
            });
            if s.group && !s.collapsed {
                self.smart_tree(ui, lib, Some(&s.id), depth + 1);
            }
        }
    }

    pub(super) fn folder_tree(
        &self,
        ui: &mut Ui,
        lib: &Library,
        parent: Option<&str>,
        depth: usize,
        counts: &std::collections::HashMap<String, usize>,
    ) {
        for f in lib.child_folders(parent) {
            let view = View::Folder(f.id.clone());
            let has_children = !lib.child_folders(Some(&f.id)).is_empty();
            let arrow = match (has_children, f.collapsed) {
                (false, _) => Arrow::Leaf,
                (true, true) => Arrow::Closed,
                (true, false) => Arrow::Open,
            };
            let indent = depth as f32 * 14.0;
            let r = tree_row(
                ui,
                self.view == view,
                indent,
                arrow,
                &f.name,
                Some(counts.get(&f.id).copied().unwrap_or(0)),
                f.color.map(rgb),
                Sense::click_and_drag(),
            );
            if r.clicked() {
                // Clicking the arrow expands/collapses; anywhere else opens the folder.
                let on_arrow = r.interact_pointer_pos().is_some_and(|p| p.x < r.rect.left() + indent + 24.0);
                if has_children && on_arrow {
                    self.act(Action::ToggleFolder(f.id.clone()));
                } else {
                    self.act(Action::SetView(view));
                }
            }
            if r.drag_started() {
                egui::DragAndDrop::set_payload(ui.ctx(), DragFolder(f.id.clone()));
            }
            if let Some(p) = r.dnd_release_payload::<DragIds>() {
                self.act(Action::AddToFolder(p.0.clone(), f.id.clone()));
            }
            if let Some(p) = r.dnd_release_payload::<DragFolder>() {
                if p.0 != f.id {
                    self.act(Action::MoveFolder(p.0.clone(), Some(f.id.clone())));
                }
            }
            r.context_menu(|ui| {
                if ui.button("新增子資料夾…").clicked() {
                    self.act(Action::NewFolder(Some(f.id.clone())));
                    ui.close_menu();
                }
                if ui.button("重新命名…").clicked() {
                    self.act(Action::RenameFolder(f.id.clone()));
                    ui.close_menu();
                }
                ui.menu_button("顏色", |ui| {
                    ui.horizontal(|ui| {
                        for c in FOLDER_COLORS {
                            let (rect, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                            ui.painter().circle_filled(rect.center(), 8.0, rgb(c));
                            if resp.clicked() {
                                self.act(Action::SetFolderColor(f.id.clone(), Some(c)));
                                ui.close_menu();
                            }
                        }
                    });
                    if ui.button("清除顏色").clicked() {
                        self.act(Action::SetFolderColor(f.id.clone(), None));
                        ui.close_menu();
                    }
                });
                if f.parent.is_some() && ui.button("移到最上層").clicked() {
                    self.act(Action::MoveFolder(f.id.clone(), None));
                    ui.close_menu();
                }
                if ui.button("刪除資料夾").clicked() {
                    self.act(Action::ConfirmDeleteFolder(f.id.clone()));
                    ui.close_menu();
                }
            });
            if !f.collapsed {
                self.folder_tree(ui, lib, Some(&f.id), depth + 1, counts);
            }
        }
    }
}
