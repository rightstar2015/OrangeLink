mod cell;
mod grid;
mod hits;
mod inspector;
mod preview;
mod settings;
mod shortcuts;
mod smooth_scroll;
mod theme;
mod sidebar;
mod toolbar;

pub(crate) use theme::*;

pub use preview::{SignalPlotCache, TimeView};
pub use sidebar::SidebarStats;
pub use smooth_scroll::Glide;
pub use toolbar::FormatGroups;

use crate::{
    app::{
        jobs::{BgJob, JobKind},
        is_candidate, Action, App, Dialog, DragFolder, DragIds, DragSmart, InlineEdit, MediaTask, SortKey, View, ViewerTab,
    },
    config::{ClipSort, ImportMode, ThemeSetting, ViewMode},
    library::{Library, Originals},
    media::{fmt_duration, fmt_precise},
    model::MediaKind,
};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontId, Key, Layout, Rect, RichText, ScrollArea, Sense, Stroke,
    TextEdit, Ui,
};

const UV: Rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
const FOLDER_COLORS: [[u8; 3]; 8] = [
    [235, 87, 87],
    [242, 153, 74],
    [242, 201, 76],
    [111, 207, 151],
    [86, 204, 242],
    [47, 128, 237],
    [155, 81, 224],
    [189, 189, 189],
];

/// Tags typed as `a、b, c`.
pub fn split_tags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text.split(['、', ',', '，']).map(str::trim).filter(|t| !t.is_empty()) {
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// A library folder or none, from `(depth, id, name)` in tree order.
pub fn folder_picker(ui: &mut Ui, id: impl std::hash::Hash, tree: &[(usize, String, String)], folder: &mut Option<String>) {
    let shown = folder.as_ref().and_then(|f| tree.iter().find(|t| t.1 == *f)).map_or(t!("（不加入資料夾）").to_string(), |t| t.2.clone());
    egui::ComboBox::from_id_salt(id).selected_text(shown).width(180.0).show_ui(ui, |ui| {
        ui.selectable_value(folder, None, t!("（不加入資料夾）"));
        for (depth, fid, name) in tree {
            ui.selectable_value(folder, Some(fid.clone()), format!("{}{name}", "　".repeat(*depth)));
        }
    });
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b < 1024.0 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", b / 1024.0 / 1024.0 / 1024.0)
    }
}

fn fmt_time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Thin playback bar along the bottom of a thumbnail during hover preview.
fn paint_progress(painter: &egui::Painter, rect: Rect, progress: Option<f32>) {
    let Some(p) = progress else { return };
    let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 4.0), rect.max);
    painter.rect_filled(bar, 0.0, Color32::from_black_alpha(140));
    let done = Rect::from_min_max(bar.min, pos2(bar.min.x + bar.width() * p.clamp(0.0, 1.0), bar.max.y));
    painter.rect_filled(done, 0.0, accent());
}

/// Largest rect with the texture's aspect ratio that fits inside `area`, centered.
fn fit_rect(area: Rect, tex_size: egui::Vec2) -> Rect {
    let scale = (area.width() / tex_size.x).min(area.height() / tex_size.y);
    Rect::from_center_size(area.center(), tex_size * scale)
}

/// A rounded label with a close button. Returns true when the close button is clicked.
fn chip(ui: &mut Ui, text: &str) -> bool {
    chip_parts(ui, text, false).1
}

/// An accented filter chip (soft orange fill + subtle border) with a close button.
fn accent_chip(ui: &mut Ui, text: &str) -> bool {
    let mut removed = false;
    let t = tokens();
    egui::Frame::none()
        .fill(t.accent_soft)
        .stroke(Stroke::new(1.0_f32, accent().gamma_multiply(0.45)))
        .rounding(CHIP_ROUNDING)
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                ui.label(RichText::new(text).small().color(accent()));
                let x = ui.add(egui::Label::new(RichText::new("✖").small().color(accent().gamma_multiply(0.8))).sense(Sense::click()));
                if x.on_hover_text(t!("移除這個條件")).clicked() {
                    removed = true;
                }
            });
        });
    removed
}

/// A chip whose label can be clicked too: (label clicked, ✖ clicked).
fn chip_parts(ui: &mut Ui, text: &str, clickable: bool) -> (bool, bool) {
    let (mut clicked, mut removed) = (false, false);
    egui::Frame::none()
        .fill(ui.visuals().widgets.inactive.bg_fill)
        .rounding(CHIP_ROUNDING)
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if clickable {
                    clicked = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_text(t!("用這個標籤篩選")).clicked();
                } else {
                    ui.label(text);
                }
                let x = ui.add(egui::Label::new(RichText::new("✖").small().weak()).sense(Sense::click()));
                if x.on_hover_text(t!("移除")).clicked() {
                    removed = true;
                }
            });
        });
    (clicked, removed)
}

/// Full-width sidebar row with optional count on the right.
fn nav_row(ui: &mut Ui, selected: bool, indent: f32, label: &str, count: Option<usize>) -> egui::Response {
    nav_row_ex(ui, selected, indent, label, count, None, Sense::click())
}

/// Tree row expander. Drawn as a shape: the bundled fonts lack ▸/▾ glyphs.
#[derive(Clone, Copy, PartialEq)]
enum Arrow {
    /// Leaf in a tree: keeps the label aligned with siblings that have arrows.
    Leaf,
    Open,
    Closed,
}

