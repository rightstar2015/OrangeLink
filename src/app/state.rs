//! Explicit state machines for state that used to be spread over several
//! `Option` / `bool` fields of `App`, where some combinations of those fields
//! meant nothing (an error *and* a running server, a texture for one item and a
//! failure flag for another).
//!
//! The types here have no egui or Windows dependency, so their transitions are
//! unit-tested on their own; `App` owns them and does the I/O around them.
//! STATE_MACHINES.md in the repository root draws every state machine in the app,
//! including the ones that stay as they are.

use std::sync::mpsc::{Receiver, TryRecvError};

/// What a background thread's channel holds right now.
///
/// Every background job (opening a library, exporting a clip, a download…) is a
/// worker thread that answers on a channel. `rx.try_recv().ok()` cannot tell
/// "still working" from "the thread died without answering" (a panic), and the
/// second left the job showing as running forever, holding up its queue.
#[derive(Debug, PartialEq)]
pub enum Reply<T> {
    /// Nothing yet; still working.
    Pending,
    Done(T),
    /// The thread ended without answering: the job is over, and failed.
    Lost,
}

pub fn poll<T>(rx: &Receiver<T>) -> Reply<T> {
    match rx.try_recv() {
        Ok(v) => Reply::Done(v),
        Err(TryRecvError::Empty) => Reply::Pending,
        Err(TryRecvError::Disconnected) => Reply::Lost,
    }
}

impl<T> Reply<T> {
    /// None while still working; otherwise the answer, or `LOST` when the
    /// thread ended without one.
    pub fn finished(self) -> Option<Result<T, &'static str>> {
        match self {
            Reply::Pending => None,
            Reply::Done(v) => Some(Ok(v)),
            Reply::Lost => Some(Err(LOST)),
        }
    }
}

/// Polls a one-answer job's channel and empties `slot` once the job is over,
/// whether it answered or died. Pending when there is no job.
pub fn take<T>(slot: &mut Option<Receiver<T>>) -> Reply<T> {
    let Some(rx) = slot else { return Reply::Pending };
    let reply = poll(rx);
    if !matches!(reply, Reply::Pending) {
        *slot = None;
    }
    reply
}

