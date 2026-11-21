//! In-app video/audio playback. ffmpeg decodes (video frames as raw RGBA,
//! audio as 48 kHz stereo PCM) and rodio plays the sound. Pausing or seeking
//! restarts ffmpeg at the new position, which keeps the code simple.
//!
//! Nothing slow runs on the UI thread: the audio device is opened once and kept
//! open, and ffmpeg is started and stopped on background threads (opening the
//! device and spawning took ~0.4 s, a visible hitch on every hover preview).

use crate::config::PlayEngine;
use eframe::egui;
use std::{
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{sync_channel, Receiver},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::Instant,
};

pub const VIEWER_MAX_WIDTH: u32 = 1280;
pub const PREVIEW_MAX_WIDTH: u32 = 480;
const SAMPLE_RATE: u32 = 48_000;

fn ffmpeg(dir: &Path) -> Command {
    let mut cmd = crate::process::hidden(dir.join("ffmpeg"));
    cmd.stderr(Stdio::null());
    cmd
}

/// The default audio output, opened once on its own thread and kept for the app's
/// lifetime (`OutputStream` must stay alive and can't move between threads).
fn audio_output() -> Option<&'static rodio::OutputStreamHandle> {
    static OUTPUT: OnceLock<Option<rodio::OutputStreamHandle>> = OnceLock::new();
    OUTPUT
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || match rodio::OutputStream::try_default() {
                Ok((stream, handle)) => {
                    let _ = tx.send(Some(handle));
                    let _stream = stream;
                    loop {
                        thread::park();
                    }
                }
                Err(_) => {
                    let _ = tx.send(None);
                }
            });
            rx.recv().ok().flatten()
        })
        .as_ref()
}

/// Opens the audio output in the background so the first preview doesn't wait for it.
pub fn warm_up() {
    thread::spawn(|| {
        audio_output();
    });
}

/// ffmpeg processes of one play run. Stopping marks it cancelled so a process
/// that is still being spawned is killed as soon as it exists.
#[derive(Default)]
struct Run {
    cancelled: AtomicBool,
    children: Mutex<Vec<Child>>,
}

impl Run {
    /// Keeps `child` for stopping later, or kills it right away if already stopped.
    fn adopt(&self, mut child: Child) -> bool {
        let mut children = crate::lock(&self.children);
        if self.cancelled.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        children.push(child);
        true
    }

