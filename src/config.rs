use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum ThemeSetting {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum ViewMode {
    #[default]
    Grid,
    /// Rows of equal height, images keep their aspect ratio.
    Justified,
    List,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum ImportMode {
    #[default]
    Ask,
    Copy,
    Link,
}

/// Per-user app settings, stored in %APPDATA%\OrangeLink\config.json.
/// A category of the settings window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SettingsPage {
    #[default]
    General,
    ImportDelete,
    Preview,
    Highlights,
    ExternalApps,
    Integration,
    Tools,
    Advanced,
}

impl SettingsPage {
    pub const ALL: [SettingsPage; 8] = [
        SettingsPage::General,
        SettingsPage::ImportDelete,
        SettingsPage::Preview,
        SettingsPage::Highlights,
        SettingsPage::ExternalApps,
        SettingsPage::Integration,
        SettingsPage::Tools,
        SettingsPage::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SettingsPage::General => "一般",
            SettingsPage::ImportDelete => "匯入與刪除",
            SettingsPage::Preview => "預覽與播放",
            SettingsPage::Highlights => "直播精華",
            SettingsPage::ExternalApps => "外部程式",
            SettingsPage::Integration => "整合",
            SettingsPage::Tools => "工具",
            SettingsPage::Advanced => "進階",
        }
    }
}

/// The order of the preview's clip list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipSort {
    #[default]
    Start,
    Name,
    /// Highlight candidates' score, strongest first; others after, by time.
    Score,
}

/// What plays video and audio in the viewer and hover previews.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayEngine {
    /// Media Foundation, and FFmpeg for what it can't open.
    #[default]
    Auto,
    MediaFoundation,
    Ffmpeg,
}

impl PlayEngine {
    pub const ALL: [PlayEngine; 3] = [PlayEngine::Auto, PlayEngine::MediaFoundation, PlayEngine::Ffmpeg];

    pub fn label(self) -> &'static str {
        match self {
            PlayEngine::Auto => "自動",
            PlayEngine::MediaFoundation => "Media Foundation",
            PlayEngine::Ffmpeg => "FFmpeg",
        }
    }
}

/// The video encoder clip exports use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEncoder {
    /// NVENC when FFmpeg has it and the card takes it, otherwise x264.
    #[default]
    Auto,
    H264Nvenc,
    HevcNvenc,
    X264,
}

impl VideoEncoder {
    pub const ALL: [VideoEncoder; 4] = [VideoEncoder::Auto, VideoEncoder::H264Nvenc, VideoEncoder::HevcNvenc, VideoEncoder::X264];

    pub fn label(self) -> &'static str {
        match self {
            VideoEncoder::Auto => "自動",
            VideoEncoder::H264Nvenc => "NVENC H.264（NVIDIA 顯卡）",
            VideoEncoder::HevcNvenc => "NVENC HEVC／H.265（NVIDIA 顯卡）",
            VideoEncoder::X264 => "x264（CPU）",
        }
    }
}

/// What an export preset makes of a clip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportShape {
    /// Re-encoded at the source's frame.
    #[default]
    Original,
    /// Cropped to 9:16 around the clip's crop centre (landscape video only).
    Vertical,
    /// Streams copied: fastest and lossless, but the start lands on a keyframe.
    Copy,
    /// The sound alone, as m4a.
    AudioOnly,
}

impl ExportShape {
    pub const ALL: [ExportShape; 4] = [ExportShape::Original, ExportShape::Vertical, ExportShape::Copy, ExportShape::AudioOnly];

    pub fn label(self) -> &'static str {
        match self {
            ExportShape::Original => "原比例（重新編碼）",
            ExportShape::Vertical => "直式 9:16",
            ExportShape::Copy => "不重新編碼（最快）",
            ExportShape::AudioOnly => "只要音訊（m4a）",
        }
    }
}

