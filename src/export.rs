//! Clip export: the FFmpeg command for a clip and an export preset, the video
//! encoder to use, and running it with a fallback from NVENC to x264.

use crate::config::{ExportPreset, ExportShape, VideoEncoder};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        OnceLock,
    },
};

/// Short-form video size.
pub const SHORTS: (u32, u32) = (1080, 1920);
/// YouTube's loudness reference.
const LOUDNORM: &str = "loudnorm=I=-14:TP=-1.5:LRA=11";

/// One clip to write.
#[derive(Clone, Debug)]
pub struct Export {
    pub input: PathBuf,
    pub start: f64,
    pub end: f64,
    /// The item is a video (otherwise audio).
    pub video: bool,
    /// The source's extension, kept for audio and stream copies.
    pub src_ext: String,
    /// 9:16 crop as (width, left edge), when the preset is vertical and the video landscape.
    /// A vertical export without one scales the whole picture into 1080×1920.
    pub crop: Option<(u32, u32)>,
    pub preset: ExportPreset,
    /// An `.ass` file burnt into the picture. FFmpeg runs in its folder and
    /// gets the bare name, which avoids escaping a Windows path in a filter.
    pub subtitles: Option<PathBuf>,
}

/// How burnt-in subtitles look, sizes relative to the picture's height.
#[derive(Clone, Debug, PartialEq)]
pub struct BurnStyle {
    pub font: String,
    /// Font size, % of the height.
    pub size: f32,
    /// Distance from the bottom, % of the height.
    pub margin: f32,
    /// Outline width in pixels at 1080 high.
    pub outline: f32,
}

/// `0:01:02.34`
fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

/// An ASS subtitle file of the cues within `start..end`, moved to start at 0,
/// for a `res` (width, height) picture. None when no cue falls in the range.
pub fn ass(cues: &[crate::media::Cue], start: f64, end: f64, res: (u32, u32), style: &BurnStyle) -> Option<String> {
    let (w, h) = (res.0.max(1), res.1.max(1));
    let size = (h as f32 * style.size / 100.0).round().max(8.0);
    let margin = (h as f32 * style.margin / 100.0).round().max(0.0);
    let outline = (style.outline * h as f32 / 1080.0 * 10.0).round() / 10.0;
    let side = (w as f32 * 0.06).round();
    // A comma or line break in the font name would end the Style line early.
    let font = style.font.replace([',', '\r', '\n'], " ");
    let mut events = String::new();
    for c in cues.iter().filter(|c| c.end > start && c.start < end) {
        let (a, b) = ((c.start - start).max(0.0), (c.end.min(end) - start).max(0.0));
        let text: String = c.text.chars().filter(|ch| !matches!(ch, '{' | '}')).collect::<String>().replace("\r\n", "\n").replace(['\r', '\n'], "\\N");
        events.push_str(&format!("Dialogue: 0,{},{},Default,,0,0,0,,{text}\n", ass_time(a), ass_time(b)));
    }
    if events.is_empty() {
        return None;
    }
    Some(format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 0\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, \
         ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Default,{font},{size},&H00FFFFFF,&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,{outline},0,2,{side},{side},{margin},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n{events}"
    ))
}

impl Export {
    /// The output extension.
    pub fn ext(&self) -> String {
        match self.preset.shape {
            ExportShape::AudioOnly => "m4a".into(),
            ExportShape::Copy => self.src_ext.clone(),
            _ if self.video => "mp4".into(),
            _ if ["mp3", "wav", "flac", "m4a", "ogg", "aac"].contains(&self.src_ext.as_str()) => self.src_ext.clone(),
            _ => "m4a".into(),
        }
    }

    /// Written as a 9:16 video: cropped from a landscape source, or a portrait one scaled.
    pub fn vertical(&self) -> bool {
        self.video && self.preset.shape == ExportShape::Vertical
    }