/// Whether the thread answering on `rx` has ended (it dropped its sender);
/// whatever it sent in between is read and dropped.
pub fn ended<T>(rx: &Receiver<T>) -> bool {
    loop {
        match rx.try_recv() {
            Ok(_) => continue,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

/// Said when a worker thread ended without answering. The panic itself is in the log.
pub const LOST: &str = "背景工作意外結束（詳見記錄）";

/// A local service the settings switch on and off: the MCP server, the browser
/// extension receiver, the global shortcut.
///
/// ```text
///            sync(Some(k)), start ok
///   Off ───────────────────────────────▶ Running(T)
///    │ ▲                                    │
///    │ └──────── sync(None) ────────────────┤
///    │            sync(Some(other k)): the old one is dropped first, then started
///    └──── sync(Some(k)), start fails ──▶ Failed(reason) ── sync(..) retries ──▶ …
/// ```
#[derive(Default)]
pub enum Service<T> {
    /// Turned off in the settings.
    #[default]
    Off,
    Running(T),
    /// Wanted, but could not start (port taken, shortcut already registered…).
    Failed(String),
}

impl<T> Service<T> {
    pub fn running(&self) -> Option<&T> {
        match self {
            Service::Running(t) => Some(t),
            _ => None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Service::Running(_))
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Service::Failed(e) => Some(e),
            _ => None,
        }
    }

    /// Brings the service in line with `want` (`None`: it should be off).
    ///
    /// A running service whose `key` already equals `want` is left alone.
    /// Otherwise the old one is dropped *before* `start` runs, so a server frees
    /// its port and a shortcut unregisters before the new one takes them. A failed
    /// service is always tried again. Returns whether anything changed.
    pub fn sync<K: PartialEq>(&mut self, want: Option<K>, key: impl Fn(&T) -> K, start: impl FnOnce(K) -> Result<T, String>) -> bool {
        match (&*self, &want) {
            (Service::Running(t), Some(w)) if key(t) == *w => return false,
            (Service::Off, None) => return false,
            _ => {}
        }
        *self = Service::Off;
        if let Some(k) = want {
            *self = match start(k) {
                Ok(t) => Service::Running(t),
                Err(e) => Service::Failed(e),
            };
        }
        true
    }
}

/// The preview's full-size image (or the frames of an animated GIF), decoded on
/// the loader thread. Generic over the texture so the tests need no GPU.
///
/// ```text
///   Idle ──request(id)──▶ Loading(id) ──decoded, still shown──▶ Ready { id, frames }
///    ▲                       │      └──decode failed, still shown──▶ Failed(id)
///    │                       └──decoded after the preview moved on──▶ Idle
///    └── close() (Ready / Failed; a load under way is kept, its result is used if
///        the preview comes back to the item first)
/// ```
#[derive(Default)]
pub enum FullImage<T> {
    #[default]
    Idle,
    Loading(String),
    Ready {
        id: String,
        /// One frame for a still image; every frame and its delay (seconds) for an animation.
        frames: Vec<(T, f32)>,
        animated: bool,
        /// When the animation started (UI clock, seconds).
        since: f64,
    },
    Failed(String),
}

impl<T> FullImage<T> {
    /// Whether `id` still has to be asked for: not loaded and not on its way.
    /// A failed item is asked for again.
    pub fn needs_request(&self, id: &str) -> bool {
        !matches!(self, FullImage::Loading(l) if l == id) && !matches!(self, FullImage::Ready { id: r, .. } if r == id)
    }

    pub fn start(&mut self, id: String) {
        *self = FullImage::Loading(id);
    }

    pub fn is_failed(&self, id: &str) -> bool {
        matches!(self, FullImage::Failed(f) if f == id)
    }

    /// A decode of `id` came back. `shown`: the preview still shows that item.
    /// `frames` is None when it could not be decoded; `animated` says the frames
    /// came from an animation.
    pub fn finish(&mut self, id: String, shown: bool, frames: Option<Vec<(T, f32)>>, animated: bool, now: f64) {
        if !shown {
            if matches!(self, FullImage::Loading(l) if *l == id) {
                *self = FullImage::Idle;
            }
            return;
        }
        *self = match frames {
            Some(frames) if !frames.is_empty() => FullImage::Ready { id, frames, animated, since: now },
            _ => FullImage::Failed(id),
        };
    }

    /// The preview closed: let go of the textures. A decode under way is kept.
    pub fn close(&mut self) {
        if !matches!(self, FullImage::Loading(_)) {
            *self = FullImage::Idle;
        }
    }

    pub fn clear(&mut self) {
        *self = FullImage::Idle;
    }

    /// The texture to draw for `id` at time `now`, and for an animation the
    /// seconds until the next frame is due.
    pub fn frame_at(&self, id: &str, now: f64) -> Option<(&T, Option<f32>)> {
        let FullImage::Ready { id: rid, frames, animated, since } = self else { return None };
        if rid != id {
            return None;
        }
        if !*animated {
            return frames.first().map(|(t, _)| (t, None));
        }
        animation_frame(frames, now - since).map(|(t, next_in)| (t, Some(next_in)))
    }
}

/// The frame of a looping animation showing `elapsed` seconds in, and the seconds
/// until the next one. `frames` are (frame, how long it shows). Shared by the
/// preview and the hover preview so both play GIFs the same way.
pub fn animation_frame<T>(frames: &[(T, f32)], elapsed: f64) -> Option<(&T, f32)> {
    let first = &frames.first()?.0;
    let total: f32 = frames.iter().map(|(_, d)| d).sum();
    let mut t = (elapsed as f32) % total.max(0.01);
    for (frame, delay) in frames {
        if t < *delay {
            return Some((frame, delay - t));
        }
        t -= delay;
    }
    // Rounding at the very end of the loop: the first frame, and look again soon.
    Some((first, 0.0))
}

/// A name being edited in place: an item's (F2 on the grid, or a click on the
/// inspector's name) or a clip's (in the preview's clip list).
///
/// ```text
///   None ──start──▶ Some { focused: false } ──first frame: focus + select all──▶ Some { focused: true }
///                                             ──focus lost──▶ commit, or cancel with Esc ──▶ None
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct InlineEdit {
    /// The item or clip being renamed.
    pub id: String,
    pub draft: String,
    /// The text box has been given the focus (done once, on its first frame).
    pub focused: bool,
    /// Item names only: the box is the inspector's, not the grid cell's.
    pub in_inspector: bool,
}

impl InlineEdit {
    pub fn new(id: impl Into<String>, draft: impl Into<String>, in_inspector: bool) -> Self {
        Self { id: id.into(), draft: draft.into(), focused: false, in_inspector }
    }

    /// The box for `id` at this place.
    pub fn is(&self, id: &str, in_inspector: bool) -> bool {
        self.id == id && self.in_inspector == in_inspector
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start_ok(port: u16) -> Result<u16, String> {
        Ok(port)
    }

    #[test]
    fn a_job_whose_thread_died_is_over_and_failed() {
        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        let mut slot = Some(rx);
        assert!(take(&mut slot).finished().is_none(), "still working");
        assert!(slot.is_some());
        drop(tx); // the thread panicked before answering
        assert_eq!(take(&mut slot).finished(), Some(Err(LOST)));
        assert!(slot.is_none(), "no longer shown as running");

        let (tx, rx) = std::sync::mpsc::channel::<u32>();
        let mut slot = Some(rx);
        tx.send(7).unwrap();
        assert_eq!(take(&mut slot).finished(), Some(Ok(7)));
        assert!(slot.is_none());
        assert!(take(&mut slot).finished().is_none(), "no job");
    }

    #[test]
    fn a_service_restarts_only_when_its_settings_change() {
        let mut s: Service<u16> = Service::Off;
        assert!(!s.sync(None, |p| *p, start_ok), "off and unwanted: nothing to do");
        assert!(s.sync(Some(47821), |p| *p, start_ok));
        assert_eq!(s.running(), Some(&47821));
        let mut started = false;
        assert!(!s.sync(Some(47821), |p| *p, |p| {
            started = true;
            Ok(p)
        }));
        assert!(!started, "same settings: the running service is kept");
        assert!(s.sync(Some(47822), |p| *p, start_ok));
        assert_eq!(s.running(), Some(&47822));
        assert!(s.sync(None, |p| *p, start_ok));
        assert!(matches!(s, Service::Off));
    }

    #[test]
    fn a_failed_service_shows_why_and_is_tried_again() {
        let mut s: Service<u16> = Service::Off;
        s.sync(Some(80), |p| *p, |_| Err("連接埠被佔用".to_string()));
        assert_eq!(s.error(), Some("連接埠被佔用"));
        assert!(!s.is_running());
        // Same settings again (the 重試 button): it is started again, not skipped.
        assert!(s.sync(Some(80), |p| *p, start_ok));
        assert!(s.is_running() && s.error().is_none());
        // Turning a failed service off clears the error.
        s.sync(Some(81), |p| *p, |_| Err("x".into()));
        assert!(s.sync(None, |p| *p, start_ok));
        assert!(s.error().is_none() && !s.is_running());
    }

    #[test]
    fn the_old_service_is_dropped_before_the_new_one_starts() {
        use std::{cell::Cell, rc::Rc};
        struct Port(Rc<Cell<bool>>);
        impl Drop for Port {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let busy = Rc::new(Cell::new(false));
        let bind = |b: &Rc<Cell<bool>>| {
            let b = b.clone();
            move |_: u16| {
                if b.get() {
                    return Err("busy".to_string());
                }
                b.set(true);
                Ok((Port(b), 0u16))
            }
        };
        let mut s: Service<(Port, u16)> = Service::Off;
        s.sync(Some(1u16), |_| 0u16, bind(&busy));
        assert!(s.is_running());
        // A different key: the old port must be free when the new one binds.
        s.sync(Some(2u16), |_| 0u16, bind(&busy));
        assert!(s.is_running(), "{:?}", s.error());
    }

    #[test]
    fn a_full_image_is_requested_once_and_again_after_a_failure() {
        let mut f: FullImage<u8> = FullImage::Idle;
        assert!(f.needs_request("a"));
        f.start("a".into());
        assert!(!f.needs_request("a"));
        assert!(f.needs_request("b"));
        f.finish("a".into(), true, None, false, 0.0);
        assert!(f.is_failed("a") && !f.is_failed("b"));
        assert!(f.needs_request("a"), "a failed image can be tried again");
        f.start("a".into());
        f.finish("a".into(), true, Some(vec![(7, 0.0)]), false, 0.0);
        assert!(!f.needs_request("a"));
        assert_eq!(f.frame_at("a", 5.0), Some((&7, None)));
        assert_eq!(f.frame_at("b", 5.0), None, "never another item's picture");
    }

    #[test]
    fn a_late_decode_is_dropped_but_does_not_cancel_the_current_one() {
        let mut f: FullImage<u8> = FullImage::Idle;
        f.start("a".into());
        f.start("b".into());
        // a comes back after the preview moved to b.
        f.finish("a".into(), false, Some(vec![(1, 0.0)]), false, 0.0);
        assert!(!f.needs_request("b"), "b is still loading");
        f.finish("b".into(), false, Some(vec![(2, 0.0)]), false, 0.0);
        assert!(matches!(f, FullImage::Idle));
        assert!(f.frame_at("b", 0.0).is_none());
    }

    #[test]
    fn closing_keeps_a_load_under_way_and_frees_the_rest() {
        let mut f: FullImage<u8> = FullImage::Idle;
        f.start("a".into());
        f.close();
        assert!(!f.needs_request("a"));
        f.finish("a".into(), true, Some(vec![(1, 0.0)]), false, 0.0);
        f.close();
        assert!(matches!(f, FullImage::Idle));
        f.finish("b".into(), true, None, false, 0.0);
        f.close();
        assert!(!f.is_failed("b"));
    }

    #[test]
    fn an_animation_shows_the_frame_for_the_time_and_loops() {
        let mut f: FullImage<u8> = FullImage::Idle;
        f.start("g".into());
        f.finish("g".into(), true, Some(vec![(0, 0.1), (1, 0.2), (2, 0.1)]), true, 10.0);
        let at = |t: f64| f.frame_at("g", t).map(|(x, n)| (*x, n.map(|n| (n * 100.0).round() / 100.0)));
        assert_eq!(at(10.0), Some((0, Some(0.1))));
        assert_eq!(at(10.15), Some((1, Some(0.15))));
        assert_eq!(at(10.35), Some((2, Some(0.05))));
        assert_eq!(at(10.45), Some((0, Some(0.05))), "loops after 0.4 s");
    }

    #[test]
    fn an_animation_never_runs_out_of_frames() {
        let none: [(u8, f32); 0] = [];
        assert_eq!(animation_frame(&none, 1.0), None);
        // All delays 0 (broken GIF): still a frame, and asked again soon.
        assert_eq!(animation_frame(&[(4u8, 0.0)], 3.0), Some((&4, 0.0)));
        assert_eq!(animation_frame(&[(1u8, 0.5), (2, 0.5)], 7.75).map(|(f, _)| *f), Some(2));
    }

    #[test]
    fn a_worker_that_dies_is_told_apart_from_one_still_working() {
        let (tx, rx) = std::sync::mpsc::channel::<u8>();
        assert_eq!(poll(&rx), Reply::Pending);
        tx.send(3).unwrap();
        assert_eq!(poll(&rx), Reply::Done(3));
        let worker = std::thread::spawn(move || {
            let _keep = tx;
            panic!("decoder bug");
        });
        assert!(worker.join().is_err());
        assert_eq!(poll(&rx), Reply::Lost);
    }

    #[test]
    fn an_inline_edit_belongs_to_one_place() {
        let e = InlineEdit::new("x", "名稱", true);
        assert!(e.is("x", true));
        assert!(!e.is("x", false) && !e.is("y", true));
        assert!(!e.focused);
    }
}