/// A named way to export clips, picked from the clip menu.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportPreset {
    pub name: String,
    pub shape: ExportShape,
    /// Vertical: scale to 1080×1920 (otherwise the crop keeps the source's pixels).
    pub full_hd: bool,
    /// Loudness normalised to -14 LUFS (YouTube's reference), when re-encoding.
    pub loudnorm: bool,
    /// Where the file goes; None is the library's `captures` folder.
    pub folder: Option<PathBuf>,
    /// Import the file into the library afterwards.
    pub import: bool,
    /// Burn the item's subtitles into the picture (video, re-encoded).
    pub subtitles: bool,
}

impl Default for ExportPreset {
    fn default() -> Self {
        Self { name: "新的預設組".into(), shape: ExportShape::Original, full_hd: true, loudnorm: false, folder: None, import: true, subtitles: false }
    }
}

pub fn default_export_presets() -> Vec<ExportPreset> {
    vec![
        ExportPreset { name: "原比例".into(), ..Default::default() },
        ExportPreset { name: "Shorts（1080×1920、-14 LUFS）".into(), shape: ExportShape::Vertical, loudnorm: true, ..Default::default() },
        ExportPreset { name: "Shorts 含字幕".into(), shape: ExportShape::Vertical, loudnorm: true, subtitles: true, ..Default::default() },
        ExportPreset { name: "快速剪下（不重新編碼）".into(), shape: ExportShape::Copy, ..Default::default() },
        ExportPreset { name: "只要音訊".into(), shape: ExportShape::AudioOnly, ..Default::default() },
    ]
}

/// Smallest and largest thumbnail, for the slider and Ctrl+wheel.
pub const THUMB_SIZES: std::ops::RangeInclusive<f32> = 90.0..=420.0;