    fn stop(&self) {
        let children: Vec<Child> = {
            let mut c = crate::lock(&self.children);
            self.cancelled.store(true, Ordering::SeqCst);
            std::mem::take(&mut *c)
        };
        for mut c in children {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// ffmpeg's PCM output as a rodio source.
struct PcmSource(BufReader<ChildStdout>);

impl Iterator for PcmSource {
    type Item = i16;
    fn next(&mut self) -> Option<i16> {
        let mut b = [0u8; 2];
        self.0.read_exact(&mut b).ok()?;
        Some(i16::from_le_bytes(b))
    }
}

impl rodio::Source for PcmSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

/// The FFmpeg engine: ffmpeg processes decode, the clock is the wall clock.
struct FfmpegPlayer {
    path: PathBuf,
    ffmpeg_dir: PathBuf,
    duration: f64,
    fps: f64,
    /// Decoded frame size; None for audio-only.
    frame_size: Option<[usize; 2]>,
    frames: Option<Receiver<Vec<u8>>>,
    frames_shown: u64,
    /// Media position when playback (re)started.
    base: f64,
    started: Option<Instant>,
    run: Option<Arc<Run>>,
    audio: Option<Arc<rodio::Sink>>,
    volume: f32,
    /// ffmpeg failing to start, reported from the spawning thread.
    spawn_error: Option<Receiver<String>>,
    /// The frame at a position sought to while paused, being decoded; the newest
    /// request wins.
    still: Option<Receiver<Vec<u8>>>,
}

impl FfmpegPlayer {
    fn new(ffmpeg_dir: PathBuf, path: PathBuf, duration: f64, fps: f64, frame_size: Option<[usize; 2]>, volume: f32) -> Self {
        Self {
            path,
            ffmpeg_dir,
            duration,
            fps,
            frame_size,
            frames: None,
            frames_shown: 0,
            base: 0.0,
            started: None,
            run: None,
            audio: None,
            volume,
            spawn_error: None,
            still: None,
        }
    }

    /// Decodes the single frame at the paused position, so seeking or stepping
    /// while paused shows where it landed.
    fn show_still(&mut self) {
        let Some([w, h]) = self.frame_size else { return };
        let (tx, rx) = sync_channel(1);
        self.still = Some(rx);
        let mut cmd = ffmpeg(&self.ffmpeg_dir);
        cmd.args(["-ss", &format!("{:.3}", self.base), "-i"])
            .arg(&self.path)
            .args(["-an", "-frames:v", "1", "-vf", &format!("scale={w}:{h}"), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped());
        thread::spawn(move || {
            if let Ok(out) = cmd.output() {
                if out.stdout.len() == w * h * 4 {
                    let _ = tx.send(out.stdout);
                }
            }
        });
    }

    fn is_playing(&self) -> bool {
        self.started.is_some()
    }

    fn position(&self) -> f64 {
        let pos = self.base + self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
        // An unknown length (0) is no end: the clock would stay at 0 forever.
        if self.duration > 0.0 { pos.min(self.duration) } else { pos.max(0.0) }
    }

    fn stop_processes(&mut self) {
        if let Some(sink) = self.audio.take() {
            sink.stop();
        }
        self.frames = None;
        if let Some(run) = self.run.take() {
            thread::spawn(move || run.stop());
        }
    }

    fn play(&mut self) {
        if self.is_playing() {
            return;
        }
        if self.duration > 0.0 && self.base >= self.duration - 0.05 {
            self.base = 0.0; // replay from the start
        }
        self.stop_processes();
        self.still = None;
        let seek = format!("{:.3}", self.base);
        let run = Arc::new(Run::default());
        self.run = Some(run.clone());

        let video = self.frame_size.map(|[w, h]| {
            let mut cmd = ffmpeg(&self.ffmpeg_dir);
            cmd.args(["-ss", &seek, "-i"])
                .arg(&self.path)
                .args(["-an", "-vf", &format!("scale={w}:{h},fps={}", self.fps), "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .stdout(Stdio::piped());
            // Small buffer: ffmpeg blocks when we're ahead, so memory stays bounded.
            let (tx, rx) = sync_channel::<Vec<u8>>(6);
            self.frames = Some(rx);
            (cmd, tx, w * h * 4)
        });

        let sink = audio_output().and_then(|h| rodio::Sink::try_new(h).ok()).map(Arc::new);
        let audio = sink.as_ref().map(|sink| {
            sink.set_volume(self.volume);
            let mut cmd = ffmpeg(&self.ffmpeg_dir);
            cmd.args(["-ss", &seek, "-i"])
                .arg(&self.path)
                .args(["-vn", "-f", "s16le", "-ac", "2", "-ar", &SAMPLE_RATE.to_string(), "-"])
                .stdout(Stdio::piped());
            (cmd, sink.clone())
        });
        self.audio = sink;

        let (err_tx, err_rx) = sync_channel(1);
        self.spawn_error = Some(err_rx);
        thread::spawn(move || {
            if let Some((mut cmd, tx, frame_bytes)) = video {
                match cmd.spawn() {
                    Ok(mut child) => {
                        let mut out = child.stdout.take().expect("piped");
                        if run.adopt(child) {
                            thread::spawn(move || loop {
                                let mut buf = vec![0u8; frame_bytes];
                                if out.read_exact(&mut buf).is_err() || tx.send(buf).is_err() {
                                    break;
                                }
                            });
                        }
                    }
                    Err(e) => {
                        let _ = err_tx.send(format!("無法啟動 ffmpeg：{e}"));
                        return;
                    }
                }
            }
            if let Some((mut cmd, sink)) = audio {
                if let Ok(mut child) = cmd.spawn() {
                    let out = child.stdout.take().expect("piped");
                    if run.adopt(child) {
                        sink.append(PcmSource(BufReader::with_capacity(1 << 16, out)));
                    }
                }
            }
        });

        self.frames_shown = 0;
        self.started = Some(Instant::now());
    }

    fn pause(&mut self) {
        if self.is_playing() {
            self.base = self.position();
            self.started = None;
            self.stop_processes();
        }
    }

    fn seek(&mut self, pos: f64) {
        let playing = self.is_playing();
        self.pause();
        self.base = pos;
        if playing {
            self.play();
        } else {
            self.show_still();
        }
    }

    fn set_volume(&mut self, v: f32) {
        self.volume = v;
        if let Some(sink) = &self.audio {
            sink.set_volume(v);
        }
    }

    /// The frame due now (or the still sought to), and a failure to start.
    fn poll(&mut self) -> (Option<Vec<u8>>, Option<String>) {
        let error = self.spawn_error.as_ref().and_then(|rx| rx.try_recv().ok());
        let mut frame = None;
        if let Some(rgba) = self.still.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.still = None;
            if !self.is_playing() {
                frame = Some(rgba);
            }
        }
        if !self.is_playing() {
            return (frame, error);
        }
        if self.duration > 0.0 && self.position() >= self.duration {
            // The clock starts before ffmpeg's sound arrives, so let the sound play out;
            // stopping it here would silence clips shorter than that delay.
            self.base = self.duration;
            self.started = None;
            self.frames = None;
            return (frame, error);
        }
        let Some(rx) = &self.frames else { return (frame, error) };
        let elapsed = self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
        let due = (elapsed * self.fps) as u64 + 1;
        // Drop late frames so video keeps up with the audio clock.
        while self.frames_shown < due {
            match rx.try_recv() {
                Ok(f) => {
                    frame = Some(f);
                    self.frames_shown += 1;
                }
                Err(_) => break,
            }
        }
        (frame, error)
    }
}

impl Drop for FfmpegPlayer {
    fn drop(&mut self) {
        self.stop_processes();
    }
}

enum Backend {
    Ffmpeg(FfmpegPlayer),
    #[cfg(windows)]
    Mf(crate::mf_player::MfPlayer),
}

/// A video or audio item being played, by whichever engine the settings pick.
/// Clips (play a range, loop it), the texture and the error are kept here, the
/// same for every engine.
pub struct Player {
    pub id: String,
    path: PathBuf,
    ffmpeg_dir: Option<PathBuf>,
    pub duration: f64,
    fps: f64,
    /// Preview frame size from the item's dimensions; None for audio-only.
    frame_size: Option<[usize; 2]>,
    backend: Backend,
    /// Media Foundation failed and FFmpeg is there to take over.
    fall_back: bool,
    pub texture: Option<egui::TextureHandle>,
    pub volume: f32,
    pub error: Option<String>,
    /// Pause when playback reaches this position (playing a marked clip).
    pub stop_at: Option<f64>,
    /// The clip being played by `play_range`, to start over when `looping`.
    range: Option<(f64, f64)>,
    pub looping: bool,
    /// Until a frame arrives after a paused seek, keep repainting to show it.
    awaiting_frame: Option<Instant>,
    /// Paused for a timeline drag; plays on from where it is dropped.
    resume_after_drag: bool,
    /// Forward speed (L), 1 = normal.
    rate: f64,
    /// Playing backwards (J) at this speed, by stepping back through keyframes;
    /// with when it last stepped.
    reverse: Option<(f64, Instant)>,
    #[cfg(test)]
    last_mean: u8,
    #[cfg(test)]
    uploads: u32,
}

/// Media Foundation is tried first unless FFmpeg is picked.
fn wants_mf(engine: PlayEngine) -> bool {
    cfg!(windows) && engine != PlayEngine::Ffmpeg
}

impl Player {
    /// `max_width` caps the decode size (small for hover previews, larger in the viewer).
    /// None when the chosen engine can't run (FFmpeg picked but not found).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        engine: PlayEngine,
        ffmpeg_dir: Option<PathBuf>,
        path: PathBuf,
        duration: f64,
        fps: Option<f64>,
        size: (u32, u32),
        video: bool,
        max_width: u32,
    ) -> Option<Self> {
        let frame_size = if video { preview_size(size.0, size.1, max_width) } else { None };
        let fps = fps.filter(|f| *f > 0.0).unwrap_or(30.0).min(60.0);
        let volume = 0.8;
        let backend = if wants_mf(engine) {
            Self::mf(&path, duration, video, max_width, volume)
        } else {
            Backend::Ffmpeg(FfmpegPlayer::new(ffmpeg_dir.clone()?, path.clone(), duration, fps, frame_size, volume))
        };
        Some(Self {
            id,
            fall_back: engine == PlayEngine::Auto && ffmpeg_dir.is_some(),
            path,
            ffmpeg_dir,
            duration,
            fps,
            frame_size,
            backend,
            texture: None,
            volume,
            error: None,
            stop_at: None,
            range: None,
            looping: false,
            awaiting_frame: None,
            resume_after_drag: false,
            rate: 1.0,
            reverse: None,
            #[cfg(test)]
            last_mean: 0,
            #[cfg(test)]
            uploads: 0,
        })
    }

    #[cfg(windows)]
    fn mf(path: &Path, duration: f64, video: bool, max_width: u32, volume: f32) -> Backend {
        Backend::Mf(crate::mf_player::MfPlayer::open(path, duration, video, max_width, volume))
    }

    #[cfg(not(windows))]
    fn mf(_: &Path, _: f64, _: bool, _: u32, _: f32) -> Backend {
        unreachable!("Media Foundation is Windows only")
    }

    /// The engine playing, for the viewer's tooltip.
    pub fn engine_name(&self) -> &'static str {
        match self.backend {
            Backend::Ffmpeg(_) => "FFmpeg",
            #[cfg(windows)]
            Backend::Mf(_) if self.frame_size.is_some() => "Media Foundation（硬體解碼）",
            #[cfg(windows)]
            Backend::Mf(_) => "Media Foundation",
        }
    }

    /// One frame's length, for stepping frame by frame.
    pub fn frame_secs(&self) -> f64 {
        1.0 / self.fps
    }

    /// Moves by `secs` (negative: back), playing on if it was playing.
    pub fn seek_by(&mut self, secs: f64) {
        self.seek(self.position() + secs);
    }

    pub fn is_playing(&self) -> bool {
        match &self.backend {
            Backend::Ffmpeg(b) => b.is_playing(),
            #[cfg(windows)]
            Backend::Mf(b) => b.is_playing(),
        }
    }

    /// Whether this is a video (has picture to capture).
    pub fn has_frames(&self) -> bool {
        self.frame_size.is_some()
    }

    /// A video wider than 9:16, which a vertical crop can be taken from.
    pub fn is_landscape(&self) -> bool {
        self.frame_size.is_some_and(|[w, h]| w * 16 > h * 9)
    }

    pub fn position(&self) -> f64 {
        match &self.backend {
            Backend::Ffmpeg(b) => b.position(),
            #[cfg(windows)]
            Backend::Mf(b) => b.position(),
        }
    }

    pub fn play(&mut self) {
        self.error = None;
        self.reverse = None;
        match &mut self.backend {
            Backend::Ffmpeg(b) => b.play(),
            #[cfg(windows)]
            Backend::Mf(b) => b.play(),
        }
    }

    pub fn pause(&mut self) {
        self.reverse = None;
        match &mut self.backend {
            Backend::Ffmpeg(b) => b.pause(),
            #[cfg(windows)]
            Backend::Mf(b) => b.pause(),
        }
    }

    /// Speeds for J and L, in order.
    pub const RATES: [f64; 3] = [1.0, 2.0, 4.0];

    /// The speed now: negative while playing backwards.
    pub fn rate(&self) -> f64 {
        self.reverse.map_or(self.rate, |(r, _)| -r)
    }

    /// Whether this engine can change speed (Media Foundation; FFmpeg can't).
    pub fn can_change_rate(&self) -> bool {
        #[cfg(windows)]
        if let Backend::Mf(_) = self.backend {
            return true;
        }
        false
    }

    fn set_rate(&mut self, rate: f64) {
        self.rate = rate;
        #[cfg(windows)]
        if let Backend::Mf(b) = &mut self.backend {
            b.set_rate(rate);
        }
    }

    /// The next speed after `r`, staying at the fastest.
    fn next_rate(r: f64) -> f64 {
        Self::RATES.iter().copied().find(|x| *x > r + 1e-9).unwrap_or(Self::RATES[Self::RATES.len() - 1])
    }

    /// L: plays forward; pressed again while playing, faster (1×, 2×, 4×).
    /// Returns the speed now.
    pub fn faster(&mut self) -> f64 {
        let next = if self.reverse.is_some() || !self.is_playing() { 1.0 } else { Self::next_rate(self.rate) };
        if self.can_change_rate() {
            self.set_rate(next);
        }
        if !self.is_playing() || self.reverse.is_some() {
            self.play();
        }
        self.rate()
    }

    /// J: plays backwards; pressed again, faster. None when the engine can't
    /// (FFmpeg), so the caller can jump back instead.
    pub fn backwards(&mut self) -> Option<f64> {
        if !self.can_change_rate() {
            return None;
        }
        let speed = match self.reverse {
            Some((r, _)) => Self::next_rate(r),
            None => 1.0,
        };
        // The engine pauses; `update` steps it back.
        match &mut self.backend {
            Backend::Ffmpeg(b) => b.pause(),
            #[cfg(windows)]
            Backend::Mf(b) => b.pause(),
        }
        self.stop_at = None;
        self.range = None;
        self.reverse = Some((speed, self.reverse.map_or_else(Instant::now, |(_, t)| t)));
        Some(-speed)
    }

    /// K: pauses and goes back to normal speed.
    pub fn stop_and_reset(&mut self) {
        self.pause();
        if self.rate != 1.0 {
            self.set_rate(1.0);
        }
    }

    /// While playing backwards: a step back on the nearest keyframe every
    /// 1/15 s, as far as the time passed at the speed asked for.
    fn step_back(&mut self, ctx: &egui::Context) {
        let Some((speed, last)) = self.reverse else { return };
        let dt = last.elapsed().as_secs_f64();
        if dt < 1.0 / 15.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / 15.0 - dt));
            return;
        }
        let to = (self.position() - dt * speed).max(0.0);
        #[cfg(windows)]
        if let Backend::Mf(b) = &mut self.backend {
            b.seek_rough(to);
        }
        self.awaiting_frame = Some(Instant::now());
        self.reverse = (to > 0.0).then(|| (speed, Instant::now()));
        ctx.request_repaint();
    }

