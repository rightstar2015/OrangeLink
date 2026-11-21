//! Carrying out the queued `Action`s.

use super::*;

impl App {
    pub(super) fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Click(id, mods) => self.select_click(id, mods),
            Action::OpenViewer(id) => self.open_viewer_at(id),
            Action::OpenViewerAt(id, t) => {
                // A subtitle file's line plays in the video it goes with.
                let media = self.lib.as_ref().and_then(|l| match l.items.get(&id) {
                    Some(it) if matches!(it.kind, MediaKind::Video | MediaKind::Audio) => Some(id.clone()),
                    Some(_) => l.media_for_subtitle(&id),
                    None => None,
                });
                match media {
                    Some(m) => {
                        self.viewer_start = Some((m.clone(), t));
                        self.open_viewer_at(m);
                    }
                    None => self.toast(t!("找不到和這個字幕同名的影片或音訊")),
                }
            }
            Action::CloseViewer => self.close_viewer(true),
            Action::ViewerStep(delta) => {
                let Some(v) = &self.viewer else { return };
                let n = self.visible.len() as i64;
                if n == 0 {
                    return;
                }
                let i = self.visible.iter().position(|x| *x == v.id).unwrap_or(0) as i64;
                let next = (i + delta as i64).rem_euclid(n) as usize;
                self.open_viewer_at(self.visible[next].clone());
            }
            Action::SelectAll => {
                self.selected = self.visible.iter().cloned().collect();
            }
            Action::ClearSelection => {
                self.selected.clear();
                self.anchor = None;
                self.caret = None;
            }
            Action::TrashSelected => {
                let ids = self.selected_ids();
                self.edit_items(&ids, |it| it.deleted = true);
                self.selected.clear();
                self.toast_undo(t!("已將 {n} 個項目移到垃圾桶", n = ids.len()));
            }
            Action::RestoreSelected => {
                let ids = self.selected_ids();
                self.edit_items(&ids, |it| it.deleted = false);
                self.selected.clear();
                self.toast(t!("已還原 {n} 個項目", n = ids.len()));
            }
            Action::ConfirmDeleteForever => {
                let ids = self.selected_ids();
                if !ids.is_empty() {
                    self.dialog = Some(Dialog::Confirm {
                        message: self.delete_message(&ids),
                        on_yes: Confirmed::DeleteForever(ids),
                    });
                }
            }
            Action::ConfirmEmptyTrash => {
                let n = self.lib.as_ref().map(|l| l.items.values().filter(|i| i.deleted).count()).unwrap_or(0);
                if n > 0 {
                    self.dialog = Some(Dialog::Confirm {
                        message: t!("清空垃圾桶？{n} 個項目將被永久刪除，無法復原。", n),
                        on_yes: Confirmed::EmptyTrash,
                    });
                }
            }
            Action::AddToFolder(ids, folder) => {
                let name = self.lib.as_ref().and_then(|l| l.folder_name(&folder)).unwrap_or_default().to_string();
                self.edit_items(&ids, |it| {
                    if !it.folders.contains(&folder) {
                        it.folders.push(folder.clone());
                    }
                });
                self.toast_undo(t!("已將 {n} 個項目加入「{name}」", n = ids.len(), name));
            }
            Action::RemoveFromFolder(ids, folder) => {
                let name = self.lib.as_ref().and_then(|l| l.folder_name(&folder)).unwrap_or_default().to_string();
                self.edit_items(&ids, |it| it.folders.retain(|f| *f != folder));
                self.toast_undo(t!("已將 {n} 個項目移出「{name}」", n = ids.len(), name));
            }
            Action::AddTag(ids, tag) => {
                let tag = tag.trim().to_string();
                if tag.is_empty() {
                    return;
                }
                self.edit_items(&ids, |it| {
                    if !it.tags.contains(&tag) {
                        it.tags.push(tag.clone());
                    }
                });
                if ids.len() > 1 {
                    self.toast_undo(t!("已為 {n} 個項目加上標籤「{tag}」", n = ids.len(), tag));
                }
            }
            Action::RemoveTag(ids, tag) => {
                self.edit_items(&ids, |it| it.tags.retain(|t| *t != tag));
            }
            Action::SetRating(ids, rating) => {
                self.edit_items(&ids, |it| it.rating = rating);
                if ids.len() > 1 {
                    let what = if rating == 0 { t!("清除了評分").to_string() } else { t!("評為 {stars}", stars = "★".repeat(rating as usize)) };
                    self.toast_undo(t!("已將 {n} 個項目{what}", n = ids.len(), what));
                }
            }
            Action::ToastButton(id) => {
                if let Some(i) = self.toast.list.iter().position(|t| t.id == id) {
                    let toast = self.toast.list.remove(i);
                    if let Some((_, action)) = toast.action {
                        self.apply(ctx, action);
                    }
                }
            }
            Action::UndoAi { first, last } => self.undo_ai(first, last, false),
            Action::GrantMcp { files } => self.grant_mcp(files),
            Action::UndoStep(seq) => {
                // A structural edit this frame is a later change than `seq`.
                self.track_structure();
                if self.edits.undo.last().is_some_and(|s| s.seq == seq) {
                    self.restore_items(false);
                } else {
                    self.toast(t!("之後又有其他修改；請用 Ctrl+Z 逐步復原"));
                }
            }
            Action::OpenPath(path) => {
                if let Err(e) = open::that_detached(&path) {
                    self.toast(t!("無法開啟：{e}", e));
                }
            }
            Action::RevealPath(path) => show_in_explorer(&path),
            Action::OpenExternal(id) => {
                self.mark_used(std::slice::from_ref(&id));
                if let Some(path) = self.item_path(&id) {
                    if let Err(e) = open::that_detached(&path) {
                        self.toast(t!("無法開啟：{e}", e));
                    }
                }
            }
            Action::PdfPage(page) => self.pdf_go(page),
            Action::PdfWheel(delta) => self.pdf_wheel(delta),
            Action::ShellMenu(ids) => {
                let paths: Vec<PathBuf> = ids.iter().filter_map(|id| self.item_path(id)).filter(|p| p.is_file()).collect();
                if paths.is_empty() {
                    self.toast(t!("找不到原始檔案"));
                } else if let Err(e) = crate::shell_menu::show(&paths, ctx.style().visuals.dark_mode) {
                    crate::log::error("shell_menu", &e);
                    self.toast(t!("無法開啟 Windows 選單：{e}", e));
                } else if let Some(lib) = &mut self.lib {
                    // Delete or rename in that menu moves files away from the library.
                    lib.refresh_missing();
                }
            }
            Action::ShowInExplorer(id) => {
                if let Some(path) = self.item_path(&id) {
                    show_in_explorer(&path);
                }
            }
            Action::CopyPaths(ids) => {
                let paths: Vec<String> = ids.iter().filter_map(|id| self.item_path(id)).map(|p| p.display().to_string()).collect();
                if !paths.is_empty() {
                    ctx.copy_text(paths.join("\r\n"));
                    self.toast(if paths.len() > 1 { t!("已複製 {n} 個檔案路徑", n = paths.len()) } else { t!("已複製檔案路徑").to_string() });
                }
            }
            Action::StartRename(id, in_inspector) => {
                if let Some(it) = self.lib.as_ref().and_then(|l| l.items.get(&id)) {
                    *self.renaming.borrow_mut() = Some(InlineEdit::new(id.clone(), it.name.clone(), in_inspector));
                    if !in_inspector {
                        self.scroll_to = Some(id);
                    }
                }
            }
            Action::RenameItem(id, name) => {
                let name = name.trim().to_string();
                let changed = self.lib.as_ref().and_then(|l| l.items.get(&id)).is_some_and(|it| it.name != name);
                if !name.is_empty() && changed {
                    self.edit_items(&[id], |it| it.name = name.clone());
                }
            }
            Action::Media(id, task) => {
                if self.viewer.as_ref().is_none_or(|v| v.id != id) {
                    self.open_viewer_at(id.clone());
                }
                self.viewer_tab = ViewerTab::Clips;
                self.pending_media = Some((id, task));
            }
            Action::ExportSelected => self.export_selected(),
            Action::ImportFiles => {
                if let Some(files) = rfd::FileDialog::new()
                    .set_title(t!("匯入圖片"))
                    .add_filter(t!("圖片"), imaging::SUPPORTED_EXTS)
                    .pick_files()
                {
                    self.apply(ctx, Action::ImportPaths(files));
                }
            }
            Action::ImportFolder => {
                if let Some(dir) = rfd::FileDialog::new().set_title(t!("匯入資料夾")).pick_folder() {
                    self.apply(ctx, Action::ImportPaths(vec![dir]));
                }
            }
            Action::ImportPaths(paths) => {
                if self.import_running() {
                    self.toast(if self.import_is_watch_scan() { t!("正在掃描監看資料夾，請稍候再試") } else { t!("匯入進行中，請稍候") });
                    return;
                }
                // A large drop (or a folder, until counted) gets the preview first.
                let min = self.config.import_preview_min;
                if min > 0 && (paths.len() > min || paths.iter().any(|p| p.is_dir())) {
                    self.preview_import(paths);
                } else {
                    self.apply(ctx, Action::ImportByMode(paths));
                }
            }
            Action::ImportByMode(paths) => match self.config.import_mode {
                ImportMode::Ask => {
                    let dirs = paths.iter().filter(|p| p.is_dir()).count();
                    self.dialog = Some(Dialog::ImportMode { paths, dirs, link: false, remember: false });
                }
                ImportMode::Copy => self.apply(ctx, Action::StartImport { paths, link: false }),
                ImportMode::Link => self.apply(ctx, Action::StartImport { paths, link: true }),
            },
            Action::StartImportWith { paths, link, folder, tags } => self.start_import(ctx, paths, link, folder, tags, false),
            Action::StartImport { paths, link } => {
                let folder = match &self.view {
                    View::Folder(f) if self.config.import_into_current_folder => Some(f.clone()),
                    _ => None,
                };
                self.start_import(ctx, paths, link, folder, Vec::new(), false);
            }
            Action::Activate(id) => {
                let kind = self.lib.as_ref().and_then(|l| l.items.get(&id)).map(|i| i.kind);
                if kind.is_some_and(|k| k != MediaKind::Image) && self.config.media_dblclick_external {
                    self.apply(ctx, Action::OpenExternal(id));
                } else {
                    self.open_viewer_at(id);
                }
            }
            Action::RefreshMissing => {
                if let Some(lib) = &mut self.lib {
                    lib.refresh_missing();
                    let n = lib.missing.len();
                    self.thumbs.failed.clear();
                    self.toast(if n == 0 { t!("所有檔案都在").to_string() } else { t!("有 {n} 個項目找不到原始檔案", n) });
                }
            }
            Action::OpenSettings => self.show_settings = true,
            Action::ShowShortcuts => self.show_shortcuts = true,
            Action::ShowUpdatePrompt => {
                if let Some(tag) = self.update.new_version().map(str::to_string) {
                    self.dialog = Some(Dialog::UpdatePrompt { tag });
                }
            }
            Action::OpenSettingsAt(page) => {
                self.config.settings_page = page;
                self.settings_search.clear();
                self.show_settings = true;
            }
            Action::InstallFfmpeg => match super::ytdlp_ui::winget_install("FFmpeg", "Gyan.FFmpeg") {
                Ok(()) => self.toast(t!("已開啟 FFmpeg 安裝視窗；裝好後重新開啟 OrangeLink，或到設定 →「工具」指定資料夾")),
                Err(e) => self.toast_error(e),
            },
            Action::CopyFiles => {
                let paths = self.selected_paths();
                if crate::clipboard::copy_files(&paths) {
                    self.mark_used(&self.selected_ids());
                    self.toast(t!("已複製 {n} 個檔案，可在檔案總管或其他程式貼上", n = paths.len()));
                }
            }
            Action::Paste => {
                let files = crate::clipboard::read_files();
                if !files.is_empty() {
                    self.apply(ctx, Action::ImportPaths(files));
                } else if let Some(lib) = &self.lib {
                    // Pasted images live in the library's inbox and are copied in like any file.
                    let inbox = lib.root.join("pasted");
                    match crate::clipboard::save_clipboard_image(&inbox) {
                        Some(path) => self.apply(ctx, Action::StartImport { paths: vec![path], link: false }),
                        None => self.toast(t!("剪貼簿裡沒有檔案或圖片")),
                    }
                }
            }
            Action::OpenWith(app, ids) => {
                let Some(program) = self.config.external_apps.get(app).cloned() else { return };
                let paths: Vec<PathBuf> = {
                    let Some(lib) = &self.lib else { return };
                    ids.iter().filter_map(|id| lib.items.get(id)).map(|it| lib.file_path(it)).collect()
                };
                self.mark_used(&ids);
                if let Err(e) = crate::process::visible(&program.path).args(&paths).spawn() {
                    self.toast(t!("無法啟動 {program}：{e}", program = program.name, e));
                }
            }
            Action::RelinkItem(id) => {
                let Some(file) = rfd::FileDialog::new().set_title(t!("選擇這個項目的新位置")).pick_file() else { return };
                if let Some(lib) = &mut self.lib {
                    lib.relink(&id, file);
                    self.thumbs.tex.remove(&id);
                    self.thumbs.failed.remove(&id);
                    self.toast(t!("已重新連結"));
                }
            }
            Action::RelinkInFolder => {
                let missing = self.lib.as_ref().map(|l| l.missing.len()).unwrap_or(0);
                if missing == 0 {
                    self.toast(t!("沒有遺失的檔案"));
                    return;
                }
                let Some(dir) = rfd::FileDialog::new().set_title(t!("在哪個資料夾尋找遺失的檔案？")).pick_folder() else { return };
                if let Some(lib) = &mut self.lib {
                    let fixed = lib.relink_in(&dir);
                    self.thumbs.failed.clear();
                    self.toast(t!("已重新連結 {fixed} / {missing} 個遺失的項目", fixed, missing));
                }
            }
            Action::WatchFoldersDialog => {
                let folders = self.lib.as_ref().map(|l| l.meta.watch_folders.as_slice()).unwrap_or_default();
                let missing = folders.iter().map(|w| w.path.clone()).filter(|p| !p.is_dir()).collect();
                self.dialog = Some(Dialog::WatchFolders { link: true, tags: Default::default(), missing });
            }
            Action::ScanWatchFolders => self.scan_watch_folders(ctx),
            Action::SaveSmartFolder => self.apply(ctx, Action::NewSmartIn { parent: None, group: false }),
            Action::NewSmartIn { parent, group } => {
                if !group && self.current_filter().is_empty() {
                    self.toast(t!("先設定搜尋或篩選條件，再儲存為智慧資料夾"));
                } else {
                    self.dialog = Some(Dialog::SmartFolderName { rename: None, name: String::new(), parent, group });
                }
            }
            Action::RenameSmartFolder(id) => {
                let name = self.lib.as_ref().and_then(|l| l.smart_folder(&id)).map(|s| s.name.clone()).unwrap_or_default();
                self.dialog = Some(Dialog::SmartFolderName { rename: Some(id), name, parent: None, group: false });
            }
            Action::ToggleSmart(id) => {
                if let Some(lib) = &mut self.lib {
                    lib.toggle_smart_collapsed(&id);
                }
            }
            Action::MoveSmart(id, parent) => {
                if let Some(lib) = &mut self.lib {
                    if !lib.move_smart_folder(&id, parent) {
                        self.toast(t!("不能把群組移到它自己裡面"));
                    }
                }
            }
            Action::UpdateSmartFilter(id) => {
                let filter = self.current_filter();
                if filter.is_empty() {
                    self.toast(t!("目前沒有任何篩選條件"));
                } else if let Some(lib) = &mut self.lib {
                    lib.update_smart_filter(&id, filter);
                    self.toast(t!("已更新智慧資料夾的條件"));
                }
            }
            Action::LinkItems(ids) => {
                let n = ids.len();
                if n < 2 {
                    return;
                }
                self.edit_items(&ids, |it| {
                    for other in &ids {
                        if *other != it.id && !it.links.contains(other) {
                            it.links.push(other.clone());
                        }
                    }
                });
                self.toast(t!("已將 {n} 個素材互相關聯", n));
            }
            Action::UnlinkItems(a, b) => {
                self.edit_items(&[a.clone(), b.clone()], |it| it.links.retain(|l| *l != a && *l != b));
            }
            Action::GoToItem(id) => {
                if self.viewer.is_some() {
                    self.open_viewer_at(id);
                    return;
                }
                self.show_in_grid(&id);
                self.selected = [id.clone()].into_iter().collect();
                self.anchor = Some(id.clone());
                self.scroll_to = Some(id);
            }
            Action::EditSmartFilter(id) => {
                if let Some(s) = self.lib.as_ref().and_then(|l| l.smart_folder(&id)) {
                    self.smart_edit = Some((id, s.filter.clone()));
                }
            }
            Action::RestoreDefaultSmart => {
                if let Some(lib) = &mut self.lib {
                    lib.seed_default_smart_folders();
                    self.toast(t!("已加入預設的智慧資料夾（已經有的群組不會重複加入）"));
                }
            }
            Action::NewTagGroup(tag) => {
                self.dialog = Some(Dialog::TagGroupName { rename: None, name: String::new(), tag });
            }
            Action::RenameTagGroup(id) => {
                let name = self
                    .lib
                    .as_ref()
                    .and_then(|l| l.meta.tag_groups.iter().find(|g| g.id == id))
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::TagGroupName { rename: Some(id), name, tag: None });
            }
            Action::DeleteTagGroup(id) => {
                if let Some(lib) = &mut self.lib {
                    lib.delete_tag_group(&id);
                }
            }
            Action::ToggleTagGroup(id) => {
                if let Some(lib) = &mut self.lib {
                    lib.toggle_tag_group(&id);
                }
            }
            Action::SetTagGroup(tag, group) => {
                if let Some(lib) = &mut self.lib {
                    lib.set_tag_group(&tag, group.as_deref());
                }
            }
            Action::ExportPremiereXml => {
                let ids = if self.selected.is_empty() { self.visible.clone() } else { self.selected_ids() };
                self.export_premiere_xml(ids);
            }
            Action::ExportPremiereXmlFor(ids) => self.export_premiere_xml(ids),
            Action::VolumeChanged => self.apply_volume(),
            Action::Undo => self.restore_items(false),
            Action::Redo => self.restore_items(true),
            Action::OpenYtdlp => self.open_ytdlp(),
            Action::OpenWhisper(ids) => self.open_whisper(ids),
            Action::ClearAiHint => self.ai_hint = 0,
            Action::CancelJob(kind) => self.cancel_job(kind),
            Action::ToggleSection(key) => {
                let list = &mut self.config.collapsed_sections;
                match list.iter().position(|k| *k == key) {
                    Some(i) => {
                        list.remove(i);
                    }
                    None => list.push(key),
                }
                self.config.save();
            }
            Action::FocusTagInput => self.focus_tag = true,
            Action::ShowJob(kind) => self.show_job(kind),
            Action::CaptureFrame(id, pos) => {
                let _ = self.capture_frame(&id, pos);
            }
            Action::FindCandidates(id) => self.find_candidates(&id),
            Action::ProcessStreams(ids) => self.process_streams(ids),
            Action::NavBack => self.go_back(),
            Action::NavForward => self.go_forward(),
            Action::AddClip(id, a, b, name) => {
                let Some(it) = self.lib.as_ref().and_then(|l| l.items.get(&id)) else { return };
                let max = it.duration.unwrap_or(f64::MAX);
                let (start, end) = (a.min(b).clamp(0.0, max), a.max(b).clamp(0.0, max));
                if end - start < 0.05 {
                    self.toast(t!("片段太短：請先設定入點 (I) 和出點 (O)"));
                    return;
                }
                let clip = crate::model::Clip {
                    id: crate::model::new_id(),
                    name: name.unwrap_or_else(|| t!("片段 {n}", n = it.clips.len() + 1)),
                    start,
                    end,
                    crop_x: self.crop_frame,
                    ..Default::default()
                };
                self.edit_items(&[id], |it| {
                    it.clips.push(clip.clone());
                    it.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
                });
                self.mark = (None, None);
                self.viewer_tab = ViewerTab::Clips;
            }
            Action::SetField(ids, name, value) => {
                let value = value.trim().to_string();
                // A new field and its values are one undo step: a single Ctrl+Z
                // takes both back, values first, never the field before its values.
                self.track_structure();
                let Some(lib) = &mut self.lib else { return };
                let meta_before = lib.meta.clone();
                let before: Vec<Item> = ids.iter().filter_map(|id| lib.items.get(id).cloned()).collect();
                lib.add_field(&name);
                for id in &ids {
                    if let Some(it) = lib.items.get_mut(id) {
                        if value.is_empty() {
                            it.fields.remove(&name);
                        } else {
                            it.fields.insert(name.clone(), value.clone());
                        }
                        lib.touch(id);
                    }
                }
                let meta = (!meta_before.same_structure(&lib.meta)).then_some(meta_before);
                self.edits.meta_seen = Some((lib.meta_rev, lib.meta.clone()));
                if !before.is_empty() || meta.is_some() {
                    self.push_undo(super::undo::UndoStep { seq: 0, time: self.now, items: before, meta });
                }
            }
            Action::FieldSmartFolder(name, value) => {
                let Some(lib) = &mut self.lib else { return };
                let key = name.replace(char::is_whitespace, "");
                // Search terms split on spaces, so match the first word of the value.
                let word = value.split_whitespace().next().unwrap_or_default();
                let filter = SavedFilter { search: format!("{key}:{word}"), ..Default::default() };
                let label = if word.is_empty() { t!("{name}：已填寫", name) } else { t!("{name}：{value}", name, value) };
                let id = lib.add_smart_folder(&label, filter, None, false);
                self.view = View::Smart(id);
                self.toast(t!("已建立智慧資料夾「{label}」", label));
            }
            Action::DeleteClips(id, cids) => {
                self.edit_items(std::slice::from_ref(&id), |it| it.clips.retain(|c| !cids.contains(&c.id)));
                self.clip_ui.sel.retain(|c| !cids.contains(c));
                self.toast_undo(t!("已刪除 {n} 個片段", n = cids.len()));
            }
            Action::DeleteCandidates(id) => {
                let n = self.lib.as_ref().and_then(|l| l.items.get(&id)).map_or(0, |it| it.clips.iter().filter(|c| super::highlights::is_candidate(&c.name)).count());
                if n > 0 {
                    self.edit_items(std::slice::from_ref(&id), |it| it.clips.retain(|c| !super::highlights::is_candidate(&c.name)));
                    self.toast_undo(t!("已刪除 {n} 個未保留的候選", n));
                }
            }
            Action::KeepClip(id, cid) => self.edit_items(&[id], |it| {
                if let Some(c) = it.clips.iter_mut().find(|c| c.id == cid) {
                    c.name = super::highlights::kept_name(&c.name);
                }
            }),
            Action::SetClipRange(id, cid, start, end) => {
                // A drag keeps clips 0.1 s long, which near either end of a short
                // clip can reach past the video: kept within it.
                let length = self.lib.as_ref().and_then(|l| l.items.get(&id)).and_then(|it| it.duration).filter(|d| d.is_finite() && *d > 0.0);
                let (start, end) = (start.max(0.0), length.map_or(end, |d| end.min(d)));
                let changed = self.lib.as_ref().and_then(|l| l.items.get(&id)).and_then(|it| it.clips.iter().find(|c| c.id == cid)).is_some_and(|c| (c.start, c.end) != (start, end));
                if changed && end > start {
                    self.edit_items(&[id], |it| {
                        if let Some(c) = it.clips.iter_mut().find(|c| c.id == cid) {
                            (c.start, c.end) = (start, end);
                        }
                        it.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
                    });
                }
            }
            Action::RenameClip(id, cid, name) => {
                let name = name.trim().to_string();
                let changed = self.lib.as_ref().and_then(|l| l.items.get(&id)).and_then(|it| it.clips.iter().find(|c| c.id == cid)).is_some_and(|c| c.name != name);
                if changed && !name.is_empty() {
                    self.edit_items(&[id], |it| {
                        if let Some(c) = it.clips.iter_mut().find(|c| c.id == cid) {
                            c.name = name.clone();
                        }
                    });
                }
            }
            Action::ExportClip(id, cid, preset) => {
                if let Some(preset) = self.export_preset(preset) {
                    self.export_clip(&id, &cid, preset);
                }
            }
            Action::ExportClips(id, cids, preset) => {
                let Some(preset) = self.export_preset(preset) else { return };
                let n = cids.len();
                let mut cids = cids.into_iter();
                // In list order: the first starts now unless an export is running.
                if self.clips.rx.is_none() {
                    let Some(first) = cids.next() else { return };
                    self.clips.queue.extend(cids.map(|c| (id.clone(), c, preset.clone())));
                    self.export_clip(&id, &first, preset);
                } else {
                    self.clips.queue.extend(cids.map(|c| (id.clone(), c, preset.clone())));
                }
                self.toast(t!("{n} 個片段排入輸出，進度在狀態列的背景工作裡", n));
            }
            Action::PickSubtitle(id) => {
                let Some(path) = rfd::FileDialog::new().add_filter(t!("字幕"), &["srt", "vtt"]).pick_file() else { return };
                self.edit_items(std::slice::from_ref(&id), |it| it.subtitle = Some(path.clone()));
                self.load_subtitles(&id);
                self.viewer_tab = ViewerTab::Subtitles;
            }
            Action::NewField => self.dialog = Some(Dialog::FieldName { rename: None, name: String::new() }),
            Action::RenameField(name) => self.dialog = Some(Dialog::FieldName { rename: Some(name.clone()), name }),
            Action::ConfirmDeleteField(name) => {
                let used = self.lib.as_ref().map_or(0, |l| l.items.values().filter(|i| i.fields.contains_key(&name)).count());
                self.dialog = Some(Dialog::Confirm {
                    message: t!("刪除欄位「{name}」？{used} 個項目上的這個欄位內容會一併清除。", name, used),
                    on_yes: Confirmed::DeleteField(name),
                });
            }
            Action::BackupsDialog => {
                let list = self.lib.as_ref().map(|l| l.list_backups()).unwrap_or_default();
                self.dialog = Some(Dialog::Backups(list));
            }
            Action::FindSimilar(id) => self.start_similarity(Some(id)),
            Action::FindDuplicates => self.start_similarity(None),
            Action::FindExactDuplicates => self.find_exact_duplicates(),
            Action::CheckIntegrity => self.start_integrity_check(),
            Action::ResolveDuplicates { merge, only_selected } => self.resolve_duplicates(merge, only_selected),
            Action::RerunResults => self.rerun_results(),
            Action::RegenerateThumbnails(ids) => {
                let Some(lib) = &self.lib else { return };
                // Removing the cached file makes the loader rebuild it from the original.
                let mut texts = Vec::new();
                for id in &ids {
                    let _ = std::fs::remove_file(lib.thumb_path(id));
                    self.thumbs.tex.remove(id);
                    self.thumbs.failed.remove(id);
                    // Re-read text excerpts too (fixes subtitles imported before Big5 support).
                    // Media items pick up the dialogue of their subtitles for search.
                    let text = lib.items.get(id).and_then(|it| match it.kind {
                        MediaKind::Video | MediaKind::Audio => lib.find_subtitle(id).and_then(|p| media::subtitle_search_text(&p)),
                        _ if it.text.is_some() => media::text_excerpt(&lib.file_path(it)),
                        _ => None,
                    });
                    if let Some(text) = text {
                        texts.push((id.clone(), text));
                    }
                }
                if let Some(lib) = &mut self.lib {
                    for (id, text) in texts {
                        if let Some(it) = lib.items.get_mut(&id) {
                            it.text = Some(text);
                        }
                        lib.touch(&id);
                    }
                }
                self.toast(t!("正在重新產生 {n} 個縮圖", n = ids.len()));
            }
            Action::DeleteSmartFolder(id) => {
                if let Some(lib) = &mut self.lib {
                    lib.delete_smart_folder(&id);
                }
                if self.view == View::Smart(id) {
                    self.view = View::All;
                }
            }
            Action::SetFolderColor(id, color) => {
                if let Some(lib) = &mut self.lib {
                    lib.set_folder_color(&id, color);
                }
            }
            Action::ToggleFolder(id) => {
                if let Some(lib) = &mut self.lib {
                    lib.toggle_folder_collapsed(&id);
                }
            }
            Action::MoveFolder(id, parent) => {
                if let Some(lib) = &mut self.lib {
                    if !lib.move_folder(&id, parent) {
                        self.toast(t!("不能把資料夾移到它自己的子資料夾裡"));
                    }
                }
            }
            Action::BatchRename(ids) => {
                if !ids.is_empty() {
                    self.dialog = Some(Dialog::BatchRename { ids, pattern: "{name}_{n}".into(), start: 1 });
                }
            }
            Action::ToggleSlideshow => {
                self.slideshow = if self.slideshow.is_some() { None } else { Some(self.now) };
            }
            Action::SetView(view) => {
                self.view = view;
                self.selected.clear();
                self.anchor = None;
                self.caret = None;
                self.close_viewer(false);
            }
            Action::ToggleTagFilter(tag) => {
                if let Some(i) = self.tag_filter.iter().position(|t| *t == tag) {
                    self.tag_filter.remove(i);
                } else {
                    self.tag_filter.push(tag);
                }
            }
            Action::SetColorFilter(c) => self.color_filter = c,
            Action::NewFolder(parent) => {
                self.dialog = Some(Dialog::FolderName { parent, rename: None, name: String::new() });
            }
            Action::RenameFolder(id) => {
                let name = self.lib.as_ref().and_then(|l| l.folder_name(&id)).unwrap_or_default().to_string();
                self.dialog = Some(Dialog::FolderName { parent: None, rename: Some(id), name });
            }
            Action::ConfirmDeleteFolder(id) => {
                let name = self.lib.as_ref().and_then(|l| l.folder_name(&id)).unwrap_or_default().to_string();
                self.dialog = Some(Dialog::Confirm {
                    message: t!("刪除資料夾「{name}」及其子資料夾？裡面的圖片仍會保留在資源庫中。", name),
                    on_yes: Confirmed::DeleteFolder(id),
                });
            }
            Action::RenameTag(tag) => {
                self.dialog = Some(Dialog::TagRename { name: tag.clone(), old: tag });
            }
            Action::ConfirmDeleteTag(tag) => {
                self.dialog = Some(Dialog::Confirm {
                    message: t!("從所有項目移除標籤「{tag}」？", tag),
                    on_yes: Confirmed::DeleteTag(tag),
                });
            }
            Action::OpenLibraryDialog => {
                if let Some(dir) = rfd::FileDialog::new().set_title(t!("開啟資源庫")).pick_folder() {
                    if Library::is_library(&dir) {
                        self.open_library(dir);
                    } else {
                        self.toast(t!("這個資料夾不是 OrangeLink 資源庫（找不到 library.json）"));
                    }
                }
            }
            Action::CreateLibraryDialog => {
                let Some(dir) = rfd::FileDialog::new().set_title(t!("選擇存放新資源庫的位置")).pick_folder() else {
                    return;
                };
                if Library::is_library(&dir) {
                    self.open_library(dir);
                    return;
                }
                // Use the chosen folder if empty; otherwise create a subfolder in it.
                let empty = std::fs::read_dir(&dir).map(|mut d| d.next().is_none()).unwrap_or(false);
                let root = if empty { dir } else { unique_path(&dir.join(t!("OrangeLink 資源庫"))) };
                match Library::create(&root) {
                    Ok(_) => {
                        self.open_library(root.clone());
                        self.toast(t!("已建立資源庫：{root}", root = root.display()));
                    }
                    Err(e) => self.toast(t!("建立資源庫失敗：{e}", e = format!("{:#}", e))),
                }
            }
            Action::OpenLibrary(path) => {
                if Library::is_library(&path) {
                    self.open_library(path);
                } else {
                    self.toast(t!("找不到資源庫：{n}", n = path.display()));
                    self.config.recent.retain(|p| *p != path);
                }
            }
        }
    }
}