fn paint_arrow(painter: &egui::Painter, center: egui::Pos2, arrow: Arrow, color: Color32) {
    let s = 4.0;
    let points = match arrow {
        Arrow::Leaf => return,
        Arrow::Open => vec![center + vec2(-s, -s * 0.6), center + vec2(s, -s * 0.6), center + vec2(0.0, s * 0.8)],
        Arrow::Closed => vec![center + vec2(-s * 0.6, -s), center + vec2(-s * 0.6, s), center + vec2(s * 0.8, 0.0)],
    };
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// A tree row: arrow (if any) then a `nav_row_ex` label.
#[allow(clippy::too_many_arguments)]
fn tree_row(
    ui: &mut Ui,
    selected: bool,
    indent: f32,
    arrow: Arrow,
    label: &str,
    count: Option<usize>,
    dot: Option<Color32>,
    sense: Sense,
) -> egui::Response {
    let resp = nav_row_ex(ui, selected, indent + 14.0, label, count, dot, sense);
    let color = if selected { accent() } else { ui.visuals().weak_text_color() };
    paint_arrow(ui.painter(), resp.rect.left_center() + vec2(14.0 + indent, 0.0), arrow, color);
    resp
}

/// `nav_row` with an optional color dot before the label and a custom sense
/// (folders are also drag sources).
fn nav_row_ex(
    ui: &mut Ui,
    selected: bool,
    indent: f32,
    label: &str,
    count: Option<usize>,
    dot: Option<Color32>,
    sense: Sense,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 25.0), sense);
    let v = ui.visuals();
    let t = tokens();
    let painter = ui.painter();
    let drop_hover = resp.dnd_hover_payload::<DragIds>().is_some()
        || resp.dnd_hover_payload::<DragFolder>().is_some()
        || resp.dnd_hover_payload::<DragSmart>().is_some();
    if selected {
        painter.rect_filled(rect, ROUNDING, t.accent_soft);
        let bar = Rect::from_min_max(rect.min + vec2(0.0, 4.0), pos2(rect.min.x + 2.5, rect.max.y - 4.0));
        painter.rect_filled(bar, 1.5, accent());
    } else if resp.hovered() || drop_hover {
        painter.rect_filled(rect, ROUNDING, v.widgets.hovered.weak_bg_fill);
    }
    if drop_hover {
        painter.rect_stroke(rect, ROUNDING, Stroke::new(1.5_f32, accent()));
    }
    let color = if selected { accent() } else { v.text_color() };
    let mut text_x = 10.0 + indent;
    if let Some(c) = dot {
        painter.circle_filled(rect.left_center() + vec2(text_x + 5.0, 0.0), 4.5, c);
        text_x += 16.0;
    }
    let count_str = count.map(|n| n.to_string());
    let right_pad = match &count_str {
        Some(s) => (s.len() as f32 * 7.5 + 16.0).max(32.0),
        None => 8.0,
    };
    let label_rect = Rect::from_min_max(rect.min + vec2(text_x, 0.0), rect.max - vec2(right_pad, 0.0));
    painter.with_clip_rect(label_rect).text(
        label_rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.5),
        color,
    );
    if let Some(s) = count_str {
        painter.text(
            rect.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            s,
            FontId::proportional(11.5),
            if selected { accent() } else { v.weak_text_color() },
        );
    }
    resp
}

/// A section's title: a click on it folds the section. `add` is the tooltip of
/// a ＋ button at the right, when the section has one. Returns (fold, add).
fn section_header(ui: &mut Ui, title: &str, open: bool, add: Option<&str>) -> (bool, bool) {
    let (mut fold, mut added) = (false, false);
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        let arrow = if open { Arrow::Open } else { Arrow::Closed };
        let (arrow_rect, arrow_resp) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::click());
        paint_arrow(ui.painter(), arrow_rect.center(), arrow, ui.visuals().weak_text_color());
        let text = RichText::new(title).small().strong().weak();
        let label_resp = ui.add(egui::Label::new(text).sense(Sense::click()));
        let tip = if open { t!("收合") } else { t!("展開") };
        fold = arrow_resp.on_hover_text(tip).clicked() || label_resp.on_hover_text(tip).clicked();
        if let Some(tip) = add {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                added = ui.small_button(t!("＋")).on_hover_text(tip).clicked();
            });
        }
    });
    (fold, added)
}

