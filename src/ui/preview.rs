//! The full preview: images, the video and audio player, clips and subtitles.

use super::*;

/// How long "+5s" stays over the video after a key jump.
const FLASH_SECS: f32 = 0.6;

/// Cached downsampled chat & loudness columns for the timeline activity strip,
/// so video playback at 60fps doesn't clone or rescan thousands of samples per frame.
#[derive(Default)]
pub struct SignalPlotCache {
    key: (String, usize, usize, usize),
    chat: Vec<f32>,
    loud: Vec<f32>,
}

impl SignalPlotCache {
    pub fn clear(&mut self) {
        self.key = (String::new(), 0, 0, 0);
        self.chat.clear();
        self.loud.clear();
    }

    fn update(&mut self, id: &str, cols: usize, chat: Option<&[f32]>, loud: Option<&[f32]>) {
        let key = (id.to_string(), cols, chat.map_or(0, <[f32]>::len), loud.map_or(0, <[f32]>::len));
        if self.key == key {
            return;
        }
        self.key = key;
        self.chat.clear();
        self.loud.clear();
        let column = |values: &[f32], c: usize, empty: f32| {
            let per = values.len() as f64 / cols as f64;
            let a = (c as f64 * per) as usize;
            let b = (((c + 1) as f64 * per).ceil() as usize).min(values.len());
            values.get(a..b.max(a + 1).min(values.len())).map_or(empty, |s| s.iter().copied().fold(empty, f32::max))
        };
        if let Some(chat) = chat.filter(|c| !c.is_empty()) {
            let raw: Vec<f32> = (0..cols).map(|c| column(chat, c, 0.0)).collect();
            let top = raw.iter().copied().fold(0.0f32, f32::max).max(1.0);
            self.chat = raw.into_iter().map(|v| (v / top).clamp(0.0, 1.0)).collect();
        }
        if let Some(loud) = loud.filter(|l| !l.is_empty()) {
            let floor = -60.0f32;
            let top = loud.iter().copied().fold(floor, f32::max).max(floor + 1.0);
            self.loud = (0..cols)
                .map(|c| {
                    let v = column(loud, c, floor).max(floor);
                    ((v - floor) / (top - floor)).clamp(0.0, 1.0)
                })
                .collect();
        }
    }
}

impl App {
    pub fn viewer_ui(&mut self, ui: &mut Ui) {
        let Some(v) = &self.viewer else { return };
        let id = v.id.clone();
        let (mut zoom, mut pan) = (v.zoom, v.pan);
        let has_player = self.player.as_ref().is_some_and(|p| p.id == id);
        if has_player {
            egui::SidePanel::right("viewer-side")
                .resizable(true)
                .default_width(310.0)
                .width_range(240.0..=560.0)
                .frame(egui::Frame::side_top_panel(ui.style()))
                .show_inside(ui, |ui| self.viewer_side(ui, &id));
        }

        // Snapshot metadata before mutable UI calls so we can borrow `self` cleanly.
        let Some((name, kind, ext, width, height, duration, text_len, as_text, decodable, is_landscape)) =
            self.lib.as_ref().and_then(|l| l.items.get(&id)).map(|it| {
                let decodable = crate::imaging::SUPPORTED_EXTS.contains(&it.ext.as_str());
                let pdf_open = self.pdf.as_ref().is_some_and(|p| p.id == id);
                let as_text = it.kind == MediaKind::Other && it.text.is_some() && !pdf_open;
                let is_landscape = it.kind == MediaKind::Video && it.width > it.height;
                (
                    it.name.clone(),
                    it.kind,
                    it.ext.clone(),
                    it.width,
                    it.height,
                    it.duration,
                    it.text.as_ref().map_or(0, String::len),
                    as_text,
                    decodable,
                    is_landscape,
                )
            })
        else {
            return;
        };
        let idx = self.visible.iter().position(|x| *x == id).unwrap_or(0);
        let engine_name = self.player.as_ref().filter(|p| p.id == id).map(|p| p.engine_name().to_string());
        let has_frames = self.player.as_ref().filter(|p| p.id == id).is_some_and(|p| p.has_frames());

        // An animated GIF gives the frame for the current time and when the next is due.
        let full = self.full.frame_at(&id, self.now).map(|(tex, next_in)| {
            if let Some(next_in) = next_in {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(next_in.max(0.01)));
            }
            (tex.id(), tex.size_vec2())
        });
        let player_tex = self
            .player
            .as_ref()
            .filter(|p| p.id == id)
            .and_then(|p| p.texture.as_ref())
            .map(|t| (t.id(), t.size_vec2()));
        let pdf = self.pdf.as_ref().filter(|p| p.id == id).map(|p| (p.page, p.total, p.error.clone(), p.pages.get(&p.page).map(|t| (t.id(), t.size_vec2()))));
        let tex = match &pdf {
            // Page 1 can stand in with its thumbnail while it renders.
            Some((page, _, _, shown)) => shown.or_else(|| if *page == 0 { self.thumb_tex(&id) } else { None }),
            None => player_tex.or(full).or_else(|| self.thumb_tex(&id)),
        };
        let img_size = match (width, tex) {
            (_, Some((_, size))) if pdf.is_some() => size,
            (w, _) if w > 0 => vec2(w as f32, height.max(1) as f32),
            (_, Some((_, size))) => size,
            _ => vec2(1.0, 1.0),
        };
        let no_preview = tex.is_none() && self.thumbs.failed.contains(&id);

