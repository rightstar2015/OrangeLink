//! Two to four pictures side by side, zoomed and moved together: picking
//! the best of a series of shots or thumbnails.

use super::*;
use std::collections::HashMap;

pub struct Compare {
    pub ids: Vec<String>,
    /// Full pictures as they arrive from the loader.
    pub tex: HashMap<String, egui::TextureHandle>,
    /// Times the fitted size, the same for every pane.
    pub zoom: f32,
    /// How far every picture is moved from its pane's centre.
    pub pan: egui::Vec2,
}

impl Compare {
    pub fn new(ids: Vec<String>) -> Self {
        Self { ids, tex: HashMap::new(), zoom: 1.0, pan: egui::Vec2::ZERO }
    }
}

/// Columns and rows for `n` pictures: side by side up to three, 2×2 for four.
pub(super) fn pane_grid(n: usize) -> (usize, usize) {
    match n {
        0 | 1 => (1, 1),
        2 | 3 => (n, 1),
        _ => (2, n.div_ceil(2)),
    }
}

/// The pictures that can be compared: 2 to 4 images, else none.
pub(crate) fn comparable(lib: &crate::library::Library, ids: &[String]) -> bool {
    (2..=4).contains(&ids.len()) && ids.iter().all(|id| lib.items.get(id).is_some_and(|it| it.kind == MediaKind::Image && !it.deleted))
}

impl App {
    pub fn compare_ui(&mut self, ui: &mut Ui) {
        let thumbs: Vec<_> = self.compare.as_ref().map_or(Vec::new(), |c| c.ids.iter().map(|id| self.thumb_tex(id)).collect());
        let Some(cmp) = &mut self.compare else { return };
        let Some(lib) = &self.lib else { return };
        let mut close = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new(t!("並排比較（{n} 張）", n = cmp.ids.len())).strong());
            ui.label(RichText::new(t!("滾輪縮放、拖曳移動，所有圖片一起動；點兩下回到符合大小")).small().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(t!("關閉 (Esc)")).clicked() {
                    close = true;
                }
                ui.label(RichText::new(format!("{:.0}%", cmp.zoom * 100.0)).small().monospace());
            });
        });
        ui.separator();
        let (cols, rows) = pane_grid(cmp.ids.len());
        let area = ui.available_rect_before_wrap();
        let (_, resp) = ui.allocate_exact_size(area.size(), Sense::click_and_drag());
        let pane_size = vec2(area.width() / cols as f32, area.height() / rows as f32);
        if resp.hovered() {
            let (scroll, pinch, pointer) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta(), i.pointer.hover_pos()));
            let factor = pinch * (scroll * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4 {
                let zoom = (cmp.zoom * factor).clamp(0.1, 40.0);
                // Keep the point under the pointer still, in its own pane.
                if let Some(p) = pointer {
                    let (c, r) = (((p.x - area.left()) / pane_size.x) as usize, ((p.y - area.top()) / pane_size.y) as usize);
                    let center = area.min + vec2((c.min(cols - 1) as f32 + 0.5) * pane_size.x, (r.min(rows - 1) as f32 + 0.5) * pane_size.y);
                    let real = zoom / cmp.zoom;
                    let at = center + cmp.pan;
                    cmp.pan = (p - (p - at) * real) - center;
                }
                cmp.zoom = zoom;
            }
        }
        if resp.dragged() {
            cmp.pan += resp.drag_delta();
        }
        if resp.double_clicked() {
            cmp.zoom = 1.0;
            cmp.pan = egui::Vec2::ZERO;
        }
        for (i, id) in cmp.ids.iter().enumerate() {
            let (c, r) = (i % cols, i / cols);
            let pane = Rect::from_min_size(area.min + vec2(c as f32 * pane_size.x, r as f32 * pane_size.y), pane_size).shrink(4.0);
            let painter = ui.painter_at(pane);
            painter.rect_filled(pane, 4.0, Color32::from_gray(18));
            let name = lib.items.get(id).map(|it| (it.name.clone(), it.width, it.height));
            let shown = cmp.tex.get(id).map(|t| (t.id(), t.size_vec2())).or(thumbs.get(i).copied().flatten());
            match shown {
                Some((tex, size)) => {
                    let size = match name {
                        Some((_, w, h)) if w > 0 && h > 0 => vec2(w as f32, h as f32),
                        _ => size,
                    };
                    let fit = (pane.width() / size.x).min(pane.height() / size.y).min(1.0);
                    let draw = Rect::from_center_size(pane.center() + cmp.pan, size * fit * cmp.zoom);
                    painter.image(tex, draw, UV, Color32::WHITE);
                }
                None => {
                    painter.text(pane.center(), Align2::CENTER_CENTER, t!("載入中…"), FontId::proportional(14.0), Color32::GRAY);
                }
            }
            if let Some((name, w, h)) = name {
                let label = if w > 0 { format!("{name}  ·  {w} × {h}") } else { name };
                let g = painter.layout_no_wrap(label, FontId::proportional(13.0), Color32::WHITE);
                let r = Rect::from_min_size(pane.left_top() + vec2(8.0, 8.0), g.size() + vec2(12.0, 6.0));
                painter.rect_filled(r, 4.0, Color32::from_black_alpha(170));
                painter.galley(r.min + vec2(6.0, 3.0), g, Color32::WHITE);
            }
        }
        if close {
            self.compare = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pane_grid;

    #[test]
    fn compared_pictures_sit_side_by_side_or_two_by_two() {
        assert_eq!(pane_grid(2), (2, 1));
        assert_eq!(pane_grid(3), (3, 1));
        assert_eq!(pane_grid(4), (2, 2));
        assert_eq!(pane_grid(1), (1, 1));
    }
}