    /// FFmpeg's arguments for writing `out` with the video `encoder` (an FFmpeg
    /// encoder name; unused for audio and copies).
    pub fn args(&self, encoder: &str, out: &Path) -> Vec<String> {
        let s = |v: &str| v.to_string();
        let mut a = vec![s("-v"), s("error"), s("-y"), s("-ss"), format!("{:.3}", self.start), s("-i")];
        a.push(self.input.to_string_lossy().into_owned());
        a.extend([s("-t"), format!("{:.3}", self.end - self.start)]);
        let loud = self.preset.loudnorm;
        match self.preset.shape {
            ExportShape::Copy => a.extend(["-map", "0:v?", "-map", "0:a?", "-c", "copy", "-avoid_negative_ts", "make_zero"].map(s)),
            ExportShape::AudioOnly => {
                a.extend(["-vn", "-map", "0:a:0", "-c:a", "aac", "-b:a", "192k"].map(s));
                if loud {
                    a.extend([s("-af"), s(LOUDNORM)]);
                }
            }
            _ if self.video => {
                let mut filters = Vec::new();
                if self.preset.shape == ExportShape::Vertical {
                    let (w, h) = SHORTS;
                    match self.crop {
                        Some((cw, x)) => {
                            filters.push(format!("crop={cw}:ih:{x}:0"));
                            if self.preset.full_hd {
                                filters.push(format!("scale={w}:{h}:flags=lanczos,setsar=1"));
                            }
                        }
                        // Already portrait: fit inside 1080×1920, black bars for
                        // anything that isn't exactly 9:16.
                        None if self.preset.full_hd => filters.push(format!(
                            "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
                        )),
                        None => {}
                    }
                }
                if let Some(name) = self.subtitles.as_ref().and_then(|p| p.file_name()) {
                    filters.push(format!("subtitles={}", name.to_string_lossy()));
                }
                if !filters.is_empty() {
                    a.extend([s("-vf"), filters.join(",")]);
                }
                a.extend(["-map", "0:v:0", "-map", "0:a?"].map(s));
                a.extend(codec_args(encoder));
                a.extend(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k"].map(s));
                if loud {
                    a.extend([s("-af"), s(LOUDNORM)]);
                }
            }
            _ => {
                a.push(s("-vn"));
                if loud {
                    a.extend([s("-af"), s(LOUDNORM)]);
                }
            }
        }
        a.push(out.to_string_lossy().into_owned());
        a
    }

    /// Re-encodes video, so the encoder matters.
    pub fn encodes_video(&self) -> bool {
        self.video && matches!(self.preset.shape, ExportShape::Original | ExportShape::Vertical)
    }

    /// What a job that doesn't re-encode video does, for the log: a stream
    /// copy, AAC, or FFmpeg's usual encoder for an audio file's own format.
    fn other_codec(&self) -> &'static str {
        match self.preset.shape {
            ExportShape::Copy => "copy",
            ExportShape::AudioOnly => "aac",
            _ => "audio",
        }
    }
}

fn codec_args(encoder: &str) -> Vec<String> {
    let v: &[&str] = match encoder {
        "h264_nvenc" => &["-c:v", "h264_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "19", "-b:v", "0"],
        "hevc_nvenc" => &["-c:v", "hevc_nvenc", "-preset", "p5", "-rc", "vbr", "-cq", "21", "-b:v", "0", "-tag:v", "hvc1"],
        _ => &["-c:v", "libx264", "-preset", "veryfast", "-crf", "18"],
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// Set once NVENC failed this session (no NVIDIA card, old driver): Auto stops trying it.
/// What `run` answers when it was cancelled.
pub const CANCELLED: &str = "已取消";

static NVENC_FAILED: AtomicBool = AtomicBool::new(false);

/// The encoders FFmpeg was built with (`ffmpeg -encoders`), asked once.
fn ffmpeg_encoders(ffmpeg_dir: &Path) -> &'static str {
    static LIST: OnceLock<String> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut cmd = command(ffmpeg_dir);
        cmd.args(["-hide_banner", "-encoders"]).stdout(Stdio::piped());
        cmd.output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    })
}

/// Encoders to try in order: the chosen one, and x264 after NVENC in Auto mode.
pub fn encoder_order(choice: VideoEncoder, available: &str, nvenc_failed: bool) -> Vec<&'static str> {
    match choice {
        VideoEncoder::Auto if available.contains("h264_nvenc") && !nvenc_failed => vec!["h264_nvenc", "libx264"],
        VideoEncoder::Auto | VideoEncoder::X264 => vec!["libx264"],
        VideoEncoder::H264Nvenc => vec!["h264_nvenc"],
        VideoEncoder::HevcNvenc => vec!["hevc_nvenc"],
    }
}

