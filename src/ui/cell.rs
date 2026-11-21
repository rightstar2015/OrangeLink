//! What is drawn on top of a thumbnail, the same in the grid and the justified
//! layout: dark pills for status (top left), length (top right), format (bottom
//! left) and rating (bottom right), the hover and selection outlines, the missing
//! file state, and the tooltip with the full name.

use super::*;

/// Pointer still this long over a cell before its tooltip shows.
const TOOLTIP_SECS: f32 = 0.6;
/// Tags listed in the tooltip.
const TOOLTIP_TAGS: usize = 5;
const PILL_FONT: f32 = 11.0;

/// `text` cut to fit `max_w` with "…", as measured by `width`; unchanged if it fits.
pub fn elide(text: &str, max_w: f32, width: impl Fn(&str) -> f32) -> String {
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    // The longest prefix that still fits with the ellipsis.
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let s: String = chars[..mid].iter().chain(std::iter::once(&'…')).collect();
        if width(&s) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo]
        .iter()
        .chain(std::iter::once(&'…'))
        .collect::<String>()
        .trim_start()
        .to_string()
}

/// A rounded dark pill holding `text`, placed by `align` against `anchor`.
fn pill(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    align: Align2,
    text: impl ToString,
    color: Color32,
) -> Rect {
    let galley = painter.layout_no_wrap(text.to_string(), FontId::proportional(PILL_FONT), color);
    let size = galley.size() + vec2(10.0, 4.0);
    let rect = align.anchor_size(anchor, size);
    painter.rect_filled(rect, size.y / 2.0, PILL_BG);
    painter.galley(rect.min + vec2(5.0, 2.0), galley, color);
    rect
}

/// Height of a group's header row in the grid and justified views.
pub(super) const GROUP_HEADER_H: f32 = 30.0;

/// A group's title, its item count and a rule, across `rect`.
pub(super) fn paint_group_header(ui: &Ui, rect: Rect, group: &crate::grouping::Group) {
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let v = ui.visuals();
    let y = rect.center().y + 2.0;
    let title = painter.text(pos2(rect.min.x + 2.0, y), Align2::LEFT_CENTER, &*group.label, FontId::proportional(14.0), v.strong_text_color());
    let count = painter.text(pos2(title.max.x + 8.0, y), Align2::LEFT_CENTER, group.len.to_string(), FontId::proportional(12.0), v.weak_text_color());
    let line_y = y + 0.5;
    painter.hline((count.max.x + 10.0)..=rect.max.x - 8.0, line_y, Stroke::new(1.0_f32, v.widgets.noninteractive.bg_stroke.color));
}

/// The name in the label row, cut with "…" to the cell's width.
pub(super) fn paint_name(ui: &Ui, rect: Rect, name: &str, size: f32, color: Color32) {
    let font = FontId::proportional(size);
    let shown = ui.fonts(|f| {
        elide(name, rect.width(), |s| {
            f.layout_no_wrap(s.to_string(), font.clone(), color)
                .size()
                .x
        })
    });
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .text(rect.min, Align2::LEFT_TOP, shown, font, color);
}

impl App {
    /// The name under a thumbnail, or the box editing it after F2: Enter or a click
    /// elsewhere keeps the new name, Esc keeps the old one.
    pub(super) fn item_name(&self, ui: &mut Ui, rect: Rect, it: &crate::model::Item, size: f32, color: Color32) {
        self.name_field(ui, rect, &it.id, &it.name, size, color, false);
    }

