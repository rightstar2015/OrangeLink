use crate::{
    imaging::{decode_file, fit_for_view, make_thumbnail, save_thumbnail, to_color_image, THUMB_SIDE},
    media,
    model::MediaKind,
};
use eframe::egui::{self, ColorImage};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
};

/// Workers for thumbnails that are quick: a cached thumbnail file, or an image to decode.
const FAST_WORKERS: usize = 4;
/// Workers for those that need FFmpeg or the Shell to read the file, 1 to 4 s each for a
/// video. Apart from the fast ones, so a folder of recordings doesn't hold up its images.
const SLOW_WORKERS: usize = 2;
/// A request whose item hasn't been on screen for this many frames is skipped when its
/// turn comes; the grid asks again if it scrolls back.
const STALE_FRAMES: u64 = 30;

/// Which items the grid still wants, by the frame it last asked in.
#[derive(Default)]
struct Wanted {
    frame: AtomicU64,
    last: Mutex<HashMap<String, u64>>,
}

impl Wanted {
    fn still(&self, id: &str) -> bool {
        let now = self.frame.load(Ordering::Relaxed);
        let last = crate::lock(&self.last).get(id).copied();
        last.is_none_or(|f| now.saturating_sub(f) <= STALE_FRAMES)
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Thumb,
    Full,
}

struct Request {
    id: String,
    /// Thumbnail file (for `Thumb`); unused for `Full`.
    thumb: PathBuf,
    /// The original file.
    source: PathBuf,
    media: MediaKind,
    ffmpeg_dir: Option<PathBuf>,
}

pub struct Loaded {
    pub id: String,
    pub kind: Kind,
    /// Not loaded because it had scrolled away; not a failure.
    pub skipped: bool,
    pub image: Option<ColorImage>,
    /// Animation frames with their delay in seconds (animated GIFs only).
    pub frames: Vec<(ColorImage, f32)>,
}

/// Decodes all frames of an animated GIF, capped to keep memory bounded.
/// `max_side` downscales frames (hover previews don't need full size).
pub fn load_gif_frames(path: &std::path::Path, max_side: Option<u32>) -> Vec<(ColorImage, f32)> {
    use image::AnimationDecoder;
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(decoder) = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)) else { return Vec::new() };
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for frame in decoder.into_frames().take(500) {
        let Ok(frame) = frame else { break };
        let (num, den) = frame.delay().numer_denom_ms();
        let delay = if den == 0 { 0.1 } else { (num as f32 / den as f32 / 1000.0).max(0.02) };
        let mut buf = frame.into_buffer();
        if let Some(side) = max_side.filter(|s| buf.width().max(buf.height()) > *s) {
            buf = image::DynamicImage::ImageRgba8(buf).thumbnail(side, side).to_rgba8();
        }
        bytes += buf.len();
        if bytes > 512 << 20 {
            break;
        }
        let size = [buf.width() as usize, buf.height() as usize];
        out.push((ColorImage::from_rgba_unmultiplied(size, buf.as_raw()), delay));
    }
    if out.len() > 1 { out } else { Vec::new() }
}

/// Decodes images off the UI thread. Thumbnails use a fast and a slow pool; full-size
/// previews use one thread that only ever works on the newest request.
pub struct Loader {
    fast_tx: Sender<Request>,
    slow_tx: Sender<Request>,
    full_tx: Sender<Request>,
    wanted: Arc<Wanted>,
    pub rx: Receiver<Loaded>,
}

/// Worker threads taking thumbnail requests from one queue.
fn spawn_pool(n: usize, out: &Sender<Loaded>, wanted: &Arc<Wanted>, ctx: &egui::Context) -> Sender<Request> {
    let (tx, rx) = channel::<Request>();
    let rx = Arc::new(Mutex::new(rx));
    for _ in 0..n {
        let (jobs, out, wanted, ctx) = (rx.clone(), out.clone(), wanted.clone(), ctx.clone());
        thread::spawn(move || loop {
            let req = match crate::lock(&jobs).recv() {
                Ok(r) => r,
                Err(_) => break,
            };
            let skipped = !wanted.still(&req.id);
            let image = if skipped { None } else { load_thumb(&req) };
            if out.send(Loaded { id: req.id, kind: Kind::Thumb, skipped, image, frames: Vec::new() }).is_err() {
                break;
            }
            ctx.request_repaint();
        });
    }
    tx
}