fn command(ffmpeg_dir: &Path) -> Command {
    let mut cmd = crate::process::hidden(ffmpeg_dir.join("ffmpeg"));
    cmd.stdout(Stdio::null()).stderr(Stdio::piped());
    cmd
}

/// Writes the clip to `out` (blocking), trying the next encoder when one fails.
/// Returns the encoder used (or "copy"/"aac"/"audio" without video), or
/// FFmpeg's last error line.
/// Encodes the clip; stops FFmpeg and removes the partial file when `cancel` is set.
pub fn run(ffmpeg_dir: &Path, choice: VideoEncoder, job: &Export, out: &Path, cancel: &AtomicBool) -> Result<&'static str, String> {
    let order = if job.encodes_video() { encoder_order(choice, ffmpeg_encoders(ffmpeg_dir), NVENC_FAILED.load(Ordering::Relaxed)) } else { vec![job.other_codec()] };
    let mut last = String::from("ffmpeg 失敗");
    for (n, encoder) in order.iter().enumerate() {
        let mut cmd = command(ffmpeg_dir);
        cmd.args(job.args(encoder, out));
        if let Some(dir) = job.subtitles.as_ref().and_then(|p| p.parent()) {
            cmd.current_dir(dir);
        }
        match crate::process::output_or_cancel(&mut cmd, cancel) {
            Ok(None) => {
                let _ = std::fs::remove_file(out);
                return Err(CANCELLED.to_string());
            }
            Ok(Some(o)) if o.status.success() && out.is_file() => return Ok(encoder),
            Ok(Some(o)) => {
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                crate::log::command_failed("clip", &cmd, format!("結束代碼 {:?}\n{err}", o.status.code()));
                last = err.lines().last().unwrap_or("ffmpeg 失敗").to_string();
                if encoder.ends_with("_nvenc") && choice == VideoEncoder::Auto {
                    NVENC_FAILED.store(true, Ordering::Relaxed);
                }
                if n + 1 < order.len() {
                    crate::log::info("clip", format!("{encoder} 失敗，改用 {}", order[n + 1]));
                    let _ = std::fs::remove_file(out);
                }
            }
            Err(e) => {
                crate::log::command_failed("clip", &cmd, format!("無法執行：{e}"));
                return Err(e.to_string());
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(shape: ExportShape, video: bool) -> Export {
        Export {
            input: "in.mkv".into(),
            start: 10.0,
            end: 25.5,
            video,
            src_ext: if video { "mkv".into() } else { "mp3".into() },
            crop: video.then_some((608, 656)),
            preset: ExportPreset { shape, ..Default::default() },
            subtitles: None,
        }
    }

    fn joined(j: &Export, enc: &str) -> String {
        j.args(enc, Path::new("out")).join(" ")
    }

    #[test]
    fn vertical_is_cropped_and_scaled_to_1080x1920() {
        let mut j = job(ExportShape::Vertical, true);
        let a = joined(&j, "libx264");
        assert!(a.contains("-vf crop=608:ih:656:0,scale=1080:1920:flags=lanczos,setsar=1"), "{a}");
        assert!(a.contains("-ss 10.000 -i in.mkv -t 15.500"));
        assert_eq!(j.ext(), "mp4");
        j.preset.full_hd = false;
        assert!(joined(&j, "libx264").contains("-vf crop=608:ih:656:0 "), "keeps the crop's pixels");
        // The original shape ignores the crop.
        assert!(!joined(&job(ExportShape::Original, true), "libx264").contains("-vf"));
    }

    #[test]
    fn a_portrait_source_is_scaled_into_1080x1920() {
        let mut j = job(ExportShape::Vertical, true);
        j.crop = None;
        let a = joined(&j, "libx264");
        assert!(a.contains("-vf scale=1080:1920:force_original_aspect_ratio=decrease:flags=lanczos,pad=1080:1920:(ow-iw)/2:(oh-ih)/2,setsar=1 "), "{a}");
        assert!(j.vertical(), "named _9x16");
        j.preset.full_hd = false;
        assert!(!joined(&j, "libx264").contains("-vf"), "keeps the source's pixels");
    }

    #[test]
    fn encoders_and_loudness() {
        let mut j = job(ExportShape::Original, true);
        assert!(joined(&j, "h264_nvenc").contains("-c:v h264_nvenc -preset p5"));
        assert!(joined(&j, "libx264").contains("-c:v libx264 -preset veryfast -crf 18"));
        assert!(!joined(&j, "libx264").contains("loudnorm"));
        j.preset.loudnorm = true;
        assert!(joined(&j, "libx264").contains("-af loudnorm=I=-14:TP=-1.5:LRA=11"));

        let list = " V....D h264_nvenc  NVIDIA NVENC H.264 encoder\n V....D libx264";
        assert_eq!(encoder_order(VideoEncoder::Auto, list, false), ["h264_nvenc", "libx264"]);
        assert_eq!(encoder_order(VideoEncoder::Auto, list, true), ["libx264"], "after NVENC failed once");
        assert_eq!(encoder_order(VideoEncoder::Auto, " V....D libx264", false), ["libx264"]);
        assert_eq!(encoder_order(VideoEncoder::HevcNvenc, "", false), ["hevc_nvenc"]);
    }

    /// Writes real files with FFmpeg (skipped without it): a vertical Shorts
    /// export comes out 1080×1920 with the automatic encoder, NVENC or not.
    #[test]
    fn exports_with_ffmpeg() {
        let Some(ff) = crate::media::ffmpeg_for_test("exports_with_ffmpeg") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-export-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let made = command(&ff)
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30:duration=4", "-f", "lavfi", "-i", "sine=frequency=440:duration=4"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success());
        let mut job = Export {
            input: src.clone(),
            start: 1.0,
            end: 3.0,
            video: true,
            src_ext: "mp4".into(),
            crop: Some((608, 656)),
            preset: ExportPreset { shape: ExportShape::Vertical, loudnorm: true, ..Default::default() },
            subtitles: None,
        };
        let out = dir.join("v.mp4");
        let enc = run(&ff, VideoEncoder::Auto, &job, &out, &AtomicBool::new(false)).unwrap();
        println!("encoder: {enc}");
        let p = crate::media::probe(&ff, &out).unwrap();
        assert_eq!((p.width, p.height), SHORTS);
        assert!((p.duration.unwrap() - 2.0).abs() < 0.2, "{:?}", p.duration);

        // Burnt-in subtitles: the frame with a line differs from the plain export.
        let cues = vec![crate::media::Cue { start: 0.5, end: 5.0, text: "測試字幕 ABC".into() }];
        let style = BurnStyle { font: "Microsoft JhengHei".into(), size: 5.0, margin: 12.0, outline: 3.0 };
        let sub = dir.join("sub.ass");
        std::fs::write(&sub, ass(&cues, 1.0, 3.0, SHORTS, &style).unwrap()).unwrap();
        job.subtitles = Some(sub);
        job.preset.loudnorm = false;
        let subbed = dir.join("s.mp4");
        run(&ff, VideoEncoder::Auto, &job, &subbed, &AtomicBool::new(false)).unwrap();
        let frame = |f: &Path| {
            let o = command(&ff).args(["-v", "error", "-ss", "1", "-i"]).arg(f).args(["-frames:v", "1", "-vf", "crop=1080:400:0:1300,scale=54:20", "-f", "rawvideo", "-pix_fmt", "gray", "-"]).stdout(Stdio::piped()).output().unwrap();
            o.stdout
        };
        let (plain, with) = (frame(&out), frame(&subbed));
        let diff: u64 = plain.iter().zip(&with).map(|(a, b)| a.abs_diff(*b) as u64).sum();
        assert!(diff > 2000, "subtitles should change the bottom of the picture: {diff}");
        job.subtitles = None;

        // A portrait source (720×1280) and a narrower one (600×1280) both come out 1080×1920.
        for (size, name) in [("720x1280", "p.mp4"), ("600x1280", "n.mp4")] {
            let src = dir.join(format!("src-{name}"));
            let made = command(&ff)
                .args(["-v", "error", "-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate=30:duration=2")])
                .args(["-c:v", "libx264", "-preset", "ultrafast"])
                .arg(&src)
                .status()
                .unwrap();
            assert!(made.success());
            let portrait = Export { input: src, crop: None, end: 1.5, start: 0.0, ..job.clone() };
            let out = dir.join(name);
            run(&ff, VideoEncoder::Auto, &portrait, &out, &AtomicBool::new(false)).unwrap();
            let p = crate::media::probe(&ff, &out).unwrap();
            assert_eq!((p.width, p.height), SHORTS, "{size}");
        }

        job.preset = ExportPreset { shape: ExportShape::AudioOnly, ..Default::default() };
        let out = dir.join("a.m4a");
        assert_eq!(run(&ff, VideoEncoder::Auto, &job, &out, &AtomicBool::new(false)).unwrap(), "aac", "logged as what it is");
        assert!(crate::media::probe(&ff, &out).unwrap().duration.is_some());

        // Cancelled: FFmpeg is ended and nothing half-written is left.
        let out = dir.join("cancelled.m4a");
        assert_eq!(run(&ff, VideoEncoder::Auto, &job, &out, &AtomicBool::new(true)), Err(CANCELLED.to_string()));
        assert!(!out.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_carriage_return_does_not_break_the_subtitle_file() {
        let cues = vec![crate::media::Cue { start: 0.0, end: 2.0, text: "上\r下".into() }];
        let style = BurnStyle { font: "Noto\r\nSans".into(), size: 4.0, margin: 12.5, outline: 3.0 };
        let ass = ass(&cues, 0.0, 2.0, (1920, 1080), &style).unwrap();
        assert!(!ass.contains('\r'), "{ass:?}");
        assert!(ass.contains("上\\N下") && ass.contains("Style: Default,Noto  Sans,"), "{ass}");
    }

    #[test]
    fn subtitles_are_moved_into_the_clip() {
        let cue = |s, e, t: &str| crate::media::Cue { start: s, end: e, text: t.into() };
        let cues = vec![cue(1.0, 4.0, "before"), cue(9.0, 12.0, "第一行\n{第二行}"), cue(30.0, 31.0, "after")];
        let style = BurnStyle { font: "Noto Sans TC".into(), size: 4.0, margin: 12.5, outline: 3.0 };
        let a = ass(&cues, 10.0, 25.0, SHORTS, &style).unwrap();
        assert!(a.contains("PlayResX: 1080\nPlayResY: 1920"));
        assert!(a.contains("Style: Default,Noto Sans TC,77,"), "{a}");
        assert!(a.contains(",1,5.3,0,2,65,65,240,1"), "{a}");
        assert!(a.contains("Dialogue: 0,0:00:00.00,0:00:02.00,Default,,0,0,0,,第一行\\N第二行"), "{a}");
        assert!(!a.contains("before") && !a.contains("after"));
        assert_eq!(ass(&cues, 13.0, 20.0, SHORTS, &style), None);
        assert_eq!(ass_time(3725.456), "1:02:05.46");

        let mut j = job(ExportShape::Vertical, true);
        j.subtitles = Some(std::env::temp_dir().join("orangelink-sub-x.ass"));
        assert!(joined(&j, "libx264").contains("setsar=1,subtitles=orangelink-sub-x.ass "));
    }

    #[test]
    fn copies_and_audio() {
        let c = job(ExportShape::Copy, true);
        assert!(joined(&c, "x").contains("-c copy"));
        assert_eq!(c.other_codec(), "copy");
        assert_eq!(c.ext(), "mkv", "a copy keeps the container");
        assert!(!c.encodes_video());
        let a = job(ExportShape::AudioOnly, true);
        assert!(joined(&a, "x").contains("-vn -map 0:a:0 -c:a aac"));
        assert_eq!(a.ext(), "m4a");
        let mp3 = job(ExportShape::Original, false);
        assert_eq!(mp3.ext(), "mp3");
        assert!(joined(&mp3, "x").contains("-vn"));
        assert!(!job(ExportShape::Vertical, false).vertical());
    }
}