/// Every field has a serde default so older config files keep loading.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Bumped when a default changes meaning; see `migrate`.
    pub config_version: u32,
    pub last_library: Option<PathBuf>,
    pub recent: Vec<PathBuf>,
    pub thumb_size: f32,
    pub view_mode: ViewMode,
    /// Headers splitting the grid by date added, kind or size.
    pub group_by: crate::grouping::GroupBy,

    pub theme: ThemeSetting,
    pub ui_scale: f32,

    pub import_mode: ImportMode,
    pub skip_duplicates: bool,
    pub import_into_current_folder: bool,

    /// Permanently deleting a linked (left-in-place) item also deletes the original file.
    /// Off by default: only the library entry is removed.
    pub delete_linked_originals: bool,
    /// When originals are deleted, send them to the Windows Recycle Bin instead of erasing.
    pub originals_to_recycle_bin: bool,

    /// Empty = look for ffmpeg/ffprobe on PATH.
    pub ffmpeg_dir: Option<PathBuf>,
    /// Double-clicking video/audio/other files opens them in the default app instead of the viewer.
    pub media_dblclick_external: bool,
    pub viewer_upscale: bool,
    /// Dragging thumbnails out of the window hands the files to other apps (Premiere, Explorer…).
    pub drag_out: bool,

    /// Programs offered under "Open with" (e.g. Premiere, Photoshop).
    pub external_apps: Vec<ExternalApp>,
    pub slideshow_secs: f32,
    /// Minutes between watch-folder scans.
    pub watch_interval_min: f32,

    /// Playback volume 0..1 (viewer and hover previews).
    pub volume: f32,
    /// Play GIF/video/audio when the cursor rests on an item.
    /// Searching looks through the whole library, not just the selected folder or tags.
    pub search_all: bool,
    pub hover_preview: bool,
    /// System-wide shortcut that brings the window up with the search box focused.
    pub hotkey_enabled: bool,
    pub hotkey: String,
    pub hover_preview_sound: bool,
    /// Media Foundation (GPU decoding) or FFmpeg for playback.
    pub play_engine: PlayEngine,
    /// Sidebar and inspector sections folded shut ("sidebar:標籤", "inspector:色彩"…).
    pub collapsed_sections: Vec<String>,
    pub clip_sort: ClipSort,
    /// The settings window opens on the category it showed last.
    pub settings_page: SettingsPage,
    /// The "FFmpeg is missing" bar over the grid was closed.
    pub ffmpeg_hint_dismissed: bool,
    /// Download a new version by itself and install it when OrangeLink closes.
    pub auto_update: bool,
    /// How long a wheel notch glides in the grid, 0..=100; 0 scrolls in steps.
    pub scroll_smoothness: u32,

    /// Local MCP server for AI assistants (127.0.0.1 only).
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    /// Secret every MCP request must carry (`Authorization: Bearer`); generated on first use.
    pub mcp_token: String,
    /// Let AI tools change tags, folders, ratings, clips etc. Off until the user
    /// turns it on.
    pub mcp_allow_write: bool,
    /// Also let AI tools reach files: import (each import is confirmed in the
    /// window), capture frames, export clips, make subtitles. Needs `mcp_allow_write`.
    pub mcp_allow_files: bool,

    /// yt-dlp downloads: last folder, simple-mode format, what to do afterwards.
    pub ytdlp_dir: Option<PathBuf>,
    pub ytdlp_format: crate::ytdlp::OutFormat,
    pub ytdlp_import: bool,
    /// Import downloads in place (true) or copy them into the library.
    pub ytdlp_link: bool,
    pub ytdlp_chat: bool,
    pub ytdlp_chat_txt: bool,

    /// Where the main window was when it was last closed.
    pub window: Option<WindowGeom>,

    /// Local receiver for the browser extension (127.0.0.1, needs the pairing token).
    pub webclip_enabled: bool,
    pub webclip_port: u16,
    pub webclip_token: String,
    /// Largest file the browser extension may send, in GB.
    pub webclip_max_gb: u64,
    /// When set, only this extension (`chrome-extension://<id>`) may call.
    pub webclip_only_origin: Option<String>,

    /// After an import, offer a prompt for organizing the new items with AI.
    pub ai_import_hint: bool,

    /// Chat words that count extra towards a highlight (laughter and the like).
    pub chat_keywords: Vec<String>,
    /// Seconds kept before and after a highlight peak.
    pub highlight_before: f32,
    pub highlight_after: f32,
    /// At most this many candidates per find.
    pub highlight_max: usize,
    /// Candidates less than this many seconds apart become one.
    pub highlight_merge: f32,
    /// Clip exports: the video encoder and the presets in the clip menu.
    pub video_encoder: VideoEncoder,
    pub export_presets: Vec<ExportPreset>,
    /// 匯出 Premiere XML: also lay the clips out on a sequence, a vertical one,
    /// and the seconds between clips.
    pub premiere_sequence: bool,
    pub premiere_vertical: bool,
    pub premiere_gap: f32,
    /// Burnt-in subtitles: font, size and distance from the bottom (% of the
    /// height), outline (px at 1080 high).
    pub burn_font: String,
    pub burn_size: f32,
    pub burn_margin: f32,
    pub burn_outline: f32,
    /// The search box's list: the last searches, and pinned ones.
    pub recent_searches: Vec<String>,
    pub pinned_searches: Vec<String>,
    /// Drops of more files than this show counts and options first (0 = never).
    pub import_preview_min: usize,

    /// Speech to subtitles: model name and language code.
    pub whisper_model: String,
    pub whisper_language: String,
}

/// Main window placement in logical points; `x`/`y` is the outer (frame) corner,
/// `w`/`h` the inner size. `ppp` is the scale it was saved at.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub maximized: bool,
    pub ppp: f32,
}