        // Immersive Studio Header (replaces the main library toolbar while preview is open).
        let mut tags_anchor: Option<Rect> = None;
        egui::Frame::none()
            .fill(tokens().bg_panel)
            .stroke(Stroke::new(1.0_f32, tokens().separator))
            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("← 返回").on_hover_text("回到素材庫（Esc）").clicked() {
                        self.act(Action::CloseViewer);
                    }
                    // Reserve right-side room so long filenames truncate cleanly instead of overlapping buttons.
                    let right_reserve = if has_player { 430.0 } else { 370.0 };
                    let name_max = (ui.available_width() - right_reserve).clamp(110.0, 360.0);
                    ui.allocate_ui_with_layout(vec2(name_max, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&name).strong().color(tokens().text)).truncate())
                            .on_hover_text(&name);
                    });
                    let mut info = format!("{} / {}", idx + 1, self.visible.len());
                    if width > 0 {
                        info += &format!("  ·  {width} × {height}");
                    }
                    if let Some(d) = duration {
                        info += &format!("  ·  {}", fmt_duration(d));
                    }
                    if let Some(eng) = &engine_name {
                        info += &format!("  ·  {eng}");
                    }
                    if ui.available_width() > right_reserve + 80.0 {
                        ui.add(egui::Label::new(RichText::new(info).small().weak()).truncate());
                    }
                    if let Some((page, total, error, _)) = &pdf {
                        ui.separator();
                        if ui.add_enabled(*page > 0, egui::Button::new("▲")).on_hover_text("上一頁 (← / PageUp)").clicked() {
                            self.act(Action::PdfPage(page - 1));
                        }
                        match total {
                            Some(t) => ui.label(format!("第 {} / {t} 頁", page + 1)),
                            None => ui.label("讀取頁數…"),
                        };
                        if ui.add_enabled(total.is_some_and(|t| page + 1 < t), egui::Button::new("▼")).on_hover_text("下一頁 (→ / PageDown)").clicked() {
                            self.act(Action::PdfPage(page + 1));
                        }
                        if let Some(e) = error {
                            ui.label(RichText::new(format!("無法讀取 PDF：{e}")).color(danger()));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (next_tip, prev_tip) = if has_player {
                            ("下一個 (Ctrl+→ / PageDown)", "上一個 (Ctrl+← / PageUp)")
                        } else {
                            ("下一張 (→)", "上一張 (←)")
                        };
                        if ui.button("›").on_hover_text(next_tip).clicked() {
                            self.act(Action::ViewerStep(1));
                        }
                        if ui.button("‹").on_hover_text(prev_tip).clicked() {
                            self.act(Action::ViewerStep(-1));
                        }
                        let tags_btn = ui
                            .selectable_label(self.viewer_tags_open, "🏷 整理")
                            .on_hover_text("為目前素材評分與編輯標籤");
                        if tags_btn.clicked() {
                            self.viewer_tags_open = !self.viewer_tags_open;
                        }
                        tags_anchor = Some(tags_btn.rect);
                        ui.separator();

                        if has_player {
                            let xml_btn = egui::Button::new(RichText::new("匯出 Premiere XML").color(Color32::WHITE))
                                .fill(tokens().selection_bg);
                            if ui.add(xml_btn).on_hover_text("將目前素材與片段匯出為 Premiere Pro XML").clicked() {
                                self.act(Action::ExportPremiereXmlFor(vec![id.clone()]));
                            }
                            if has_frames && is_landscape {
                                let on = self.crop_frame.is_some();
                                if ui
                                    .selectable_label(on, "9:16 (V)")
                                    .on_hover_text("顯示直式 Shorts 裁切框（V），可左右拖曳；拖曳支援 Ctrl+Z 復原")
                                    .clicked()
                                {
                                    self.crop_frame = if on { None } else { Some(0.5) };
                                }
                            }
                            if ui
                                .add_enabled(self.can_undo(), egui::Button::new("↩"))
                                .on_hover_text(self.undo_tip())
                                .on_disabled_hover_text("沒有可以復原的操作（Ctrl+Z）")
                                .clicked()
                            {
                                self.act(Action::Undo);
                            }
                        } else {
                            let slideshow_label = if self.slideshow.is_some() { "⏸ 停止播放" } else { "▶ 幻燈片" };
                            if ui.button(slideshow_label).on_hover_text(format!("每 {:.0} 秒換下一張", self.config.slideshow_secs)).clicked() {
                                self.act(Action::ToggleSlideshow);
                            }
                            ui.separator();
                            if ui.button("適合視窗").clicked() {
                                zoom = 1.0;
                                pan = egui::Vec2::ZERO;
                            }
                            if ui.button("100%").clicked() {
                                zoom = -1.0; // resolved below once the canvas size is known
                                pan = egui::Vec2::ZERO;
                            }
                            if ui
                                .add_enabled(self.can_undo(), egui::Button::new("↩"))
                                .on_hover_text(self.undo_tip())
                                .on_disabled_hover_text("沒有可以復原的操作（Ctrl+Z）")
                                .clicked()
                            {
                                self.act(Action::Undo);
                            }
                            if !decodable {
                                let label = if kind == MediaKind::Image { "以預設程式開啟" } else { "▶ 以預設程式開啟" };
                                if ui.button(label).clicked() {
                                    self.act(Action::OpenExternal(id.clone()));
                                }
                            } else if full.is_none() {
                                if self.full.is_failed(&id) {
                                    ui.label(RichText::new("無法載入原圖").color(danger()));
                                } else {
                                    ui.spinner();
                                }
                            }
                        }
                    });
                });
            });

        if self.viewer_tags_open {
            if let Some(anchor) = tags_anchor {
                self.viewer_tags_popover(ui.ctx(), anchor, &id);
            }
        }

        let has_curves = self.signals.as_ref().is_some_and(|s| s.id == id && (s.chat.is_some() || s.loud.is_some()));
        let controls_h = match (has_player, has_curves) {
            (false, _) => 0.0,
            (true, false) => 92.0,
            (true, true) => 142.0,
        };
        let avail = ui.available_size();
        let canvas_size = vec2(avail.x.max(1.0), (avail.y - controls_h).max(1.0));
        let (rect, resp) = ui.allocate_exact_size(canvas_size, Sense::click_and_drag());
        let mut fit = (rect.width() / img_size.x.max(1.0)).min(rect.height() / img_size.y.max(1.0));
        // PDF pages always fill the height: page 1 first shows as its small thumbnail.
        if !self.config.viewer_upscale && pdf.is_none() {
            fit = fit.min(1.0);
        }
        fit = fit.max(0.01);
        if zoom < 0.0 {
            zoom = 1.0 / fit;
        }

        if resp.hovered() {
            let (mut scroll, pinch, pointer) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
            if pdf.is_some() {
                // The wheel turns pages; Ctrl+wheel still zooms.
                let notch = ui.input(|i| i.raw_scroll_delta.y);
                if notch != 0.0 && !ui.input(|i| i.modifiers.command) {
                    self.act(Action::PdfWheel(notch));
                }
                scroll = 0.0;
            }
            let factor = pinch * (scroll * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let new_zoom = (zoom * factor).clamp(0.05, 64.0);
                let real = new_zoom / zoom;
                // Keep the point under the cursor fixed while zooming.
                if let Some(p) = pointer {
                    let c = rect.center() + pan;
                    pan = (p - (p - c) * real) - rect.center();
                }
                zoom = new_zoom;
            }
        }
        if resp.dragged() {
            pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            if (zoom - 1.0).abs() < 1e-3 {
                // If already 100% (small image with upscale off), zoom in to 200% instead of a no-op.
                zoom = if (1.0 / fit - 1.0).abs() < 0.05 { 2.0 } else { 1.0 / fit };
                pan = egui::Vec2::ZERO;
            } else {
                zoom = 1.0;
                pan = egui::Vec2::ZERO;
            }
        }

        let scale = fit * zoom;
        let draw = Rect::from_center_size(rect.center() + pan, img_size * scale);
        let painter = ui.painter_at(rect);

        // Cache parsed cues when viewing standalone .srt / .vtt files so we don't re-parse every frame.
        let is_cue_file = as_text && matches!(ext.as_str(), "srt" | "vtt");
        if is_cue_file && self.text_cues_cache.as_ref().is_none_or(|(cid, _)| *cid != id) {
            let text = self.lib.as_ref().and_then(|l| l.items.get(&id)).and_then(|it| it.text.as_deref()).unwrap_or_default();
            self.text_cues_cache = Some((id.clone(), crate::media::parse_cues(text)));
        }
        let has_text_cues = is_cue_file && self.text_cues_cache.as_ref().is_some_and(|(cid, cues)| *cid == id && !cues.is_empty());

        match tex {
            _ if has_text_cues => {
                // Virtualized standalone subtitle lines; a click plays that line in the video.
                let area = rect.shrink2(vec2((rect.width() - 760.0).max(40.0) / 2.0, 20.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                let cut = text_len >= 60 * 1024;
                let note = if cut { "點一句在同名的影片中從那裡播放（只列出檔案開頭的部分）" } else { "點一句在同名的影片中從那裡播放" };
                let total_cues = self.text_cues_cache.as_ref().map_or(0, |(_, c)| c.len());
                child.label(RichText::new(format!("{total_cues} 句　·　{note}")).small().color(tokens().text_muted));
                child.add_space(4.0);
                let mut open_at: Option<f64> = None;
                let row_h = 26.0;
                ScrollArea::vertical().id_salt("viewer-cues").auto_shrink([false, false]).show_rows(&mut child, row_h, total_cues, |ui, range| {
                    if let Some((_, cues)) = &self.text_cues_cache {
                        for c in &cues[range] {
                            let line = format!("{}   {}", fmt_duration(c.start), c.text.replace('\n', " "));
                            let text = RichText::new(line).size(14.5).color(tokens().text);
                            if ui.add_sized([ui.available_width(), row_h], egui::SelectableLabel::new(false, text)).on_hover_text("在影片中播放這句").clicked() {
                                open_at = Some(c.start);
                            }
                        }
                    }
                });
                if let Some(t) = open_at {
                    self.act(Action::OpenViewerAt(id.clone(), t));
                }
            }
            _ if as_text => {
                let text = self.lib.as_ref().and_then(|l| l.items.get(&id)).and_then(|it| it.text.as_deref()).unwrap_or_default();
                let shown = if matches!(ext.as_str(), "srt" | "vtt" | "ass" | "ssa" | "lrc") {
                    crate::media::subtitle_lines(text)
                } else {
                    text.to_string()
                };
                let area = rect.shrink2(vec2((rect.width() - 760.0).max(40.0) / 2.0, 30.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                ScrollArea::vertical().id_salt("viewer-text").show(&mut child, |ui| {
                    ui.add(egui::Label::new(RichText::new(shown).size(15.0).color(tokens().text)).wrap());
                });
            }
            Some((t, _)) => {
                painter.image(t, draw, UV, Color32::WHITE);
            }
            None if no_preview => {
                painter.text(rect.center(), Align2::CENTER_CENTER, kind.icon(), FontId::proportional(96.0), tokens().text_muted);
                painter.text(
                    rect.center() + vec2(0.0, 72.0),
                    Align2::CENTER_TOP,
                    format!("{}  ·  沒有預覽，可用右上角按鈕以預設程式開啟", ext.to_uppercase()),
                    FontId::proportional(14.0),
                    tokens().text_muted,
                );
            }
            None => {
                painter.text(rect.center(), Align2::CENTER_CENTER, "載入中…", FontId::proportional(16.0), tokens().text_muted);
            }
        }
        if has_player && is_landscape && tex.is_some() {
            self.crop_overlay(ui, &id, draw, rect);
        }
        let cue = self
            .subs
            .as_ref()
            .filter(|s| self.sub_overlay && s.id == id)
            .zip(self.player.as_ref().filter(|p| p.id == id))
            .and_then(|(s, p)| s.at(p.position()).map(|i| s.cues[i].text.clone()));
        if let Some(text) = cue {
            let galley = painter.layout(text, FontId::proportional(20.0), Color32::WHITE, rect.width() * 0.8);
            let bottom = draw.bottom().min(rect.bottom()) - 18.0;
            let at = pos2(rect.center().x - galley.size().x / 2.0, bottom - galley.size().y);
            painter.rect_filled(Rect::from_min_size(at, galley.size()).expand(6.0), 4.0, PILL_BG);
            painter.galley(at, galley, Color32::WHITE);
        }
        if let Some((text, at)) = &self.seek_flash {
            let age = (self.now - at) as f32;
            if age < FLASH_SECS {
                let alpha = (1.0 - age / FLASH_SECS).clamp(0.0, 1.0);
                let galley = painter.layout_no_wrap(text.clone(), FontId::proportional(28.0), Color32::WHITE.gamma_multiply(alpha));
                let at = rect.center() - galley.size() / 2.0;
                painter.rect_filled(Rect::from_min_size(at, galley.size()).expand2(vec2(14.0, 8.0)), 8.0, Color32::from_black_alpha((150.0 * alpha) as u8));
                painter.galley(at, galley, Color32::WHITE);
                ui.ctx().request_repaint();
            }
        }
        painter.text(
            rect.right_bottom() - vec2(12.0, 10.0),
            Align2::RIGHT_BOTTOM,
            format!("{:.0}%", scale * 100.0),
            FontId::proportional(12.0),
            tokens().text_muted,
        );

        if let Some(v) = &mut self.viewer {
            v.zoom = zoom;
            v.pan = pan;
        }
        if has_player {
            self.player_controls(ui);
        }
    }

    /// Popover anchored under the studio header's 🏷 整理 button for quick rating & tagging.
    fn viewer_tags_popover(&mut self, ctx: &egui::Context, anchor: Rect, id: &str) {
        let stats = self.cached_sidebar_stats();
        let Some((name, rating, tags)) = self.lib.as_ref().and_then(|l| l.items.get(id)).map(|it| (it.name.clone(), it.rating, it.tags.clone())) else {
            self.viewer_tags_open = false;
            return;
        };
        let ids = vec![id.to_string()];
        let area = egui::Area::new(egui::Id::new("viewer-tags-popover"))
            .order(egui::Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor.right_bottom() + vec2(0.0, 4.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(280.0);
                    ui.add(egui::Label::new(RichText::new(&name).strong()).truncate());
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        for i in 1..=5u8 {
                            let filled = rating >= i;
                            let star_text = RichText::new(if filled { "★" } else { "☆" })
                                .size(20.0)
                                .color(if filled { star() } else { ui.visuals().weak_text_color() });
                            if ui.add(egui::Label::new(star_text).sense(Sense::click())).on_hover_text(format!("{i} 星（數字鍵 {i}）")).clicked() {
                                self.act(Action::SetRating(ids.clone(), if rating == i { 0 } else { i }));
                            }
                        }
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new("標籤").small().weak());
                    ui.horizontal_wrapped(|ui| {
                        if tags.is_empty() {
                            ui.label(RichText::new("無").small().weak());
                        }
                        for t in &tags {
                            if chip(ui, t) {
                                self.act(Action::RemoveTag(ids.clone(), t.clone()));
                            }
                        }
                    });
                    for t in inspector::tag_box(ui, &mut self.tag_input, &mut self.tag_pick, &stats.tag_counts, &tags, "輸入標籤後按 Enter", false) {
                        self.act(Action::AddTag(ids.clone(), t));
                    }
                });
            });
        let clicked_out = ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| area.response.rect.contains(p) || anchor.contains(p));
        let escaped = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if clicked_out || escaped {
            self.viewer_tags_open = false;
        }
    }

    /// The 9:16 frame over a landscape video. It follows the clip being played when that
    /// clip has one; dragging it moves the frame and saves it on that clip (undoable).
    pub(super) fn crop_overlay(&mut self, ui: &mut Ui, id: &str, draw: Rect, clip_to: Rect) {
        let Some(shown) = self.crop_frame else { return };
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        let Some(it) = self.lib.as_ref().and_then(|l| l.items.get(id)) else { return };
        let active = it.clips.iter().find(|c| pos >= c.start && pos < c.end).map(|c| (c.id.clone(), c.crop_x));
        let centre = active.as_ref().and_then(|(_, x)| *x).unwrap_or(shown);
        let w = draw.height() * 9.0 / 16.0;
        if w >= draw.width() || draw.width() <= 1.0 {
            return;
        }
        let half = w / 2.0 / draw.width();
        let centre = centre.clamp(half, 1.0 - half);
        let frame = Rect::from_center_size(pos2(draw.left() + centre * draw.width(), draw.center().y), vec2(w, draw.height()));
        let painter = ui.painter_at(clip_to);
        let dim = Color32::from_black_alpha(140);
        painter.rect_filled(Rect::from_x_y_ranges(draw.left()..=frame.left(), draw.y_range()), 0.0, dim);
        painter.rect_filled(Rect::from_x_y_ranges(frame.right()..=draw.right(), draw.y_range()), 0.0, dim);
        painter.rect_stroke(frame, 0.0, Stroke::new(2.0_f32, accent()));
        let r = ui.interact(frame, ui.id().with("crop-frame"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if r.drag_started() && active.is_some() {
            self.crop_drag_before = Some(it.clone());
        }
        if r.dragged() {
            let moved = (centre + r.drag_delta().x / draw.width()).clamp(half, 1.0 - half);
            self.crop_frame = Some(moved);
            if let Some((cid, _)) = &active {
                if let Some(lib) = &mut self.lib {
                    if let Some(c) = lib.items.get_mut(id).and_then(|it| it.clips.iter_mut().find(|c| c.id == *cid)) {
                        c.crop_x = Some(moved);
                    }
                }
            }
        }
        if r.drag_stopped() {
            if let Some(before) = self.crop_drag_before.take() {
                if let Some(lib) = &mut self.lib {
                    lib.touch(id);
                }
                self.record_undo(vec![before], false);
            }
        }
    }

    /// Integrated timeline dock: dual-track chat/loudness activity strip, seek slider,
    /// clip blocks, transport + keycap I/O controls, and volume.
    pub(super) fn player_controls(&mut self, ui: &mut Ui) {
        let Some(p) = &self.player else { return };
        let item_id = p.id.clone();
        let dur = p.duration.max(0.1);
        let pos = p.position();
        let has_curves = self.signals.as_ref().is_some_and(|s| s.id == item_id && (s.chat.is_some() || s.loud.is_some()));
        // Update downsampled curve cache before borrowing `self.player` mutably.
        let plot_cols = (ui.available_width() - 32.0).max(1.0) as usize;
        if has_curves {
            if let Some(s) = self.signals.as_ref().filter(|s| s.id == item_id) {
                self.signal_plot_cache.update(&item_id, plot_cols, s.chat.as_deref(), s.loud.as_deref());
            }
        }

        let Some(p) = &mut self.player else { return };
        let clips: Vec<crate::model::Clip> = self.lib.as_ref().and_then(|l| l.items.get(&p.id)).map(|i| i.clips.clone()).unwrap_or_default();
        let selected: std::collections::HashSet<String> = self.clip_ui.sel.clone();
        let mut drag = self.clip_ui.drag.clone();
        let mut picked: Option<String> = None;
        let mut range_done: Option<(String, f64, f64)> = None;
        let mark = self.mark;
        let mark_color = info();
        let mut hover = None;

        egui::Frame::none()
            .fill(tokens().bg_panel)
            .stroke(Stroke::new(1.0_f32, tokens().separator))
            .inner_margin(egui::Margin::symmetric(12.0, 8.0))
            .show(ui, |ui| {
                if has_curves {
                    if let Some(t) = activity_strip(ui, &self.signal_plot_cache, dur, pos, mark) {
                        p.seek(t);
                    }
                }
                let mut seek_to = pos;
                ui.spacing_mut().slider_width = ui.available_width().max(1.0);
                let r = ui.add(egui::Slider::new(&mut seek_to, 0.0..=dur).show_value(false));
                // Media Foundation follows the drag (nearest keyframe); restarting
                // ffmpeg on every step would stutter, so it seeks on release.
                if r.dragged() && r.changed() {
                    p.scrub(seek_to);
                }
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    p.seek(seek_to);
                }
                let handle = r.rect.height() / 2.5;
                let span = (r.rect.width() - 2.0 * handle).max(1.0);
                if let (true, Some(at)) = (p.has_frames(), r.hover_pos()) {
                    let t = ((at.x - r.rect.left() - handle) / span).clamp(0.0, 1.0) as f64 * dur;
                    hover = Some((t, r.id, ui.layer_id(), p.id.clone(), dur));
                }
                let (strip, _) = ui.allocate_exact_size(vec2(r.rect.width().max(1.0), if clips.is_empty() { 6.0 } else { 13.0 }), Sense::hover());
                let x = |t: f64| r.rect.left() + handle + (t / dur).clamp(0.0, 1.0) as f32 * span;
                let t_at = |px: f32| ((px - r.rect.left() - handle) / span).clamp(0.0, 1.0) as f64 * dur;
                ui.painter().rect_filled(strip, 2.0, ui.visuals().extreme_bg_color);
                // Clips on the timeline: click one to select it and jump to its start;
                // drag either end to move its in or out point (one undo step).
                const EDGE: f32 = 5.0;
                for c in &clips {
                    let (mut a, mut b) = (c.start, c.end);
                    if let Some((cid, end, t)) = &drag {
                        if *cid == c.id {
                            if *end {
                                b = *t;
                            } else {
                                a = *t;
                            }
                        }
                    }
                    let block = Rect::from_x_y_ranges(x(a)..=x(b).max(x(a) + 2.0), strip.y_range());
                    let candidate = crate::app::is_candidate(&c.name);
                    let color = if selected.contains(&c.id) { Color32::WHITE } else { accent() };
                    let body = ui.interact(block, ui.id().with(("clip-block", &c.id)), Sense::click());
                    if candidate {
                        ui.painter().rect_filled(block, 2.0, color.gamma_multiply(0.25));
                        dashed_rect(ui.painter(), block.shrink(0.5), Stroke::new(1.0_f32, color));
                    } else {
                        ui.painter().rect_filled(block, 2.0, if body.hovered() { color.gamma_multiply(1.2) } else { color });
                    }
                    let body = body.on_hover_text(format!("{}（{} – {}）\n點一下選取並跳到開頭；拖曳左右邊緣調整入點／出點", c.name, fmt_precise(c.start), fmt_precise(c.end)));
                    if body.clicked() {
                        picked = Some(c.id.clone());
                        p.seek(c.start);
                    }
                    for (end, px) in [(false, x(a)), (true, x(b))] {
                        let grip = Rect::from_x_y_ranges(px - EDGE..=px + EDGE, strip.y_range());
                        let g = ui.interact(grip, ui.id().with(("clip-edge", &c.id, end)), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                        if g.dragged() {
                            if let Some(pos) = g.interact_pointer_pos() {
                                // At least 0.1 s long; the other end stays put.
                                let t = t_at(pos.x);
                                let t = if end { t.max(c.start + 0.1) } else { t.min(c.end - 0.1) };
                                drag = Some((c.id.clone(), end, t));
                                if !p.is_playing() {
                                    p.scrub(t);
                                }
                            }
                        }
                        if g.drag_stopped() {
                            if let Some((_, _, t)) = drag.take().filter(|(cid, e, _)| *cid == c.id && *e == end) {
                                let (s, e) = if end { (c.start, t) } else { (t, c.end) };
                                range_done = Some((c.id.clone(), s, e));
                                p.seek(if end { e } else { s });
                            }
                        }
                    }
                }
                let painter = ui.painter();
                if let (Some(a), Some(b)) = mark {
                    let band = Rect::from_x_y_ranges(x(a.min(b))..=x(a.max(b)), strip.y_range());
                    painter.rect_filled(band, 2.0, mark_color.gamma_multiply(0.6));
                }
                for (m, label) in [(mark.0, "I"), (mark.1, "O")] {
                    if let Some(t) = m {
                        let px = x(t);
                        painter.line_segment([pos2(px, r.rect.top()), pos2(px, strip.bottom())], Stroke::new(2.0_f32, mark_color));
                        painter.text(pos2(px + 3.0, r.rect.top()), Align2::LEFT_TOP, label, FontId::proportional(10.0), mark_color);
                    }
                }

                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    let label = if p.is_playing() { "⏸" } else { "▶" };
                    let play_btn = egui::Button::new(RichText::new(label).size(15.0).color(Color32::WHITE))
                        .fill(tokens().selection_bg);
                    if ui.add_sized([36.0, 26.0], play_btn).on_hover_text("播放／暫停（空白鍵；K 暫停並回到 1×）\nL 播放，再按 2×、4×；J 倒播，再按加快\n← → 跳 5 秒、Shift+← → 跳 1 秒、, . 逐格、Home / End 開頭 / 結尾").clicked() {
                        p.toggle();
                    }
                    let rate = p.rate();
                    if rate != 1.0 {
                        let text = if rate < 0.0 { format!("◀◀ {}×", -rate) } else { format!("{rate}×") };
                        ui.label(RichText::new(text).strong().color(accent())).on_hover_text("播放速度；按 K 回到正常速度");
                    }
                    ui.label(RichText::new(format!("{} / {}", fmt_precise(pos), fmt_duration(p.duration))).monospace().color(tokens().text))
                        .on_hover_text(format!("播放引擎：{}（可在設定 →「預覽與播放」更改）", p.engine_name()));
                    ui.separator();
                    if ui.button("入點 (I)").on_hover_text("把目前位置設為片段開頭（快速鍵 I）").clicked() {
                        self.mark.0 = Some(pos);
                    }
                    if ui.button("出點 (O)").on_hover_text("把目前位置設為片段結尾（快速鍵 O）").clicked() {
                        self.mark.1 = Some(pos);
                    }
                    if mark.0.is_some() || mark.1.is_some() {
                        let (start, end) = crate::app::clip_from_marks(mark, pos, p.duration);
                        let add_btn = egui::Button::new(RichText::new("＋ 加入片段 (Enter)").color(accent()))
                            .fill(tokens().accent_soft)
                            .stroke(Stroke::new(1.0_f32, accent().gamma_multiply(0.6)));
                        if ui.add(add_btn).on_hover_text("加入片段（快速鍵 Enter）").clicked() {
                            self.acts.borrow_mut().push(Action::AddClip(p.id.clone(), start, end, None));
                        }
                        if ui.small_button("✖").on_hover_text("清除入點／出點").clicked() {
                            self.mark = (None, None);
                        }
                    }
                    if p.has_frames() && ui.button("📷").on_hover_text("擷取目前畫面成圖片").clicked() {
                        self.acts.borrow_mut().push(Action::CaptureFrame(p.id.clone(), pos));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let mut volume = p.volume;
                        ui.spacing_mut().slider_width = 80.0;
                        if ui.add(egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false)).changed() {
                            p.set_volume(volume);
                            self.config.volume = volume;
                        }
                        ui.label(RichText::new("🔊").color(tokens().text_muted));
                    });
                });
                if let Some(e) = &p.error {
                    ui.label(RichText::new(e).small().color(danger()));
                }
            });
        self.clip_ui.drag = drag;
        if let Some(cid) = picked {
            self.clip_ui.sel = std::collections::HashSet::from([cid]);
        }
        if let Some((cid, start, end)) = range_done {
            self.acts.borrow_mut().push(Action::SetClipRange(item_id, cid, start, end));
        }
        // The frame at the hovered time, above the pointer.
        if let Some((t, widget, layer, id, dur)) = hover {
            let frame = self.scrub_frame(&id, t, dur);
            egui::show_tooltip_at_pointer(ui.ctx(), layer, widget.with("scrub"), |ui| {
                match frame {
                    Some(tex) => {
                        ui.image(tex);
                    }
                    None => {
                        ui.spinner();
                    }
                }
                ui.label(RichText::new(fmt_precise(t)).small());
            });
        }
    }

    /// Three spacious studio tabs beside the player: Clips, Subtitles, Highlights.
    /// (Rating & tags live in the studio header's 🏷 整理 popover.)
    pub(super) fn viewer_side(&mut self, ui: &mut Ui, id: &str) {
        let clip_count = self.lib.as_ref().and_then(|l| l.items.get(id)).map_or(0, |i| i.clips.len());
        let cue_count = self.subs.as_ref().map_or(0, |s| s.cues.len());
        ui.add_space(4.0);
        ui.columns(3, |cols| {
            cols[0].vertical_centered_justified(|ui| {
                ui.selectable_value(&mut self.viewer_tab, ViewerTab::Clips, format!("✂ 片段 ({clip_count})"));
            });
            cols[1].vertical_centered_justified(|ui| {
                ui.selectable_value(&mut self.viewer_tab, ViewerTab::Subtitles, format!("💬 字幕 ({cue_count})"));
            });
            cols[2].vertical_centered_justified(|ui| {
                ui.selectable_value(&mut self.viewer_tab, ViewerTab::Highlights, "⚡ 精華");
            });
        });
        ui.separator();
        match self.viewer_tab {
            ViewerTab::Clips => self.clips_panel(ui, id),
            ViewerTab::Subtitles => self.subtitles_panel(ui, id),
            ViewerTab::Highlights => crate::app::candidates_ui(self, ui, id),
        }
    }

    pub(super) fn clips_panel(&mut self, ui: &mut Ui, id: &str) {
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        if self.mark.0.is_some() || self.mark.1.is_some() {
            let show = |m: Option<f64>| m.map(fmt_precise).unwrap_or_else(|| "—".to_string());
            egui::Frame::none()
                .fill(tokens().accent_soft)
                .rounding(ROUNDING)
                .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("入點 {}   →   出點 {}", show(self.mark.0), show(self.mark.1))).small().color(accent()));
                });
            ui.add_space(4.0);
        }

        let mut pending = Vec::new();
        let (mut play, mut seek) = (None, None);
        let Some(it) = self.lib.as_ref().and_then(|l| l.items.get(id)) else { return };
        let landscape = it.kind == MediaKind::Video && it.width > it.height;
        let video = it.kind == MediaKind::Video;
        let presets = self.config.export_presets.clone();
        let mut clips = it.clips.clone();

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.clip_loop, "循環播放").on_hover_text("按片段的 ▶ 播放時，到出點就回到入點重播");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = match self.config.clip_sort {
                    ClipSort::Start => "依時間",
                    ClipSort::Name => "依名稱",
                    ClipSort::Score => "依分數",
                };
                egui::ComboBox::from_id_salt("clip-sort").width(86.0).selected_text(RichText::new(label).small()).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.config.clip_sort, ClipSort::Start, "依開始時間");
                    ui.selectable_value(&mut self.config.clip_sort, ClipSort::Name, "依名稱");
                    ui.selectable_value(&mut self.config.clip_sort, ClipSort::Score, "依精華分數").on_hover_text("自動找到的候選依聊天和音量的突出程度排列，最突出的在前");
                });
            });
        });

        if clips.is_empty() {
            ui.add_space(12.0);
            egui::Frame::none()
                .fill(tokens().bg_elevated)
                .stroke(Stroke::new(1.0_f32, tokens().separator))
                .rounding(ROUNDING)
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("尚無片段").strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new("播放時按 I 設入點、O 設出點，再按 Enter 加入片段；或到「⚡ 精華」分頁自動偵測直播高峰。").small().weak());
                });
            return;
        }
        self.clip_ui.sel.retain(|c| clips.iter().any(|x| x.id == *c));
        match self.config.clip_sort {
            ClipSort::Start => {}
            ClipSort::Name => clips.sort_by(|a, b| a.name.cmp(&b.name).then(a.start.total_cmp(&b.start))),
            ClipSort::Score => clips.sort_by(|a, b| b.score.unwrap_or(f32::MIN).total_cmp(&a.score.unwrap_or(f32::MIN)).then(a.start.total_cmp(&b.start))),
        }
        let candidates = clips.iter().filter(|c| is_candidate(&c.name)).count();
        let mods = ui.input(|i| i.modifiers);

        if candidates > 0 && ui.small_button(format!("刪除所有未保留的候選（{candidates}）")).clicked() {
            pending.push(Action::DeleteCandidates(id.to_string()));
        }
        // Always there, so selecting does not move the list under the pointer.
        let sel: Vec<String> = clips.iter().filter(|c| self.clip_ui.sel.contains(&c.id)).map(|c| c.id.clone()).collect();
        let any = !sel.is_empty();
        ui.horizontal_wrapped(|ui| {
            let label = if any { RichText::new(format!("已選取 {}", sel.len())).small().color(accent()) } else { RichText::new("Ctrl／Shift＋點選多選").small().weak() };
            ui.label(label);
            ui.add_enabled_ui(any, |ui| {
                ui.menu_button(RichText::new("輸出 ▼").small(), |ui| {
                    if let Some(p) = preset_menu(ui, &presets, video) {
                        pending.push(Action::ExportClips(id.to_string(), sel.clone(), p));
                    }
                })
                .response
                .on_hover_text("用輸出預設組寫成檔案；預設組在設定 →「直播精華」");
            });
            if ui.add_enabled(any, egui::Button::new(RichText::new("刪除").small())).on_hover_text("Delete").clicked() {
                pending.push(Action::DeleteClips(id.to_string(), sel.clone()));
            }
            if any && ui.small_button("取消").clicked() {
                self.clip_ui.sel.clear();
            }
        });
        ui.add_space(2.0);

        let mut click: Option<(usize, bool)> = None;
        ScrollArea::vertical().id_salt("clips").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            for (n, c) in clips.iter().enumerate() {
                let active = pos >= c.start && pos < c.end;
                let selected = self.clip_ui.sel.contains(&c.id);
                let candidate = is_candidate(&c.name);
                let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
                let fill = if selected {
                    tokens().accent_soft
                } else if active {
                    accent().gamma_multiply(0.14)
                } else if row.hovered() {
                    ui.visuals().widgets.hovered.weak_bg_fill
                } else {
                    tokens().bg_elevated
                };
                ui.painter().rect_filled(rect, ROUNDING, fill);
                if selected {
                    ui.painter().rect_stroke(rect, ROUNDING, Stroke::new(1.0_f32, accent().gamma_multiply(0.8)));
                } else if candidate {
                    dashed_rect(ui.painter(), rect.shrink(0.5), Stroke::new(1.0_f32, ui.visuals().weak_text_color()));
                } else {
                    ui.painter().rect_stroke(rect, ROUNDING, Stroke::new(1.0_f32, tokens().separator));
                }
                if selected || active {
                    let bar = Rect::from_min_max(rect.min + vec2(0.0, 5.0), pos2(rect.min.x + 2.5, rect.max.y - 5.0));
                    ui.painter().rect_filled(bar, 1.5, accent());
                }
                if row.clicked() {
                    click = Some((n, false));
                }
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 5.0))), |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("▶").on_hover_text("播放這個片段，到出點停止").clicked() {
                            play = Some((c.start, c.end));
                        }
                        let len = RichText::new(fmt_duration(c.end - c.start)).small().monospace().color(accent());
                        let editing = self.clip_ui.renaming.as_ref().is_some_and(|e| e.id == c.id);
                        let w = (ui.available_width() - 78.0).max(40.0);
                        if editing {
                            let Some(InlineEdit { id: cid, draft, focused, .. }) = self.clip_ui.renaming.as_mut() else { return };
                            let r = ui.add(TextEdit::singleline(draft).desired_width(w));
                            if !*focused {
                                r.request_focus();
                                *focused = true;
                            } else if r.lost_focus() {
                                if !ui.input(|i| i.key_pressed(Key::Escape)) {
                                    pending.push(Action::RenameClip(id.to_string(), cid.clone(), draft.clone()));
                                }
                                self.clip_ui.renaming = None;
                            }
                        } else {
                            let text = if candidate { RichText::new(&c.name).italics().weak() } else { RichText::new(&c.name).strong() };
                            let r = ui.allocate_ui_with_layout(vec2(w, 18.0), Layout::left_to_right(Align::Center), |ui| ui.add(egui::Label::new(text).truncate().sense(Sense::click()))).inner;
                            if r.on_hover_text("點一下重新命名；Ctrl／Shift＋點一下多選").clicked() {
                                click = Some((n, true));
                            }
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.menu_button("…", |ui| {
                                ui.menu_button("輸出", |ui| {
                                    if landscape && c.crop_x.is_none() {
                                        ui.label(RichText::new("還沒設定 9:16 框，直式會裁切畫面中央").small().weak());
                                    }
                                    if let Some(p) = preset_menu(ui, &presets, video) {
                                        pending.push(Action::ExportClip(id.to_string(), c.id.clone(), p));
                                    }
                                });
                                if ui.button("重新命名").clicked() {
                                    self.clip_ui.renaming = Some(InlineEdit::new(c.id.clone(), c.name.clone(), false));
                                    ui.close_menu();
                                }
                                if ui.button("刪除片段").clicked() {
                                    pending.push(Action::DeleteClips(id.to_string(), vec![c.id.clone()]));
                                    ui.close_menu();
                                }
                            });
                            ui.label(len).on_hover_text("片段長度");
                        });
                    });
                    ui.horizontal(|ui| {
                        let range = format!("{} → {}", fmt_precise(c.start), fmt_precise(c.end));
                        let r = ui.add(egui::Label::new(RichText::new(range).small().monospace().weak()).sense(Sense::click()));
                        let tip = match &c.note {
                            Some(note) => format!("跳到片段開頭\n\n為什麼選這段：{note}{}", c.score.map_or(String::new(), |s| format!("\n分數 {s:.1}"))),
                            None => "跳到片段開頭".to_string(),
                        };
                        if c.note.is_some() {
                            ui.label(RichText::new("ⓘ").small().weak()).on_hover_text(&tip);
                        }
                        if r.on_hover_text(tip).clicked() {
                            seek = Some(c.start);
                        }
                        if candidate {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("刪除").on_hover_text("刪除這個候選").clicked() {
                                    pending.push(Action::DeleteClips(id.to_string(), vec![c.id.clone()]));
                                }
                                if ui.small_button("保留").on_hover_text("改成一般片段").clicked() {
                                    pending.push(Action::KeepClip(id.to_string(), c.id.clone()));
                                }
                            });
                        }
                    });
                });
            }
        });

        // Ctrl toggles, Shift selects the run from the last clicked; a plain click
        // on the name edits it, elsewhere on the row selects that clip alone.
        if let Some((n, on_name)) = click {
            let cid = clips[n].id.clone();
            if mods.command {
                if !self.clip_ui.sel.remove(&cid) {
                    self.clip_ui.sel.insert(cid.clone());
                }
                self.clip_ui.anchor = Some(cid);
            } else if mods.shift {
                let from = self.clip_ui.anchor.as_ref().and_then(|a| clips.iter().position(|c| c.id == *a)).unwrap_or(n);
                let (lo, hi) = (from.min(n), from.max(n));
                self.clip_ui.sel = clips[lo..=hi].iter().map(|c| c.id.clone()).collect();
            } else {
                self.clip_ui.sel = std::iter::once(cid.clone()).collect();
                self.clip_ui.anchor = Some(cid.clone());
                if on_name {
                    self.clip_ui.renaming = Some(InlineEdit::new(cid, clips[n].name.clone(), false));
                }
            }
        }
        if let Some(p) = &mut self.player {
            if let Some((a, b)) = play {
                p.play_range(a, b);
            } else if let Some(t) = seek {
                p.seek(t);
            }
        }
        if self.clip_exporting() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("正在輸出片段…").small());
            });
        }
        for a in pending {
            self.act(a);
        }
    }

    pub(super) fn subtitles_panel(&mut self, ui: &mut Ui, id: &str) {
        let pos = self.player.as_ref().map_or(0.0, |p| p.position());
        let playing = self.player.as_ref().is_some_and(|p| p.is_playing());
        let mut pending = Vec::new();
        let (mut seek, mut clip) = (None, None);
        let Some(subs) = self.subs.as_mut().filter(|s| s.id == id) else { return };
        match &subs.path {
            None => {
                ui.add_space(8.0);
                egui::Frame::none()
                    .fill(tokens().bg_elevated)
                    .stroke(Stroke::new(1.0_f32, tokens().separator))
                    .rounding(ROUNDING)
                    .inner_margin(12.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("找不到同名的字幕檔（.srt / .vtt）").strong());
                        ui.add_space(4.0);
                        ui.label(RichText::new("可以直接用本機 Whisper 產生繁體中文字幕，或手動選擇既有的字幕檔。").small().weak());
                        ui.add_space(8.0);
                        ui.horizontal_wrapped(|ui| {
                            let whisper_btn = egui::Button::new(RichText::new("🎤 用 Whisper 產生字幕…").color(Color32::WHITE))
                                .fill(tokens().selection_bg);
                            if ui.add(whisper_btn).clicked() {
                                pending.push(Action::OpenWhisper(vec![id.to_string()]));
                            }
                            if ui.button("選擇字幕檔…").clicked() {
                                pending.push(Action::PickSubtitle(id.to_string()));
                            }
                        });
                    });
            }
            Some(path) => {
                ui.horizontal(|ui| {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(format!("📄 {name}")).small()).truncate()).on_hover_text(path.display().to_string());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("更換…").clicked() {
                            pending.push(Action::PickSubtitle(id.to_string()));
                        }
                    });
                });
                ui.checkbox(&mut self.sub_overlay, "在畫面上顯示字幕");
                ui.add(TextEdit::singleline(&mut subs.search).hint_text("🔍 搜尋台詞…").desired_width(f32::INFINITY));
                if subs.cues.is_empty() {
                    ui.label(RichText::new("字幕檔中沒有可讀取的內容").weak());
                }
                let current = subs.at(pos);
                let follow = playing && current.is_some() && current != subs.last_cue && subs.search.is_empty();
                subs.last_cue = current;
                let q = subs.search.trim().to_lowercase();
                let filtered: Vec<usize> = if q.is_empty() {
                    (0..subs.cues.len()).collect()
                } else {
                    subs.cues
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.text.to_lowercase().contains(&q))
                        .map(|(i, _)| i)
                        .collect()
                };
                ui.label(RichText::new("點一句跳到該時間；右鍵可從這句建立片段").small().weak());
                let row_h = 26.0;
                let mut scroll = ScrollArea::vertical().id_salt("cues").auto_shrink([false, false]);
                let scroll_target = subs.focus.or(if follow { current } else { None });
                if let Some(target_idx) = scroll_target {
                    if let Some(row_pos) = filtered.iter().position(|&idx| idx == target_idx) {
                        let view_h = ui.available_height().max(row_h);
                        let target_y = (row_pos as f32 * row_h - view_h * 0.4).max(0.0);
                        scroll = scroll.vertical_scroll_offset(target_y);
                    }
                }
                scroll.show_rows(ui, row_h, filtered.len(), |ui, range| {
                    for &i in &filtered[range] {
                        let c = &subs.cues[i];
                        let is_curr = current == Some(i);
                        let single_line = c.text.replace('\n', " ");
                        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                        let fill = if is_curr {
                            tokens().accent_soft
                        } else if r.hovered() {
                            ui.visuals().widgets.hovered.weak_bg_fill
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(rect, 3.0, fill);
                        if is_curr {
                            let bar = Rect::from_min_max(rect.min + vec2(0.0, 3.0), pos2(rect.min.x + 2.0, rect.max.y - 3.0));
                            ui.painter().rect_filled(bar, 1.0, accent());
                        }
                        if subs.focus == Some(i) && !is_curr {
                            ui.painter().rect_stroke(rect, 3.0, egui::Stroke::new(1.0_f32, accent()));
                        }
                        let ts = fmt_duration(c.start);
                        let ts_w = 46.0;
                        ui.painter().text(rect.left_center() + vec2(6.0, 0.0), Align2::LEFT_CENTER, &ts, FontId::monospace(11.0), accent());
                        let text_rect = Rect::from_min_max(rect.min + vec2(6.0 + ts_w, 0.0), rect.max - vec2(4.0, 0.0));
                        ui.painter().with_clip_rect(text_rect.intersect(ui.clip_rect())).text(
                            text_rect.left_center(),
                            Align2::LEFT_CENTER,
                            &single_line,
                            FontId::proportional(12.0),
                            tokens().text,
                        );
                        if r.clicked() {
                            seek = Some(c.start);
                        }
                        r.on_hover_text(format!("{ts}  {}", c.text)).context_menu(|ui| {
                            if ui.button("從這句建立片段").clicked() {
                                clip = Some((c.start, c.end, single_line.chars().take(40).collect::<String>()));
                                ui.close_menu();
                            }
                        });
                    }
                });
                if subs.focus.is_some_and(|f| current == Some(f) || seek.is_some()) {
                    subs.focus = None;
                }
            }
        }
        if let (Some(t), Some(p)) = (seek, &mut self.player) {
            p.seek(t);
            p.play();
        }
        if let Some((a, b, name)) = clip {
            pending.push(Action::AddClip(id.to_string(), a, b, Some(name)));
        }
        for a in pending {
            self.act(a);
        }
    }
}