    pub fn toggle(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Plays `start..end` once.
    pub fn play_range(&mut self, start: f64, end: f64) {
        self.seek(start);
        self.play();
        self.stop_at = Some(end);
        self.range = Some((start, end));
    }

    pub fn seek(&mut self, pos: f64) {
        self.stop_at = None;
        self.range = None;
        let pos = pos.clamp(0.0, self.duration.max(0.0));
        if !self.is_playing() {
            self.awaiting_frame = Some(Instant::now());
        }
        match &mut self.backend {
            Backend::Ffmpeg(b) => b.seek(pos),
            #[cfg(windows)]
            Backend::Mf(b) => b.seek(pos, false),
        }
        if std::mem::take(&mut self.resume_after_drag) {
            self.play();
        }
    }

    /// Follows the timeline while it is dragged, showing the nearest keyframe;
    /// `seek` on release lands exactly. Only Media Foundation is quick enough:
    /// the FFmpeg engine waits for the release. Returns whether it moved.
    pub fn scrub(&mut self, pos: f64) -> bool {
        #[cfg(windows)]
        if let Backend::Mf(b) = &mut self.backend {
            self.stop_at = None;
            self.range = None;
            // Paused, the engine lands each seek sooner (playing it decodes on).
            if b.is_playing() {
                b.pause();
                self.resume_after_drag = true;
            }
            self.awaiting_frame = Some(Instant::now());
            b.seek(pos.clamp(0.0, self.duration.max(0.0)), true);
            return true;
        }
        let _ = pos;
        false
    }

    pub fn set_volume(&mut self, v: f32) {
        self.volume = v;
        match &mut self.backend {
            Backend::Ffmpeg(b) => b.set_volume(v),
            #[cfg(windows)]
            Backend::Mf(b) => b.set_volume(v),
        }
    }

    /// Advances to the frame for the current time and uploads it. Call every frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        self.step_back(ctx);
        let (frame, error) = match &mut self.backend {
            Backend::Ffmpeg(b) => {
                let (rgba, error) = b.poll();
                (rgba.zip(self.frame_size), error)
            }
            #[cfg(windows)]
            Backend::Mf(b) => {
                let (frame, error, duration) = b.poll();
                if let Some(d) = duration.filter(|_| self.duration <= 0.0) {
                    self.duration = d;
                }
                (frame.map(|f| (f.rgba, f.size)), error)
            }
        };
        if let Some(e) = error {
            if self.fall_back {
                self.switch_to_ffmpeg(&e);
            } else {
                self.error = Some(e);
            }
        }
        if let Some((rgba, size)) = frame {
            self.awaiting_frame = None;
            self.upload(ctx, size, &rgba);
        }
        if self.awaiting_frame.is_some_and(|t| t.elapsed().as_secs_f64() < 5.0) {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
        if !self.is_playing() {
            return;
        }
        if let Some(stop) = self.stop_at {
            if self.position() >= stop {
                match self.range.filter(|_| self.looping) {
                    Some((start, end)) => self.play_range(start, end),
                    None => {
                        self.stop_at = None;
                        self.pause();
                    }
                }
                return;
            }
        }
        ctx.request_repaint();
    }

    /// Media Foundation couldn't play the file: FFmpeg carries on from the same
    /// place, playing or paused as before.
    fn switch_to_ffmpeg(&mut self, why: &str) {
        let Some(dir) = self.ffmpeg_dir.clone() else { return };
        crate::log::info("player", format!("改用 FFmpeg 播放 {}：{why}", self.path.display()));
        let (pos, playing) = (self.position(), self.is_playing());
        let mut ff = FfmpegPlayer::new(dir, self.path.clone(), self.duration, self.fps, self.frame_size, self.volume);
        self.fall_back = false;
        ff.base = pos;
        if playing {
            ff.play();
        } else {
            ff.show_still();
            self.awaiting_frame = Some(Instant::now());
        }
        self.backend = Backend::Ffmpeg(ff);
    }

    #[cfg(test)]
    fn last_still_brightness(&self) -> u8 {
        self.last_mean
    }

    fn upload(&mut self, ctx: &egui::Context, [w, h]: [usize; 2], rgba: &[u8]) {
        if rgba.len() != w * h * 4 {
            return;
        }
        #[cfg(test)]
        {
            self.uploads += 1;
            self.last_mean = (rgba.as_chunks::<4>().0.iter().map(|p| p[0] as u64).sum::<u64>() / (rgba.len() as u64 / 4).max(1)) as u8;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([w, h], rgba);
        match &mut self.texture {
            Some(t) => t.set(image, egui::TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture(format!("player-{}", self.id), image, egui::TextureOptions::LINEAR)),
        }
    }
}

/// The decode size of a `w`×`h` video: at most `max_width` wide, even height
/// (matching ffmpeg's scale=W:-2). Both engines decode to this size.
pub fn preview_size(w: u32, h: u32, max_width: u32) -> Option<[usize; 2]> {
    if w == 0 || h == 0 {
        return None;
    }
    let tw = w.min(max_width);
    let th = ((h as f64 * tw as f64 / w as f64 / 2.0).round() * 2.0) as u32;
    Some([tw as usize, th.max(2) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_of_unknown_length_still_moves_past_zero() {
        let mut ff = FfmpegPlayer::new(PathBuf::new(), PathBuf::new(), 0.0, 30.0, None, 1.0);
        ff.base = 5.0;
        assert_eq!(ff.position(), 5.0, "the clock was held at 0");
        let mut ff = FfmpegPlayer::new(PathBuf::new(), PathBuf::new(), 3.0, 30.0, None, 1.0);
        ff.base = 5.0;
        assert_eq!(ff.position(), 3.0);
    }

    fn ff_engine(p: &Player) -> &FfmpegPlayer {
        match &p.backend {
            Backend::Ffmpeg(b) => b,
            #[cfg(windows)]
            Backend::Mf(_) => panic!("not FFmpeg"),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orangelink-{name}-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs `p` for `secs`, updating like the UI does.
    fn run_for(p: &mut Player, ctx: &egui::Context, secs: f64) {
        let start = Instant::now();
        while start.elapsed().as_secs_f64() < secs {
            p.update(ctx);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    #[test]
    #[ignore]
    fn ui_thread_cost() {
        let Some(ff) = crate::media::ffmpeg_for_test("ui_thread_cost") else { return };
        let clip = std::path::PathBuf::from(std::env::var("OL_CLIP").unwrap());
        for engine in [PlayEngine::Ffmpeg, PlayEngine::MediaFoundation] {
            for _ in 0..5 {
                let mut p = Player::new("t".into(), engine, Some(ff.clone()), clip.clone(), 60.0, Some(30.0), (1920, 1080), true, PREVIEW_MAX_WIDTH).unwrap();
                p.set_volume(0.0);
                let t = Instant::now();
                p.play();
                let start = t.elapsed();
                std::thread::sleep(std::time::Duration::from_millis(300));
                let t = Instant::now();
                drop(p);
                println!("{engine:?}: start {:?}  stop {:?}", start, t.elapsed());
            }
            // Sweeping across many items: stopped before the engine has even started.
            let t = Instant::now();
            for _ in 0..30 {
                let mut p = Player::new("t".into(), engine, Some(ff.clone()), clip.clone(), 60.0, Some(30.0), (1920, 1080), true, PREVIEW_MAX_WIDTH).unwrap();
                p.set_volume(0.0);
                p.play();
            }
            println!("{engine:?}: 30 start+stop: {:?}", t.elapsed());
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let out = crate::process::hidden("tasklist").args(["/fi", "imagename eq ffmpeg.exe", "/fo", "csv", "/nh"]).output().unwrap();
        let left = String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains("ffmpeg")).count();
        println!("ffmpeg left running: {left}");
    }

    /// Plays `OL_CLIP` (e.g. a 4K recording) with each engine and prints the
    /// process's CPU time, and how long seeks take to show their frame.
    #[test]
    #[ignore]
    fn engine_cost() {
        let ff = crate::media::find_ffmpeg_dir(None);
        let clip = std::path::PathBuf::from(std::env::var("OL_CLIP").unwrap());
        let info = crate::media::probe(ff.as_deref().unwrap(), &clip).unwrap();
        println!("{}x{} {:?} fps, {:?} s", info.width, info.height, info.fps, info.duration);
        let ctx = egui::Context::default();
        for engine in [PlayEngine::Ffmpeg, PlayEngine::MediaFoundation] {
            let mut p = Player::new("t".into(), engine, ff.clone(), clip.clone(), info.duration.unwrap_or(600.0), info.fps, (info.width, info.height), true, VIEWER_MAX_WIDTH).unwrap();
            p.set_volume(0.0);
            p.play();
            run_for(&mut p, &ctx, 2.0);
            let cpu = cpu_seconds();
            let t = Instant::now();
            run_for(&mut p, &ctx, 10.0);
            let used = cpu_seconds() - cpu;
            println!("{engine:?}: {:.0}% of one core while playing ({} frames shown)", used / t.elapsed().as_secs_f64() * 100.0, p.uploads);
            p.pause();
            for at in [60.0, 300.0, 120.0, 30.0] {
                let t = Instant::now();
                let before = p.uploads;
                p.seek(at);
                while p.uploads == before && t.elapsed().as_secs() < 10 {
                    p.update(&ctx);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                println!("{engine:?}: paused seek to {at}s shown after {:?}", t.elapsed());
            }
            for at in [200.0, 90.0] {
                let t = Instant::now();
                let before = p.uploads;
                if !p.scrub(at) {
                    break;
                }
                while p.uploads == before && t.elapsed().as_secs() < 10 {
                    p.update(&ctx);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                println!("{engine:?}: dragged to {at}s, shown after {:?}", t.elapsed());
            }
            // Dragging while playing: a burst of nearby seeks, as the pointer moves.
            p.play();
            run_for(&mut p, &ctx, 1.0);
            let t = Instant::now();
            let before = p.uploads;
            let mut at = 1000.0;
            while t.elapsed().as_secs_f64() < 1.0 {
                at += 5.0;
                p.scrub(at);
                p.update(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
            println!("{engine:?}: 1 s of dragging while playing showed {} frames", p.uploads - before);
        }
    }

    /// CPU time used so far by this process and every running ffmpeg.
    fn cpu_seconds() -> f64 {
        let ps = crate::process::hidden("powershell")
            .args(["-NoProfile", "-Command", &format!("((Get-Process -Id {}).TotalProcessorTime.TotalSeconds) + ((Get-Process ffmpeg -ErrorAction SilentlyContinue | ForEach-Object {{ $_.TotalProcessorTime.TotalSeconds }} | Measure-Object -Sum).Sum)", std::process::id())])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&ps.stdout);
        text.trim().replace(',', ".").parse().unwrap_or(0.0)
    }

    #[test]
    fn short_clip_keeps_its_sound_after_the_clock_ends() {
        let Some(ff) = crate::media::ffmpeg_for_test("short_clip_keeps_its_sound_after_the_clock_ends") else { return };
        let dir = temp_dir("short");
        let clip = dir.join("blip.wav");
        assert!(ffmpeg(&ff).args(["-y", "-f", "lavfi", "-i", "sine=frequency=440", "-t", "0.4"]).arg(&clip).status().unwrap().success());

        let ctx = egui::Context::default();
        let mut p = Player::new("t".into(), PlayEngine::Ffmpeg, Some(ff), clip, 0.4, None, (0, 0), false, VIEWER_MAX_WIDTH).unwrap();
        p.set_volume(0.0);
        p.play();
        let had_output = ff_engine(&p).audio.is_some();
        while p.is_playing() {
            p.update(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(p.position(), 0.4);
        assert_eq!(ff_engine(&p).audio.is_some(), had_output, "sound is not cut off at the end");
        // It can still be played again.
        p.play();
        assert!(p.is_playing() && p.position() < 0.2);
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_plays_seeks_and_pauses() {
        plays_seeks_and_pauses(PlayEngine::Ffmpeg);
    }

    #[cfg(windows)]
    #[test]
    fn media_foundation_plays_seeks_and_pauses() {
        plays_seeks_and_pauses(PlayEngine::MediaFoundation);
    }

    /// Real-time playback checks need a machine that decodes in time: a shared
    /// CI runner (no GPU, two cores) can't, so they are skipped there, and say so.
    fn needs_real_time(test: &str) -> bool {
        let ci = std::env::var_os("CI").is_some();
        if ci {
            eprintln!("SKIPPED {test}: real-time playback is not measured on CI runners");
        }
        !ci
    }

    fn plays_seeks_and_pauses(engine: PlayEngine) {
        if !needs_real_time("plays_seeks_and_pauses") {
            return;
        }
        let Some(ff) = crate::media::ffmpeg_for_test("plays_seeks_and_pauses") else { return };
        let dir = temp_dir("play");
        let clip = dir.join("clip.mp4");
        let ok = ffmpeg(&ff)
            .args(["-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30", "-f", "lavfi", "-i", "sine=frequency=440"])
            .args(["-t", "4", "-pix_fmt", "yuv420p", "-shortest"])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);

        let ctx = egui::Context::default();
        let mut p = Player::new("t".into(), engine, Some(ff), clip, 4.0, Some(30.0), (640, 360), true, VIEWER_MAX_WIDTH).unwrap();
        p.fall_back = false;
        p.set_volume(0.0);
        p.play();
        let start = Instant::now();
        // The engine can be slow to start while other tests are busy; give the first frame time.
        while start.elapsed().as_secs_f64() < 1.2 || (p.texture.is_none() && start.elapsed().as_secs_f64() < 3.5) {
            p.update(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert_eq!(p.error, None);
        assert!(p.texture.is_some(), "video frames decoded");
        assert_eq!(p.texture.as_ref().unwrap().size(), [640, 360]);
        assert!(p.uploads >= 20, "kept up with playback: {}", p.uploads);
        // Media Foundation's clock starts once the file is open, a little after `play`.
        let slack = if engine == PlayEngine::Ffmpeg { 0.3 } else { 0.8 };
        assert!((p.position() - start.elapsed().as_secs_f64()).abs() < slack, "position {} after {:?}", p.position(), start.elapsed());

        p.seek(3.0);
        assert!(p.is_playing() && p.position() >= 3.0);
        run_for(&mut p, &ctx, 0.2);
        assert!(p.position() >= 3.0, "playing on from the seek: {}", p.position());
        p.pause();
        let paused_at = p.position();
        run_for(&mut p, &ctx, 0.2);
        assert!((p.position() - paused_at).abs() < 0.05, "stays put while paused");
        // Played to the end, it stops there and plays again from the start.
        p.play();
        run_for(&mut p, &ctx, 2.0);
        assert!(!p.is_playing(), "stopped at the end");
        assert!(p.position() > 3.9);
        p.play();
        run_for(&mut p, &ctx, 0.3);
        assert!(p.is_playing() && p.position() < 1.5, "starts over: {}", p.position());
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_quick_seeks_add_up() {
        quick_seeks_add_up(PlayEngine::Ffmpeg);
    }

    #[cfg(windows)]
    #[test]
    fn media_foundation_quick_seeks_add_up() {
        quick_seeks_add_up(PlayEngine::MediaFoundation);
    }

    /// → pressed four times in quick succession while paused goes 20 s on, not
    /// less: each press starts from where the one before asked to be, even
    /// while the engine is still landing that seek.
    fn quick_seeks_add_up(engine: PlayEngine) {
        if !needs_real_time("quick_seeks_add_up") {
            return;
        }
        let Some(ff) = crate::media::ffmpeg_for_test("quick_seeks_add_up") else { return };
        let dir = temp_dir("quick");
        let clip = dir.join("clip.mp4");
        let ok = ffmpeg(&ff)
            .args(["-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=30", "-pix_fmt", "yuv420p", "-preset", "ultrafast", "-g", "300"])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let ctx = egui::Context::default();
        let mut p = Player::new("t".into(), engine, Some(ff), clip, 30.0, Some(30.0), (640, 360), true, VIEWER_MAX_WIDTH).unwrap();
        p.fall_back = false;
        p.set_volume(0.0);
        p.seek(0.0);
        let start = Instant::now();
        while (p.awaiting_frame.is_some() || p.texture.is_none()) && start.elapsed().as_secs() < 10 {
            p.update(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for _ in 0..4 {
            p.seek_by(5.0);
            // One or two frames of the window between key repeats.
            for _ in 0..2 {
                p.update(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        }
        run_for(&mut p, &ctx, 1.0);
        assert!((p.position() - 20.0).abs() < 0.1, "{engine:?}: {}", p.position());
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_paused_seeks_and_loops() {
        paused_seeks_and_loops(PlayEngine::Ffmpeg);
    }

    #[cfg(windows)]
    #[test]
    fn media_foundation_paused_seeks_and_loops() {
        paused_seeks_and_loops(PlayEngine::MediaFoundation);
    }

    /// A paused seek shows its frame, a looped clip starts over, a clip stops at its end.
    fn paused_seeks_and_loops(engine: PlayEngine) {
        if !needs_real_time("paused_seeks_and_loops") {
            return;
        }
        let Some(ff) = crate::media::ffmpeg_for_test("paused_seeks_and_loops") else { return };
        let dir = temp_dir("still");
        let clip = dir.join("clip.mp4");
        // Black for 1 s, then white: which frame shows tells where the seek landed.
        let ok = ffmpeg(&ff)
            .args(["-y", "-f", "lavfi", "-i", "color=black:size=320x180:rate=30:duration=1", "-f", "lavfi", "-i", "color=white:size=320x180:rate=30:duration=1"])
            .args(["-filter_complex", "[0][1]concat=n=2:v=1", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let ctx = egui::Context::default();
        let mut p = Player::new("t".into(), engine, Some(ff), clip, 2.0, Some(30.0), (320, 180), true, VIEWER_MAX_WIDTH).unwrap();
        p.fall_back = false;
        p.set_volume(0.0);
        assert!((p.frame_secs() - 1.0 / 30.0).abs() < 1e-9);
        let brightness = |p: &mut Player, at: f64| {
            p.seek(at);
            let start = Instant::now();
            while p.awaiting_frame.is_some() && start.elapsed().as_secs() < 10 {
                p.update(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(p.texture.is_some(), "a frame while paused");
            p.last_still_brightness()
        };
        assert!(brightness(&mut p, 0.5) < 30, "black part");
        assert!(brightness(&mut p, 1.5) > 220, "white part");
        assert!(brightness(&mut p, 0.4) < 30, "back to the black part");
        // Dragging the timeline: Media Foundation follows, FFmpeg waits for the release.
        let moved = p.scrub(1.6);
        assert_eq!(moved, engine != PlayEngine::Ffmpeg);
        if moved {
            assert!((p.position() - 1.6).abs() < 0.05);
            let start = Instant::now();
            while p.awaiting_frame.is_some() && start.elapsed().as_secs() < 10 {
                p.update(&ctx);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(p.last_still_brightness() > 220, "a frame near 1.6 s");
        }
        assert!(!p.is_playing());
        assert_eq!(p.error, None);

        p.looping = true;
        p.play_range(0.2, 0.5);
        let start = Instant::now();
        let mut wrapped = false;
        let mut last = 0.0;
        while start.elapsed().as_secs_f64() < 2.0 {
            p.update(&ctx);
            let now = p.position();
            wrapped |= now + 0.1 < last;
            last = now;
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(wrapped && p.is_playing(), "started over at the in point");
        p.looping = false;
        p.play_range(0.2, 0.4);
        run_for(&mut p, &ctx, 1.2);
        assert!(!p.is_playing(), "stopped at the out point");

        // J / K / L: faster forwards, backwards; FFmpeg can't change speed.
        p.seek(0.2);
        assert_eq!(p.faster(), 1.0);
        let fast = p.faster();
        if engine == PlayEngine::Ffmpeg {
            assert_eq!((fast, p.backwards()), (1.0, None));
        } else {
            assert_eq!(fast, 2.0);
            let from = p.position();
            run_for(&mut p, &ctx, 0.4);
            let moved = p.position() - from;
            assert!(moved > 0.55, "about twice as far as 0.4 s: {moved}");
            p.seek(1.8);
            assert_eq!(p.backwards(), Some(-1.0));
            assert_eq!(p.backwards(), Some(-2.0));
            run_for(&mut p, &ctx, 0.5);
            assert!(!p.is_playing() && p.position() < 1.5, "went back: {}", p.position());
            assert_eq!(p.rate(), -2.0);
        }
        p.stop_and_reset();
        assert_eq!(p.rate(), 1.0);
        assert!(!p.is_playing());
        assert_eq!(Player::next_rate(4.0), 4.0, "stays at the fastest");
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// In automatic mode a file Windows can't decode goes to FFmpeg, which
    /// carries on from where playback was.
    #[cfg(windows)]
    #[test]
    fn automatic_mode_hands_what_windows_cannot_play_to_ffmpeg() {
        let Some(ff) = crate::media::ffmpeg_for_test("automatic_mode_hands_what_windows_cannot_play_to_ffmpeg") else { return };
        let dir = temp_dir("fallback");
        // FFV1 in Matroska: Windows opens the container but has no decoder.
        let clip = dir.join("clip.mkv");
        let ok = ffmpeg(&ff)
            .args(["-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30", "-t", "3", "-c:v", "ffv1"])
            .arg(&clip)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let ctx = egui::Context::default();
        let mut p = Player::new("t".into(), PlayEngine::Auto, Some(ff.clone()), clip.clone(), 3.0, Some(30.0), (320, 180), true, VIEWER_MAX_WIDTH).unwrap();
        p.set_volume(0.0);
        p.play();
        let start = Instant::now();
        while (p.engine_name() != "FFmpeg" || p.texture.is_none()) && start.elapsed().as_secs() < 10 {
            p.update(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert_eq!(p.engine_name(), "FFmpeg");
        assert!(p.texture.is_some() && p.is_playing());
        assert_eq!(p.error, None);
        drop(p);

        // Media Foundation alone reports it instead.
        let mut p = Player::new("t".into(), PlayEngine::MediaFoundation, Some(ff), clip, 3.0, Some(30.0), (320, 180), true, VIEWER_MAX_WIDTH).unwrap();
        p.play();
        let start = Instant::now();
        while p.error.is_none() && start.elapsed().as_secs() < 10 {
            p.update(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        assert!(p.error.is_some());
        drop(p);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_is_needed_only_when_picked() {
        let path = PathBuf::from("x.mp4");
        assert!(Player::new("t".into(), PlayEngine::Ffmpeg, None, path.clone(), 1.0, None, (16, 9), true, 480).is_none());
        if cfg!(windows) {
            let p = Player::new("t".into(), PlayEngine::Auto, None, path, 1.0, None, (16, 9), true, 480).unwrap();
            assert!(!p.fall_back, "nothing to fall back to");
        }
    }

    #[test]
    fn preview_sizes_are_capped_and_even() {
        assert_eq!(preview_size(3840, 2160, 1280), Some([1280, 720]));
        assert_eq!(preview_size(640, 360, 1280), Some([640, 360]));
        assert_eq!(preview_size(1080, 1920, 480), Some([480, 854]));
        assert_eq!(preview_size(0, 100, 480), None);
    }
}