impl Config {
    /// Puts the settings of one category back to their defaults. Tokens, the
    /// library list and window placement are left alone.
    pub fn reset_page(&mut self, page: SettingsPage) {
        let d = Config::default();
        match page {
            SettingsPage::General => {
                self.theme = d.theme;
                self.ui_scale = d.ui_scale;
                self.scroll_smoothness = d.scroll_smoothness;
                self.drag_out = d.drag_out;
            }
            SettingsPage::ImportDelete => {
                self.import_mode = d.import_mode;
                self.skip_duplicates = d.skip_duplicates;
                self.import_into_current_folder = d.import_into_current_folder;
                self.watch_interval_min = d.watch_interval_min;
                self.delete_linked_originals = d.delete_linked_originals;
                self.originals_to_recycle_bin = d.originals_to_recycle_bin;
            }
            SettingsPage::Preview => {
                self.volume = d.volume;
                self.media_dblclick_external = d.media_dblclick_external;
                self.viewer_upscale = d.viewer_upscale;
                self.hover_preview = d.hover_preview;
                self.hover_preview_sound = d.hover_preview_sound;
                self.play_engine = d.play_engine;
                self.slideshow_secs = d.slideshow_secs;
            }
            SettingsPage::Highlights => {
                self.chat_keywords = d.chat_keywords;
                self.highlight_before = d.highlight_before;
                self.highlight_after = d.highlight_after;
                self.highlight_max = d.highlight_max;
                self.highlight_merge = d.highlight_merge;
                self.video_encoder = d.video_encoder;
                self.export_presets = d.export_presets;
                self.premiere_sequence = d.premiere_sequence;
                self.premiere_vertical = d.premiere_vertical;
                self.premiere_gap = d.premiere_gap;
                self.burn_font = d.burn_font;
                self.burn_size = d.burn_size;
                self.burn_margin = d.burn_margin;
                self.burn_outline = d.burn_outline;
            }
            SettingsPage::ExternalApps => self.external_apps = d.external_apps,
            SettingsPage::Integration => {
                self.mcp_enabled = d.mcp_enabled;
                self.mcp_port = d.mcp_port;
                self.mcp_allow_write = d.mcp_allow_write;
                self.mcp_allow_files = d.mcp_allow_files;
                self.ai_import_hint = d.ai_import_hint;
                self.hotkey_enabled = d.hotkey_enabled;
                self.hotkey = d.hotkey;
                self.webclip_enabled = d.webclip_enabled;
                self.webclip_port = d.webclip_port;
                self.webclip_max_gb = d.webclip_max_gb;
                self.webclip_only_origin = d.webclip_only_origin;
            }
            SettingsPage::Tools => {
                self.ffmpeg_dir = d.ffmpeg_dir;
                self.whisper_model = d.whisper_model;
                self.whisper_language = d.whisper_language;
            }
            SettingsPage::Advanced => {
                self.auto_update = d.auto_update;
                self.collapsed_sections = d.collapsed_sections;
                self.clip_sort = d.clip_sort;
            }
        }
    }
}