/// Chat activity (bars) and loudness (line) along the video, lined up with the progress
/// bar below, with I/O range highlight. Returns the time clicked.
fn activity_strip(ui: &mut Ui, cache: &SignalPlotCache, dur: f64, pos: f64, mark: (Option<f64>, Option<f64>)) -> Option<f64> {
    let width = ui.available_width().max(1.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    // The same inset as the slider's handle, so a time lines up with the bar below.
    let inset = ui.spacing().interact_size.y / 2.5;
    let inner = rect.shrink2(vec2(inset, 0.0));
    let inner_w = inner.width().max(1.0);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, tokens().bg_sunken);
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, tokens().separator));

    // Selected I/O range band across the activity strip.
    if let (Some(a), Some(b)) = mark {
        let xa = inner.left() + (a.min(b) / dur).clamp(0.0, 1.0) as f32 * inner_w;
        let xb = inner.left() + (a.max(b) / dur).clamp(0.0, 1.0) as f32 * inner_w;
        let band = Rect::from_x_y_ranges(xa..=xb, rect.y_range());
        painter.rect_filled(band, 0.0, tokens().accent_soft);
    }

    if !cache.chat.is_empty() {
        let step = inner_w / cache.chat.len() as f32;
        for (c, &norm) in cache.chat.iter().enumerate() {
            if norm <= 0.005 {
                continue;
            }
            let h = norm * (inner.height() - 2.0);
            let x = inner.left() + (c as f32 + 0.5) * step;
            painter.line_segment([pos2(x, inner.bottom()), pos2(x, inner.bottom() - h)], Stroke::new(step.max(1.0), accent().gamma_multiply(0.72)));
        }
    }
    if !cache.loud.is_empty() {
        let step = inner_w / cache.loud.len() as f32;
        let points: Vec<egui::Pos2> = cache
            .loud
            .iter()
            .enumerate()
            .map(|(c, &norm)| pos2(inner.left() + (c as f32 + 0.5) * step, inner.bottom() - norm * (inner.height() - 2.0)))
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.1_f32, info())));
    }
    let x = inner.left() + (pos / dur).clamp(0.0, 1.0) as f32 * inner_w;
    painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5_f32, Color32::WHITE));
    let resp = resp.on_hover_text("橘色：聊天熱度　藍色：音量（點一下跳到該時間）");
    resp.clicked().then(|| resp.interact_pointer_pos()).flatten().map(|p| ((p.x - inner.left()) / inner_w).clamp(0.0, 1.0) as f64 * dur)
}