    /// The name of item `item_id`, or the box editing it when its rename is under
    /// way here (`in_inspector` tells the grid's place from the inspector's).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn name_field(&self, ui: &mut Ui, rect: Rect, item_id: &str, name: &str, size: f32, color: Color32, in_inspector: bool) {
        let mut renaming = self.renaming.borrow_mut();
        let Some(InlineEdit { id, draft, focused, .. }) = renaming.as_mut().filter(|e| e.is(item_id, in_inspector)) else {
            drop(renaming);
            return paint_name(ui, rect, name, size, color);
        };
        let at = Rect::from_min_size(rect.min - vec2(2.0, 2.0), vec2(rect.width() + 4.0, size + 8.0));
        let edit = egui::TextEdit::singleline(draft).font(FontId::proportional(size)).margin(vec2(2.0, 2.0));
        let r = ui.put(at, edit);
        if !*focused {
            r.request_focus();
            // Select the whole name, as Explorer does.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                let all = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(draft.chars().count()));
                state.cursor.set_char_range(Some(all));
                state.store(ui.ctx(), r.id);
            }
            *focused = true;
        } else if r.lost_focus() {
            // Esc cancels the rename only (not the selection or the preview too).
            let cancel = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
            if !cancel {
                self.act(Action::RenameItem(id.clone(), draft.clone()));
            }
            *renaming = None;
        }
    }

    /// Everything on top of a thumbnail in `img`. `previewing` is true while the hover
    /// preview plays in it.
    pub(super) fn paint_cell_overlay(
        &self,
        ui: &mut Ui,
        img: Rect,
        rounding: f32,
        it: &crate::model::Item,
        resp: &egui::Response,
        previewing: bool,
    ) {
        let selected = self.selected.contains(&it.id);
        let missing = self
            .lib
            .as_ref()
            .is_some_and(|l| l.missing.contains(&it.id));
        let hovered = resp.hovered() || resp.contains_pointer();
        let hover_fg = ui.visuals().widgets.hovered.fg_stroke.color;
        let painter = ui.painter().clone();
        let pad = 6.0;
        // Small cells (narrow images in the justified layout) keep only what matters.
        let roomy = img.width() >= 90.0 && img.height() >= 60.0;

        if missing {
            painter.rect_filled(img, rounding, Color32::from_black_alpha(150));
        } else if it.kind == MediaKind::Video
            && hovered
            && !previewing
            && !self.config.hover_preview
        {
            let r = Rect::from_center_size(img.center(), vec2(36.0, 36.0));
            painter.circle_filled(r.center(), 18.0, Color32::from_black_alpha(150));
            painter.text(
                r.center() + vec2(2.0, 0.0),
                Align2::CENTER_CENTER,
                "▶",
                FontId::proportional(18.0),
                Color32::WHITE,
            );
        }

        let status = if missing {
            Some(("⚠", ERROR_ON_DARK))
        } else if it.source.is_some() {
            Some(("🔗", Color32::WHITE))
        } else {
            None
        };
        if let Some((glyph, color)) = status {
            pill(
                &painter,
                img.left_top() + vec2(pad, pad),
                Align2::LEFT_TOP,
                glyph,
                color,
            );
        }
        if let Some(d) = it.duration {
            pill(
                &painter,
                img.right_top() + vec2(-pad, pad),
                Align2::RIGHT_TOP,
                fmt_duration(d),
                Color32::WHITE,
            );
        }
        if roomy {
            pill(
                &painter,
                img.left_bottom() + vec2(pad, -pad),
                Align2::LEFT_BOTTOM,
                it.ext.to_uppercase(),
                Color32::WHITE,
            );
            if it.rating > 0 {
                pill(
                    &painter,
                    img.right_bottom() + vec2(-pad, -pad),
                    Align2::RIGHT_BOTTOM,
                    "★".repeat(it.rating as usize),
                    STAR_ON_DARK,
                );
            }
        }

        if hovered && !missing && img.width() >= 130.0 && img.height() >= 90.0 && !egui::DragAndDrop::has_any_payload(ui.ctx()) {
            let label = match it.kind {
                MediaKind::Video => "✂ 剪輯",
                _ => "👁 預覽",
            };
            let btn = egui::Button::new(RichText::new(label).small().color(Color32::WHITE))
                .fill(Color32::from_black_alpha(185))
                .stroke(Stroke::new(1.0_f32, Color32::from_white_alpha(60)));
            let at = Rect::from_center_size(pos2(img.center().x, img.bottom() - 16.0), vec2(58.0, 20.0));
            if ui.put(at, btn).on_hover_text("開啟預覽／剪輯工作台").clicked() {
                self.act(Action::OpenViewer(it.id.clone()));
            }
        }

        if missing {
            let label = pill(
                &painter,
                img.center(),
                Align2::CENTER_BOTTOM,
                "⚠ 找不到檔案",
                ERROR_ON_DARK,
            );
            if roomy && img.height() >= 90.0 {
                let button = egui::Button::new(RichText::new("重新指定位置…").small());
                let at = Rect::from_center_size(
                    pos2(img.center().x, label.max.y + 14.0),
                    vec2(img.width().min(120.0), 20.0),
                );
                if ui.put(at, button).clicked() {
                    self.act(Action::RelinkItem(it.id.clone()));
                }
            }
        }

        if selected {
            painter.rect_stroke(img, rounding, Stroke::new(3.0_f32, accent()));
        } else if hovered {
            painter.rect_stroke(
                img,
                rounding,
                Stroke::new(1.5_f32, hover_fg.gamma_multiply(0.45)),
            );
        }
        self.cell_tooltip(ui, resp, it);
    }

    /// Full name, size, length and tags, once the pointer rests on the cell.
    fn cell_tooltip(&self, ui: &Ui, resp: &egui::Response, it: &crate::model::Item) {
        if !resp.hovered()
            || ui.ctx().dragged_id().is_some()
            || egui::DragAndDrop::has_any_payload(ui.ctx())
            || self.renaming.borrow().as_ref().is_some_and(|e| e.is(&it.id, false))
        {
            return;
        }
        let still = ui.input(|i| i.pointer.time_since_last_movement());
        if still < TOOLTIP_SECS {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f32(
                    TOOLTIP_SECS - still + 0.02,
                ));
            return;
        }
        egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), resp.id.with("tip"), |ui| {
            ui.set_max_width(320.0);
            ui.label(RichText::new(&it.name).strong());
            let mut facts = Vec::new();
            if it.width > 0 {
                facts.push(format!("{} × {}", it.width, it.height));
            }
            facts.push(human_size(it.size));
            if let Some(d) = it.duration {
                facts.push(fmt_duration(d));
            }
            ui.label(RichText::new(facts.join("  ·  ")).weak());
            if !it.tags.is_empty() {
                let mut tags = it
                    .tags
                    .iter()
                    .take(TOOLTIP_TAGS)
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                if it.tags.len() > TOOLTIP_TAGS {
                    tags += &format!("  +{}", it.tags.len() - TOOLTIP_TAGS);
                }
                ui.label(tags);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::elide;

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        // One unit per character.
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(elide("short", 10.0, w), "short");
        assert_eq!(elide("a long file name", 8.0, w), "a long …");
        assert_eq!(elide("素材名稱很長很長", 5.0, w), "素材名稱…");
        assert_eq!(elide("abc", 0.5, w), "…");
    }
}
