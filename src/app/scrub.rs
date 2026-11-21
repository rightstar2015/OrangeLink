//! Small frames shown while hovering the progress bar. Frames are grabbed by FFmpeg one
//! at a time on a background thread, at fixed steps, and kept for the item on screen.

use super::{state, App, Reply};
use eframe::egui;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc::{channel, Receiver},
};

/// Frames kept per item; the earliest grabbed go first.
const MAX_FRAMES: usize = 400;
const WIDTH: u32 = 200;

#[derive(Default)]
pub struct Scrub {
    id: String,
    frames: HashMap<u32, egui::TextureHandle>,
    failed: HashSet<u32>,
    order: Vec<u32>,
    pending: Option<(u32, Receiver<Option<egui::ColorImage>>)>,
    /// The step under the pointer most recently, fetched once the current grab is done.
    wanted: Option<u32>,
}

impl Scrub {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Seconds between frames: about 300 steps over the video, never under 2 seconds.
pub fn step_secs(duration: f64) -> f64 {
    (duration / 300.0).max(2.0)
}

fn grab(ffmpeg_dir: &Path, input: &Path, t: f64) -> Option<egui::ColorImage> {
    let mut cmd = crate::process::hidden(ffmpeg_dir.join("ffmpeg"));
    let out = cmd
        .args(["-v", "error", "-ss", &format!("{t:.2}"), "-i"])
        .arg(input)
        .args(["-frames:v", "1", "-vf", &format!("scale={WIDTH}:-2"), "-f", "image2pipe", "-vcodec", "png", "-"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

impl App {
    /// The frame near `t` of the item in the preview, if grabbed already; asks for it if not.
    pub fn scrub_frame(&mut self, id: &str, t: f64, duration: f64) -> Option<(egui::TextureId, egui::Vec2)> {
        if self.scrub.id != id {
            self.scrub = Scrub { id: id.to_string(), ..Default::default() };
        }
        let step = (t.max(0.0) / step_secs(duration)) as u32;
        if let Some(tex) = self.scrub.frames.get(&step) {
            return Some((tex.id(), tex.size_vec2()));
        }
        if !self.scrub.failed.contains(&step) {
            self.scrub.wanted = Some(step);
            self.start_scrub_grab(duration);
        }
        // Meanwhile show the nearest frame grabbed so far.
        let near = self.scrub.frames.keys().min_by_key(|k| k.abs_diff(step))?;
        self.scrub.frames.get(near).map(|tex| (tex.id(), tex.size_vec2()))
    }

    fn start_scrub_grab(&mut self, duration: f64) {
        if self.scrub.pending.is_some() {
            return;
        }
        let Some(step) = self.scrub.wanted.take() else { return };
        let (Some(lib), Some(ff)) = (&self.lib, self.ffmpeg_dir.clone()) else { return };
        let Some(it) = lib.items.get(&self.scrub.id) else { return };
        let input: PathBuf = lib.file_path(it);
        let t = (step as f64 + 0.5) * step_secs(duration);
        let (tx, rx) = channel();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(grab(&ff, &input, t));
            ctx.request_repaint();
        });
        self.scrub.pending = Some((step, rx));
    }

    pub(super) fn poll_scrub(&mut self) {
        let Some((step, rx)) = &self.scrub.pending else { return };
        let frame = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(img) => img,
            Reply::Lost => None,
        };
        let step = *step;
        self.scrub.pending = None;
        if let Some(img) = frame {
            let tex = self.egui_ctx.load_texture(format!("scrub-{step}"), img, egui::TextureOptions::LINEAR);
            self.scrub.frames.insert(step, tex);
            self.scrub.order.push(step);
            if self.scrub.order.len() > MAX_FRAMES {
                let old = self.scrub.order.remove(0);
                self.scrub.frames.remove(&old);
            }
        } else {
            self.scrub.failed.insert(step);
        }
        // Without a player the length is unknown; the next hover asks again.
        let Some(duration) = self.player.as_ref().map(|p| p.duration) else { return };
        self.start_scrub_grab(duration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_scale_with_length() {
        assert_eq!(step_secs(60.0), 2.0);
        assert_eq!(step_secs(3.0 * 3600.0), 36.0);
    }

    #[test]
    fn grabs_a_small_frame() {
        let Some(ff) = crate::media::ffmpeg_for_test("grabs_a_small_frame") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-scrub-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("c.mp4");
        let ok = crate::process::hidden(ff.join("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=10", "-t", "3", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let img = grab(&ff, &clip, 1.5).unwrap();
        assert_eq!(img.size, [200, 112]);
        assert!(grab(&ff, &dir.join("missing.mp4"), 1.0).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