impl Loader {
    pub fn new(ctx: &egui::Context) -> Self {
        let (out_tx, rx) = channel::<Loaded>();
        let wanted = Arc::new(Wanted::default());
        let fast_tx = spawn_pool(FAST_WORKERS, &out_tx, &wanted, ctx);
        let slow_tx = spawn_pool(SLOW_WORKERS, &out_tx, &wanted, ctx);

        let (full_tx, full_rx) = channel::<Request>();
        let ctx = ctx.clone();
        thread::spawn(move || {
            while let Ok(mut req) = full_rx.recv() {
                while let Ok(newer) = full_rx.try_recv() {
                    req = newer;
                }
                let is_gif = media::ext_of(&req.source) == "gif";
                let frames = if is_gif { load_gif_frames(&req.source, None) } else { Vec::new() };
                let image = if crate::font_sample::is_font(&req.source) {
                    std::fs::read(&req.source).ok().and_then(|b| crate::font_sample::specimen(&b, 1400)).map(|img| to_color_image(&image::DynamicImage::ImageRgba8(img)))
                } else if frames.is_empty() {
                    decode_file(&req.source).ok().map(|img| to_color_image(&fit_for_view(img)))
                } else {
                    None
                };
                if out_tx.send(Loaded { id: req.id, kind: Kind::Full, skipped: false, image, frames }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });

        Self { fast_tx, slow_tx, full_tx, wanted, rx }
    }

    pub fn request_thumb(&self, id: String, thumb: PathBuf, source: PathBuf, media: MediaKind, ffmpeg_dir: Option<PathBuf>) {
        let req = Request { id, thumb, source, media, ffmpeg_dir };
        let _ = if is_quick(&req) { &self.fast_tx } else { &self.slow_tx }.send(req);
    }

    /// The items on screen this frame, so requests for ones scrolled away can be skipped.
    pub fn mark_wanted<'a>(&self, frame: u64, ids: impl IntoIterator<Item = &'a String>) {
        self.wanted.frame.store(frame, Ordering::Relaxed);
        let mut last = crate::lock(&self.wanted.last);
        for id in ids {
            last.insert(id.clone(), frame);
        }
        if last.len() > 20_000 {
            last.retain(|_, f| frame.saturating_sub(*f) <= STALE_FRAMES);
        }
    }

    pub fn request_full(&self, id: String, source: PathBuf) {
        let _ = self.full_tx.send(Request {
            id,
            thumb: PathBuf::new(),
            source,
            media: MediaKind::Image,
            ffmpeg_dir: None,
        });
    }
}

/// A thumbnail already made, or an image `image` can decode: no FFmpeg or Shell needed.
fn is_quick(req: &Request) -> bool {
    req.thumb.is_file() || (req.media == MediaKind::Image && crate::imaging::SUPPORTED_EXTS.contains(&media::ext_of(&req.source).as_str()))
}

/// Loads the cached thumbnail, regenerating it from the original if missing.
fn load_thumb(req: &Request) -> Option<ColorImage> {
    if let Ok(img) = decode_file(&req.thumb) {
        return Some(to_color_image(&img));
    }
    if req.media == MediaKind::Image {
        let img = crate::imaging::representative_gif_frame(&req.source).map(Ok).unwrap_or_else(|| decode_file(&req.source));
        if let Ok(img) = img {
            let _ = save_thumbnail(&img, &req.thumb);
            return Some(to_color_image(&make_thumbnail(&img)));
        }
    }
    if !req.source.is_file() {
        return None;
    }
    // A font shows its own sample text, Chinese where it has it.
    if crate::font_sample::is_font(&req.source) {
        if let Some(img) = std::fs::read(&req.source).ok().and_then(|b| crate::font_sample::thumbnail(&b, THUMB_SIDE)) {
            let img = image::DynamicImage::ImageRgba8(img);
            let _ = save_thumbnail(&img, &req.thumb);
            return Some(to_color_image(&make_thumbnail(&img)));
        }
    }
    // Never read a whole video into memory here; only ffmpeg can make these.
    if let (Some(ff), false) = (&req.ffmpeg_dir, req.media == MediaKind::Other) {
        let duration = media::probe(ff, &req.source).and_then(|p| p.duration);
        let made = media::thumbnail(ff, &req.source, &req.thumb, duration, THUMB_SIDE)
            || (req.media == MediaKind::Audio && media::waveform(ff, &req.source, &req.thumb, THUMB_SIDE));
        if made {
            return decode_file(&req.thumb).ok().map(|img| to_color_image(&img));
        }
    }
    // Windows renders PDFs itself, with no thumbnail handler installed.
    if req.source.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
        if let Some(img) = crate::pdf::first_page(&req.source, THUMB_SIDE) {
            let img = image::DynamicImage::ImageRgba8(img);
            let _ = save_thumbnail(&img, &req.thumb);
            return Some(to_color_image(&make_thumbnail(&img)));
        }
    }
    shell_thumb(req)
}

