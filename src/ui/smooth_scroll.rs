//! Smooth wheel scrolling in the grid, justified and list views.
//!
//! A mouse wheel moves the content in fixed steps, which reads as jumps. Here
//! each notch moves a target instead and the view glides to it with an
//! exponential ease, so a burst of notches is one movement. How long the glide
//! takes is `config.scroll_smoothness`; at 0 the wheel is left to egui as
//! before. A touchpad already reports small pixel steps and is never smoothed.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/explorer/smooth_scroll.rs`),
//! MIT License, Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use super::*;

/// The glide under way.
pub struct Glide {
    target: f32,
    /// The offset the last frame set: anything else moving the view (the
    /// scrollbar, a jump to an item, zooming) ends the glide rather than fight it.
    last_set: f32,
    view: ViewMode,
}

/// Time constant of the glide, in seconds, for a smoothness of 0..=100: up to
/// 0.16 s, past which the view feels like it lags the wheel.
fn time_constant(smoothness: u32) -> f32 {
    smoothness.min(100) as f32 * 0.0016
}

/// How far a glide from `current` to `target` gets in `dt` seconds.
fn advance(current: f32, target: f32, dt: f32, smoothness: u32) -> f32 {
    let t = 1.0 - (-dt / time_constant(smoothness).max(0.001)).exp();
    current + (target - current) * t
}

impl App {
    /// Before egui sees the input: takes the mouse wheel over the thumbnails out
    /// of it and keeps it for `glide`. Leaves touchpads, Ctrl (zoom) and Shift
    /// (sideways) alone, and everything when smoothing is off.
    pub fn take_wheel(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let Some((area, layer)) = self.wheel_area.take() else {
            // No view to glide in: what a view left behind is stale.
            self.wheel_pending = 0.0;
            return;
        };
        if self.config.scroll_smoothness == 0 {
            return;
        }
        let moved = raw.events.iter().rev().find_map(|e| match e {
            egui::Event::PointerMoved(p) => Some(*p),
            _ => None,
        });
        let Some(pos) = moved.or_else(|| ctx.input(|i| i.pointer.latest_pos())) else { return };
        if !area.contains(pos) || ctx.layer_id_at(pos) != Some(layer) {
            return;
        }
        let line = ctx.options(|o| o.line_scroll_speed);
        raw.events.retain(|e| match e {
            // A sideways tilt of the wheel (no vertical part) is left to egui.
            egui::Event::MouseWheel { unit, delta, modifiers } if !(modifiers.ctrl || modifiers.command || modifiers.shift) && delta.y != 0.0 => {
                let px = match unit {
                    egui::MouseWheelUnit::Point => return true,
                    egui::MouseWheelUnit::Line => delta.y * line,
                    egui::MouseWheelUnit::Page => delta.y * area.height(),
                };
                self.wheel_pending += px;
                false
            }
            _ => true,
        });
    }

    /// The offset for this frame of the view's scroll area, while a glide is
    /// under way. `content_h` is the height of everything in it. Call before the
    /// other offsets (a jump to an item, the selection box) so they win.
    pub(super) fn glide(&mut self, ui: &Ui, content_h: f32) -> Option<f32> {
        // Where the wheel is taken from next frame.
        self.wheel_area = Some((ui.available_rect_before_wrap(), ui.layer_id()));
        let view = self.config.view_mode;
        let current = self.grid_scroll;
        let wheel = std::mem::take(&mut self.wheel_pending);
        let max = (content_h - self.grid_view_h).max(0.0);
        let glide = self.glide.take().filter(|g| g.view == view && (g.last_set - current).abs() < 0.5);
        let from = glide.as_ref().map_or(current, |g| g.target);
        let target = if wheel != 0.0 { (from - wheel).clamp(0.0, max) } else { glide?.target };
        if (target - current).abs() < 0.5 {
            return (target != current).then_some(target);
        }
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let next = advance(current, target, dt, self.config.scroll_smoothness);
        self.glide = Some(Glide { target, last_set: next, view });
        ui.ctx().request_repaint();
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::{advance, time_constant};

    #[test]
    fn a_sideways_wheel_tilt_is_left_for_egui() {
        use eframe::egui::{self, pos2, vec2};
        let ctx = egui::Context::default();
        let id = egui::Id::new("wheel-test");
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::Area::new(id).fixed_pos(pos2(0.0, 0.0)).show(ctx, |ui| ui.allocate_space(vec2(400.0, 300.0)));
        });
        let config = crate::config::Config { scroll_smoothness: 50, ..Default::default() };
        let mut app = crate::app::App::with_config(&ctx, config);
        let wheel = |delta| egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta, modifiers: egui::Modifiers::NONE };
        let mut take = |delta| {
            app.wheel_area = Some((egui::Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0)), egui::LayerId::new(egui::Order::Middle, id)));
            let mut raw = egui::RawInput { events: vec![egui::Event::PointerMoved(pos2(100.0, 100.0)), wheel(delta)], ..Default::default() };
            app.take_wheel(&ctx, &mut raw);
            raw.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. }))
        };
        assert!(!take(vec2(0.0, -1.0)), "an ordinary notch is smoothed");
        assert!(take(vec2(1.0, 0.0)), "the sideways tilt was swallowed");
    }

    #[test]
    fn smoothness_sets_how_long_the_glide_takes() {
        assert_eq!(time_constant(0), 0.0);
        assert!((time_constant(50) - 0.08).abs() < 1e-6);
        assert!((time_constant(500) - 0.16).abs() < 1e-6);
    }

    #[test]
    fn the_glide_eases_towards_the_target() {
        let a = advance(0.0, 100.0, 1.0 / 60.0, 50);
        assert!(a > 0.0 && a < 100.0);
        // A longer frame gets further; it never overshoots.
        let b = advance(0.0, 100.0, 0.1, 50);
        assert!(b > a && b < 100.0);
        // Smoother means slower.
        assert!(advance(0.0, 100.0, 1.0 / 60.0, 100) < a);
    }
}
