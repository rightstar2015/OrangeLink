//! Widgets that are clicked in one place and dragged by part of it.
//!
//! egui (0.29) gives a click to nothing when the topmost widget under the pointer
//! senses only drags (`hit_test.rs`): a drag-only handle laid over a clickable
//! area takes every click on that part. So the handle senses clicks too, and its
//! clicks count as the area's.

use eframe::egui::{self, Rect, Response, Sense, Ui};

/// `body` (already interacted, sensing clicks) with a drag handle over `grab`:
/// the returned response is clicked, double-clicked, right-clicked or hovered
/// when either is. The handle's own response is returned for its drags.
pub fn with_handle(ui: &Ui, body: Response, grab: Rect, id: egui::Id) -> (Response, Response) {
    let handle = ui.interact(grab, id, Sense::click_and_drag());
    (body.union(handle.clone()), handle)
}

#[cfg(test)]
pub mod pointer {
    //! Drives a real egui `Context` with pointer events, for tests of what a
    //! click or a drag reaches.
    use eframe::egui::{self, Event, PointerButton, Pos2, RawInput, Rect, Ui};

    /// Runs `frame` for the frames of a click at `pos` (move, press, release) and
    /// returns what it reported on each.
    pub fn click<T>(pos: Pos2, mut frame: impl FnMut(&mut Ui) -> T) -> Vec<T> {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let button = |pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let steps = [vec![Event::PointerMoved(pos)], vec![button(true)], vec![button(false)], vec![]];
        let mut out = Vec::new();
        for (n, events) in steps.into_iter().enumerate() {
            let input = RawInput { screen_rect: Some(screen), events, time: Some(n as f64 * 0.05), ..Default::default() };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| out.push(frame(ui)));
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::pos2;

    const ROW: Rect = Rect { min: egui::Pos2 { x: 10.0, y: 10.0 }, max: egui::Pos2 { x: 400.0, y: 40.0 } };
    const GRAB: Rect = Rect { min: egui::Pos2 { x: 10.0, y: 10.0 }, max: egui::Pos2 { x: 150.0, y: 40.0 } };

    fn clicked_row(at: egui::Pos2, handle: impl Fn(&Ui, Response) -> Response) -> bool {
        pointer::click(at, |ui| {
            let row = ui.interact(ROW, egui::Id::new("row"), Sense::click());
            handle(ui, row).clicked()
        })
        .into_iter()
        .any(|c| c)
    }

    #[test]
    fn a_click_on_the_drag_handle_reaches_the_row_under_it() {
        let with = |ui: &Ui, row| with_handle(ui, row, GRAB, egui::Id::new("grab")).0;
        assert!(clicked_row(pos2(50.0, 25.0), with), "on the handle (the list's thumbnail and name)");
        assert!(clicked_row(pos2(300.0, 25.0), with), "beside it");
        // What the list did before: a drag-only handle on top takes the click.
        let drag_only = |ui: &Ui, row| {
            ui.interact(GRAB, egui::Id::new("grab"), Sense::drag());
            row
        };
        assert!(!clicked_row(pos2(50.0, 25.0), drag_only), "egui no longer drops these clicks: hits.rs can be simplified");
    }
}
