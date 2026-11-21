//! 設定 →「工具」: which outside tools are there and working, checked on a
//! thread of its own (each check starts the tool), shown as a list.

use super::*;
use std::sync::mpsc::channel;

/// What the check found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolStatus {
    pub ffmpeg: Option<PathBuf>,
    /// FFmpeg has the whisper filter (語音轉文字 needs it).
    pub whisper_filter: bool,
    /// The chosen Whisper model is downloaded.
    pub model: bool,
    pub ytdlp: Option<(PathBuf, Option<String>)>,
    pub deno: Option<PathBuf>,
}

impl ToolStatus {
    pub fn run(ffmpeg_dir: Option<PathBuf>, model: &str) -> Self {
        let whisper_filter = ffmpeg_dir.as_deref().is_some_and(crate::whisper::has_filter);
        let ytdlp = crate::ytdlp::find_ytdlp().map(|p| {
            let v = crate::ytdlp::version(&p);
            (p, v)
        });
        ToolStatus { ffmpeg: ffmpeg_dir, whisper_filter, model: crate::whisper::has_model(model), ytdlp, deno: crate::ytdlp::find_deno() }
    }

    /// One line per tool: whether it is ready, and what for.
    pub fn lines(&self) -> Vec<(bool, String)> {
        let mut out = vec![match &self.ffmpeg {
            Some(d) => (true, t!("FFmpeg：{d}", d = d.display()).to_string()),
            None => (false, t!("FFmpeg：找不到（影片縮圖、播放、片段輸出都需要）").to_string()),
        }];
        if self.ffmpeg.is_some() {
            out.push(if self.whisper_filter {
                (true, t!("FFmpeg 支援 Whisper 語音轉文字").to_string())
            } else {
                (false, t!("這個 FFmpeg 不含 Whisper，無法產生字幕（需要 8.0 以上的完整版）").to_string())
            });
        }
        out.push(if self.model { (true, t!("Whisper 模型已下載").to_string()) } else { (false, t!("Whisper 模型尚未下載（在語音轉文字視窗下載）").to_string()) });
        out.push(match &self.ytdlp {
            Some((p, v)) => (true, t!("yt-dlp {v}：{p}", v = v.as_deref().unwrap_or("?"), p = p.display()).to_string()),
            None => (false, t!("yt-dlp：找不到（從網址下載需要，可在下載視窗安裝）").to_string()),
        });
        out.push(match &self.deno {
            Some(p) => (true, t!("deno：{p}", p = p.display()).to_string()),
            None => (false, t!("deno：找不到（部分 YouTube 影片需要）").to_string()),
        });
        out
    }
}

#[derive(Default)]
pub struct ToolsCheck {
    pub result: Option<ToolStatus>,
    rx: Option<Receiver<ToolStatus>>,
}

impl ToolsCheck {
    pub fn checking(&self) -> bool {
        self.rx.is_some()
    }
}

impl App {
    /// Checks the tools again, on a thread; the settings page shows a spinner meanwhile.
    pub(crate) fn check_tools(&mut self) {
        if self.tools_check.checking() {
            return;
        }
        let (tx, rx) = channel();
        let (ff, model, ctx) = (self.ffmpeg_dir.clone(), self.config.whisper_model.clone(), self.egui_ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(ToolStatus::run(ff, &model));
            ctx.request_repaint();
        });
        self.tools_check.rx = Some(rx);
    }

    pub(super) fn poll_tools_check(&mut self) {
        let Some(rx) = &self.tools_check.rx else { return };
        match state::poll(rx) {
            Reply::Pending => {}
            Reply::Done(s) => {
                self.tools_check.result = Some(s);
                self.tools_check.rx = None;
            }
            Reply::Lost => self.tools_check.rx = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 工具 page said where FFmpeg was and sent everything else to other
    /// windows: a missing model or yt-dlp was found only when a job failed.
    #[test]
    fn the_tools_page_says_what_is_missing_and_what_it_is_for() {
        let none = ToolStatus::default();
        let lines = none.lines();
        assert!(lines.iter().all(|(ok, _)| !ok));
        assert!(lines[0].1.contains("FFmpeg") && lines[0].1.contains("縮圖"));
        assert_eq!(lines.len(), 4, "no FFmpeg: its Whisper support isn't listed");
        let all = ToolStatus {
            ffmpeg: Some("C:/ff".into()),
            whisper_filter: true,
            model: true,
            ytdlp: Some(("C:/yt-dlp.exe".into(), Some("2026.11.01".into()))),
            deno: Some("C:/deno.exe".into()),
        };
        let lines = all.lines();
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().all(|(ok, _)| *ok));
        assert!(lines[3].1.contains("2026.11.01"));
    }
}