impl App {
    pub fn welcome_ui(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.22);
                ui.label(RichText::new("OrangeLink").size(40.0).strong().color(accent()));
                ui.label(RichText::new(t!("素材管理與直播精華剪輯")).size(16.0).weak());
                ui.add_space(28.0);
                if ui.add_sized([240.0, 36.0], egui::Button::new(t!("＋ 新建資源庫…"))).clicked() {
                    self.act(Action::CreateLibraryDialog);
                }
                ui.add_space(6.0);
                if ui.add_sized([240.0, 36.0], egui::Button::new(t!("📂 開啟資源庫…"))).clicked() {
                    self.act(Action::OpenLibraryDialog);
                }
                if !self.config.recent.is_empty() {
                    ui.add_space(24.0);
                    ui.label(RichText::new(t!("最近使用")).weak());
                    for p in &self.config.recent {
                        if ui.link(p.display().to_string()).clicked() {
                            self.act(Action::OpenLibrary(p.clone()));
                        }
                    }
                }
            });
        });
    }

    pub fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            // One row when both groups fit; otherwise the view controls move to a
            // second, right-aligned row so nothing overlaps on narrow windows.
            let (left_w, right_w) = self.toolbar_w;
            let one_row = ui.available_width() >= left_w + right_w + 24.0;
            let row_h = ui.spacing().interact_size.y + 6.0;
            if one_row {
                ui.horizontal(|ui| {
                    let l = ui.horizontal(|ui| self.toolbar_left(ui, ctx)).response.rect.width();
                    let r = ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.toolbar_right(ui));
                    self.toolbar_w = (l, r.response.rect.width());
                });
            } else {
                let l = ui.horizontal_wrapped(|ui| self.toolbar_left(ui, ctx)).response.rect;
                // A wrapped row is as wide as the window; only trust unwrapped widths.
                if l.height() <= row_h {
                    self.toolbar_w.0 = l.width();
                }
                ui.horizontal(|ui| {
                    let r = ui.with_layout(Layout::right_to_left(Align::Center), |ui| self.toolbar_right(ui));
                    self.toolbar_w.1 = r.response.rect.width();
                });
            }
            if one_row != (ui.available_width() >= self.toolbar_w.0 + self.toolbar_w.1 + 24.0) {
                ctx.request_repaint();
            }

            self.condition_row(ui);
            ui.add_space(2.0);
        });
    }

    pub fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(t!("{n} 個項目", n = self.visible.len())).small());
                if !self.selected.is_empty() {
                    ui.label(RichText::new(t!("已選取 {n}", n = self.selected.len())).small().color(accent()));
                }
                let jobs = self.background_jobs();
                self.jobs_button(ui, &jobs);
                if self.save_error.is_some() {
                    self.unsaved_badge(ui);
                } else if let Some(lib) = &self.lib {
                    ui.separator();
                    if lib.has_unsaved() {
                        ui.label(RichText::new(t!("• 儲存中…")).small().weak());
                    } else {
                        ui.label(RichText::new(t!("✔ 已自動存檔")).small().color(good().gamma_multiply(0.85)));
                    }
                }
                // Thumbnails come and go with scrolling: not worth a toast.
                // Nor are jobs over in a moment (a quick scan at start).
                let running = jobs.iter().any(|j| j.kind != JobKind::Thumbnails);
                match (running, self.jobs_since) {
                    (true, None) => self.jobs_since = Some(self.now),
                    (false, Some(since)) => {
                        self.jobs_since = None;
                        if self.now - since >= 3.0 {
                            self.toast(t!("背景工作都結束了"));
                        }
                    }
                    _ => {}
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    self.status_badges(ui);
                    if self.mcp.is_running() {
                        let r = ui
                            .add(egui::Button::new(RichText::new("● MCP").small().color(good())).frame(false))
                            .on_hover_text(t!("AI 助理可以透過 MCP 連線（設定 → 整合）。按一下查看 AI 操作紀錄"));
                        if r.clicked() {
                            self.show_ai_log = true;
                        }
                    } else if ui
                        .add(egui::Button::new(RichText::new("○ MCP").small().weak()).frame(false))
                        .on_hover_text(t!("AI 助理整合（MCP）尚未啟用；按一下開啟整合設定"))
                        .clicked()
                    {
                        self.act(Action::OpenSettingsAt(crate::config::SettingsPage::Integration));
                    }
                    if let Some(lib) = &self.lib {
                        ui.add(egui::Label::new(RichText::new(lib.name()).small().weak()).truncate())
                            .on_hover_text(lib.root.display().to_string());
                    }
                });
            });
        });
    }

    /// While saving keeps failing: how much is waiting, the reason on hover, and a
    /// click retries now.
    fn unsaved_badge(&mut self, ui: &mut Ui) {
        let (Some(err), Some(lib)) = (&self.save_error, &self.lib) else { return };
        let (items, meta) = lib.unsaved();
        let what = match (items, meta) {
            (0, _) => t!("資源庫設定").to_string(),
            (n, false) => t!("{n} 個素材", n),
            (n, true) => t!("{n} 個素材和資源庫設定", n),
        };
        ui.separator();
        let r = ui
            .add(egui::Button::new(RichText::new(t!("⚠ {what}尚未存檔", what)).small().color(danger())).frame(false))
            .on_hover_text(t!("{err}\n\n每 2 秒自動重試；處理好問題（關掉鎖住檔案的程式、清出磁碟空間）後會自動存好。按一下立刻重試。", err));
        if r.clicked() {
            self.last_flush = f64::NEG_INFINITY;
        }
    }

    /// "⟳ 3 個工作進行中" with the overall progress; a click lists each job with
    /// its progress, time left and a cancel button.
    fn jobs_button(&mut self, ui: &mut Ui, jobs: &[BgJob]) {
        if jobs.is_empty() {
            return;
        }
        ui.separator();
        let known: Vec<f32> = jobs.iter().filter_map(|j| j.progress).collect();
        let r = ui.add(egui::Button::new(RichText::new(t!("⟳ {n} 個工作進行中", n = jobs.len())).small()).frame(false));
        if !known.is_empty() {
            let overall = known.iter().sum::<f32>() / known.len() as f32;
            ui.add(egui::ProgressBar::new(overall).desired_width(80.0).desired_height(4.0));
        } else {
            ui.spinner();
        }
        let r = r.on_hover_text(t!("查看背景工作"));
        let id = ui.id().with("jobs-popup");
        if r.clicked() {
            ui.memory_mut(|m| m.toggle_popup(id));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        egui::popup::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(340.0);
            for (n, job) in jobs.iter().enumerate() {
                if n > 0 {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&job.name).strong()).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if job.can_cancel && ui.small_button(t!("取消")).clicked() {
                            self.act(Action::CancelJob(job.kind));
                        }
                        if job.has_window && ui.small_button(t!("開啟")).on_hover_text(t!("開啟這個工作的視窗")).clicked() {
                            self.act(Action::ShowJob(job.kind));
                            ui.memory_mut(|m| m.close_popup());
                        }
                    });
                });
                match job.progress {
                    Some(p) => {
                        ui.add(egui::ProgressBar::new(p).desired_width(320.0).desired_height(6.0));
                    }
                    None => {
                        ui.spinner();
                    }
                }
                let mut detail = job.detail.clone();
                if let Some(eta) = job.eta {
                    detail += &t!("  ·  剩 {eta}", eta = fmt_duration(eta));
                }
                ui.label(RichText::new(detail).small().weak());
            }
        });
    }

    /// Small icons on the right: unreadable items, a new version, the AI hint.
    /// Each opens its details.
    fn status_badges(&mut self, ui: &mut Ui) {
        let warn = warning();
        if let Some(tag) = self.update.new_version().map(str::to_string) {
            let r = ui.add(egui::Button::new(RichText::new("⬆").color(warn)).frame(false)).on_hover_text(t!("GitHub 有新版 {tag}", tag));
            let id = ui.id().with("badge-update");
            if r.clicked() {
                ui.memory_mut(|m| m.toggle_popup(id));
            }
            let url = crate::update::release_tag_url(&tag);
            egui::popup::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(250.0);
                ui.label(RichText::new(t!("GitHub 上有新版 {tag}", tag)).strong());
                ui.label(RichText::new(t!("目前版本：v{version}", version = env!("CARGO_PKG_VERSION"))).small().weak());
                ui.label(RichText::new(t!("請前往 GitHub 手動下載新版本並覆蓋安裝。")).small().weak());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let btn = egui::Button::new(RichText::new(t!("前往 GitHub 下載")).color(Color32::WHITE)).fill(tokens().selection_bg);
                    if ui.add(btn).clicked() {
                        let _ = open::that_detached(&url);
                        ui.memory_mut(|m| m.close_popup());
                    }
                    if ui.button(t!("複製連結")).clicked() {
                        ui.ctx().copy_text(url.clone());
                        self.toast(t!("已複製 GitHub 下載連結"));
                        ui.memory_mut(|m| m.close_popup());
                    }
                });
            });
        }
        let unreadable = self.lib.as_ref().map_or(0, |l| l.unreadable.len());
        if unreadable > 0 {
            let r = ui.add(egui::Button::new(RichText::new(format!("⚠ {unreadable}")).small().color(warn)).frame(false));
            if r.on_hover_text(t!("{unreadable} 個素材讀取失敗，點一下查看清單或從備份還原", unreadable)).clicked() {
                self.dialog = Some(Dialog::Unreadable);
            }
        }
        if self.ai_hint > 0 {
            let n = self.ai_hint;
            let r = ui.add(egui::Button::new(RichText::new(format!("✨ {n}")).small()).frame(false)).on_hover_text(t!("{n} 個新素材可請 AI 整理", n));
            let id = ui.id().with("badge-ai");
            if r.clicked() {
                ui.memory_mut(|m| m.toggle_popup(id));
            }
            egui::popup::popup_above_or_below_widget(ui, id, &r, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                ui.set_min_width(260.0);
                ui.label(t!("{n} 個新素材可請 AI 整理", n));
                ui.label(RichText::new(t!("複製指令後，貼給已連接 OrangeLink MCP 的 AI 助理")).small().weak());
                ui.horizontal(|ui| {
                    if ui.button(t!("複製指令")).clicked() {
                        ui.ctx().copy_text(t!("請用 OrangeLink MCP 整理我最近匯入的 {n} 個素材：用 search_items 依加入時間找出最新的 {n} 個項目，用 get_thumbnail 看內容，用 add_tags 加上描述內容的標籤；需要時建議適合的資料夾，先告訴我再搬移。", n));
                        self.act(Action::ClearAiHint);
                        ui.memory_mut(|m| m.close_popup());
                    }
                    if ui.button(t!("略過")).clicked() {
                        self.act(Action::ClearAiHint);
                        ui.memory_mut(|m| m.close_popup());
                    }
                });
            });
        }
    }

    pub fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            self.dialog_focused = false;
            return;
        };
        let mut close = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut commit = false;
        let mut small_import = None;
        let focus_once = !std::mem::replace(&mut self.dialog_focused, true);
        let window = |title: &str| {
            egui::Window::new(title.to_string())
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        };
        match &mut dialog {
            Dialog::FolderName { rename, name, .. } => {
                let title = if rename.is_some() { t!("重新命名資料夾") } else { t!("新增資料夾") };
                window(title).show(ctx, |ui| {
                    let r = ui.add(TextEdit::singleline(name).hint_text(t!("資料夾名稱")).desired_width(260.0));
                    if focus_once {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        commit = true;
                    }
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("確定")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::TagRename { name, .. } => {
                window(t!("重新命名標籤")).show(ctx, |ui| {
                    let r = ui.add(TextEdit::singleline(name).desired_width(260.0));
                    if focus_once {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        commit = true;
                    }
                    ui.label(RichText::new(t!("若新名稱已存在，兩個標籤會合併")).small().weak());
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("確定")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::Confirm { message, .. } => {
                window(t!("確認")).show(ctx, |ui| {
                    ui.set_max_width(360.0);
                    ui.label(message.as_str());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        commit |= ui.button(RichText::new(t!("確定")).color(danger())).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::FieldName { rename, name } => {
                let title = if rename.is_some() { t!("重新命名欄位") } else { t!("新增自訂欄位") };
                window(title).show(ctx, |ui| {
                    let r = ui.add(TextEdit::singleline(name).hint_text(t!("例如：授權、客戶、拍攝地點")).desired_width(260.0));
                    if focus_once {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        commit = true;
                    }
                    ui.label(RichText::new(t!("欄位會出現在每個素材的右側面板，內容也能被搜尋")).small().weak());
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("確定")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::TagGroupName { rename, name, .. } => {
                let title = if rename.is_some() { t!("重新命名標籤群組") } else { t!("新增標籤群組") };
                window(title).show(ctx, |ui| {
                    let r = ui.add(TextEdit::singleline(name).hint_text(t!("例如：角色、場景、情緒")).desired_width(260.0));
                    if focus_once {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        commit = true;
                    }
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("確定")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::SmartFolderName { rename, name, group, .. } => {
                let title = match (rename.is_some(), *group) {
                    (true, _) => t!("重新命名"),
                    (false, true) => t!("新增智慧資料夾群組"),
                    (false, false) => t!("儲存為智慧資料夾"),
                };
                window(title).show(ctx, |ui| {
                    let r = ui.add(TextEdit::singleline(name).hint_text(t!("名稱")).desired_width(260.0));
                    if focus_once {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        commit = true;
                    }
                    if rename.is_none() && !*group {
                        ui.label(RichText::new(t!("會記住目前的搜尋文字、標籤、格式、評分、顏色與類型條件")).small().weak());
                    }
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("確定")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::BatchRename { ids, pattern, start } => {
                let preview = self.batch_rename_preview(ids, pattern, *start);
                window(t!("批次重新命名")).show(ctx, |ui| {
                    ui.set_max_width(400.0);
                    ui.label(t!("{n} 個項目", n = ids.len()));
                    ui.horizontal(|ui| {
                        ui.label(t!("格式"));
                        let r = ui.add(TextEdit::singleline(pattern).desired_width(240.0));
                        if focus_once {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            commit = true;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t!("起始編號"));
                        ui.add(egui::DragValue::new(start).range(0..=99999));
                    });
                    ui.label(RichText::new(t!("{name} = 原名稱，{n} = 流水號。只改顯示名稱，不改硬碟上的檔名。")).small().weak());
                    ui.add_space(4.0);
                    ScrollArea::vertical().id_salt("batch-rename-preview").max_height(140.0).show(ui, |ui| {
                        for (_, old, new) in &preview {
                            ui.label(RichText::new(format!("{old}  →  {new}")).small());
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        commit |= ui.button(t!("套用")).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::WatchFolders { link, tags, missing } => {
                let mut remove = None;
                let mut add = None;
                let mut scan = false;
                let mut rules: Vec<(std::path::PathBuf, Option<String>, Vec<String>)> = Vec::new();
                let folders = self.lib.as_ref().map(|l| l.meta.watch_folders.clone()).unwrap_or_default();
                let tree: Vec<(usize, String, String)> = self.lib.as_ref().map(|l| l.folder_tree().into_iter().map(|(d, f)| (d, f.id.clone(), f.name.clone())).collect()).unwrap_or_default();
                let interval = self.config.watch_interval_min;
                window(t!("監看資料夾")).show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(RichText::new(t!("每 {interval} 分鐘檢查一次，自動匯入新加入的檔案（例如錄影或輸出資料夾）。", interval = format!("{:.0}", interval))).small().weak());
                    ui.add_space(4.0);
                    if folders.is_empty() {
                        ui.label(RichText::new(t!("尚未設定")).weak());
                    }
                    for w in &folders {
                        ui.horizontal(|ui| {
                            if ui.small_button("✖").on_hover_text(t!("停止監看")).clicked() {
                                remove = Some(w.path.clone());
                            }
                            let mode = if w.link { t!("🔗 保留原位置") } else { t!("複製") };
                            ui.label(RichText::new(mode).small().weak());
                            let exists = !missing.contains(&w.path);
                            let mut text = RichText::new(w.path.display().to_string()).small();
                            if !exists {
                                text = text.color(danger());
                            }
                            let lbl = ui.label(text);
                            if !exists {
                                lbl.on_hover_text(t!("找不到這個資料夾（可能已被移除、改名，或外接硬碟／網路磁碟未連線）"));
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            let mut folder = w.folder.clone();
                            ui.label(RichText::new(t!("加入")).small());
                            folder_picker(ui, ("watch-folder", &w.path), &tree, &mut folder);
                            ui.label(RichText::new(t!("標籤")).small());
                            let draft = tags.entry(w.path.clone()).or_insert_with(|| w.tags.join(t!("、")));
                            ui.add(TextEdit::singleline(draft).desired_width(150.0).hint_text(t!("例如 直播、待處理")))
                                .on_hover_text(t!("這個資料夾的新檔案自動加上這些標籤，用頓號或逗號分隔"));
                            let parsed = split_tags(draft);
                            if folder != w.folder || parsed != w.tags {
                                rules.push((w.path.clone(), folder, parsed));
                            }
                        });
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.radio_value(link, true, t!("保留在原位置"));
                        ui.radio_value(link, false, t!("複製到資源庫"));
                        if ui.button(t!("＋ 新增資料夾…")).clicked() {
                            add = rfd::FileDialog::new().set_title(t!("選擇要監看的資料夾")).pick_folder();
                        }
                    });
                    ui.horizontal(|ui| {
                        scan = ui.button(t!("立即掃描")).clicked();
                        close |= ui.button(t!("關閉")).clicked();
                    });
                });
                if let Some(lib) = &mut self.lib {
                    for (path, folder, t) in rules {
                        lib.set_watch_rule(&path, folder, t);
                    }
                    if let Some(p) = remove {
                        // Added back later, the folder starts from its own (empty) tags.
                        tags.remove(&p);
                        lib.remove_watch_folder(&p);
                    }
                    if let Some(p) = add {
                        lib.add_watch_folder(p, *link);
                        scan = true;
                    }
                }
                if scan {
                    self.act(Action::ScanWatchFolders);
                }
            }
            Dialog::ImportPreview { paths, link, folder, tags, scan } => {
                let s = crate::lock(scan).clone();
                let min = self.config.import_preview_min;
                if s.counted && s.files <= min {
                    // Small after all: the usual way.
                    small_import = Some(std::mem::take(paths));
                    close = true;
                } else {
                    let tree: Vec<(usize, String, String)> = self.lib.as_ref().map(|l| l.folder_tree().into_iter().map(|(d, f)| (d, f.id.clone(), f.name.clone())).collect()).unwrap_or_default();
                    let skip = self.config.skip_duplicates;
                    let policy = self.originals_policy();
                    window(t!("匯入前確認")).show(ctx, |ui| {
                        ui.set_max_width(460.0);
                        if !s.counted {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(t!("計算檔案數量…"));
                            });
                            close |= ui.button(t!("取消")).clicked();
                            return;
                        }
                        ui.label(RichText::new(t!("{files} 個檔案，共 {bytes}", files = s.files, bytes = human_size(s.bytes))).strong());
                        let dup = if s.checked < s.files {
                            t!("檢查重複中… {checked} / {files}（已找到 {dupes} 個）", checked = s.checked, files = s.files, dupes = s.dupes)
                        } else if s.dupes == 0 {
                            t!("沒有重複的檔案").to_string()
                        } else if skip {
                            t!("{dupes} 個已在資源庫裡或彼此重複，會略過", dupes = s.dupes)
                        } else {
                            t!("{dupes} 個已在資源庫裡或彼此重複（設定為不略過，會再匯入一次）", dupes = s.dupes)
                        };
                        ui.label(RichText::new(dup).small().weak());
                        ui.add_space(6.0);
                        egui::Grid::new("import-preview").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                            ui.label(t!("存放"));
                            ui.horizontal(|ui| {
                                ui.radio_value(link, false, t!("複製到資源庫"));
                                ui.radio_value(link, true, t!("保留在原位置"));
                            });
                            ui.end_row();
                            ui.label(t!("加入資料夾"));
                            folder_picker(ui, "import-folder", &tree, folder);
                            ui.end_row();
                            ui.label(t!("加標籤"));
                            ui.add(TextEdit::singleline(tags).desired_width(240.0).hint_text(t!("用頓號或逗號分隔，例如 直播、待處理")));
                            ui.end_row();
                        });
                        if *link && policy != Originals::Keep {
                            ui.label(RichText::new(t!("⚠ 目前設定：在 OrangeLink 永久刪除時也會處理硬碟上的原始檔案（可在設定改為保留）")).small().color(danger()));
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            commit |= ui.button(RichText::new(t!("匯入")).strong()).clicked();
                            close |= ui.button(t!("取消")).clicked();
                        });
                    });
                    if !s.counted || s.checked < s.files {
                        ctx.request_repaint_after(std::time::Duration::from_millis(150));
                    }
                }
            }
            Dialog::Backups(backups) => {
                let mut restore = None;
                window(t!("從備份還原")).show(ctx, |ui| {
                    ui.set_max_width(460.0);
                    ui.label(RichText::new(t!("會還原資料夾、智慧資料夾、標籤群組，以及仍存在之項目的標籤、評分、註解等資料。備份之後才加入的素材會保留。還原前會先自動備份目前狀態。")).small().weak());
                    ui.add_space(6.0);
                    if backups.is_empty() {
                        ui.label(RichText::new(t!("還沒有備份（每天第一次開啟資源庫時會自動建立）")).weak());
                    }
                    ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for (path, n) in backups.iter() {
                            ui.horizontal(|ui| {
                                let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                                ui.label(t!("{name}（{n} 個項目）", name, n));
                                if ui.small_button(t!("還原")).clicked() {
                                    restore = Some(path.clone());
                                }
                            });
                        }
                    });
                    close |= ui.button(t!("關閉")).clicked();
                });
                if let Some(path) = restore {
                    close = true;
                    if let Some(lib) = &mut self.lib {
                        match lib.restore_backup(&path) {
                            Ok(n) => self.toast(t!("已從備份還原 {n} 個項目的資料", n)),
                            Err(e) => self.toast(t!("還原失敗：{e}", e = format!("{:#}", e))),
                        }
                    }
                }
            }
            Dialog::Integrity(found) => {
                use crate::app::IntegrityFix as Fix;
                // Every frame while the dialog is open: read the cached count.
                let refs = self.with_sidebar_stats(|s| s.missing_folder_refs);
                let mut fix = None;
                window(t!("檢查資源庫")).show(ctx, |ui| {
                    ui.set_max_width(480.0);
                    let mut any = false;
                    if !found.leftovers.is_empty() {
                        any = true;
                        ui.label(RichText::new(t!("{n} 個殘留的資料夾", n = found.leftovers.len())).strong());
                        ui.label(RichText::new(t!("中斷的匯入（例如當機或斷電）留下的素材資料夾，裡面沒有素材資料，不會出現在資源庫中。原始檔案不受影響。")).small().weak());
                        if ui.button(t!("移到資源回收筒")).clicked() {
                            fix = Some(Fix::RemoveLeftovers);
                        }
                        ui.add_space(8.0);
                    }
                    if refs > 0 {
                        any = true;
                        ui.label(RichText::new(t!("{refs} 個素材指向已不存在的資料夾", refs)).strong());
                        ui.label(RichText::new(t!("通常是還原舊備份或手動編輯造成的。移除後，不在任何資料夾的素材會回到「未分類」。")).small().weak());
                        if ui.button(t!("移除失效的資料夾")).clicked() {
                            fix = Some(Fix::DropFolderRefs);
                        }
                        ui.add_space(8.0);
                    }
                    if !found.changed.is_empty() {
                        any = true;
                        ui.label(RichText::new(t!("{n} 個素材的檔案和匯入時不同", n = found.changed.len())).strong());
                        ui.label(
                            RichText::new(t!("連結的原始檔被編輯過，或資源庫裡的檔案損壞。找相同檔案時會用匯入時的資料，可能找不到或認錯；確認內容沒問題後，可以改用目前的內容。"))
                                .small()
                                .weak(),
                        );
                        ui.horizontal(|ui| {
                            if ui.button(t!("顯示這些素材")).clicked() {
                                fix = Some(Fix::ShowChanged);
                            }
                            if ui.button(t!("以目前內容為準")).clicked() {
                                fix = Some(Fix::AcceptChanged);
                            }
                        });
                        ui.add_space(8.0);
                    }
                    if found.unreadable_files > 0 {
                        ui.label(RichText::new(t!("另有 {unreadable_files} 個檔案無法讀取，沒有比對。", unreadable_files = found.unreadable_files)).small().weak());
                    }
                    if !any {
                        ui.label(t!("都處理好了。"));
                    }
                    ui.add_space(4.0);
                    close |= ui.button(t!("關閉")).clicked();
                });
                if let Some(fix) = fix {
                    let show = matches!(fix, Fix::ShowChanged);
                    self.apply_integrity_fix(fix, found);
                    close |= show;
                }
            }
            Dialog::Unreadable => {
                let mut restore = false;
                let mut open_dir = None;
                window(t!("素材讀取失敗")).show(ctx, |ui| {
                    ui.set_max_width(520.0);
                    ui.label(
                        RichText::new(
                            t!("這些素材的 metadata.json 損壞或無法讀取，所以沒有顯示在資源庫中，原始檔案沒有被刪除。\
                             在處理之前，每日自動備份會暫停，避免舊備份被輪替掉。"),
                        )
                        .small()
                        .weak(),
                    );
                    ui.add_space(6.0);
                    if let Some(lib) = &self.lib {
                        ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                            for u in &lib.unreadable {
                                ui.horizontal(|ui| {
                                    if ui.small_button("📁").on_hover_text(t!("開啟這個素材的資料夾")).clicked() {
                                        open_dir = Some(crate::library::item_dir(&lib.root, &u.id));
                                    }
                                    ui.label(RichText::new(&u.id).small().monospace());
                                    ui.label(RichText::new(&u.error).small().weak());
                                });
                            }
                        });
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        restore = ui.button(t!("從備份還原這些素材")).on_hover_text(t!("使用含有該素材的最新備份；損壞的檔案會保留為 metadata.json.broken")).clicked();
                        close |= ui.button(t!("關閉")).clicked();
                    });
                });
                if let Some(dir) = open_dir {
                    let _ = open::that_detached(dir);
                }
                if restore {
                    if let Some(lib) = &mut self.lib {
                        match lib.restore_unreadable() {
                            Ok((n, 0)) => self.toast(t!("已從備份還原 {n} 個素材", n)),
                            Ok((n, left)) => self.toast(t!("已還原 {n} 個素材；{left} 個在備份中找不到，請手動檢查", n, left)),
                            Err(e) => self.toast(t!("還原失敗：{e}", e = format!("{:#}", e))),
                        }
                    }
                    close |= self.lib.as_ref().is_some_and(|l| l.unreadable.is_empty());
                }
            }
            Dialog::ImportMode { paths, dirs, link, remember } => {
                let policy = self.originals_policy();
                let deletes = policy != Originals::Keep;
                window(t!("匯入方式")).show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    let n_dirs = *dirs;
                    let n_files = paths.len() - n_dirs;
                    let mut what = Vec::new();
                    if n_dirs > 0 {
                        what.push(t!("{n_dirs} 個資料夾", n_dirs));
                    }
                    if n_files > 0 {
                        what.push(t!("{n_files} 個檔案", n_files));
                    }
                    ui.label(t!("即將匯入 {what}。檔案要怎麼存放？", what = what.join(t!("、"))));
                    ui.add_space(6.0);
                    ui.radio_value(link, false, RichText::new(t!("複製到資源庫")).strong());
                    ui.label(RichText::new(t!("    複製一份到資源庫，原始檔案不受影響。佔用額外空間。")).small().weak());
                    ui.add_space(4.0);
                    ui.radio_value(link, true, RichText::new(t!("保留在原位置（不複製）")).strong());
                    ui.label(
                        RichText::new(t!("    只建立索引與縮圖，檔案留在原本的資料夾。適合大型素材庫；拖到 Premiere 時會直接引用原始路徑。"))
                            .small()
                            .weak(),
                    );
                    if *link {
                        ui.add_space(6.0);
                        let warning = match policy {
                            Originals::Keep => t!("注意：在 OrangeLink 中永久刪除時，只會移除資源庫裡的資料，硬碟上的原始檔案會保留。搬移或改名原始檔案後，可用「重新連結」找回。"),
                            Originals::RecycleBin => t!("⚠ 目前設定：永久刪除時，原始檔案會被移到 Windows 資源回收筒。（可在設定中改為保留原始檔案）"),
                            Originals::Erase => t!("⚠ 警告：目前設定為永久刪除時直接刪除硬碟上的原始檔案，無法還原！（可在設定中改為保留原始檔案）"),
                        };
                        egui::Frame::none()
                            .fill(danger().linear_multiply(0.16))
                            .rounding(6.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new(warning).color(if deletes { danger() } else { ui.visuals().text_color() }));
                            });
                    }
                    ui.add_space(6.0);
                    ui.checkbox(remember, t!("記住我的選擇（之後可在設定中變更）"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        commit |= ui.button(RichText::new(t!("匯入")).strong()).clicked();
                        close |= ui.button(t!("取消")).clicked();
                    });
                });
            }
            Dialog::UpdatePrompt { tag } => {
                let url = crate::update::release_tag_url(tag);
                let mut copied = false;
                let mut skip = false;
                window(t!("發現新版本")).show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.label(RichText::new(t!("偵測到 GitHub 上有新版本 {tag}", tag)).strong());
                    ui.label(RichText::new(t!("目前版本：v{version}", version = env!("CARGO_PKG_VERSION"))).small().weak());
                    ui.add_space(6.0);
                    ui.label(t!("是否要前往 GitHub 下載頁面手動下載新版本？"));
                    ui.add_space(6.0);
                    egui::Frame::none()
                        .fill(tokens().bg_elevated)
                        .stroke(Stroke::new(1.0_f32, tokens().separator))
                        .rounding(ROUNDING)
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.add(egui::Label::new(RichText::new(&url).small().monospace().color(tokens().link)).truncate());
                                if ui.small_button(t!("複製")).on_hover_text(t!("複製 GitHub 下載連結")).clicked() {
                                    ui.ctx().copy_text(url.clone());
                                    copied = true;
                                }
                            });
                        });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let go = egui::Button::new(RichText::new(t!("是，前往 GitHub 下載")).strong().color(Color32::WHITE))
                            .fill(tokens().selection_bg);
                        commit |= ui.add(go).clicked();
                        close |= ui.button(t!("否，稍後再說")).clicked();
                        skip = ui.button(t!("略過這個版本")).on_hover_text(t!("不再為這個版本跳出視窗，狀態列仍會顯示；有更新的版本時再提示")).clicked();
                    });
                });
                if copied {
                    self.toast(t!("已複製 GitHub 下載連結"));
                }
                if skip {
                    self.config.skipped_update = Some(tag.clone());
                    self.config.save();
                    close = true;
                }
            }
        }

        if commit {
            match dialog {
                Dialog::FolderName { parent, rename, name } => self.apply_folder_dialog(parent, rename, name),
                Dialog::TagRename { old, name } => self.apply_tag_rename(old, name),
                Dialog::Confirm { on_yes, .. } => self.apply_confirmed(on_yes),
                Dialog::SmartFolderName { rename, name, parent, group } => self.apply_smart_folder_dialog(rename, name, parent, group),
                Dialog::TagGroupName { rename, name, tag } => self.apply_tag_group_dialog(rename, name, tag),
                Dialog::BatchRename { ids, pattern, start } => self.apply_batch_rename(ids, pattern, start),
                Dialog::FieldName { rename, name } => self.apply_field_dialog(rename, name),
                Dialog::WatchFolders { .. } | Dialog::Backups(_) | Dialog::Unreadable | Dialog::Integrity(_) => {}
                Dialog::UpdatePrompt { tag } => {
                    let _ = open::that_detached(crate::update::release_tag_url(&tag));
                }
                Dialog::ImportPreview { paths, link, folder, tags, .. } => {
                    self.act(Action::StartImportWith { paths, link, folder, tags: split_tags(&tags) });
                }
                Dialog::ImportMode { paths, link, remember, .. } => {
                    if remember {
                        self.config.import_mode = if link { ImportMode::Link } else { ImportMode::Copy };
                        self.config.save();
                    }
                    self.act(Action::StartImport { paths, link });
                }
            }
            self.dialog_focused = false;
        } else if !close {
            self.dialog = Some(dialog);
        } else {
            self.dialog_focused = false;
        }
        if let Some(paths) = small_import {
            self.act(Action::ImportByMode(paths));
        }
    }

    pub fn toasts_ui(&self, ctx: &egui::Context) {
        if self.toasts().is_empty() {
            return;
        }
        // Newest at the bottom, nearest the corner; older ones are pushed up. The area
        // is only as wide as the widest toast, so it blocks no more than it covers.
        let area = egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, [-16.0, -36.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for t in self.toasts() {
                    let mut frame = egui::Frame::popup(ui.style());
                    if t.error {
                        frame = frame.stroke(Stroke::new(1.5_f32, danger()));
                    }
                    frame.show(ui, |ui| {
                        ui.set_max_width(380.0);
                        match &t.action {
                            None => {
                                ui.label(t.msg.as_str());
                            }
                            Some((label, _)) => {
                                ui.horizontal(|ui| {
                                    // The message wraps in what the button leaves.
                                    let button = egui::Button::new(RichText::new(label).color(accent()));
                                    let text = egui::WidgetText::from(t.msg.as_str()).into_galley(ui, Some(egui::TextWrapMode::Wrap), 300.0, egui::TextStyle::Body);
                                    ui.label(text);
                                    if ui.add(button).clicked() {
                                        self.act(Action::ToastButton(t.id));
                                    }
                                });
                            }
                        }
                    });
                }
            });
        let pointer = ctx.input(|i| i.pointer.latest_pos());
        if pointer.is_some_and(|p| area.response.rect.expand(4.0).contains(p)) {
            self.toast.hovered.set(true);
        }
    }

    pub fn drag_overlay(&self, ctx: &egui::Context) {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("dnd-overlay")));
        let drag_tip = |text: String| {
            if let Some(pos) = ctx.pointer_latest_pos() {
                let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
                let rect = Rect::from_min_size(pos + vec2(14.0, 14.0), galley.size() + vec2(16.0, 8.0));
                painter.rect_filled(rect, ROUNDING, accent());
                painter.galley(rect.min + vec2(8.0, 4.0), galley, Color32::WHITE);
            }
        };
        if let Some(payload) = egui::DragAndDrop::payload::<DragIds>(ctx) {
            drag_tip(t!("{n} 個項目 → 放到資料夾或標籤", n = payload.0.len()));
        } else if let Some(payload) = egui::DragAndDrop::payload::<DragFolder>(ctx) {
            let name = self.lib.as_ref().and_then(|l| l.folder_name(&payload.0)).unwrap_or(t!("資料夾"));
            drag_tip(t!("📁 移動「{name}」", name));
        } else if let Some(payload) = egui::DragAndDrop::payload::<DragSmart>(ctx) {
            let name = self.lib.as_ref().and_then(|l| l.smart_folder(&payload.0)).map_or(t!("智慧資料夾"), |s| s.name.as_str());
            drag_tip(t!("✨ 移動「{name}」", name));
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.screen_rect();
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(175));
            painter.rect_stroke(screen.shrink(12.0), 12.0, Stroke::new(3.0_f32, accent()));
            let folder_note = match (&self.view, self.config.import_into_current_folder, &self.lib) {
                (View::Folder(fid), true, Some(lib)) => lib.folder_name(fid).map(|n| t!("加入資料夾「{n}」", n)),
                _ => None,
            };
            let mode_note = match self.config.import_mode {
                ImportMode::Copy => t!("複製到資源庫"),
                ImportMode::Link => t!("保留在原位置"),
                ImportMode::Ask => t!("放開後選擇存放方式"),
            };
            painter.text(screen.center() - vec2(0.0, 10.0), Align2::CENTER_CENTER, t!("放開以匯入"), FontId::proportional(28.0), Color32::WHITE);
            let sub = match folder_note {
                Some(f) => format!("{mode_note}  ·  {f}"),
                None => mode_note.to_string(),
            };
            painter.text(screen.center() + vec2(0.0, 24.0), Align2::CENTER_CENTER, sub, FontId::proportional(14.0), tokens().text_muted);
        }
    }
}