impl WindowGeom {
    /// True when the title bar would land on a connected monitor, so a window
    /// last closed on an unplugged screen doesn't open off-screen.
    pub fn on_screen(&self) -> bool {
        #[cfg(windows)]
        {
            use windows::Win32::{
                Foundation::POINT,
                Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONULL},
            };
            let pt = POINT { x: ((self.x + 80.0) * self.ppp) as i32, y: ((self.y + 12.0) * self.ppp) as i32 };
            #[allow(unsafe_code)]
            // SAFETY: takes a point by value and only returns a handle; nothing is borrowed.
            let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) };
            !monitor.is_invalid()
        }
        #[cfg(not(windows))]
        true
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ExternalApp {
    pub name: String,
    pub path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            last_library: None,
            recent: Vec::new(),
            thumb_size: 180.0,
            view_mode: ViewMode::Grid,
            group_by: crate::grouping::GroupBy::None,
            theme: ThemeSetting::Dark,
            ui_scale: 1.0,
            import_mode: ImportMode::Ask,
            skip_duplicates: true,
            import_into_current_folder: true,
            delete_linked_originals: false,
            originals_to_recycle_bin: true,
            ffmpeg_dir: None,
            media_dblclick_external: false,
            viewer_upscale: false,
            drag_out: true,
            external_apps: Vec::new(),
            slideshow_secs: 3.0,
            watch_interval_min: 1.0,
            volume: 0.8,
            search_all: false,
            hover_preview: true,
            hotkey_enabled: true,
            hotkey: crate::hotkey::DEFAULT.to_string(),
            hover_preview_sound: true,
            play_engine: PlayEngine::Auto,
            scroll_smoothness: 50,
            collapsed_sections: Vec::new(),
            clip_sort: ClipSort::Start,
            settings_page: SettingsPage::General,
            ffmpeg_hint_dismissed: false,
            auto_update: true,
            mcp_enabled: false,
            mcp_port: 47821,
            mcp_token: String::new(),
            mcp_allow_write: false,
            mcp_allow_files: false,
            ytdlp_dir: None,
            ytdlp_format: crate::ytdlp::OutFormat::Mp4,
            ytdlp_import: true,
            ytdlp_link: true,
            ytdlp_chat: true,
            ytdlp_chat_txt: true,
            window: None,
            webclip_enabled: false,
            webclip_port: 47822,
            webclip_token: String::new(),
            webclip_max_gb: crate::webclip::DEFAULT_MAX_GB,
            webclip_only_origin: None,
            ai_import_hint: true,
            chat_keywords: ["草", "www", "笑死", "哈哈", "XD", "lol", "kusa", "8888"].map(String::from).to_vec(),
            highlight_before: 20.0,
            highlight_after: 10.0,
            highlight_max: 20,
            highlight_merge: 10.0,
            premiere_sequence: true,
            premiere_vertical: false,
            premiere_gap: 0.0,
            burn_font: "Microsoft JhengHei".into(),
            burn_size: 4.0,
            burn_margin: 15.0,
            burn_outline: 3.0,
            recent_searches: Vec::new(),
            pinned_searches: Vec::new(),
            import_preview_min: 20,
            video_encoder: VideoEncoder::Auto,
            export_presets: default_export_presets(),
            whisper_model: "small".into(),
            whisper_language: "auto".into(),
        }
    }
}

/// Copies an unreadable config to `config.broken.json` (or a numbered name when that
/// already holds something else). The same bytes already kept aren't copied again.
fn keep_broken(path: &Path, bytes: &[u8]) -> Option<PathBuf> {
    let dir = path.parent()?;
    for n in 0..100 {
        let name = if n == 0 { "config.broken.json".to_string() } else { format!("config.broken-{n}.json") };
        let p = dir.join(name);
        match std::fs::read(&p) {
            Ok(old) if old == bytes => return Some(p),
            Ok(_) => continue,
            Err(_) => return std::fs::write(&p, bytes).ok().map(|_| p),
        }
    }
    None
}

fn config_path() -> Option<PathBuf> {
    // Tests never read or write the user's real config (an App dropped in a
    // test saves its config); they use save_to and load_from with a path.
    if cfg!(test) {
        return None;
    }
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("OrangeLink").join("config.json"))
}

const CONFIG_VERSION: u32 = 3;

/// Why the config file couldn't be read this run, and where the original was kept.
static LOAD_PROBLEM: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Set when config.json existed but couldn't be read; the app says so once.
pub fn load_problem() -> Option<&'static str> {
    LOAD_PROBLEM.get().map(String::as_str)
}

impl Config {
    pub fn load() -> Self {
        let Some(path) = config_path() else { return Self::loaded(Self::default()) };
        let (config, problem) = Self::load_from(&path);
        if let Some(p) = problem {
            crate::log::error("config", &p);
            let _ = LOAD_PROBLEM.set(p);
        }
        config
    }