/// What Windows itself shows: a thumbnail from a handler (PSD, PDF, HEIC, fonts...),
/// cached or made now, else the file type's icon. Only real thumbnails are saved, so a
/// handler installed later still gets its chance.
fn shell_thumb(req: &Request) -> Option<ColorImage> {
    use crate::shell_thumb::{thumbnail, Mode};
    let found = thumbnail(&req.source, THUMB_SIDE, Mode::Cached).or_else(|| thumbnail(&req.source, THUMB_SIDE, Mode::Generate));
    if let Some(img) = found {
        let img = image::DynamicImage::ImageRgba8(img);
        let _ = save_thumbnail(&img, &req.thumb);
        return Some(to_color_image(&make_thumbnail(&img)));
    }
    let icon = thumbnail(&req.source, 256, Mode::Icon)?;
    Some(ColorImage::from_rgba_premultiplied([icon.width() as usize, icon.height() as usize], icon.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Videos asked for first must not hold up the images asked for after them.
    #[test]
    fn images_are_not_held_up_by_videos() {
        let Some(ff) = crate::media::ffmpeg_for_test("images_are_not_held_up_by_videos") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-loader-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = crate::process::hidden(ff.join("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30", "-t", "20", "-pix_fmt", "yuv420p"])
            .arg(dir.join("v.mp4"))
            .status()
            .unwrap()
            .success();
        assert!(ok);
        for i in 0..40 {
            image::RgbImage::from_pixel(64, 64, image::Rgb([i as u8, 0, 0])).save(dir.join(format!("i{i}.png"))).unwrap();
        }
        let loader = Loader::new(&egui::Context::default());
        for i in 0..8 {
            let v = dir.join(format!("v{i}.mp4"));
            std::fs::copy(dir.join("v.mp4"), &v).unwrap();
            loader.request_thumb(format!("v{i}"), dir.join(format!("v{i}.thumb.png")), v, MediaKind::Video, Some(ff.clone()));
        }
        for i in 0..40 {
            loader.request_thumb(format!("i{i}"), dir.join(format!("i{i}.thumb.png")), dir.join(format!("i{i}.png")), MediaKind::Image, None);
        }
        let order: Vec<String> = (0..48).map(|_| loader.rx.recv().unwrap().id).collect();
        let last_image = order.iter().rposition(|id| id.starts_with('i')).unwrap();
        let first_video = order.iter().position(|id| id.starts_with('v')).unwrap();
        assert!(last_image < first_video, "every image before any video: {order:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn requests_scrolled_away_are_skipped() {
        let dir = std::env::temp_dir().join(format!("orangelink-loader-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbImage::new(8, 8).save(dir.join("a.png")).unwrap();
        let loader = Loader::new(&egui::Context::default());
        loader.mark_wanted(1, [&"gone".to_string(), &"here".to_string()]);
        loader.mark_wanted(100, [&"here".to_string()]);
        for id in ["gone", "here"] {
            loader.request_thumb(id.into(), dir.join(format!("{id}.thumb.png")), dir.join("a.png"), MediaKind::Image, None);
        }
        let mut got: Vec<(String, bool, bool)> = (0..2).map(|_| loader.rx.recv().unwrap()).map(|l| (l.id, l.skipped, l.image.is_some())).collect();
        got.sort();
        assert_eq!(got, vec![("gone".into(), true, false), ("here".into(), false, true)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