/// The export presets that fit the item, as menu buttons; returns the index clicked.
/// Vertical presets need a video: a landscape one is cropped, a portrait one scaled.
fn preset_menu(ui: &mut Ui, presets: &[crate::config::ExportPreset], video: bool) -> Option<usize> {
    use crate::config::ExportShape;
    let mut picked = None;
    for (i, p) in presets.iter().enumerate() {
        let mut what = vec![p.shape.label().to_string()];
        if p.shape == ExportShape::Vertical && p.full_hd {
            what.push("1080×1920".into());
        }
        if p.loudnorm && p.shape != ExportShape::Copy {
            what.push("-14 LUFS".into());
        }
        if p.subtitles && video && matches!(p.shape, ExportShape::Original | ExportShape::Vertical) {
            what.push("燒入字幕".into());
        }
        if !video && p.shape == ExportShape::Vertical {
            continue;
        }
        what.push(match &p.folder {
            Some(f) => format!("存到 {}", f.display()),
            None => "存到資源庫的 captures".into(),
        });
        if p.import {
            what.push("加入資源庫".into());
        }
        if ui.button(&p.name).on_hover_text(what.join("・")).clicked() {
            picked = Some(i);
            ui.close_menu();
        }
    }
    picked
}

/// A rectangle outlined with dashes: a candidate clip not kept yet.
fn dashed_rect(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
    painter.extend(egui::Shape::dashed_line(&pts, stroke, 4.0, 3.0));
}