    /// No file is a first start. A file that doesn't parse (damaged, or written by a
    /// newer version) is copied aside before the defaults are used, since the next
    /// save would replace it: its tokens, libraries and presets stay recoverable.
    fn load_from(path: &Path) -> (Self, Option<String>) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::loaded(Self::default()), None),
            Err(e) => return (Self::loaded(Self::default()), Some(format!("無法讀取設定檔 {}：{e}，這次使用預設值", path.display()))),
        };
        match serde_json::from_slice::<Self>(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)) {
            Ok(c) => (Self::loaded(c), None),
            Err(e) => {
                let kept = keep_broken(path, &bytes);
                let kept = kept.map_or_else(|| "（備份原檔失敗）".to_string(), |k| format!("原檔已保留為 {}", k.display()));
                (Self::loaded(Self::default()), Some(format!("設定檔格式錯誤（{e}），改用預設設定；{kept}")))
            }
        }
    }

    fn loaded(mut config: Self) -> Self {
        config.migrate();
        config
    }

    fn migrate(&mut self) {
        if self.config_version < 2 {
            // v1 shipped with "delete linked originals" on by default; v2 keeps originals.
            self.delete_linked_originals = false;
        }
        if self.config_version < 3 {
            // v3 added in-app playback, so double-click previews media by default.
            self.media_dblclick_external = false;
        }
        self.config_version = CONFIG_VERSION;
    }

    pub fn save(&self) {
        if let Some(path) = config_path() {
            let _ = self.save_to(&path);
        }
    }

    /// Through a temp file, so a crash or power cut never leaves a half-written
    /// config; and not at all when nothing changed (every start saves). Returns
    /// whether it wrote.
    fn save_to(&self, path: &Path) -> anyhow::Result<bool> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::library::write_json_if_changed(path, self)
    }

    pub fn remember(&mut self, lib: &Path) {
        self.last_library = Some(lib.to_path_buf());
        self.recent.retain(|p| p != lib);
        self.recent.insert(0, lib.to_path_buf());
        self.recent.truncate(8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resetting_a_category_leaves_the_others_and_the_tokens() {
        let mut c = Config { volume: 0.1, ui_scale: 1.5, mcp_port: 1234, mcp_token: "secret".into(), ..Config::default() };
        c.reset_page(SettingsPage::Preview);
        assert_eq!(c.volume, Config::default().volume);
        assert_eq!(c.ui_scale, 1.5, "another category is untouched");
        c.reset_page(SettingsPage::Integration);
        assert_eq!(c.mcp_port, Config::default().mcp_port);
        assert_eq!(c.mcp_token, "secret", "tokens are never reset");
        // Every category has a name.
        assert!(SettingsPage::ALL.iter().all(|p| !p.label().is_empty()));
    }

    #[test]
    fn unreadable_config_is_kept_aside() {
        let dir = std::env::temp_dir().join(format!("orangelink-config-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        let (c, problem) = Config::load_from(&path);
        assert!(problem.is_none(), "no file is a first start");
        assert_eq!(c.config_version, CONFIG_VERSION);

        std::fs::write(&path, b"{\"mcp_token\": \"abc\", \"recent\": [").unwrap();
        let (c, problem) = Config::load_from(&path);
        assert!(problem.unwrap().contains("config.broken.json"));
        assert!(c.mcp_token != "abc", "defaults are used");
        assert_eq!(std::fs::read(dir.join("config.broken.json")).unwrap(), std::fs::read(&path).unwrap());
        Config::load_from(&path);
        assert!(!dir.join("config.broken-1.json").exists(), "the same broken file is kept once");

        std::fs::write(&path, b"not json").unwrap();
        Config::load_from(&path);
        assert_eq!(std::fs::read(dir.join("config.broken-1.json")).unwrap(), b"not json", "another broken file doesn't replace the first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_replaces_the_file_whole() {
        let dir = std::env::temp_dir().join(format!("orangelink-config-{}", crate::model::new_id()));
        let path = dir.join("config.json");
        let mut c = Config { webclip_port: 50001, ..Default::default() };
        assert!(c.save_to(&path).unwrap());
        assert!(!c.save_to(&path).unwrap(), "nothing changed: the file is left alone");
        c.webclip_port = 50002;
        assert!(c.save_to(&path).unwrap());
        let back: Config = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.webclip_port, 50002);
        let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("config.json")], "no temp file left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
