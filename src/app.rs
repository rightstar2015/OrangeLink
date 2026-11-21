mod actions;
mod ai_log;
mod file_jobs;
mod filter;
mod highlights;
mod integrity;
pub use integrity::IntegrityFix;
mod history;
pub mod jobs;
mod mcp_tools;
mod pdf_view;
mod scrub;
mod undo;
mod smart_edit;
pub mod state;
mod subtitle_index;
mod watch_folders;
mod whisper_ui;
mod ytdlp_ui;

use crate::{
    config::{Config, ImportMode, ThemeSetting},
    imaging::{self, color_distance},
    importer::{self, ImportEvent, ImportOptions},
    library::{Library, LibraryLock, Originals},
    loader::{Kind, Loader},
    media,
    model::{Item, MediaKind, SavedFilter, WatchFolder},
};
use eframe::egui;
use filter::{in_view, Criteria};
pub use state::{FullImage, InlineEdit, Reply, Service};
pub use highlights::{candidates_ui, is_candidate};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
        Arc,
    },
};

/// See `App::multi_summary`.
#[derive(Default)]
pub struct MultiSummary {
    revision: u64,
    visible_gen: u64,
    selected: HashSet<String>,
    /// Selected items that exist, in the order shown.
    pub ids: Vec<String>,
    pub size: u64,
    /// The rating when all items share it.
    pub rating: Option<u8>,
    /// Tags on every item, in the first item's order.
    pub common_tags: Vec<String>,
    /// Per custom field: the value all items share, or `None` when they differ.
    pub fields: Vec<(String, Option<String>)>,
    pub folders: HashSet<String>,
}

impl MultiSummary {
    pub fn compute(lib: &Library, ids: Vec<String>) -> Self {
        let items: Vec<&Item> = ids.iter().filter_map(|id| lib.items.get(id)).collect();
        let first = items.first();
        let rating = first.map(|f| f.rating).filter(|r| items.iter().all(|i| i.rating == *r));
        let mut tag_count: HashMap<&str, usize> = HashMap::new();
        for it in &items {
            let mut seen = HashSet::new();
            for t in &it.tags {
                if seen.insert(t.as_str()) {
                    *tag_count.entry(t.as_str()).or_default() += 1;
                }
            }
        }
        let common_tags = first
            .map(|f| f.tags.iter().filter(|t| tag_count.get(t.as_str()) == Some(&items.len())).cloned().collect())
            .unwrap_or_default();
        let fields = lib
            .meta
            .fields
            .iter()
            .map(|name| {
                let value = |i: &Item| i.fields.get(name).map_or("", String::as_str).to_string();
                let shared = first.map(|f| value(f)).filter(|v| items.iter().all(|i| value(i) == *v));
                (name.clone(), shared)
            })
            .collect();
        Self {
            size: items.iter().map(|i| i.size).sum(),
            rating,
            common_tags,
            fields,
            folders: items.iter().flat_map(|i| i.folders.iter().cloned()).collect(),
            ids: items.iter().map(|i| i.id.clone()).collect(),
            ..Default::default()
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum View {
    All,
    Uncategorized,
    Untagged,
    Trash,
    Folder(String),
    Kind(MediaKind),
    Smart(String),
    /// A fixed, ordered list of items (similarity / duplicate results).
    /// `groups` are the lengths of consecutive runs shown as groups (empty:
    /// one plain list), each with the item to keep first.
    Ids { label: String, ids: Vec<String>, groups: Vec<usize>, source: IdsSource },
}

/// Where a list of items came from, for the results toolbar.
#[derive(Clone, Debug, PartialEq)]
pub enum IdsSource {
    Plain,
    /// Similar thumbnails: to one item, or grouped across the library.
    Similar(Option<String>),
    /// Byte-identical files.
    Exact,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortKey {
    Added,
    Modified,
    Name,
    FileSize,
    Dimensions,
    Rating,
    LastUsed,
    UseCount,
}

impl SortKey {
    pub const ALL: [SortKey; 8] = [
        SortKey::Added,
        SortKey::Modified,
        SortKey::Name,
        SortKey::FileSize,
        SortKey::Dimensions,
        SortKey::Rating,
        SortKey::LastUsed,
        SortKey::UseCount,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Added => "加入時間",
            SortKey::Modified => "修改時間",
            SortKey::Name => "名稱",
            SortKey::FileSize => "檔案大小",
            SortKey::Dimensions => "尺寸",
            SortKey::Rating => "評分",
            SortKey::LastUsed => "最近使用",
            SortKey::UseCount => "使用次數",
        }
    }
}

/// Drag-and-drop payload when dragging thumbnails onto folders or tags.
pub struct DragIds(pub Vec<String>);

/// Drag-and-drop payload when dragging a folder in the sidebar.
pub struct DragFolder(pub String);

/// Drag-and-drop payload when dragging a smart folder in the sidebar.
pub struct DragSmart(pub String);

/// Everything the grid contents depend on; see `refresh_visible`.
#[derive(Clone, PartialEq)]
struct ViewKey {
    everywhere: bool,
    revision: u64,
    view: View,
    search: String,
    tags: Vec<String>,
    exts: Vec<String>,
    min_rating: u8,
    color: Option<[u8; 3]>,
    /// The shortcut search matches loosely.
    summoned: bool,
    sort: SortKey,
    sort_desc: bool,
    group_by: crate::grouping::GroupBy,
    /// Date groups move at midnight.
    today: chrono::NaiveDate,
}

/// The least recently used entries to drop to get under `budget` bytes, from
/// (id, frame last used, bytes). Entries used in the current frame are on screen and
/// always kept, even past the budget.
fn lru_victims(mut used: Vec<(String, u64, usize)>, frame: u64, budget: usize) -> Vec<String> {
    let mut total: usize = used.iter().map(|u| u.2).sum();
    if total <= budget {
        return Vec::new();
    }
    used.sort_by_key(|u| u.1);
    let mut out = Vec::new();
    for (id, last, bytes) in used {
        if total <= budget || last >= frame {
            break;
        }
        total -= bytes;
        out.push(id);
    }
    out
}

/// Everything the UI wants to change goes through here, so drawing code can
/// stay on `&self` and mutations happen in one place after the frame is laid out.
pub enum Action {
    Click(String, egui::Modifiers),
    /// The Windows right-click menu for these items.
    ShellMenu(Vec<String>),
    /// Show this page of the previewed PDF.
    PdfPage(u32),
    /// Mouse wheel over a PDF page, in points.
    PdfWheel(f32),
    OpenViewer(String),
    /// Preview a video or audio starting at this time, on the subtitles tab.
    /// A subtitle item stands for the video it goes with (a line clicked in
    /// the subtitle file's preview).
    OpenViewerAt(String, f64),
    CloseViewer,
    ViewerStep(i32),
    SelectAll,
    ClearSelection,
    TrashSelected,
    RestoreSelected,
    ConfirmDeleteForever,
    ConfirmEmptyTrash,
    AddToFolder(Vec<String>, String),
    RemoveFromFolder(Vec<String>, String),
    AddTag(Vec<String>, String),
    RemoveTag(Vec<String>, String),
    SetRating(Vec<String>, u8),
    /// (items, field, value); an empty value clears the field.
    SetField(Vec<String>, String, String),
    /// Smart folder for items whose field contains the value (any value when empty).
    FieldSmartFolder(String, String),
    OpenExternal(String),
    ShowInExplorer(String),
    /// File paths of these items, one per line.
    CopyPaths(Vec<String>),
    /// The button on toast `id` was clicked.
    ToastButton(u64),
    /// Undo, if the newest change is still this step.
    UndoStep(u64),
    /// Undo what an AI tool call changed (its undo steps, first..=last seq).
    UndoAi { first: u64, last: u64 },
    /// Allow AI assistants to change the library (and with `files`, use files) until OrangeLink closes.
    GrantMcp { files: bool },
    OpenPath(PathBuf),
    RevealPath(PathBuf),
    /// Edit an item's name in place: in the grid (false) or the inspector (true).
    StartRename(String, bool),
    RenameItem(String, String),
    /// From the item menu: open the preview on the item and run this there, as its
    /// clips panel would.
    Media(String, MediaTask),
    ExportSelected,
    ImportFiles,
    ImportFolder,
    ImportPaths(Vec<PathBuf>),
    SetView(View),
    ToggleTagFilter(String),
    SetColorFilter(Option<[u8; 3]>),
    NewFolder(Option<String>),
    RenameFolder(String),
    ConfirmDeleteFolder(String),
    RenameTag(String),
    ConfirmDeleteTag(String),
    OpenLibraryDialog,
    CreateLibraryDialog,
    OpenLibrary(PathBuf),
    /// Double-click / Enter: preview, or open media files externally (per settings).
    Activate(String),
    StartImport { paths: Vec<PathBuf>, link: bool },
    /// Imports as the 匯入方式 setting says (asking, copying or linking).
    ImportByMode(Vec<PathBuf>),
    /// From the import preview: where the files go and their tags.
    StartImportWith { paths: Vec<PathBuf>, link: bool, folder: Option<String>, tags: Vec<String> },
    RefreshMissing,
    OpenSettings,
    /// The settings window on one category.
    OpenSettingsAt(crate::config::SettingsPage),
    /// FFmpeg through winget, as the download window does.
    InstallFfmpeg,
    /// The shortcuts window (? or F1).
    ShowShortcuts,
    /// Open the prompt dialog offering the newer GitHub release link.
    ShowUpdatePrompt,
    /// Ctrl+C: selected files to the clipboard.
    CopyFiles,
    /// Ctrl+V: import files or an image from the clipboard.
    Paste,
    /// Open items with a program from settings (index into `external_apps`).
    OpenWith(usize, Vec<String>),
    RelinkItem(String),
    RelinkInFolder,
    WatchFoldersDialog,
    ScanWatchFolders,
    SaveSmartFolder,
    RenameSmartFolder(String),
    DeleteSmartFolder(String),
    SetFolderColor(String, Option<[u8; 3]>),
    ToggleFolder(String),
    MoveFolder(String, Option<String>),
    BatchRename(Vec<String>),
    ToggleSlideshow,
    ToggleSmart(String),
    MoveSmart(String, Option<String>),
    UpdateSmartFilter(String),
    /// Opens the conditions editor of a smart folder.
    EditSmartFilter(String),
    /// Relates these items to each other (undoable).
    LinkItems(Vec<String>),
    UnlinkItems(String, String),
    /// Shows an item: selected in the grid (everything shown if it is filtered
    /// out), or in the preview when one is open.
    GoToItem(String),
    /// Save the current filters as a smart folder inside a group (or as a new group).
    NewSmartIn { parent: Option<String>, group: bool },
    RestoreDefaultSmart,
    NewTagGroup(Option<String>),
    RenameTagGroup(String),
    DeleteTagGroup(String),
    ToggleTagGroup(String),
    SetTagGroup(String, Option<String>),
    ExportPremiereXml,
    /// Export exactly these items (right-click menu).
    ExportPremiereXmlFor(Vec<String>),
    RegenerateThumbnails(Vec<String>),
    VolumeChanged,
    Undo,
    Redo,
    /// Save the frame at `pos` seconds of a video as a new image item.
    CaptureFrame(String, f64),
    BackupsDialog,
    FindSimilar(String),
    FindDuplicates,
    /// Byte-identical files, grouped.
    FindExactDuplicates,
    CheckIntegrity,
    /// In the duplicate results: keep one per group (merging the others into it
    /// or not) and trash the rest; only groups with a selected item, or all.
    ResolveDuplicates { merge: bool, only_selected: bool },
    /// Run the search behind the results again.
    RerunResults,
    OpenYtdlp,
    CancelJob(jobs::JobKind),
    /// Folds or unfolds a sidebar or inspector section.
    ToggleSection(String),
    /// The sidebar's tag ＋: focus the inspector's tag box.
    FocusTagInput,
    ShowJob(jobs::JobKind),
    OpenWhisper(Vec<String>),
    ClearAiHint,
    /// (item, start, end, name): mark a clip on a video/audio item.
    AddClip(String, f64, f64, Option<String>),
    /// Back and forward through the views visited.
    NavBack,
    NavForward,
    /// 整場處理: loudness, candidates and subtitles for each, in the background.
    ProcessStreams(Vec<String>),
    /// Clips around the item's chat and loudness peaks.
    FindCandidates(String),
    /// Clips of an item, with a 復原 toast.
    DeleteClips(String, Vec<String>),
    /// Every candidate clip of an item not kept yet.
    DeleteCandidates(String),
    /// A candidate becomes an ordinary clip.
    KeepClip(String, String),
    RenameClip(String, String, String),
    /// (item, clip, start, end): new in and out points.
    SetClipRange(String, String, f64, f64),
    /// Several clips, one after another.
    /// Item id, clip ids, and the export preset (index in the settings).
    ExportClips(String, Vec<String>, usize),
    /// Write a clip to its own file with an export preset (and import it,
    /// when the preset says so). Item id, clip id, preset index.
    ExportClip(String, String, usize),
    PickSubtitle(String),
    NewField,
    RenameField(String),
    ConfirmDeleteField(String),
}

pub enum Dialog {
    FolderName { parent: Option<String>, rename: Option<String>, name: String },
    TagRename { old: String, name: String },
    Confirm { message: String, on_yes: Confirmed },
    /// `dirs`: how many of `paths` are folders, counted once (not every frame:
    /// a disconnected network drive makes each check slow).
    ImportMode { paths: Vec<PathBuf>, dirs: usize, link: bool, remember: bool },
    /// A large drop: counts, and where the files go, before importing.
    ImportPreview { paths: Vec<PathBuf>, link: bool, folder: Option<String>, tags: String, scan: Arc<std::sync::Mutex<crate::importer::ImportScan>> },
    Backups(Vec<(PathBuf, usize)>),
    Unreadable,
    SmartFolderName { rename: Option<String>, name: String, parent: Option<String>, group: bool },
    TagGroupName { rename: Option<String>, name: String, tag: Option<String> },
    BatchRename { ids: Vec<String>, pattern: String, start: usize },
    /// `tags` holds the tag text being typed per watch folder.
    /// `missing`: watch folders not found when the dialog opened, checked then
    /// rather than every frame (an offline network drive stalls each check).
    WatchFolders { link: bool, tags: std::collections::HashMap<PathBuf, String>, missing: std::collections::HashSet<PathBuf> },
    FieldName { rename: Option<String>, name: String },
    /// What 「檢查資源庫」 found; fixes take their part out.
    Integrity(crate::library::Integrity),
    /// A newer signed release was found on GitHub; asks whether to open the download page.
    UpdatePrompt { tag: String },
}

/// Thumbnail textures and the bookkeeping around loading them.
#[derive(Default)]
pub struct ThumbCache {
    tex: HashMap<String, Thumb>,
    pending: HashSet<String>,
    /// When thumbnails started waiting, so a short wait is not shown as a job.
    busy_since: Option<f64>,
    /// Items whose thumbnail could not be made; not asked for again.
    pub failed: HashSet<String>,
    /// Asked for while drawing (`&self`), loaded after the frame.
    wants: RefCell<Vec<String>>,
}

/// The messages in the corner.
#[derive(Default)]
pub struct Toasts {
    list: Vec<Toast>,
    seq: u64,
    /// The pointer is over a toast: their countdown waits.
    pub hovered: Cell<bool>,
    clock: f64,
}

/// What files being imported are to be tied to.
#[derive(Default)]
struct ImportLinks {
    /// Files that came from the web, by `importer::path_key`.
    web_sources: HashMap<String, WebSource>,
    /// Files about to be imported (by source key) and what to relate them to,
    link_to: HashMap<String, LinkTo>,
    /// and the items imported so far per group.
    groups: HashMap<String, Vec<String>>,
    /// Clip exports under way: the file being written and the item it is cut from.
    clips: HashMap<PathBuf, String>,
}

/// The preview's clip list and timeline handles.
#[derive(Default)]
pub struct ClipUi {
    /// Clips selected in the list.
    pub sel: HashSet<String>,
    /// A clip end being dragged on the timeline: (clip, is the out point, time).
    pub drag: Option<(String, bool, f64)>,
    /// The clip a Shift+click counts from.
    pub anchor: Option<String>,
    /// A clip's name being edited in the list.
    pub renaming: Option<InlineEdit>,
}

/// The background searches behind "find similar" and "find exact duplicates".
#[derive(Default)]
struct DupSearch {
    similar_rx: Option<Receiver<SimilarFound>>,
    /// Confirming large files by their contents.
    exact_rx: Option<Receiver<Vec<Vec<String>>>>,
    /// Stops the search under way.
    cancel: Arc<AtomicBool>,
}

/// Marked clips being encoded: the one in FFmpeg, and the ones waiting
/// (item, clip, preset).
#[derive(Default)]
struct ClipExports {
    rx: Option<Receiver<ClipExported>>,
    /// Stops the clip being written (FFmpeg is ended, the partial file removed).
    cancel: Arc<AtomicBool>,
    queue: std::collections::VecDeque<(String, String, crate::config::ExportPreset)>,
}

pub enum Confirmed {
    DeleteForever(Vec<String>),
    EmptyTrash,
    DeleteFolder(String),
    DeleteTag(String),
    DeleteField(String),
    /// Open another library although the current one has changes that could not be saved.
    SwitchLibrary(PathBuf),
    /// Close although changes could not be saved (no update is installed then).
    CloseUnsaved,
    /// An import an AI assistant asked for (MCP `import_files`).
    AiImport { paths: Vec<PathBuf>, link: bool },
    /// Undo an AI change and the changes made after it.
    UndoAi { first: u64, last: u64 },
    /// Create a library where the command line named one, then import the rest.
    CreateLibrary(PathBuf, Vec<PathBuf>),
}

/// What the command line (`orangelink [library] [files or folders...]`) asks for.
#[derive(Debug, PartialEq)]
enum Launch {
    /// No arguments: the last library.
    Last,
    /// A library, and what to import into it.
    Open(PathBuf, Vec<PathBuf>),
    /// Files or folders that aren't a library (the first one too): imported into
    /// the last library. A library is never made inside someone's media folder.
    Import(Vec<PathBuf>),
    /// A path that doesn't exist yet, or an empty folder: a new library there,
    /// after the user says yes (a typo shouldn't make folders).
    AskCreate(PathBuf, Vec<PathBuf>),
}

impl Launch {
    fn from_args(args: Vec<PathBuf>) -> Self {
        let mut args = args.into_iter();
        let Some(first) = args.next() else { return Self::Last };
        let rest: Vec<PathBuf> = args.collect();
        let empty_dir = first.is_dir() && std::fs::read_dir(&first).is_ok_and(|mut d| d.next().is_none());
        if Library::is_library(&first) {
            Self::Open(first, rest)
        } else if !first.exists() || empty_dir {
            Self::AskCreate(first, rest)
        } else {
            Self::Import(std::iter::once(first).chain(rest).collect())
        }
    }
}

/// Playing preview of the hovered item: GIF frames or a small player.
pub struct HoverPreview {
    pub id: String,
    pub frames: Vec<(egui::TextureHandle, f32)>,
    pub start: f64,
    pub player: Option<crate::player::Player>,
    gif_rx: Option<Receiver<Vec<(egui::ColorImage, f32)>>>,
}

/// Subtitles of the item open in the viewer.
pub struct Subtitles {
    pub id: String,
    pub path: Option<PathBuf>,
    pub cues: Vec<crate::media::Cue>,
    pub search: String,
    /// Cue shown last frame, to scroll the list along with playback.
    pub last_cue: Option<usize>,
    /// Cue to mark until playback reaches it, e.g. the line a search found.
    pub focus: Option<usize>,
    /// The list still has to scroll to `focus`; it does so once, then scrolls freely.
    pub scroll_to_focus: bool,
}

impl Subtitles {
    /// Marks `i` and scrolls the list to it once.
    pub fn focus_on(&mut self, i: Option<usize>) {
        self.focus = i;
        self.scroll_to_focus = i.is_some();
    }

    /// Where the list scrolls this frame: the focused line once, otherwise the
    /// playing line when following playback.
    pub fn scroll_target(&mut self, follow: Option<usize>) -> Option<usize> {
        if std::mem::take(&mut self.scroll_to_focus) {
            return self.focus.or(follow);
        }
        follow
    }

    /// The first cue with the most words of `search` in it (`欄位:值` words are skipped).
    pub fn find(&self, search: &str) -> Option<usize> {
        let words = filter::search_words(search);
        let score = |c: &crate::media::Cue| {
            let text = c.text.to_lowercase();
            words.iter().filter(|w| text.contains(w.as_str())).count()
        };
        let best = self.cues.iter().map(score).max().filter(|n| *n > 0)?;
        self.cues.iter().position(|c| score(c) == best)
    }

    /// Index of the cue on screen at `pos`.
    pub fn at(&self, pos: f64) -> Option<usize> {
        let i = self.cues.partition_point(|c| c.start <= pos);
        (i > 0 && pos < self.cues[i - 1].end).then(|| i - 1)
    }
}

/// What the item menu's 影音 submenu can start in the preview.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaTask {
    Loudness,
    Candidates,
    /// Just show the item's clips.
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewerTab {
    Clips,
    Subtitles,
    /// Highlight detection (chat activity, loudness analysis, auto-candidates).
    Highlights,
}

/// A message in the bottom right corner, maybe with a button.
pub struct Toast {
    pub id: u64,
    pub msg: String,
    pub until: f64,
    /// Drawn with a red border, and stays twice as long.
    pub error: bool,
    /// The button's label and what it does.
    pub action: Option<(String, Action)>,
}

/// A message that reports something going wrong.
pub fn looks_like_error(msg: &str) -> bool {
    let first = msg.lines().next().unwrap_or_default();
    ["失敗", "錯誤", "無法", "找不到"].iter().any(|w| first.contains(w))
}

/// Seconds a toast stays, doubled for errors and toasts with a button.
const TOAST_SECS: f64 = 4.0;
/// Toasts shown at once; older ones go first.
const TOAST_MAX: usize = 4;

pub struct Viewer {
    pub id: String,
    /// Multiplier on top of "fit to window".
    pub zoom: f32,
    pub pan: egui::Vec2,
}

struct Thumb {
    tex: egui::TextureHandle,
    /// Its RGBA size, counted against `THUMB_BYTES`.
    bytes: usize,
    last_used: Cell<u64>,
}

/// Thumbnail pixels kept in memory, as RGBA bytes (each also has a copy on the GPU).
/// A screen of the largest tiles is about 100 of them, 50 MB, so this keeps several.
const THUMB_BYTES: usize = 256 << 20;
/// Seconds an item must stay selected in the preview before it is loaded.
const VIEWER_SETTLE: f64 = 0.035;

/// A finished clip export: the written file, whether to import it, and the
/// encoder used; or FFmpeg's error.
type ClipExported = Result<(PathBuf, bool, &'static str), String>;

/// Similarity results: the view's label, the item compared to (if one), and
/// the groups found.
type SimilarFound = (String, Option<String>, Vec<Vec<String>>);
/// A library opened on a background thread, with its lock when one could be made.
type LibOpened = anyhow::Result<(Library, Option<LibraryLock>)>;

/// Splits files that share a partial hash into groups with exactly the same
/// bytes (two or more each). A file that can't be read matches nothing.
fn split_by_contents(files: Vec<(String, PathBuf)>, cancel: &AtomicBool) -> Vec<Vec<String>> {
    let mut classes: Vec<(PathBuf, Vec<String>)> = Vec::new();
    for (id, path) in files {
        if cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        match classes.iter_mut().find(|(p, _)| crate::imaging::same_contents(p, &path).unwrap_or(false)) {
            Some((_, ids)) => ids.push(id),
            None if path.is_file() => classes.push((path, vec![id])),
            None => {}
        }
    }
    classes.into_iter().map(|(_, ids)| ids).filter(|g| g.len() > 1).collect()
}

/// Seconds of no edits before the smart folder counts are redone.
const SMART_COUNT_DELAY: f64 = 0.3;

pub struct App {
    pub config: Config,
    pub lib: Option<Library>,
    loader: Loader,
    pub thumbs: ThumbCache,
    /// When each running job was first seen and how far along, for the time left.
    job_clock: HashMap<jobs::JobKind, (f64, f32)>,
    /// Since when jobs have been running, for the toast when all are done.
    pub jobs_since: Option<f64>,
    /// The inspector's tag box takes the focus next frame.
    pub focus_tag: bool,
    /// The preview's full-size image or GIF frames: idle, loading, ready or failed.
    pub full: FullImage<egui::TextureHandle>,
    frame: u64,
    pub now: f64,

    pub view: View,
    pub search: String,
    pub tag_filter: Vec<String>,
    /// Any of these formats.
    pub ext_filter: Vec<String>,
    pub filter_open: bool,
    /// The custom colour being picked, applied only with 套用.
    pub color_draft: Option<[u8; 3]>,
    /// Formats in the library by kind, and the library revision they were counted at.
    pub format_cache: RefCell<(Option<u64>, crate::ui::FormatGroups)>,
    pub min_rating: u8,
    pub color_filter: Option<[u8; 3]>,
    pub sort: SortKey,
    pub sort_desc: bool,

    pub selected: HashSet<String>,
    anchor: Option<String>,
    pub viewer: Option<Viewer>,
    pub pdf: Option<pdf_view::PdfView>,
    pub visible: Vec<String>,
    /// Runs of `visible` under a header each, when the grid is grouped.
    pub groups: Vec<crate::grouping::Group>,
    pub grid_cols: usize,
    pub grid_scroll: f32,
    pub grid_view_h: f32,
    /// After Ctrl+wheel: the item under the pointer, how far down its row the pointer
    /// was (0..1) and the pointer's height in the viewport, to keep it there.
    pub zoom_anchor: Option<(usize, f32, f32)>,
    pub scroll_to: Option<String>,
    /// Grid scroll offset to go back to (history), applied by the next grid frame.
    pub restore_scroll: Option<f32>,
    /// (when looked up, today's date) for date groups.
    today: (f64, chrono::NaiveDate),
    /// 匯出 Premiere XML asking about sequences, for these items.
    premiere_dialog: Option<Vec<String>>,
    /// A smart folder's conditions being edited: (id, draft).
    smart_edit: Option<(String, SavedFilter)>,
    pub history: history::History,

    /// The import under way, if any (only one runs at a time).
    import: Option<ImportRun>,
    /// Imports requested while another import is still running (drained on finish).
    import_queue: std::collections::VecDeque<ImportRequest>,

    pub toast: Toasts,
    pub dialog: Option<Dialog>,
    pub tag_input: String,
    /// The suggestion picked with the arrow keys under a tag box.
    pub tag_pick: Option<usize>,
    pub tag_search: String,
    /// Custom field values typed in the multi-selection panel, by field name.
    pub field_drafts: HashMap<String, String>,
    pub last_flush: f64,
    pub(crate) acts: RefCell<Vec<Action>>,

    pub show_settings: bool,
    /// What AI tools changed while OrangeLink has been open.
    ai_log: Vec<ai_log::AiEntry>,
    pub show_ai_log: bool,
    /// Permissions granted to AI assistants until OrangeLink closes.
    pub mcp_grant: ai_log::McpGrant,
    /// Slider value for UI scale; copied to the config when the drag ends.
    pub ui_scale_draft: f32,
    pub settings_search: String,
    pub show_shortcuts: bool,
    /// Server settings changed: when to apply them.
    pub settings_apply_at: Option<f64>,
    /// (library revision, count per smart folder id).
    /// Item counts per smart folder and the library revision they are for
    /// (`u64::MAX`: not counted yet).
    pub smart_counts: (u64, HashMap<String, usize>),
    /// The revision the counts are waiting to catch up with, and since when.
    smart_counts_due: (u64, f64),
    egui_ctx: egui::Context,
    /// A library being opened in the background.
    pub lib_loading: Option<(PathBuf, Receiver<LibOpened>)>,
    /// Keeps other OrangeLink windows from opening the same library.
    lib_lock: Option<LibraryLock>,
    /// Imports requested before the library finished loading (command line):
    /// (the library they are for, the paths).
    pending_import: Option<(PathBuf, Vec<PathBuf>)>,
    /// The local servers and the launcher shortcut, each off, running or failed
    /// with the reason (see `state::Service`).
    pub mcp: Service<crate::mcp::McpServer>,
    pub web: Service<crate::webclip::WebServer>,
    /// The registered launcher shortcut and its spec.
    pub hotkey: Service<(String, crate::hotkey::Hotkey)>,
    /// The window came up through the shortcut: Enter / Esc in the search box send it back.
    pub summoned: bool,
    pub focus_search: bool,
    /// What files being imported are to be tied to.
    links: ImportLinks,
    pub player: Option<crate::player::Player>,
    /// Item under the cursor this frame (set while drawing).
    pub hover_now: Cell<Option<String>>,
    /// Natural widths of the toolbar's left and right groups (last frame), used to
    /// decide whether they fit on one row.
    pub toolbar_w: (f32, f32),
    /// Drag-box selection: start point in content coordinates, and the selection
    /// kept underneath it (Ctrl/Shift add to the existing selection).
    pub marquee: Option<(egui::Pos2, HashSet<String>)>,
    /// Content-space rects of the cells drawn this frame (all frames of a drag-box).
    pub cell_rects: RefCell<HashMap<String, egui::Rect>>,
    /// Screen position of the scrolled content's origin, set while drawing cells.
    pub cell_origin: Cell<egui::Pos2>,
    hover_since: (Option<String>, f64),
    pub preview: Option<HoverPreview>,
    /// Undo and redo.
    edits: undo::Edits,
    /// Similarity results: the view's label, the item compared to (if one), and
    /// the groups found.
    /// "Find similar" and "find exact duplicates" running in the background.
    dupes: DupSearch,
    /// Selected files being copied out, video frames being written.
    files: file_jobs::FileJobs,
    /// 「檢查資源庫」 comparing files.
    integrity: Option<integrity::IntegrityRun>,
    /// The last save error while it keeps failing (the status bar shows it).
    pub save_error: Option<String>,
    /// Largest thumbnail-hash distance (of 64 bits) for 可能重複 and for 相似.
    pub similar_dupes: u32,
    pub similar_near: u32,
    /// Resolved ffmpeg location (None = not available).
    pub ffmpeg_dir: Option<PathBuf>,
    /// Set when OS drag-and-drop ended, so egui's stuck button state can be released.
    release_pointer: bool,
    /// Smooth scrolling: where the view took the wheel from last frame, the wheel
    /// taken since, and the glide under way.
    pub wheel_area: Option<(egui::Rect, egui::LayerId)>,
    pub wheel_pending: f32,
    pub glide: Option<crate::ui::Glide>,
    drag_out_pos: egui::Pos2,
    dragged_out: HashSet<PathBuf>,

    visible_key: Option<ViewKey>,
    watch: watch_folders::WatchState,
    subtitle_index: Option<subtitle_index::SubtitleIndex>,
    /// Time of the last slideshow advance; None = slideshow off.
    pub slideshow: Option<f64>,
    v_was_down: bool,
    /// In/out marks for the next clip in the viewer.
    pub mark: (Option<f64>, Option<f64>),
    /// Centre of the 9:16 frame shown over a landscape video, when shown.
    pub crop_frame: Option<f32>,
    /// "+5s" and the like, shown briefly over the video after a key jump.
    pub seek_flash: Option<(String, f64)>,
    /// Clips played from the list start over at their out point.
    pub clip_loop: bool,
    pub subs: Option<Subtitles>,
    /// Chat and loudness curves of the item in the preview.
    pub signals: Option<highlights::Signals>,
    /// 整場處理.
    stream: highlights::StreamJobs,
    /// A 影音 task from the item menu, run once the preview has loaded the item.
    pending_media: Option<(String, MediaTask)>,
    /// An item's name being edited in place: F2 on the grid, or a click on the
    /// inspector's name (`InlineEdit::in_inspector`).
    pub renaming: RefCell<Option<InlineEdit>>,
    scrub: scrub::Scrub,
    /// The previewed item still to load, and when.
    viewer_loading: Option<(String, f64)>,
    /// Where the item being opened should start playing (`OpenViewerAt`).
    viewer_start: Option<(String, f64)>,
    pub sub_overlay: bool,
    pub viewer_tab: ViewerTab,
    /// Marked clips being encoded, and the ones waiting.
    clips: ClipExports,
    /// The preview's clip list and timeline handles.
    pub clip_ui: ClipUi,
    pub yt: ytdlp_ui::YtState,
    whisper: whisper_ui::WhisperState,
    /// Checking for a newer signed release on GitHub (see `UpdateState`).
    pub update: UpdateState,
    /// The user chose to close despite a failed save: let the close through.
    force_close: bool,
    /// New items not yet offered to the AI assistant.
    pub ai_hint: usize,
    /// What the window's title bar was last set to (dark or not).
    title_bar_dark: Option<bool>,
    /// Cached sidebar & inspector counts, keyed by `lib.revision`.
    pub sidebar_cache: RefCell<(Option<u64>, crate::ui::SidebarStats)>,
    /// What the selected items have in common, for the multi-selection inspector
    /// and action bar; rebuilt when the library, selection or shown order changes.
    multi_summary: Option<std::rc::Rc<MultiSummary>>,
    /// Counts recomputations of `visible`, so caches can tell the order changed.
    visible_gen: u64,
    /// Search syntax quick-reference popover open.
    pub syntax_open: bool,
    /// Studio header 🏷 整理 popover open.
    pub viewer_tags_open: bool,
    /// Cached downsampled chat/loudness curves for the timeline activity strip.
    pub signal_plot_cache: crate::ui::SignalPlotCache,
    /// Cached parsed cues when previewing a standalone `.srt` / `.vtt` file.
    pub text_cues_cache: Option<(String, Vec<crate::media::Cue>)>,
    /// Item snapshot captured when starting a 9:16 crop-box drag (for undo).
    pub crop_drag_before: Option<Item>,
    /// Editable draft for `config.chat_keywords` so trailing separators aren't eaten.
    pub chat_keywords_draft: Option<String>,
    /// Whether the currently open modal dialog's text input already received initial focus.
    pub dialog_focused: bool,
    /// A text box had the keyboard at the end of the last frame. egui drops that
    /// focus as soon as Esc arrives, before `handle_keys` can see it was typing.
    typing: bool,
    /// Active keyboard focus item (extends Shift+arrow range from `anchor`).
    caret: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        crate::ui::apply_theme(&cc.egui_ctx);
        crate::player::warm_up();
        let mut app = Self::with_config(&cc.egui_ctx, Config::load());
        app.ffmpeg_dir = media::find_ffmpeg_dir(app.config.ffmpeg_dir.as_deref());
        log_ffmpeg_choice(app.config.ffmpeg_dir.as_deref(), app.ffmpeg_dir.as_deref());
        app.ui_scale_draft = app.config.ui_scale;
        if let Some(p) = crate::config::load_problem() {
            app.toast_error(p.to_string());
        }
        app.sync_mcp();
        app.sync_webclip();
        app.sync_hotkey();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::update::newer_release());
            ctx.request_repaint();
        });
        app.update = UpdateState::Checking(rx);
        app.apply_settings(&cc.egui_ctx);
        // `orangelink [library] [files or folders to import...]`
        match Launch::from_args(std::env::args_os().skip(1).map(PathBuf::from).collect()) {
            Launch::Last => {
                app.open_last_library();
            }
            Launch::Open(path, imports) => {
                if !imports.is_empty() {
                    app.pending_import = Some((path.clone(), imports));
                }
                app.open_library(path);
            }
            Launch::Import(paths) => match app.open_last_library() {
                Some(lib) => app.pending_import = Some((lib, paths)),
                None => app.toast_error("沒有開啟過的資源庫可以匯入：請先建立或開啟資源庫，再匯入這些檔案"),
            },
            Launch::AskCreate(path, imports) => {
                app.open_last_library();
                app.dialog = Some(Dialog::Confirm {
                    message: format!("要在「{}」建立新的資源庫嗎？\n（這個位置還不存在或是空資料夾；按「取消」不會建立任何東西）", path.display()),
                    on_yes: Confirmed::CreateLibrary(path, imports),
                });
            }
        }
        app
    }

    /// The app's state with nothing started: no services, no update check, no
    /// library, FFmpeg not looked up. `new` starts those; tests use this alone.
    pub(crate) fn with_config(ctx: &egui::Context, config: Config) -> Self {
        Self {
            loader: Loader::new(ctx),
            config,
            lib: None,
            thumbs: ThumbCache::default(),
            job_clock: HashMap::new(),
            jobs_since: None,
            focus_tag: false,
            full: FullImage::Idle,
            frame: 0,
            now: 0.0,
            view: View::All,
            search: String::new(),
            tag_filter: Vec::new(),
            ext_filter: Vec::new(),
            filter_open: false,
            color_draft: None,
            format_cache: RefCell::new((None, Vec::new())),
            min_rating: 0,
            color_filter: None,
            sort: SortKey::Added,
            sort_desc: true,
            selected: HashSet::new(),
            anchor: None,
            viewer: None,
            pdf: None,
            visible: Vec::new(),
            grid_cols: 1,
            grid_scroll: 0.0,
            grid_view_h: 0.0,
            zoom_anchor: None,
            scroll_to: None,
            restore_scroll: None,
            premiere_dialog: None,
            today: (f64::NEG_INFINITY, chrono::Local::now().date_naive()),
            smart_edit: None,
            history: Default::default(),
            import: None,
            import_queue: std::collections::VecDeque::new(),
            toast: Toasts::default(),
            dialog: None,
            tag_input: String::new(),
            tag_pick: None,
            tag_search: String::new(),
            field_drafts: HashMap::new(),
            last_flush: 0.0,
            acts: RefCell::new(Vec::new()),
            show_settings: false,
            ai_log: Vec::new(),
            show_ai_log: false,
            mcp_grant: Default::default(),
            ui_scale_draft: 1.0,
            settings_search: String::new(),
            show_shortcuts: false,
            settings_apply_at: None,
            smart_counts: (u64::MAX, HashMap::new()),
            smart_counts_due: (u64::MAX, 0.0),
            egui_ctx: ctx.clone(),
            lib_loading: None,
            lib_lock: None,
            pending_import: None,
            mcp: Service::Off,
            web: Service::Off,
            hotkey: Service::Off,
            summoned: false,
            focus_search: false,
            links: ImportLinks::default(),
            player: None,
            hover_now: Cell::new(None),
            toolbar_w: (0.0, 0.0),
            marquee: None,
            cell_rects: RefCell::new(HashMap::new()),
            cell_origin: Cell::new(egui::Pos2::ZERO),
            hover_since: (None, 0.0),
            preview: None,
            edits: undo::Edits::default(),
            dupes: DupSearch::default(),
            files: file_jobs::FileJobs::default(),
            integrity: None,
            save_error: None,
            similar_dupes: crate::similarity::DUPES,
            similar_near: crate::similarity::NEAR,
            ffmpeg_dir: None,
            release_pointer: false,
            wheel_area: None,
            wheel_pending: 0.0,
            glide: None,
            drag_out_pos: egui::Pos2::ZERO,
            dragged_out: HashSet::new(),
            visible_key: None,
            groups: Vec::new(),
            watch: watch_folders::WatchState::default(),
            subtitle_index: None,
            slideshow: None,
            v_was_down: false,
            mark: (None, None),
            crop_frame: None,
            seek_flash: None,
            clip_loop: false,
            subs: None,
            signals: None,
            stream: highlights::StreamJobs::default(),
            pending_media: None,
            renaming: RefCell::new(None),
            scrub: Default::default(),
            viewer_loading: None,
            viewer_start: None,
            sub_overlay: true,
            viewer_tab: ViewerTab::Clips,
            clips: ClipExports::default(),
            clip_ui: ClipUi::default(),
            yt: Default::default(),
            whisper: Default::default(),
            update: UpdateState::Latest,
            force_close: false,
            ai_hint: 0,
            title_bar_dark: None,
            sidebar_cache: RefCell::new((None, crate::ui::SidebarStats::default())),
            multi_summary: None,
            visible_gen: 0,
            syntax_open: false,
            viewer_tags_open: false,
            signal_plot_cache: crate::ui::SignalPlotCache::default(),
            text_cues_cache: None,
            crop_drag_before: None,
            chat_keywords_draft: None,
            dialog_focused: false,
            typing: false,
            caret: None,
        }
    }

    pub fn act(&self, action: Action) {
        self.acts.borrow_mut().push(action);
    }

    /// A message; one that reads as a failure is shown as an error.
    /// A sidebar or inspector section is unfolded (they all start that way).
    pub fn section_open(&self, key: &str) -> bool {
        !self.config.collapsed_sections.iter().any(|k| k == key)
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        let error = looks_like_error(&msg);
        self.push_toast(msg, error, None);
    }

    pub fn toast_error(&mut self, msg: impl Into<String>) {
        self.push_toast(msg.into(), true, None);
    }

    /// A message with a button that runs `action`.
    pub fn toast_with(&mut self, msg: impl Into<String>, label: &str, action: Action) {
        self.push_toast(msg.into(), false, Some((label.to_string(), action)));
    }

    /// A message with 復原 for the change just made.
    pub fn toast_undo(&mut self, msg: impl Into<String>) {
        match self.edits.undo.last().map(|s| s.seq) {
            Some(seq) => self.toast_with(msg, "復原", Action::UndoStep(seq)),
            None => self.toast(msg),
        }
    }

    fn push_toast(&mut self, msg: String, error: bool, action: Option<(String, Action)>) {
        self.toast.seq += 1;
        let secs = if error || action.is_some() { TOAST_SECS * 2.0 } else { TOAST_SECS };
        self.toast.list.push(Toast { id: self.toast.seq, msg, until: self.now + secs, error, action });
        if self.toast.list.len() > TOAST_MAX {
            self.toast.list.drain(..self.toast.list.len() - TOAST_MAX);
        }
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toast.list
    }

    /// Counts the toasts down, holding while the pointer is over them.
    fn tick_toasts(&mut self, ctx: &egui::Context) {
        let dt = (self.now - self.toast.clock).max(0.0);
        self.toast.clock = self.now;
        if self.toast.hovered.take() {
            for t in &mut self.toast.list {
                t.until += dt;
            }
        }
        let now = self.now;
        self.toast.list.retain(|t| t.until > now);
        if !self.toast.list.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Returns the thumbnail texture if loaded; otherwise queues it for loading.
    pub fn thumb_tex(&self, id: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        match self.thumbs.tex.get(id) {
            Some(t) => {
                t.last_used.set(self.frame);
                Some((t.tex.id(), t.tex.size_vec2()))
            }
            None => {
                if !self.thumbs.failed.contains(id) {
                    self.thumbs.wants.borrow_mut().push(id.to_string());
                }
                None
            }
        }
    }

    /// Selected ids in display order (selected items outside the current view last).
    /// The selection's shared rating, tags and field values, computed once per
    /// change instead of every frame (a select-all of a large library is slow to scan).
    pub fn multi_summary(&mut self) -> std::rc::Rc<MultiSummary> {
        let Some(lib) = &self.lib else { return Default::default() };
        let fresh = self.multi_summary.as_ref().is_some_and(|m| {
            m.revision == lib.revision && m.visible_gen == self.visible_gen && m.selected == self.selected
        });
        if !fresh {
            let ids = self.selected_ids();
            let lib = self.lib.as_ref().expect("checked");
            let mut sum = MultiSummary::compute(lib, ids);
            sum.revision = lib.revision;
            sum.visible_gen = self.visible_gen;
            sum.selected = self.selected.clone();
            self.multi_summary = Some(std::rc::Rc::new(sum));
        }
        self.multi_summary.clone().expect("set above")
    }

    pub fn selected_ids(&self) -> Vec<String> {
        let mut out: Vec<String> = self.visible.iter().filter(|id| self.selected.contains(*id)).cloned().collect();
        if out.len() < self.selected.len() {
            let shown: HashSet<&String> = out.iter().collect();
            let rest: Vec<String> = self.selected.iter().filter(|id| !shown.contains(id)).cloned().collect();
            out.extend(rest);
        }
        out
    }

    pub fn import_running(&self) -> bool {
        self.import.is_some()
    }

    /// The import running is a watch-folder scan (it reports only what arrived).
    pub fn import_is_watch_scan(&self) -> bool {
        self.import.as_ref().is_some_and(|r| r.silent)
    }

    /// Opens a library on a background thread (big libraries take a while);
    /// `poll_library_loading` finishes the switch.
    /// Saves the current library first. If that fails, or background work for
    /// it is still running, the switch waits for the user to choose between
    /// staying and switching anyway.
    fn open_library(&mut self, path: PathBuf) {
        let mut parts = Vec::new();
        if let Some(lib) = &mut self.lib {
            if let Err(e) = lib.flush() {
                crate::log::error("save", format!("切換資源庫前儲存失敗：{e:#}"));
                parts.push(format!("目前資源庫有修改沒能儲存：\n{e:#}\n切換後沒存到的修改會遺失；留下來的話程式會繼續自動重試儲存。"));
            } else {
                lib.backup_if_changed(std::time::Duration::ZERO);
            }
            if self.lib_loading.is_none() {
                let jobs = self.library_jobs();
                if !jobs.is_empty() {
                    let list: String = jobs.iter().map(|j| format!("\n・{j}")).collect();
                    parts.push(format!("這個資源庫還有工作在進行：{list}\n切換會取消這些工作，結果不會寫進另一個資源庫。"));
                }
            }
        }
        if parts.is_empty() {
            return self.switch_library(path);
        }
        self.dialog = Some(Dialog::Confirm {
            message: format!("{}\n\n按「確定」仍要開啟另一個資源庫，按「取消」留在目前的資源庫。", parts.join("\n\n")),
            on_yes: Confirmed::SwitchLibrary(path),
        });
    }

    /// Work started for the open library whose result would be written back
    /// into it, named for the switch prompt.
    fn library_jobs(&self) -> Vec<String> {
        let mut jobs = Vec::new();
        if self.import.is_some() {
            jobs.push("匯入".to_string());
        }
        if let Some(job) = self.ytdlp_job() {
            jobs.push(job.name);
        } else if !self.yt.pending_import.is_empty() {
            jobs.push("匯入 yt-dlp 下載的檔案".to_string());
        }
        if let Some(job) = self.whisper_job() {
            jobs.push(job.name);
        }
        if let Some(job) = self.loudness_job() {
            jobs.push(job.name);
        }
        if let Some(job) = self.stream_job() {
            let left = self.stream.queue.len();
            jobs.push(if left > 0 { format!("{}（還有 {left} 個）", job.name) } else { job.name });
        }
        if self.clips.rx.is_some() {
            let left = self.clips.queue.len();
            jobs.push(if left > 0 { format!("輸出片段（還有 {left} 個等待中）") } else { "輸出片段".to_string() });
        }
        if !self.files.captures.is_empty() {
            jobs.push("擷取畫面".to_string());
        }
        if self.integrity.is_some() {
            jobs.push("檢查資源庫".to_string());
        }
        if self.dupes.similar_rx.is_some() || self.dupes.exact_rx.is_some() {
            jobs.push("找出重複／相似".to_string());
        }
        jobs
    }

    /// Stops the work tied to the library being closed and drops its results,
    /// so nothing finished later lands in the next library by id. Files already
    /// written stay in the old library's folder. The subtitle index restarts
    /// for the new library once it is open.
    fn stop_library_jobs(&mut self) {
        let mut stopped = self.library_jobs();
        if let Some(run) = self.import.take() {
            run.cancel.store(true, Ordering::Relaxed);
        }
        self.import_queue.clear();
        self.cancel_ytdlp();
        self.yt.pending_import.clear();
        self.drop_whisper();
        self.cancel_loudness();
        self.cancel_stream();
        self.signals = None;
        self.pending_media = None;
        self.cancel_clip_queue();
        self.clips.rx = None;
        self.cancel_dup_search();
        // A frame for this library must not be imported into the next one.
        self.cancel_file_jobs(jobs::JobKind::Capture);
        self.files.captures.clear();
        self.cancel_integrity();
        if let Some(job) = self.subtitle_index.take() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        if !stopped.is_empty() {
            stopped.dedup();
            crate::log::info("library", format!("切換資源庫，已取消：{}", stopped.join("、")));
        }
    }

    /// Opens the library used last, if it is still there; returns its path.
    fn open_last_library(&mut self) -> Option<PathBuf> {
        let path = self.config.last_library.clone().filter(|p| Library::is_library(p))?;
        self.open_library(path.clone());
        Some(path)
    }

    /// Opens `path` without saving the current library.
    fn switch_library(&mut self, path: PathBuf) {
        self.stop_library_jobs();
        self.lib = None;
        self.lib_lock = None;
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        let ctx = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let opened = LibraryLock::acquire(&p).and_then(|lock| Ok((Library::open(&p)?, lock)));
            let _ = tx.send(opened);
            ctx.request_repaint();
        });
        self.lib_loading = Some((path, rx));
    }

    /// Tears down the active preview/studio cleanly so Media Foundation / PDF
    /// background threads exit, textures are released, and no stale audio plays.
    pub(super) fn close_viewer(&mut self, scroll_back: bool) {
        if let Some(v) = self.viewer.take() {
            if scroll_back {
                self.scroll_to = Some(v.id);
            }
        }
        self.full.close();
        self.pdf = None;
        self.slideshow = None;
        self.player = None;
        self.subs = None;
        self.signals = None;
        self.viewer_loading = None;
        self.viewer_start = None;
        self.clear_studio();
    }

    /// Workbench state that belongs to the item in the preview: the in/out
    /// marks, the 9:16 frame, the clip list's selection, the timeline thumbnails.
    fn clear_studio(&mut self) {
        self.mark = (None, None);
        self.crop_frame = None;
        self.crop_drag_before = None;
        self.seek_flash = None;
        self.clip_ui = ClipUi::default();
        self.viewer_tags_open = false;
        self.text_cues_cache = None;
        self.signal_plot_cache.clear();
        self.scrub.clear();
    }

    fn poll_library_loading(&mut self) {
        let Some((_, rx)) = &self.lib_loading else { return };
        let result = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(result) => result,
            // Opening panicked: say so instead of showing 正在開啟 forever.
            Reply::Lost => Err(anyhow::anyhow!(state::LOST)),
        };
        let Some((path, _)) = self.lib_loading.take() else { return };
        match result {
            Ok((lib, lock)) => {
                self.close_viewer(false);
                self.lib = Some(lib);
                self.lib_lock = lock;
                // Step numbers keep counting: a 復原 on a toast or in the AI log
                // from the last library must not match a step made in this one.
                self.edits = undo::Edits { seq: self.edits.seq, ..Default::default() };
                self.ai_log.clear();
                self.visible_key = None;
                self.smart_counts = (u64::MAX, HashMap::new());
                self.sidebar_cache.borrow_mut().0 = None;
                self.format_cache.borrow_mut().0 = None;
                self.clear_history();
                self.watch.last_scan = f64::NEG_INFINITY;
                self.watch.scan_queue.clear();
                self.watch.ready.clear();
                self.thumbs.tex.clear();
                self.thumbs.pending.clear();
                self.thumbs.failed.clear();
                self.full.clear();
                self.view = View::All;
                self.selected.clear();
                self.anchor = None;
                self.caret = None;
                self.search.clear();
                self.tag_filter.clear();
                self.ext_filter.clear();
                self.min_rating = 0;
                self.color_filter = None;
                self.filter_open = false;
                self.syntax_open = false;
                self.field_drafts.clear();
                *self.renaming.borrow_mut() = None;
                self.save_error = None;
                self.config.remember(&path);
                self.config.save();
                let unreadable = self.lib.as_ref().map_or(0, |l| l.unreadable.len());
                if unreadable > 0 {
                    self.toast(format!("有 {unreadable} 個素材讀取失敗，可在狀態列查看或從備份還原"));
                }
                if let Some(note) = self.lib.as_mut().and_then(|l| l.recovered.take()) {
                    self.toast_error(note);
                }
                self.start_subtitle_index();
                if let Some((_, paths)) = self.pending_import.take_if(|(lib, _)| *lib == path) {
                    self.act(Action::ImportPaths(paths));
                }
            }
            Err(e) => {
                crate::log::error("library", format!("無法開啟資源庫 {}：{e:#}", path.display()));
                self.toast(format!("無法開啟資源庫：{e:#}"));
            }
        }
    }

    /// A search that ignores the selected folder and tags. The trash stays separate.
    pub fn searching_everywhere(&self) -> bool {
        self.config.search_all && !self.search.trim().is_empty() && self.view != View::Trash
    }

    /// The toolbar's current filters as a savable value.
    pub fn current_filter(&self) -> SavedFilter {
        let everywhere = self.searching_everywhere();
        SavedFilter {
            search: self.search.trim().to_string(),
            tags: if everywhere { Vec::new() } else { self.tag_filter.clone() },
            exts: self.ext_filter.clone(),
            min_rating: self.min_rating,
            color: self.color_filter,
            kind: match &self.view {
                View::Kind(k) if !everywhere => Some(*k),
                _ => None,
            },
            ..Default::default()
        }
    }

    fn view_key(&self) -> ViewKey {
        ViewKey {
            everywhere: self.searching_everywhere(),
            revision: self.lib.as_ref().map(|l| l.revision).unwrap_or(0),
            view: self.view.clone(),
            search: self.search.clone(),
            tags: self.tag_filter.clone(),
            exts: self.ext_filter.clone(),
            min_rating: self.min_rating,
            color: self.color_filter,
            summoned: self.summoned,
            sort: self.sort,
            sort_desc: self.sort_desc,
            group_by: self.config.group_by,
            today: self.today.1,
        }
    }

    /// Whether `k` still describes what the grid shows, compared in place so a
    /// frame where nothing changed copies nothing.
    fn view_key_holds(&self, k: &ViewKey) -> bool {
        k.everywhere == self.searching_everywhere()
            && k.revision == self.lib.as_ref().map(|l| l.revision).unwrap_or(0)
            && k.search == self.search
            && k.tags == self.tag_filter
            && k.exts == self.ext_filter
            && k.min_rating == self.min_rating
            && k.color == self.color_filter
            && k.summoned == self.summoned
            && k.sort == self.sort
            && k.sort_desc == self.sort_desc
            && k.group_by == self.config.group_by
            && k.today == self.today.1
            && k.view == self.view
    }

    /// Recomputes the filtered, sorted list only when something it depends on changed.
    fn refresh_visible(&mut self) {
        // The date (for date groups) is looked up once a minute, not every frame.
        if self.now - self.today.0 >= 60.0 || self.now < self.today.0 {
            self.today = (self.now, chrono::Local::now().date_naive());
        }
        if !self.visible_key.as_ref().is_some_and(|k| self.view_key_holds(k)) {
            (self.visible, self.groups) = self.compute_visible();
            self.visible_key = Some(self.view_key());
            self.visible_gen += 1;
        }
        self.refresh_smart_counts();
    }

    /// Item counts per smart folder, recomputed when the library changes.
    ///
    /// Counting runs every smart folder over every item (and subtitles, for
    /// searches), so it waits until edits pause for `SMART_COUNT_DELAY`: a run
    /// of ratings is counted once. Folded away, the section is not counted at all.
    fn refresh_smart_counts(&mut self) {
        let Some(lib) = &self.lib else { return };
        let rev = lib.revision;
        if self.smart_counts.0 == rev || !self.section_open("sidebar:智慧資料夾") {
            return;
        }
        if self.smart_counts.0 != u64::MAX {
            if self.smart_counts_due.0 != rev {
                self.smart_counts_due = (rev, self.now);
            }
            let wait = SMART_COUNT_DELAY - (self.now - self.smart_counts_due.1);
            if wait > 0.0 {
                self.egui_ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
                return;
            }
        }
        let live: Vec<&Item> = lib.items.values().filter(|i| !i.deleted).collect();
        let counts = lib
            .meta
            .smart_folders
            .iter()
            .filter(|s| !s.group)
            .map(|s| {
                let c = Criteria::for_library(&s.filter, lib);
                (s.id.clone(), live.iter().filter(|it| c.matches(it)).count())
            })
            .collect();
        self.smart_counts = (rev, counts);
    }

    fn compute_visible(&self) -> (Vec<String>, Vec<crate::grouping::Group>) {
        let Some(lib) = &self.lib else { return Default::default() };
        // Summoned by the global shortcut: a plain search matches loosely
        // (letters in order), best match first, then the most recently used.
        let search = self.search.trim();
        if self.summoned && !search.is_empty() && filter::is_plain_search(search) {
            let rest = Criteria::new(&SavedFilter { search: String::new(), ..self.current_filter() });
            let everywhere = self.searching_everywhere();
            let mut v: Vec<(i32, &Item)> = lib
                .items
                .values()
                .filter(|it| in_view(&self.view, everywhere, it) && rest.matches(it))
                .filter_map(|it| filter::fuzzy_item_score(search, it).map(|s| (s, it)))
                .collect();
            v.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.last_used.cmp(&a.1.last_used)).then(a.1.id.cmp(&b.1.id)));
            return (v.into_iter().map(|(_, it)| it.id.clone()).collect(), Vec::new());
        }
        let toolbar = Criteria::new(&self.current_filter());
        let everywhere = self.searching_everywhere();
        let smart = match &self.view {
            View::Smart(id) if !everywhere => lib.smart_folder(id).map(|s| Criteria::for_library(&s.filter, lib)),
            _ => None,
        };
        if let (View::Ids { ids, groups, .. }, false) = (&self.view, everywhere) {
            // Keep the result order (most similar first / duplicates grouped).
            let shown = |id: &String| lib.items.get(id).is_some_and(|it| !it.deleted && toolbar.matches(it));
            if groups.is_empty() {
                return (ids.iter().filter(|id| shown(id)).cloned().collect(), Vec::new());
            }
            // A group left with one item (the others trashed or filtered out) is no longer a group.
            let (mut out, mut runs, mut at) = (Vec::new(), Vec::new(), 0);
            for len in groups {
                let run: Vec<String> = ids[at..(at + len).min(ids.len())].iter().filter(|id| shown(id)).cloned().collect();
                at += len;
                if run.len() > 1 {
                    runs.push(crate::grouping::Group { label: format!("第 {} 組", runs.len() + 1).into(), start: out.len(), len: run.len() });
                    out.extend(run);
                }
            }
            return (out, runs);
        }
        let mut v: Vec<&Item> = lib
            .items
            .values()
            .filter(|it| in_view(&self.view, everywhere, it) && toolbar.matches(it) && smart.as_ref().is_none_or(|s| s.matches(it)))
            .collect();
        // Tie-break on id: HashMap order is random, the grid must not reshuffle.
        match self.sort {
            SortKey::Added => v.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.id.cmp(&b.id))),
            SortKey::Modified => v.sort_by(|a, b| a.modified_at.cmp(&b.modified_at).then(a.id.cmp(&b.id))),
            SortKey::Name => v.sort_by_cached_key(|it| (it.name.to_lowercase(), it.id.clone())),
            SortKey::FileSize => v.sort_by(|a, b| a.size.cmp(&b.size).then(a.id.cmp(&b.id))),
            SortKey::Dimensions => v.sort_by(|a, b| {
                (a.width as u64 * a.height as u64)
                    .cmp(&(b.width as u64 * b.height as u64))
                    .then(a.id.cmp(&b.id))
            }),
            SortKey::Rating => v.sort_by(|a, b| a.rating.cmp(&b.rating).then(a.added_at.cmp(&b.added_at))),
            SortKey::LastUsed => v.sort_by(|a, b| a.last_used.cmp(&b.last_used).then(a.id.cmp(&b.id))),
            SortKey::UseCount => v.sort_by(|a, b| a.use_count.cmp(&b.use_count).then(a.added_at.cmp(&b.added_at))),
        }
        if self.sort_desc {
            v.reverse();
        }
        let groups = crate::grouping::group_items(&mut v, self.config.group_by, chrono::Local::now().date_naive());
        (v.into_iter().map(|it| it.id.clone()).collect(), groups)
    }

    fn poll_loader(&mut self, ctx: &egui::Context) {
        while let Ok(loaded) = self.loader.rx.try_recv() {
            match loaded.kind {
                Kind::Thumb => {
                    self.thumbs.pending.remove(&loaded.id);
                    match loaded.image {
                        _ if loaded.skipped => {} // asked for again if it comes back on screen
                        Some(img) => {
                            let bytes = img.width() * img.height() * 4;
                            let tex = ctx.load_texture(format!("thumb-{}", loaded.id), img, egui::TextureOptions::LINEAR);
                            self.thumbs.tex.insert(loaded.id, Thumb { tex, bytes, last_used: Cell::new(self.frame) });
                        }
                        None => {
                            self.thumbs.failed.insert(loaded.id);
                        }
                    }
                }
                Kind::Full => {
                    let shown = self.viewer.as_ref().is_some_and(|v| v.id == loaded.id);
                    // Textures only for the item still in the preview.
                    let animated = !loaded.frames.is_empty();
                    let frames = if !shown {
                        None
                    } else if animated {
                        Some(
                            loaded
                                .frames
                                .into_iter()
                                .enumerate()
                                .map(|(i, (img, delay))| {
                                    (ctx.load_texture(format!("anim-{}-{i}", loaded.id), img, egui::TextureOptions::LINEAR), delay)
                                })
                                .collect(),
                        )
                    } else {
                        loaded.image.map(|img| vec![(ctx.load_texture(format!("full-{}", loaded.id), img, egui::TextureOptions::LINEAR), 0.0)])
                    };
                    self.full.finish(loaded.id, shown, frames, animated, self.now);
                }
            }
        }
    }

    fn request_full(&mut self, id: &str) {
        if !self.full.needs_request(id) {
            return;
        }
        let Some(lib) = &self.lib else { return };
        let Some(it) = lib.items.get(id) else { return };
        if !imaging::SUPPORTED_EXTS.contains(&it.ext.as_str()) {
            // Not decodable here; the viewer shows the thumbnail or an icon.
            // The previous item's textures are not needed any more.
            self.full.close();
            return;
        }
        self.loader.request_full(id.to_string(), lib.file_path(it));
        self.full.start(id.to_string());
    }

    fn process_thumb_wants(&mut self) {
        let wants = std::mem::take(&mut *self.thumbs.wants.borrow_mut());
        if !wants.is_empty() {
            self.loader.mark_wanted(self.frame, &wants);
        }
        let Some(lib) = &self.lib else { return };
        for id in wants {
            if self.thumbs.pending.contains(&id) || self.thumbs.tex.contains_key(&id) {
                continue;
            }
            if let Some(it) = lib.items.get(&id) {
                self.loader.request_thumb(
                    id.clone(),
                    lib.thumb_path(&id),
                    lib.file_path(it),
                    it.kind,
                    self.ffmpeg_dir.clone(),
                );
                self.thumbs.pending.insert(id);
            }
        }
    }

    fn evict_thumbs(&mut self) {
        let used = self.thumbs.tex.iter().map(|(id, t)| (id.clone(), t.last_used.get(), t.bytes)).collect();
        for id in lru_victims(used, self.frame, THUMB_BYTES) {
            self.thumbs.tex.remove(&id);
        }
    }

    fn poll_import(&mut self) {
        let Some(run) = &self.import else { return };
        let mut events: Vec<ImportEvent> = Vec::new();
        let mut lost = false;
        loop {
            match state::poll(&run.rx) {
                Reply::Pending => break,
                Reply::Done(ev) => events.push(ev),
                Reply::Lost => {
                    lost = true;
                    break;
                }
            }
        }
        let mut finished = false;
        for ev in events {
            // Events are handled one by one (`self` is needed in between), so the
            // run is looked up again for each.
            let Some(run) = self.import.as_mut() else { break };
            match ev {
                ImportEvent::Imported(mut item) => {
                    run.imported += 1;
                    if !run.silent {
                        run.new.push(item.id.clone());
                    }
                    let from = item.source.as_ref().or(item.origin.as_ref()).map(|p| importer::source_key(p));
                    let web = from.as_ref().and_then(|k| self.links.web_sources.get(k));
                    let id = web.is_some().then(|| item.id.clone());
                    if let Some(web) = web {
                        item.url = web.url.clone();
                        item.folders.extend(web.folder.clone());
                        if web.temp.is_some() {
                            item.origin = None; // only a temp file, gone after the import
                        }
                    }
                    let new_id = item.id.clone();
                    let link = from.as_ref().and_then(|k| self.links.link_to.remove(k));
                    if let Some(lib) = &mut self.lib {
                        lib.insert_item(item);
                        if let Some(id) = id {
                            lib.touch(&id);
                        }
                        match link {
                            Some(LinkTo::Item(other)) => {
                                lib.link_items(&new_id, &other);
                            }
                            Some(LinkTo::Group(g)) => {
                                let members = self.links.groups.entry(g).or_default();
                                for m in members.iter() {
                                    lib.link_items(&new_id, m);
                                }
                                members.push(new_id);
                            }
                            None => {}
                        }
                    }
                }
                ImportEvent::Duplicate(path) => {
                    run.duplicates += 1;
                    self.file_duplicate(&path);
                }
                ImportEvent::Failed(path, err) => {
                    crate::log::error("import", format!("{}：{err}", path.display()));
                    run.failed += 1;
                    run.errors.push(format!("{}: {err}", path.display()));
                }
                ImportEvent::Progress { done, total } => run.progress = Some((done, total)),
                ImportEvent::Finished => finished = true,
            }
        }
        // The importer ended without Finished (a panic): end the import with what
        // arrived, instead of blocking every later import.
        if lost && !finished {
            crate::log::error("import", state::LOST);
            if let Some(run) = self.import.as_mut() {
                run.failed += 1;
                run.errors.push(state::LOST.to_string());
            }
            finished = true;
        }
        if finished {
            let Some(run) = self.import.take() else { return };
            self.links.groups.clear();
            let (ok, dup, fail) = (run.imported, run.duplicates, run.failed);
            if self.mcp.is_running() && self.config.ai_import_hint {
                self.ai_hint += ok;
            }
            if run.silent {
                if ok > 0 {
                    self.toast(format!("監看資料夾：新增 {ok} 個項目"));
                }
            } else {
                let cancelled = run.cancel.load(Ordering::Relaxed);
                let mut msg = format!("{}：{ok} 個新項目", if cancelled { "匯入已取消" } else { "匯入完成" });
                if dup > 0 {
                    msg += &format!("，略過 {dup} 個重複");
                }
                if fail > 0 {
                    msg += &format!("，{fail} 個失敗");
                    if let Some(first) = run.errors.first() {
                        msg += &format!("\n{first}");
                    }
                }
                if ok > 0 {
                    let ids = run.new;
                    self.toast_with(msg, "查看", Action::SetView(View::Ids { label: "剛匯入的項目".into(), ids, groups: Vec::new(), source: IdsSource::Plain }));
                } else {
                    self.toast(msg);
                }
            }
            if let Some(next) = self.import_queue.pop_front() {
                let ctx = self.egui_ctx.clone();
                self.start_import(&ctx, next.paths, next.link, next.folder, next.tags, next.silent);
            }
        }
    }

    /// A web file already in the library still goes into the folder the user picked.
    fn file_duplicate(&mut self, path: &std::path::Path) {
        let source = importer::path_key(path).and_then(|k| self.links.web_sources.get(&k));
        let Some(folder) = source.and_then(|s| s.folder.clone()) else { return };
        let Some(lib) = &self.lib else { return };
        let Ok(hash) = crate::imaging::quick_hash(path) else { return };
        let same = |it: &Item| !crate::imaging::is_partial_hash(&hash) || crate::imaging::same_contents(&lib.file_path(it), path).unwrap_or(false);
        let found = lib.items.values().find(|it| !it.deleted && it.hash == hash && same(it)).map(|it| it.id.clone());
        if let Some(id) = found {
            self.edit_items(&[id], |it| {
                if !it.folders.contains(&folder) {
                    it.folders.push(folder.clone());
                }
            });
        }
    }

    fn has_floating_window(&self) -> bool {
        self.show_shortcuts
            || self.show_settings
            || self.show_ai_log
            || self.smart_edit.is_some()
            || self.premiere_dialog.is_some()
            || self.yt.open
            || self.whisper.open
    }

    fn close_top_floating_window(&mut self) -> bool {
        if self.smart_edit.take().is_some() {
            return true;
        }
        if self.premiere_dialog.take().is_some() {
            return true;
        }
        if self.whisper.open {
            self.whisper.open = false;
            return true;
        }
        if self.yt.open {
            self.yt.open = false;
            return true;
        }
        if self.show_shortcuts {
            self.show_shortcuts = false;
            return true;
        }
        if self.show_ai_log {
            self.show_ai_log = false;
            return true;
        }
        if self.show_settings {
            self.show_settings = false;
            self.chat_keywords_draft = None;
            self.config.save();
            return true;
        }
        false
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        use egui::Key;
        // Esc in a text box only leaves the box (the box itself cancels an edit); it
        // doesn't also close the preview, clear the selection or close a window.
        if self.typing && ctx.input(|i| i.key_pressed(Key::Escape)) {
            return;
        }
        // The mouse's side buttons go back and forward even while typing in the search box.
        let (side_back, side_fwd) = ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2)));
        let (alt_back, alt_fwd) = if ctx.wants_keyboard_input() {
            (false, false)
        } else {
            ctx.input(|i| (i.modifiers.alt && i.key_pressed(Key::ArrowLeft), i.modifiers.alt && i.key_pressed(Key::ArrowRight)))
        };
        if !self.has_floating_window() {
            if side_back || alt_back {
                return self.act(Action::NavBack);
            }
            if side_fwd || alt_fwd {
                return self.act(Action::NavForward);
            }
        }
        if ctx.wants_keyboard_input() {
            return;
        }
        // ? (Shift+/ on most layouts, so read as typed text) or F1: the shortcuts window.
        let help = ctx.input(|i| i.key_pressed(Key::F1) || i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if t == "?" || t == "？")));
        if help {
            self.show_shortcuts = !self.show_shortcuts;
            return;
        }
        // Floating windows capture keyboard shortcuts; Esc closes the topmost one.
        if self.has_floating_window() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
                self.close_top_floating_window();
            }
            return;
        }
        // Close open toolbar / studio popovers on Esc before touching selection or viewer.
        if (self.filter_open || self.syntax_open || self.viewer_tags_open)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape))
        {
            self.filter_open = false;
            self.syntax_open = false;
            self.viewer_tags_open = false;
            return;
        }

        let (mods, select_all, delete, open, esc, left, right, up, down, undo, redo) = ctx.input(|i| {
            let cmd = i.modifiers.command;
            (
                i.modifiers,
                cmd && i.key_pressed(Key::A),
                i.key_pressed(Key::Delete),
                i.key_pressed(Key::Enter) || i.key_pressed(Key::Space),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                cmd && !i.modifiers.shift && i.key_pressed(Key::Z),
                cmd && (i.key_pressed(Key::Y) || (i.modifiers.shift && i.key_pressed(Key::Z))),
            )
        });

        // Undo / Redo works everywhere (grid AND preview/clip studio).
        if undo {
            self.act(Action::Undo);
            return;
        }
        if redo {
            self.act(Action::Redo);
            return;
        }

        // egui turns Ctrl+C/V into Copy/Paste events, and drops Ctrl+V entirely when
        // the clipboard holds files or an image, so also watch the physical V key.
        let v_down = key_down(0x56);
        let v_edge = v_down && !self.v_was_down && key_down(0x11);
        self.v_was_down = v_down;
        let (copy, paste) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
            )
        });
        if copy && !self.selected.is_empty() {
            self.act(Action::CopyFiles);
        }
        if (paste || v_edge) && ctx.input(|i| i.focused) {
            self.act(Action::Paste);
        }

        if let Some(v) = &self.viewer {
            // egui drops the focus on Esc before this runs: Esc ends a clip's rename
            // and leaves the preview open.
            if esc && self.clip_ui.renaming.take().is_some() {
                return;
            }
            // Delete removes the clips selected in the list (with 復原).
            if delete && self.viewer_tab == ViewerTab::Clips && !self.clip_ui.sel.is_empty() {
                self.act(Action::DeleteClips(v.id.clone(), self.clip_ui.sel.iter().cloned().collect()));
                return;
            }
            let rating = ctx.input(|i| {
                [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].iter().position(|k| !i.modifiers.command && i.key_pressed(*k))
            });
            if let Some(r) = rating {
                self.act(Action::SetRating(vec![v.id.clone()], r as u8));
            }
            if self.player.as_ref().is_some_and(|p| p.id == v.id) {
                self.media_keys(ctx, mods, esc);
                return;
            }
            let space = ctx.input(|i| i.key_pressed(Key::Space));
            if esc || open || space {
                self.act(Action::CloseViewer);
            }
            // A PDF pages first and moves to the next item past its last page;
            // Ctrl+arrow always changes item.
            let (page_up, page_down) = ctx.input(|i| (i.key_pressed(Key::PageUp), i.key_pressed(Key::PageDown)));
            if (left || up || page_up) && (mods.command || !self.pdf_step(-1)) {
                self.act(Action::ViewerStep(-1));
            }
            if (right || down || page_down) && (mods.command || !self.pdf_step(1)) {
                self.act(Action::ViewerStep(1));
            }
            return;
        }

        if select_all {
            self.act(Action::SelectAll);
        }
        let rating = ctx.input(|i| {
            [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5]
                .iter()
                .position(|k| !i.modifiers.command && i.key_pressed(*k))
        });
        if let (Some(r), false) = (rating, self.selected.is_empty()) {
            self.act(Action::SetRating(self.selected_ids(), r as u8));
        }
        if delete && !self.selected.is_empty() {
            if self.view == View::Trash {
                self.act(Action::ConfirmDeleteForever);
            } else {
                self.act(Action::TrashSelected);
            }
        }
        // Esc that ends a rename (egui has already taken the box's focus) cancels it
        // and keeps the selection.
        if esc && self.renaming.borrow_mut().take().is_none() {
            self.act(Action::ClearSelection);
        }
        if ctx.input(|i| i.key_pressed(Key::F2)) {
            match self.selected_ids().as_slice() {
                [] => {}
                [one] => self.act(Action::StartRename(one.clone(), false)),
                many => self.act(Action::BatchRename(many.to_vec())),
            }
        }
        let current = self
            .caret
            .as_ref()
            .or(self.anchor.as_ref())
            .and_then(|a| self.visible.iter().position(|id| id == a));
        if open {
            if let Some(i) = current {
                self.act(Action::OpenViewer(self.visible[i].clone()));
            }
        }
        let cols = self.grid_cols.max(1) as i64;
        let step = if left { -1 } else if right { 1 } else if up { -cols } else if down { cols } else { 0 };
        if step != 0 {
            self.arrow_step(step, mods);
        }
    }

    /// An arrow key in the grid: moves the caret `step` items (a row is
    /// `grid_cols`). With Shift the anchor stays and the range follows the caret.
    fn arrow_step(&mut self, step: i64, mods: egui::Modifiers) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.caret.as_ref().or(self.anchor.as_ref()).and_then(|a| self.visible.iter().position(|id| id == a));
        let next = match current {
            Some(i) => (i as i64 + step).clamp(0, self.visible.len() as i64 - 1) as usize,
            None => 0,
        };
        let id = self.visible[next].clone();
        let mods = if mods.shift { mods } else { egui::Modifiers::NONE };
        self.act(Action::Click(id.clone(), mods));
        self.scroll_to = Some(id);
    }

    /// Keys while a video or audio plays in the preview: an editor's, not the grid's.
    fn media_keys(&mut self, ctx: &egui::Context, mods: egui::Modifiers, esc: bool) {
        use egui::Key;
        let k = |key: Key| ctx.input(|i| i.key_pressed(key));
        let plain = !mods.command && !mods.alt;
        let (left, right) = (k(Key::ArrowLeft), k(Key::ArrowRight));
        if esc {
            self.act(Action::CloseViewer);
            return;
        }
        // Another item: Ctrl+arrow, PageUp/PageDown, or the up and down arrows.
        let item_step = if (left && mods.command) || k(Key::PageUp) || k(Key::ArrowUp) {
            -1
        } else if (right && mods.command) || k(Key::PageDown) || k(Key::ArrowDown) {
            1
        } else {
            0
        };
        if item_step != 0 {
            self.act(Action::ViewerStep(item_step));
            return;
        }
        let Some(p) = &mut self.player else { return };
        let mut jumped = None;
        if (left || right) && !mods.command {
            let secs = if mods.shift { 1.0 } else { 5.0 };
            let d = if left { -secs } else { secs };
            p.seek_by(d);
            jumped = Some(format!("{}{secs:.0}s", if left { "−" } else { "+" }));
        }
        let (prev_frame, next_frame) = (plain && k(Key::Comma), plain && k(Key::Period));
        if prev_frame || next_frame {
            p.pause();
            let f = p.frame_secs();
            p.seek_by(if prev_frame { -f } else { f });
            jumped = Some(if prev_frame { "−1 格".into() } else { "+1 格".into() });
        }
        // J / K / L as in editing programs: backwards and forwards, faster each press.
        let speed = |r: f64| if r < 0.0 { format!("◀◀ {}×", -r) } else { format!("▶ {r}×") };
        if plain && k(Key::J) {
            match p.backwards() {
                Some(r) => jumped = Some(speed(r)),
                None => {
                    p.seek_by(-5.0);
                    jumped = Some("−5s（FFmpeg 播放引擎不能倒播）".into());
                }
            }
        }
        if plain && k(Key::K) {
            p.stop_and_reset();
        }
        if plain && k(Key::L) {
            let was_playing = p.is_playing() && p.rate() > 0.0;
            let r = p.faster();
            if was_playing && !p.can_change_rate() {
                jumped = Some("FFmpeg 播放引擎不能變速（設定 →「預覽與播放」可改用 Media Foundation）".into());
            } else if r != 1.0 || was_playing {
                jumped = Some(speed(r));
            }
        }
        if plain && k(Key::Space) {
            p.toggle();
        }
        if plain && k(Key::Home) {
            p.seek(0.0);
            jumped = Some("開頭".into());
        }
        if plain && k(Key::End) {
            p.seek((p.duration - p.frame_secs()).max(0.0));
            jumped = Some("結尾".into());
        }
        let pos = p.position();
        let (id, duration, landscape) = (p.id.clone(), p.duration, p.has_frames() && p.is_landscape());
        if plain && k(Key::I) {
            self.mark.0 = Some(pos);
        }
        if plain && k(Key::O) {
            self.mark.1 = Some(pos);
        }
        if plain && k(Key::Enter) && (self.mark.0.is_some() || self.mark.1.is_some()) {
            let (start, end) = clip_from_marks(self.mark, pos, duration);
            self.act(Action::AddClip(id, start, end, None));
        }
        if plain && k(Key::V) && landscape {
            self.crop_frame = if self.crop_frame.is_some() { None } else { Some(0.5) };
        }
        if let Some(text) = jumped {
            self.seek_flash = Some((text, self.now));
            ctx.request_repaint();
        }
    }

    /// Makes the first of `ids` in display order the anchor for Shift-click and arrow keys.
    pub fn anchor_to_first(&mut self, ids: &HashSet<String>) {
        if let Some(first) = self.visible.iter().find(|id| ids.contains(*id)) {
            self.anchor = Some(first.clone());
            self.caret = Some(first.clone());
        }
    }

    fn select_click(&mut self, id: String, mods: egui::Modifiers) {
        if mods.shift {
            let a = self.anchor.as_ref().and_then(|a| self.visible.iter().position(|x| x == a));
            let b = self.visible.iter().position(|x| *x == id);
            if let (Some(a), Some(b)) = (a, b) {
                if !mods.command {
                    self.selected.clear();
                }
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                self.selected.extend(self.visible[lo..=hi].iter().cloned());
                self.caret = Some(id);
                return;
            }
        }
        if mods.command {
            if !self.selected.remove(&id) {
                self.selected.insert(id.clone());
            }
        } else {
            self.selected.clear();
            self.selected.insert(id.clone());
        }
        self.anchor = Some(id.clone());
        self.caret = Some(id);
    }

    /// Applies `f` to items, recording the previous state for undo.
    fn edit_items(&mut self, ids: &[String], mut f: impl FnMut(&mut Item)) {
        let Some(lib) = &self.lib else { return };
        let before: Vec<Item> = ids.iter().filter_map(|id| lib.items.get(id).cloned()).collect();
        self.record_undo(before, false);
        let Some(lib) = &mut self.lib else { return };
        for id in ids {
            if let Some(it) = lib.items.get_mut(id) {
                f(it);
                lib.touch(id);
            }
        }
    }

    /// Keeps the dialogue of a video or audio item searchable.
    fn index_subtitles(&mut self, id: &str, path: &std::path::Path) {
        let text = media::subtitle_search_text(path);
        let Some(lib) = &mut self.lib else { return };
        if let Some(it) = lib.items.get_mut(id).filter(|it| text.is_some() && it.text != text) {
            it.text = text;
            lib.touch(id);
        }
    }

    fn load_subtitles(&mut self, id: &str) {
        let Some(lib) = &self.lib else { return };
        let is_media = lib.items.get(id).is_some_and(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio));
        let path = if is_media { lib.find_subtitle(id) } else { None };
        let cues = path
            .as_deref()
            .and_then(|p| media::read_text(p, 16 << 20))
            .map(|t| media::parse_cues(&t))
            .unwrap_or_default();
        if let Some(p) = &path {
            self.index_subtitles(id, p);
        }
        self.subs = is_media.then(|| Subtitles { id: id.to_string(), path, cues, search: String::new(), last_cue: None, focus: None, scroll_to_focus: false });
    }

    /// Encodes a marked clip to `captures/` on a background thread; it is imported when done.
    /// The export preset at `index` in the settings.
    pub(super) fn export_preset(&self, index: usize) -> Option<crate::config::ExportPreset> {
        self.config.export_presets.get(index).cloned()
    }

    fn export_clip(&mut self, id: &str, cid: &str, preset: crate::config::ExportPreset) {
        if self.clips.rx.is_some() {
            self.clips.queue.push_back((id.to_string(), cid.to_string(), preset));
            self.toast(format!("已加入輸出佇列（{} 個等待中）", self.clips.queue.len()));
            return;
        }
        let (Some(lib), Some(ff)) = (&self.lib, self.ffmpeg_dir.clone()) else {
            self.toast("需要 FFmpeg 才能輸出片段");
            return;
        };
        let Some((it, c)) = lib.items.get(id).and_then(|it| Some((it, it.clips.iter().find(|c| c.id == cid)?))) else {
            return self.toast("要輸出的片段已經被刪除");
        };
        let dir = preset.folder.clone().unwrap_or_else(|| lib.root.join("captures"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.toast(format!("無法建立輸出資料夾 {}：{e}", dir.display()));
        }
        let video = it.kind == MediaKind::Video;
        let vertical = preset.shape == crate::config::ExportShape::Vertical;
        let crop = (vertical && video).then(|| crop_9x16(it.width, it.height, c.crop_x.unwrap_or(0.5))).flatten();
        // Burnt-in subtitles: the cues in the clip, written as a temporary .ass.
        let mut note = "";
        let mut subtitles = None;
        if preset.subtitles && video && matches!(preset.shape, crate::config::ExportShape::Original | crate::config::ExportShape::Vertical) {
            let cues = lib.find_subtitle(id).and_then(|p| media::read_text(&p, 16 << 20)).map(|t| media::parse_cues(&t)).unwrap_or_default();
            let res = if crop.is_some() || (vertical && preset.full_hd) { crate::export::SHORTS } else { (it.width, it.height) };
            let cfg = &self.config;
            let style = crate::export::BurnStyle { font: cfg.burn_font.clone(), size: cfg.burn_size, margin: cfg.burn_margin, outline: cfg.burn_outline };
            match crate::export::ass(&cues, c.start, c.end, res, &style) {
                Some(text) => {
                    let file = std::env::temp_dir().join(format!("orangelink-sub-{}.ass", crate::model::new_id()));
                    match std::fs::write(&file, text) {
                        Ok(()) => subtitles = Some(file),
                        Err(e) => return self.toast(format!("無法寫入暫存字幕檔：{e}")),
                    }
                }
                None if cues.is_empty() => note = "（找不到字幕，輸出不含字幕的版本；可以先「產生字幕」）",
                None => note = "（這段沒有字幕）",
            }
        }
        let job = crate::export::Export {
            input: lib.file_path(it),
            start: c.start,
            end: c.end,
            video,
            src_ext: it.ext.clone(),
            crop,
            preset,
            subtitles,
        };
        let suffix = if job.vertical() { "_9x16" } else { "" };
        let out = unique_path(&dir.join(format!("{}_{}{suffix}.{}", sanitize_file_name(&it.name), sanitize_file_name(&c.name), job.ext())));
        let name = c.name.clone();
        if job.preset.import {
            self.links.clips.insert(out.clone(), id.to_string());
        }
        let encoder = self.config.video_encoder;
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.egui_ctx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.clips.cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = crate::export::run(&ff, encoder, &job, &out, &cancel).map(|enc| (out, job.preset.import, enc));
            if let Some(sub) = &job.subtitles {
                let _ = std::fs::remove_file(sub);
            }
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        self.clips.rx = Some(rx);
        self.toast(format!("正在輸出片段「{name}」…{note}"));
    }

    fn poll_clip_export(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.clips.rx else { return };
        let result = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(result) => result,
            // The next clip in the queue still goes.
            Reply::Lost => Err(state::LOST.to_string()),
        };
        self.clips.rx = None;
        match result {
            Ok((path, import, encoder)) => {
                crate::log::info("clip", format!("{}（{encoder}）", path.display()));
                if import {
                    // Related to the item it was cut from, once imported.
                    if let (Some(src), Some(key)) = (self.links.clips.remove(&path), importer::path_key(&path)) {
                        self.links.link_to.insert(key, LinkTo::Item(src));
                    }
                    self.toast_with("片段輸出完成，匯入中", "在檔案總管中顯示", Action::RevealPath(path.clone()));
                    self.start_import(ctx, vec![path], false, None, Vec::new(), false);
                } else {
                    self.toast_with("片段輸出完成", "在檔案總管中顯示", Action::RevealPath(path));
                }
            }
            Err(e) if e == crate::export::CANCELLED => self.toast("已取消輸出片段"),
            Err(e) => self.toast_error(format!("輸出片段失敗：{e}")),
        }
        // A clip that can't start (deleted, its folder can't be made) says so and
        // the next one goes; nothing else would start the rest of the queue.
        while self.clips.rx.is_none() {
            let Some((id, cid, preset)) = self.clips.queue.pop_front() else { break };
            self.export_clip(&id, &cid, preset);
        }
    }

    /// The clip export under way, for the status bar's jobs list.
    pub(super) fn clip_export_job(&self) -> Option<jobs::BgJob> {
        self.clips.rx.as_ref()?;
        let left = self.clips.queue.len();
        let detail = if left > 0 { format!("還有 {left} 個等待中") } else { "FFmpeg 處理中".to_string() };
        Some(jobs::BgJob::new(jobs::JobKind::ClipExport, "輸出片段", detail, None))
    }

    /// Stops the clip being written and drops the ones waiting.
    /// On closing: cancels the jobs that run other programs (FFmpeg, curl,
    /// yt-dlp) and waits up to `within` for their threads to end them. A program
    /// left running would keep its files open after OrangeLink is gone.
    fn stop_programs(&mut self, within: std::time::Duration) {
        self.cancel_imports();
        self.cancel_ytdlp();
        self.cancel_whisper();
        self.cancel_model_download();
        self.cancel_clip_queue();
        self.cancel_dup_search();
        self.cancel_file_jobs(jobs::JobKind::Capture);
        self.cancel_file_jobs(jobs::JobKind::FileCopy);
        let end = std::time::Instant::now() + within;
        while !(self.clips.rx.as_ref().is_none_or(state::ended) && self.whisper_programs_ended()) {
            if std::time::Instant::now() >= end {
                crate::log::error("exit", "背景工作沒有在時限內結束");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn cancel_clip_queue(&mut self) {
        self.clips.queue.clear();
        self.clips.cancel.store(true, Ordering::Relaxed);
    }

    pub fn clip_exporting(&self) -> bool {
        self.clips.rx.is_some()
    }

    pub fn apply_field_dialog(&mut self, rename: Option<String>, name: String) {
        let Some(lib) = &mut self.lib else { return };
        match rename {
            Some(old) => lib.rename_field(&old, &name),
            None => {
                lib.add_field(&name);
            }
        }
    }

    /// Finds items that look like `target` (or, with None, groups of near-duplicates)
    /// by comparing perceptual hashes of their thumbnails, on a background thread.
    fn start_similarity(&mut self, target: Option<String>) {
        let Some(lib) = &self.lib else { return };
        // Videos are also compared by three frames (with FFmpeg); missing files can't be.
        let items: Vec<crate::similarity::Source> = lib
            .items
            .values()
            .filter(|i| !i.deleted)
            .map(|i| crate::similarity::Source {
                id: i.id.clone(),
                thumb: lib.thumb_path(&i.id),
                video: (i.kind == MediaKind::Video && !lib.missing.contains(&i.id)).then(|| i.duration.map(|d| (lib.file_path(i), d))).flatten(),
            })
            .collect();
        let root = lib.root.clone();
        let ff = self.ffmpeg_dir.clone();
        let label = match &target {
            Some(id) => format!("與「{}」相似", lib.items.get(id).map(|i| i.name.as_str()).unwrap_or("?")),
            None => "可能重複的素材".to_string(),
        };
        let limit = if target.is_some() { self.similar_near } else { self.similar_dupes };
        let ctx = self.egui_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = self.new_dup_search();
        std::thread::spawn(move || {
            // Only thumbnails new or rebuilt (and videos changed) since the last search are read.
            let hashes = crate::similarity::hashes(&root, &items, ff.as_deref(), &cancel);
            let groups = match &target {
                Some(t) => crate::similarity::near(&hashes, t, limit, 300).into_iter().collect(),
                None => crate::similarity::groups(&hashes, limit),
            };
            let _ = tx.send((label, target, groups));
            ctx.request_repaint();
        });
        self.dupes.similar_rx = Some(rx);
        self.toast("正在比對相似的素材…");
    }

    fn poll_similarity(&mut self) {
        let Some(rx) = &self.dupes.similar_rx else { return };
        let (label, target, groups) = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(found) => found,
            Reply::Lost => {
                self.dupes.similar_rx = None;
                return self.toast_error(format!("比對相似的素材失敗：{}", state::LOST));
            }
        };
        self.dupes.similar_rx = None;
        let source = IdsSource::Similar(target.clone());
        // To one item: a plain list, closest first. Across the library: groups.
        if target.is_some() {
            let ids = groups.into_iter().next().unwrap_or_default();
            self.show_results(label, if ids.len() > 1 { vec![ids] } else { Vec::new() }, source, false);
        } else {
            self.show_results(label, groups, source, true);
        }
    }

    /// Items whose files are byte for byte the same, grouped. Grouped by the
    /// content hash taken on import; a large file's hash covers only its start
    /// and end, so those groups are confirmed by comparing the files on a
    /// background thread (files that can't be read are left out).
    fn find_exact_duplicates(&mut self) {
        let Some(lib) = &self.lib else { return };
        let mut by_hash: HashMap<&str, Vec<&Item>> = HashMap::new();
        for it in lib.items.values().filter(|i| !i.deleted && !i.hash.is_empty()) {
            by_hash.entry(it.hash.as_str()).or_default().push(it);
        }
        let mut sure: Vec<Vec<String>> = Vec::new();
        let mut check: Vec<Vec<(String, PathBuf)>> = Vec::new();
        for (hash, g) in by_hash.into_iter().filter(|(_, g)| g.len() > 1) {
            if crate::imaging::is_partial_hash(hash) {
                check.push(g.iter().map(|it| (it.id.clone(), lib.file_path(it))).collect());
            } else {
                sure.push(g.iter().map(|it| it.id.clone()).collect());
            }
        }
        if check.is_empty() {
            return self.show_results("完全相同的檔案".into(), sure, IdsSource::Exact, true);
        }
        let ctx = self.egui_ctx.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = self.new_dup_search();
        std::thread::spawn(move || {
            let mut groups = sure;
            for g in check {
                groups.extend(split_by_contents(g, &cancel));
            }
            let _ = tx.send(groups);
            ctx.request_repaint();
        });
        self.dupes.exact_rx = Some(rx);
        self.toast("正在比對大檔案的內容…");
    }

    /// A fresh cancel flag for a duplicate search; the one before it is stopped.
    fn new_dup_search(&mut self) -> Arc<AtomicBool> {
        self.cancel_dup_search();
        self.dupes.cancel = Arc::default();
        self.dupes.cancel.clone()
    }

    /// The duplicate or similarity search under way, for the status bar's jobs list.
    pub(super) fn dup_search_job(&self) -> Option<jobs::BgJob> {
        let name = if self.dupes.similar_rx.is_some() {
            "比對相似的素材"
        } else if self.dupes.exact_rx.is_some() {
            "比對大檔案的內容"
        } else {
            return None;
        };
        Some(jobs::BgJob::new(jobs::JobKind::Duplicates, name, "", None))
    }

    /// Stops the search; its result is never shown.
    pub(super) fn cancel_dup_search(&mut self) {
        self.dupes.cancel.store(true, Ordering::Relaxed);
        self.dupes.similar_rx = None;
        self.dupes.exact_rx = None;
    }

    fn poll_exact_duplicates(&mut self) {
        let Some(rx) = &self.dupes.exact_rx else { return };
        let groups = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(groups) => groups,
            Reply::Lost => {
                self.dupes.exact_rx = None;
                return self.toast_error(format!("找出完全相同的檔案失敗：{}", state::LOST));
            }
        };
        self.dupes.exact_rx = None;
        self.show_results("完全相同的檔案".into(), groups, IdsSource::Exact, true);
    }

    /// Shows result groups, each led by the item to keep (highest rating, then
    /// added first), the biggest groups first. `grouped` shows them as groups.
    fn show_results(&mut self, label: String, mut groups: Vec<Vec<String>>, source: IdsSource, grouped: bool) {
        let Some(lib) = &self.lib else { return };
        groups.retain(|g| g.len() > 1);
        if groups.is_empty() {
            self.toast(format!("{label}：沒有找到"));
            if matches!(&self.view, View::Ids { source: s, .. } if *s == source) {
                self.view = View::Ids { label, ids: Vec::new(), groups: Vec::new(), source };
            }
            return;
        }
        if grouped {
            for g in &mut groups {
                g.sort_by_key(|id| lib.items.get(id).map(|it| (std::cmp::Reverse(it.rating), it.added_at, it.id.clone())));
            }
            groups.sort_by_key(|g| (std::cmp::Reverse(g.len()), g[0].clone()));
        }
        let total: usize = groups.iter().map(Vec::len).sum();
        self.toast(if grouped { format!("{label}：{} 組、{total} 個項目", groups.len()) } else { format!("{label}：{total} 個項目") });
        let lens = if grouped { groups.iter().map(Vec::len).collect() } else { Vec::new() };
        self.view = View::Ids { label, ids: groups.concat(), groups: lens, source };
        self.selected.clear();
        self.anchor = None;
        self.caret = None;
        self.close_viewer(false);
    }

    /// The groups shown now (after filters), as lists of ids.
    pub(crate) fn result_groups(&self) -> Vec<Vec<String>> {
        self.groups.iter().map(|g| self.visible[g.range()].to_vec()).collect()
    }

    /// Keeps one item of each group and moves the rest to the trash. In a group
    /// with a selected item that one is kept, otherwise the first. With
    /// `merge`, the kept item first takes on the others' tags, folders, notes,
    /// clips, fields and use. One undo step. `only_selected`: just the groups
    /// that have a selected item.
    pub(crate) fn resolve_duplicates(&mut self, merge: bool, only_selected: bool) {
        let Some(lib) = &self.lib else { return };
        let mut new: HashMap<String, Item> = HashMap::new();
        let (mut kept, mut trashed) = (0, 0);
        for g in self.result_groups() {
            let pick = g.iter().find(|id| self.selected.contains(*id)).cloned();
            if only_selected && pick.is_none() {
                continue;
            }
            let keep = pick.unwrap_or_else(|| g[0].clone());
            let Some(mut keeper) = lib.items.get(&keep).cloned() else { continue };
            for id in g.iter().filter(|id| **id != keep) {
                let Some(other) = lib.items.get(id) else { continue };
                if merge {
                    crate::model::merge_into(&mut keeper, other);
                }
                let mut gone = other.clone();
                gone.deleted = true;
                new.insert(id.clone(), gone);
                trashed += 1;
            }
            new.insert(keep, keeper);
            kept += 1;
        }
        if new.is_empty() {
            return self.toast("先在要保留的項目上點一下（每組選一個）");
        }
        let ids: Vec<String> = new.keys().cloned().collect();
        self.edit_items(&ids, |it| {
            if let Some(n) = new.get(&it.id) {
                *it = n.clone();
            }
        });
        self.selected.clear();
        let what = if merge { "合併" } else { "保留" };
        self.toast_undo(format!("{kept} 組各{what}一個，{trashed} 個移到垃圾桶"));
    }

    /// Runs the search the results came from again (after changing the threshold).
    pub(crate) fn rerun_results(&mut self) {
        match &self.view {
            View::Ids { source: IdsSource::Similar(t), .. } => self.start_similarity(t.clone()),
            View::Ids { source: IdsSource::Exact, .. } => self.find_exact_duplicates(),
            _ => {}
        }
    }

    /// Counts items handed to another app (not an undoable edit).
    fn mark_used(&mut self, ids: &[String]) {
        let Some(lib) = &mut self.lib else { return };
        let now = crate::model::now_ms();
        for id in ids {
            if let Some(it) = lib.items.get_mut(id) {
                it.use_count += 1;
                it.last_used = Some(now);
                lib.touch(id);
            }
        }
    }

    /// The preview's item left the grid (deleted, or edited out of the folder shown).
    fn close_viewer_if_hidden(&mut self) {
        if self.viewer.as_ref().is_some_and(|v| !self.visible.contains(&v.id)) {
            self.close_viewer(false);
        }
    }

    /// Clears the folder, search and filters when `id` is not among the items shown,
    /// so it is.
    pub(super) fn show_in_grid(&mut self, id: &str) {
        if !self.visible.iter().any(|v| v == id) {
            self.view = View::All;
            self.search.clear();
            self.tag_filter.clear();
            self.ext_filter.clear();
            self.min_rating = 0;
            self.color_filter = None;
        }
    }

    fn open_viewer_at(&mut self, id: String) {
        // The preview closes when its item is not among those shown (a related
        // item, the video of a subtitle line, a finished stream's 查看).
        self.show_in_grid(&id);
        self.selected.clear();
        self.selected.insert(id.clone());
        // Both: arrow keys after the preview go on from the item paged to.
        self.anchor = Some(id.clone());
        self.caret = Some(id.clone());
        self.preview = None;
        self.player = None;
        self.subs = None;
        self.signals = None;
        self.pdf = None;
        // The last video's 9:16 frame would otherwise be given to the next one's clips.
        self.clear_studio();
        self.viewer = Some(Viewer { id: id.clone(), zoom: 1.0, pan: egui::Vec2::ZERO });
        // Holding an arrow key steps through many items: load only where it stops.
        self.viewer_loading = Some((id, self.now + VIEWER_SETTLE));
        self.egui_ctx.request_repaint_after(std::time::Duration::from_secs_f64(VIEWER_SETTLE));
    }

    /// The expensive part of opening the preview (full image, player, subtitles, curves),
    /// once the item has stayed selected for `VIEWER_SETTLE`.
    fn finish_viewer_open(&mut self) {
        let Some((id, due)) = self.viewer_loading.clone() else { return };
        if self.viewer.as_ref().is_none_or(|v| v.id != id) {
            self.viewer_loading = None;
            return;
        }
        if self.now < due {
            self.egui_ctx.request_repaint_after(std::time::Duration::from_secs_f64(due - self.now));
            return;
        }
        self.viewer_loading = None;
        self.request_full(&id);
        self.player = self.make_player(&id, crate::player::VIEWER_MAX_WIDTH, self.config.volume);
        self.load_subtitles(&id);
        self.load_signals(&id);
        self.open_pdf(&id);
        // Opened from a line of its subtitle file: start there.
        if let Some((_, t)) = self.viewer_start.take_if(|(s, _)| *s == id) {
            if let Some(p) = &mut self.player {
                p.seek(t);
            }
            if let Some(subs) = self.subs.as_mut().filter(|s| s.id == id) {
                subs.focus_on(subs.at(t + 0.001));
                self.viewer_tab = ViewerTab::Subtitles;
            }
            return;
        }
        // Opened from a search that matched the dialogue: start at that line.
        if let Some(subs) = self.subs.as_mut().filter(|s| s.id == id) {
            if let Some(i) = subs.find(&self.search) {
                subs.focus_on(Some(i));
                self.viewer_tab = ViewerTab::Subtitles;
                if let Some(p) = &mut self.player {
                    p.seek(subs.cues[i].start);
                }
            }
        }
    }

    /// A started player for a video/audio item: Media Foundation, or FFmpeg if
    /// it is picked (and found).
    fn make_player(&self, id: &str, max_width: u32, volume: f32) -> Option<crate::player::Player> {
        let lib = self.lib.as_ref()?;
        let it = lib.items.get(id).filter(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio))?;
        let mut p = crate::player::Player::new(
            id.to_string(),
            self.config.play_engine,
            self.ffmpeg_dir.clone(),
            lib.file_path(it),
            it.duration.unwrap_or(0.0),
            it.fps,
            (it.width, it.height),
            it.kind == MediaKind::Video,
            max_width,
        )?;
        p.set_volume(volume);
        p.play();
        Some(p)
    }

    /// Starts/stops the hover preview for the item under the cursor.
    fn update_hover_preview(&mut self, ctx: &egui::Context) {
        let hovered = self.hover_now.take();
        let dragging = egui::DragAndDrop::has_any_payload(ctx);
        if !self.config.hover_preview || self.viewer.is_some() || dragging || hovered.is_none() {
            self.preview = None;
            self.hover_since = (None, 0.0);
            return;
        }
        let id = hovered.expect("checked");
        if self.hover_since.0.as_deref() != Some(id.as_str()) {
            self.hover_since = (Some(id.clone()), self.now);
        }
        if let Some(p) = &mut self.preview {
            if p.id == id {
                if p.gif_rx.is_some() {
                    // A decoder that died leaves the still thumbnail, not a wait forever.
                    if let Some(Ok(frames)) = state::take(&mut p.gif_rx).finished() {
                        p.frames = frames
                            .into_iter()
                            .enumerate()
                            .map(|(i, (img, d))| (ctx.load_texture(format!("hover-{i}"), img, egui::TextureOptions::LINEAR), d))
                            .collect();
                        p.start = self.now;
                    }
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                }
                if let Some(player) = &mut p.player {
                    player.update(ctx);
                }
                if !p.frames.is_empty() {
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                }
                return;
            }
        }
        self.preview = None;
        // Short delay so sweeping the mouse across the grid doesn't start everything.
        let wait = 0.35 - (self.now - self.hover_since.1);
        if wait > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
            return;
        }
        let Some(lib) = &self.lib else { return };
        let Some(it) = lib.items.get(&id) else { return };
        let mut preview = HoverPreview { id: id.clone(), frames: Vec::new(), start: self.now, player: None, gif_rx: None };
        if it.ext == "gif" {
            let path = lib.file_path(it);
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx2 = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(crate::loader::load_gif_frames(&path, Some(360)));
                ctx2.request_repaint();
            });
            preview.gif_rx = Some(rx);
        } else if matches!(it.kind, MediaKind::Video | MediaKind::Audio) {
            let volume = if self.config.hover_preview_sound { self.config.volume } else { 0.0 };
            preview.player = self.make_player(&id, crate::player::PREVIEW_MAX_WIDTH, volume);
        } else {
            return;
        }
        self.preview = Some(preview);
        ctx.request_repaint();
    }

    /// Texture to show for `id` while it is being hover-previewed.
    pub fn preview_tex(&self, id: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        let p = self.preview.as_ref().filter(|p| p.id == id)?;
        if let Some((tex, _)) = state::animation_frame(&p.frames, self.now - p.start) {
            return Some((tex.id(), tex.size_vec2()));
        }
        p.player.as_ref()?.texture.as_ref().map(|t| (t.id(), t.size_vec2()))
    }

    /// Playback progress 0..1 of a hover-previewed video/audio item.
    pub fn preview_progress(&self, id: &str) -> Option<f32> {
        let p = self.preview.as_ref().filter(|p| p.id == id)?.player.as_ref()?;
        (p.duration > 0.0).then(|| (p.position() / p.duration) as f32)
    }

    pub fn apply_volume(&mut self) {
        let v = self.config.volume;
        if let Some(p) = &mut self.player {
            p.set_volume(v);
        }
        let sound = self.config.hover_preview_sound;
        if let Some(p) = self.preview.as_mut().and_then(|p| p.player.as_mut()) {
            p.set_volume(if sound { v } else { 0.0 });
        }
    }

    fn process_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut *self.acts.borrow_mut());
        for action in actions {
            self.apply(ctx, action);
        }
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(lib) = &self.lib else { return Vec::new() };
        self.selected_ids()
            .iter()
            .filter_map(|id| lib.items.get(id))
            .map(|it| lib.file_path(it))
            .filter(|p| p.is_file())
            .collect()
    }

    pub fn apply_tag_group_dialog(&mut self, rename: Option<String>, name: String, tag: Option<String>) {
        let name = name.trim().to_string();
        let Some(lib) = &mut self.lib else { return };
        if name.is_empty() {
            return;
        }
        match rename {
            Some(id) => lib.rename_tag_group(&id, &name),
            None => {
                let id = lib.add_tag_group(&name);
                if let Some(t) = tag {
                    lib.set_tag_group(&t, Some(&id));
                }
            }
        }
    }

    pub fn apply_smart_folder_dialog(&mut self, rename: Option<String>, name: String, parent: Option<String>, group: bool) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let filter = if group { SavedFilter::default() } else { self.current_filter() };
        let Some(lib) = &mut self.lib else { return };
        match rename {
            Some(id) => lib.rename_smart_folder(&id, &name),
            None if group => {
                lib.add_smart_folder(&name, filter, parent, true);
            }
            None => {
                let id = lib.add_smart_folder(&name, filter, parent, false);
                // Show the new smart folder with a clean toolbar.
                self.search.clear();
                self.tag_filter.clear();
                self.ext_filter.clear();
                self.min_rating = 0;
                self.color_filter = None;
                self.view = View::Smart(id);
            }
        }
    }

    /// Applies a rename pattern: `{name}` original name, `{n}` running number.
    /// Returns `(id, old_name, new_name)` for each existing item.
    pub fn batch_rename_preview(&self, ids: &[String], pattern: &str, start: usize) -> Vec<(String, String, String)> {
        let Some(lib) = &self.lib else { return Vec::new() };
        let width = (start + ids.len()).to_string().len().max(2);
        ids.iter()
            .filter_map(|id| lib.items.get(id))
            .enumerate()
            .map(|(i, it)| {
                let n = format!("{:0width$}", start + i);
                (it.id.clone(), it.name.clone(), pattern.replace("{name}", &it.name).replace("{n}", &n))
            })
            .collect()
    }

    pub fn apply_batch_rename(&mut self, ids: Vec<String>, pattern: String, start: usize) {
        let names = self.batch_rename_preview(&ids, &pattern, start);
        if names.is_empty() {
            return;
        }
        let before: Vec<Item> = match &self.lib {
            Some(lib) => names.iter().filter_map(|(id, _, _)| lib.items.get(id).cloned()).collect(),
            None => return,
        };
        self.record_undo(before, false);
        let Some(lib) = &mut self.lib else { return };
        let mut count = 0;
        for (id, _, new) in names {
            if let Some(it) = lib.items.get_mut(&id) {
                it.name = new;
                lib.touch(&id);
                count += 1;
            }
        }
        self.toast_undo(format!("已重新命名 {count} 個項目"));
    }

    fn delete_message(&self, ids: &[String]) -> String {
        let linked = self
            .lib
            .as_ref()
            .map(|l| ids.iter().filter(|id| l.items.get(*id).is_some_and(|i| i.source.is_some())).count())
            .unwrap_or(0);
        delete_message(ids.len(), linked, self.originals_policy())
    }

    pub fn originals_policy(&self) -> Originals {
        originals_policy(&self.config)
    }

    /// Items `delete_forever` left because their metadata.json couldn't go.
    fn report_kept_items(&mut self, kept: usize) {
        if kept > 0 {
            self.toast_error(format!("{kept} 個項目無法刪除（檔案可能正被其他程式使用），仍留在原處；關閉那個程式後再試一次"));
        }
    }

    fn report_delete_failures(&mut self, failed: Vec<PathBuf>) {
        if let Some(first) = failed.first() {
            self.toast(format!("{} 個原始檔案無法刪除（可能正被其他程式使用）：\n{}", failed.len(), first.display()));
        }
    }

    /// Applies theme and UI scale from the config.
    pub fn apply_settings(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.config.theme {
            ThemeSetting::Dark => egui::ThemePreference::Dark,
            ThemeSetting::Light => egui::ThemePreference::Light,
            ThemeSetting::System => egui::ThemePreference::System,
        });
        // Both themes' styles again, so whichever is picked has OrangeLink's colours.
        crate::ui::apply_theme(ctx);
        ctx.set_zoom_factor(self.config.ui_scale.clamp(0.6, 2.0));
    }

    /// Starts, restarts or stops the MCP server to match the settings.
    pub fn sync_mcp(&mut self) {
        self.update_log_secrets();
        if self.config.mcp_enabled && self.config.mcp_token.is_empty() {
            self.config.mcp_token = crate::webclip::new_token();
            self.config.save();
        }
        let c = &self.config;
        let want = c.mcp_enabled.then(|| (c.mcp_port, c.mcp_token.clone()));
        let ctx = self.egui_ctx.clone();
        // The old server is dropped first: that unblocks it and frees the port.
        self.mcp.sync(
            want,
            |m| (m.port, m.token.clone()),
            |(port, token)| {
                crate::mcp::McpServer::start(port, token, ctx, mcp_tools::tool_defs()).map_err(|e| format!("無法在連接埠 {port} 啟動：{e}"))
            },
        );
    }

    /// Starts, restarts or stops the browser extension receiver to match the settings.
    fn update_log_secrets(&self) {
        crate::log::set_secrets([self.config.mcp_token.clone(), self.config.webclip_token.clone()]);
    }

    pub fn sync_webclip(&mut self) {
        self.update_log_secrets();
        if self.config.webclip_enabled && self.config.webclip_token.is_empty() {
            self.config.webclip_token = crate::webclip::new_token();
            self.config.save();
        }
        let c = &self.config;
        let want = c.webclip_enabled.then(|| (c.webclip_port, c.webclip_token.clone()));
        let ctx = self.egui_ctx.clone();
        // The old server is dropped first: that unblocks it and frees the port.
        self.web.sync(
            want,
            |w| (w.port, w.token.clone()),
            |(port, token)| crate::webclip::WebServer::start(port, token, ctx).map_err(|e| format!("無法在連接埠 {port} 啟動：{e}")),
        );
    }

    pub fn sync_hotkey(&mut self) {
        let c = &self.config;
        let want = c.hotkey_enabled.then(|| c.hotkey.trim().to_string());
        let ctx = self.egui_ctx.clone();
        // The old shortcut is unregistered before the new one is registered.
        self.hotkey.sync(
            want,
            |(spec, _)| spec.clone(),
            |spec| match crate::hotkey::Hotkey::register(&spec, move || ctx.request_repaint()) {
                Ok(h) => Ok((spec, h)),
                Err(e) => {
                    crate::log::error("hotkey", &e);
                    Err(e)
                }
            },
        );
    }

    /// The shortcut was pressed: window to the front, search box focused, text selected
    /// so typing replaces it.
    fn poll_hotkey(&mut self, ctx: &egui::Context) {
        if !self.hotkey.running().is_some_and(|(_, h)| h.take()) {
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.summoned = true;
        self.focus_search = true;
        self.selected.clear();
        self.anchor = None;
    }

    /// Enter in the search box: the selection, or else the first result, onto the
    /// clipboard as files (paste into Premiere's project panel or Explorer).
    pub fn copy_search_result(&mut self, ctx: &egui::Context) {
        let ids = if self.selected.is_empty() { self.visible.first().cloned().into_iter().collect() } else { self.selected_ids() };
        let Some(lib) = &self.lib else { return };
        let paths: Vec<PathBuf> = ids.iter().filter_map(|id| lib.items.get(id)).map(|it| lib.file_path(it)).filter(|p| p.is_file()).collect();
        if paths.is_empty() {
            self.toast("沒有符合的素材");
            return;
        }
        if crate::clipboard::copy_files(&paths) {
            self.mark_used(&ids);
            let name = paths[0].file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            self.toast(if paths.len() == 1 { format!("已複製「{name}」") } else { format!("已複製 {} 個檔案", paths.len()) });
            self.send_back(ctx);
        }
    }

    /// After a shortcut search, back to the program the user came from.
    pub fn send_back(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.summoned) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
    }

    fn process_mcp_calls(&mut self) {
        let Some(server) = self.mcp.running() else { return };
        let calls: Vec<crate::mcp::McpCall> = server.rx.try_iter().collect();
        for call in calls {
            // A tool that panics answers with an error instead of closing the window.
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.mcp_call(&call.name, &call.args))).unwrap_or_else(|p| {
                let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                crate::log::error("mcp", format!("{} 發生內部錯誤：{why}", call.name));
                crate::mcp::ToolOutput::error(format!("OrangeLink 執行 {} 時發生內部錯誤（詳見記錄）", call.name))
            });
            let _ = call.reply.send(out);
        }
    }

    pub fn set_ffmpeg_dir(&mut self, dir: Option<PathBuf>) {
        self.config.ffmpeg_dir = dir;
        self.ffmpeg_dir = media::find_ffmpeg_dir(self.config.ffmpeg_dir.as_deref());
        log_ffmpeg_choice(self.config.ffmpeg_dir.as_deref(), self.ffmpeg_dir.as_deref());
        self.thumbs.failed.clear();
    }

    /// When thumbnails are dragged past the window edge, hand the files to the OS
    /// drag-and-drop so they can be dropped into Premiere, Explorer, etc.
    fn maybe_drag_out(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.config.drag_out {
            return;
        }
        let Some(payload) = egui::DragAndDrop::payload::<DragIds>(ctx) else { return };
        let screen = ctx.screen_rect().shrink(4.0);
        let pos = ctx.input(|i| i.pointer.latest_pos());
        if pos.is_some_and(|p| screen.contains(p)) || !left_button_down() {
            return;
        }
        let Some(lib) = &self.lib else { return };
        let items: Vec<&Item> = payload.0.iter().filter_map(|id| lib.items.get(id)).collect();
        let paths: Vec<PathBuf> = items.iter().map(|it| lib.file_path(it)).filter(|p| p.is_file()).collect();
        if paths.is_empty() {
            return;
        }
        let thumb = items.first().map(|it| lib.thumb_path(&it.id)).filter(|p| p.is_file());
        let image = match thumb {
            Some(t) => drag::Image::File(t),
            None => drag::Image::Raw(fallback_drag_image()),
        };
        egui::DragAndDrop::clear_payload(ctx);
        self.dragged_out = paths.iter().cloned().collect();
        let used: Vec<String> = payload.0.clone();
        self.mark_used(&used);
        self.drag_out_pos = pos.unwrap_or(screen.center());
        if let Err(e) = drag::start_drag(frame, drag::DragItem::Files(paths), image, |_, _| {}, drag::Options::default()) {
            self.toast(format!("無法拖曳到外部程式：{e}"));
        }
        self.release_pointer = true;
        ctx.request_repaint();
    }

    pub fn apply_confirmed(&mut self, c: Confirmed) {
        match c {
            Confirmed::SwitchLibrary(path) => {
                if self.lib.as_ref().is_some_and(|l| l.has_unsaved()) {
                    crate::log::error("save", "使用者選擇不儲存，直接切換資源庫");
                }
                return self.switch_library(path);
            }
            Confirmed::CreateLibrary(path, imports) => {
                if let Err(e) = Library::create(&path) {
                    return self.toast_error(format!("建立資源庫失敗：{e:#}"));
                }
                if !imports.is_empty() {
                    self.pending_import = Some((path.clone(), imports));
                }
                return self.open_library(path);
            }
            Confirmed::AiImport { paths, link } => {
                if self.import_running() {
                    self.toast("目前有匯入正在進行，請稍後再請 AI 匯入");
                } else {
                    self.act(Action::StartImport { paths, link });
                }
                return;
            }
            Confirmed::UndoAi { first, last } => return self.undo_ai(first, last, true),
            Confirmed::CloseUnsaved => {
                crate::log::error("save", "使用者選擇不儲存，直接關閉");
                self.force_close = true;
                self.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                return;
            }
            _ => {}
        }
        let policy = self.originals_policy();
        let Some(lib) = &mut self.lib else { return };
        match c {
            Confirmed::DeleteForever(ids) => {
                let failed = lib.delete_forever(&ids, policy);
                let kept = ids.iter().filter(|id| lib.items.contains_key(*id)).count();
                for id in &ids {
                    self.thumbs.tex.remove(id);
                    self.selected.remove(id);
                }
                self.toast(format!("已永久刪除 {} 個項目", ids.len() - kept));
                self.report_delete_failures(failed);
                self.report_kept_items(kept);
            }
            Confirmed::EmptyTrash => {
                let ids: Vec<String> = lib.items.values().filter(|i| i.deleted).map(|i| i.id.clone()).collect();
                let failed = lib.delete_forever(&ids, policy);
                let kept = ids.iter().filter(|id| lib.items.contains_key(*id)).count();
                self.report_delete_failures(failed);
                for id in &ids {
                    self.thumbs.tex.remove(id);
                }
                self.selected.clear();
                self.toast(format!("已清空垃圾桶（{} 個項目）", ids.len() - kept));
                self.report_kept_items(kept);
            }
            Confirmed::DeleteFolder(id) => {
                lib.delete_folder(&id);
                if self.view == View::Folder(id) {
                    self.view = View::All;
                }
            }
            Confirmed::DeleteTag(tag) => {
                lib.delete_tag(&tag);
                self.tag_filter.retain(|t| *t != tag);
            }
            Confirmed::SwitchLibrary(_) | Confirmed::CloseUnsaved | Confirmed::AiImport { .. } | Confirmed::UndoAi { .. } | Confirmed::CreateLibrary(..) => {}
            Confirmed::DeleteField(name) => {
                lib.delete_field(&name);
                self.toast(format!("已刪除欄位「{name}」"));
            }
        }
    }

    pub fn apply_folder_dialog(&mut self, parent: Option<String>, rename: Option<String>, name: String) {
        let name = name.trim();
        let Some(lib) = &mut self.lib else { return };
        if name.is_empty() {
            return;
        }
        match rename {
            Some(id) => lib.rename_folder(&id, name),
            None => {
                lib.add_folder(name, parent);
            }
        }
    }

    pub fn apply_tag_rename(&mut self, old: String, name: String) {
        let name = name.trim().to_string();
        let Some(lib) = &mut self.lib else { return };
        if name.is_empty() || name == old {
            return;
        }
        lib.rename_tag(&old, &name);
        for t in &mut self.tag_filter {
            if *t == old {
                *t = name.clone();
            }
        }
    }

    fn item_path(&self, id: &str) -> Option<PathBuf> {
        let lib = self.lib.as_ref()?;
        lib.items.get(id).map(|it| lib.file_path(it))
    }

    /// Asks how to export the selection as Premiere-importable XML.
    fn export_premiere_xml(&mut self, ids: Vec<String>) {
        let Some(lib) = &self.lib else { return };
        if !ids.iter().any(|id| lib.items.get(id).is_some_and(|it| crate::premiere::is_exportable(lib, it))) {
            self.toast("沒有可匯出的項目：Premiere 只能使用圖片、影片和音訊（字幕、XML 等其他檔案會略過）");
            return;
        }
        self.premiere_dialog = Some(ids);
    }

    /// The options for 匯出 Premiere XML: sequences of the clips or not.
    fn premiere_window(&mut self, ctx: &egui::Context) {
        let Some(ids) = &self.premiere_dialog else { return };
        let Some(lib) = &self.lib else { return };
        let (clips, vertical, count) = (crate::premiere::clip_count(lib, ids, false), crate::premiere::clip_count(lib, ids, true), ids.len());
        let mut cancel = false;
        let mut open = true;
        let mut go = false;
        let c = &mut self.config;
        egui::Window::new("匯出 Premiere 用 XML").open(&mut open).resizable(false).collapsible(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.label(format!("{count} 個項目會放進素材箱（依資源庫資料夾分組），片段是子剪輯。"));
            ui.add_space(4.0);
            ui.add_enabled(clips > 0, egui::Checkbox::new(&mut c.premiere_sequence, format!("同時建立序列：{clips} 個片段依序排在時間軸上")))
                .on_hover_text("每段開頭有 marker，名稱是片段名稱，註解是精華候選的原因");
            ui.add_enabled(vertical > 0, egui::Checkbox::new(&mut c.premiere_vertical, format!("另建 1080×1920 直式序列（{vertical} 個影片片段，套用 9:16 框的位置）")));
            ui.add_enabled_ui(clips > 0 && (c.premiere_sequence || c.premiere_vertical), |ui| {
                ui.horizontal(|ui| {
                    ui.label("片段間隔");
                    ui.selectable_value(&mut c.premiere_gap, 0.0, "無");
                    ui.selectable_value(&mut c.premiere_gap, 1.0, "1 秒");
                });
            });
            if clips == 0 {
                ui.label(egui::RichText::new("選取的項目沒有片段，所以不能建立序列。").small().weak());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                go = ui.button("匯出…").clicked();
                cancel = ui.button("取消").clicked();
            });
        });
        if !open || cancel {
            self.premiere_dialog = None;
        }
        if go {
            if let Some(ids) = self.premiere_dialog.take() {
                let c = &self.config;
                let seq = crate::premiere::Sequences { sequence: c.premiere_sequence && clips > 0, gap: c.premiere_gap as f64, vertical: c.premiere_vertical && vertical > 0 };
                self.write_premiere_xml(ids, seq);
            }
        }
    }

    fn write_premiere_xml(&mut self, ids: Vec<String>, seq: crate::premiere::Sequences) {
        let Some(lib) = &self.lib else { return };
        let usable = ids
            .iter()
            .filter(|id| lib.items.get(*id).is_some_and(|it| crate::premiere::is_exportable(lib, it)))
            .count();
        let skipped = ids.len() - usable;
        if usable == 0 {
            self.toast("沒有可匯出的項目：Premiere 只能使用圖片、影片和音訊（字幕、XML 等其他檔案會略過）");
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("匯出 Premiere 用 XML")
            .add_filter("Final Cut Pro XML", &["xml"])
            .set_file_name(format!("{}.xml", lib.name()))
            .save_file()
        else {
            return;
        };
        let xml = crate::premiere::build_xml(lib, &ids, &lib.name(), seq);
        self.mark_used(&ids);
        match std::fs::write(&path, xml) {
            Ok(_) => {
                let mut msg = format!("已匯出 {usable} 個項目。在 Premiere 選「檔案 → 匯入」並選這個 XML");
                if seq.sequence || seq.vertical {
                    msg += "；序列在專案面板的最下面";
                }
                if skipped > 0 {
                    msg += &format!("\n略過 {skipped} 個 Premiere 無法使用的檔案（字幕、文件或遺失的檔案）");
                }
                self.toast_with(msg, "在檔案總管中顯示", Action::RevealPath(path.clone()));
            }
            Err(e) => self.toast(format!("匯出失敗：{e}")),
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.take_wheel(ctx, raw);
        // OLE's drag loop swallowed the mouse-up; tell egui the button is released.
        if std::mem::take(&mut self.release_pointer) {
            raw.events.push(egui::Event::PointerButton {
                pos: self.drag_out_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            raw.events.push(egui::Event::PointerGone);
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.frame += 1;
        let dark = ctx.style().visuals.dark_mode;
        crate::ui::set_light_theme(!dark);
        // The title bar follows, also when "system" flips with Windows' own setting.
        if self.title_bar_dark != Some(dark) {
            crate::ui::set_title_bar_dark(frame, dark);
            self.title_bar_dark = Some(dark);
        }
        self.now = ctx.input(|i| i.time);
        self.guard_close(ctx);
        self.track_window(ctx);
        self.poll_hotkey(ctx);
        self.poll_loader(ctx);
        self.poll_import();
        self.poll_library_loading();
        self.process_mcp_calls();
        self.poll_similarity();
        self.poll_exact_duplicates();
        self.poll_file_copy();
        self.poll_captures(ctx);
        self.poll_integrity();
        self.poll_clip_export(ctx);
        self.poll_ytdlp(ctx);
        self.poll_webclip();
        self.poll_whisper();
        self.poll_subtitle_index();
        self.finish_viewer_open();
        self.poll_pdf(ctx);
        self.poll_signals();
        self.poll_stream();
        self.run_pending_media();
        self.poll_scrub();
        self.poll_update_check();
        if let Some(p) = &mut self.player {
            p.looping = self.clip_loop;
            p.update(ctx);
        }

        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        // Our own drag-out released back over this window: not an import.
        let own = !dropped.is_empty() && dropped.iter().all(|p| self.dragged_out.contains(p));
        if !dropped.is_empty() {
            self.dragged_out.clear();
        }
        if !dropped.is_empty() && !own && self.lib.is_some() {
            self.act(Action::ImportPaths(dropped));
        }

        if let Some((path, _)) = &self.lib_loading {
            let name = path.display().to_string();
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.4);
                        ui.spinner();
                        ui.label(format!("正在開啟資源庫…\n{name}"));
                    });
                });
            });
        } else if self.lib.is_none() {
            self.welcome_ui(ctx);
        } else {
            self.refresh_visible();
            self.poll_watcher(ctx);
            // With events working, the full rescan is only a fallback for missed ones.
            let mut interval = self.config.watch_interval_min.max(0.25) as f64 * 60.0;
            if self.watch.watcher.is_some() {
                interval = interval.max(600.0);
            }
            if self.now - self.watch.last_scan > interval {
                self.act(Action::ScanWatchFolders);
            }
            if let (Some(last), Some(_)) = (self.slideshow, &self.viewer) {
                let secs = self.config.slideshow_secs.max(0.5) as f64;
                if self.now - last >= secs {
                    self.slideshow = Some(self.now);
                    self.act(Action::ViewerStep(1));
                }
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(secs));
            }
            self.close_viewer_if_hidden();
            self.handle_keys(ctx);
            if self.viewer.is_none() {
                self.top_bar(ctx);
            }
            self.status_bar(ctx);
            if self.viewer.is_some() {
                egui::CentralPanel::default()
                    .frame(egui::Frame::none().fill(crate::ui::tokens().bg_viewer))
                    .show(ctx, |ui| crate::ui::in_dark_viewer(ui, |ui| self.viewer_ui(ui)));
            } else {
                self.sidebar(ctx);
                self.inspector(ctx);
                egui::CentralPanel::default().show(ctx, |ui| self.grid_ui(ui));
            }
            self.drag_overlay(ctx);
        }
        self.update_hover_preview(ctx);
        self.dialogs(ctx);
        self.settings_window(ctx);
        self.ai_log_window(ctx);
        self.shortcuts_window(ctx);
        self.ytdlp_window(ctx);
        self.whisper_window(ctx);
        self.premiere_window(ctx);
        self.smart_edit_window(ctx);
        self.toasts_ui(ctx);

        self.process_actions(ctx);
        self.track_structure();
        self.track_history();
        self.typing = ctx.wants_keyboard_input();
        self.maybe_drag_out(ctx, frame);
        self.process_thumb_wants();
        self.evict_thumbs();
        self.tick_toasts(ctx);

        if self.now - self.last_flush > 2.0 {
            self.last_flush = self.now;
            if let Some(lib) = &mut self.lib {
                if lib.has_unsaved() {
                    match lib.flush() {
                        // Retried every 2 seconds: told once, then shown in the status bar.
                        Err(e) => {
                            let msg = format!("{e:#}");
                            if self.save_error.is_none() {
                                self.toast_error(format!("儲存失敗，會自動重試：{msg}"));
                            }
                            self.save_error = Some(msg);
                        }
                        Ok(()) => {
                            if self.save_error.take().is_some() {
                                self.toast("之前沒存到的修改已經儲存");
                            }
                        }
                    }
                } else {
                    self.save_error = None;
                }
            }
            // The day's newest backup follows the work (see `backup_if_changed`).
            if let (None, Some(lib)) = (&self.save_error, &mut self.lib) {
                lib.backup_if_changed(crate::library::BACKUP_EVERY);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(2));
        }
    }
}

impl App {
    /// Saves before the window closes. If saving fails the close is cancelled
    /// and the user picks between staying (to fix it and retry) and closing anyway.
    fn guard_close(&mut self, ctx: &egui::Context) {
        if self.force_close || !ctx.input(|i| i.viewport().close_requested()) {
            return;
        }
        let Some(lib) = &mut self.lib else { return };
        if let Err(e) = lib.flush() {
            crate::log::error("save", format!("關閉前儲存失敗：{e:#}"));
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Confirm {
                message: format!(
                    "有修改沒能儲存：\n{e:#}\n\n按「確定」仍要關閉，沒存到的修改會遺失。\n按「取消」留在程式中，處理好問題（例如關掉鎖住檔案的程式、清出磁碟空間）後再關閉一次即可重試。"
                ),
                on_yes: Confirmed::CloseUnsaved,
            });
        }
    }

    /// Keeps `config.window` up to date (saved on exit). While maximized or
    /// minimized only the flag changes, so the normal size is kept for restoring.
    fn track_window(&mut self, ctx: &egui::Context) {
        let (outer, inner, maximized, minimized, ppp) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.inner_rect, v.maximized, v.minimized, v.native_pixels_per_point)
        });
        if minimized == Some(true) {
            return;
        }
        let maximized = maximized.unwrap_or(false);
        let mut geom = self.config.window;
        match (geom.as_mut(), outer, inner) {
            (Some(g), _, _) if maximized => g.maximized = true,
            (_, Some(o), Some(r)) if !maximized => {
                geom = Some(crate::config::WindowGeom {
                    x: o.min.x,
                    y: o.min.y,
                    w: r.width(),
                    h: r.height(),
                    maximized: false,
                    ppp: ppp.unwrap_or(1.0),
                });
            }
            _ => {}
        }
        self.config.window = geom;
    }
}

/// Checking for a newer signed version on GitHub Releases.
///
/// ```text
///   Checking ──nothing newer, or no answer──▶ Latest
///      └──newer signed release──▶ Available (asks user to open GitHub Releases)
/// ```
pub enum UpdateState {
    Checking(Receiver<Option<crate::update::Release>>),
    /// This is the newest version (or the check could not tell).
    Latest,
    /// A newer signed release is published on GitHub Releases for manual download.
    Available { release: crate::update::Release },
}

impl UpdateState {
    /// The newer version's tag, when one is available.
    pub fn new_version(&self) -> Option<&str> {
        match self {
            UpdateState::Available { release } => Some(&release.tag),
            UpdateState::Checking(_) | UpdateState::Latest => None,
        }
    }
}

impl App {
    /// The check started with the app came back.
    fn poll_update_check(&mut self) {
        let UpdateState::Checking(rx) = &self.update else { return };
        let found = match state::poll(rx) {
            Reply::Pending => return,
            Reply::Done(found) => found,
            // The checking thread ended without an answer: nothing to offer.
            Reply::Lost => None,
        };
        let prompt_tag = found.as_ref().map(|r| r.tag.clone());
        self.update = match found {
            Some(release) => UpdateState::Available { release },
            None => UpdateState::Latest,
        };
        if let Some(tag) = prompt_tag.filter(|_| self.config.auto_update) {
            if self.dialog.is_none() {
                self.dialog = Some(Dialog::UpdatePrompt { tag });
            } else {
                self.toast_with(format!("偵測到 GitHub 上有新版本 {tag}"), "查看更新", Action::ShowUpdatePrompt);
            }
        }
    }
}

/// An import under way. `App::import` is None when nothing is importing.
///
/// ```text
///   None ──start_import──▶ Some(ImportRun) ──Imported / Duplicate / Failed / Progress──▶ (counts)
///     ▲                         │ cancel: the importer stops early, still sends Finished
///     └──── Finished: toast (查看 the new items), or switching library: dropped ────┘
/// ```
/// A queued import waiting for the active `ImportRun` to finish.
struct ImportRequest {
    paths: Vec<PathBuf>,
    link: bool,
    folder: Option<String>,
    tags: Vec<String>,
    silent: bool,
}

struct ImportRun {
    rx: Receiver<ImportEvent>,
    cancel: Arc<AtomicBool>,
    /// A watch-folder scan: reports only when something new arrived, and its
    /// progress shows only once the importer reports some.
    silent: bool,
    /// (done, total); total 0 while the files are still being counted.
    progress: Option<(usize, usize)>,
    imported: usize,
    duplicates: usize,
    failed: usize,
    errors: Vec<String>,
    /// Items added, for the finished toast's 查看 (not kept for watch scans).
    new: Vec<String>,
}

/// Where an imported file came from on the web.
/// What a file about to be imported gets related to.
enum LinkTo {
    Item(String),
    /// Every other file imported with the same group (one download).
    Group(String),
}

struct WebSource {
    url: String,
    /// Library folder chosen in the browser menu.
    folder: Option<String>,
    /// The received copy in the inbox, removed once imported.
    temp: Option<PathBuf>,
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_programs(std::time::Duration::from_secs(3));
        if let Some(lib) = &mut self.lib {
            if let Err(e) = lib.flush() {
                crate::log::error("save", format!("關閉時儲存失敗：{e:#}"));
            } else {
                // The day's newest backup has everything done in this window.
                lib.backup_if_changed(std::time::Duration::ZERO);
            }
        }
        self.config.save();
    }
}

fn show_in_explorer(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = crate::process::visible("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = path.parent() {
            let _ = open::that_detached(dir);
        }
    }
}

/// Physical key/button state, bypassing egui (which swallows some shortcuts).
#[cfg(windows)]
fn key_down(vkey: i32) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
    }
    // SAFETY: takes an integer by value and returns one; no memory is shared.
    unsafe { GetAsyncKeyState(vkey) as u16 & 0x8000 != 0 }
}

#[cfg(not(windows))]
fn key_down(_vkey: i32) -> bool {
    false
}

/// Whether the left mouse button is physically held. OLE drag-and-drop treats a
/// released button as "drop here", so never start one without it.
fn left_button_down() -> bool {
    key_down(0x01)
}

/// Drag image for items without a thumbnail.
fn fallback_drag_image() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(96, 96, image::Rgba([255, 140, 50, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// Says in the log which FFmpeg is used, and when the one set by hand isn't usable.
fn log_ffmpeg_choice(wanted: Option<&std::path::Path>, found: Option<&std::path::Path>) {
    match (wanted, found) {
        (Some(w), Some(f)) if w != f => crate::log::error("ffmpeg", format!("設定的 FFmpeg 資料夾 {} 找不到 ffmpeg，改用 {}", w.display(), f.display())),
        (Some(w), None) => crate::log::error("ffmpeg", format!("設定的 FFmpeg 資料夾 {} 找不到 ffmpeg，也沒有其他可用的 FFmpeg", w.display())),
        (None, None) => crate::log::info("ffmpeg", "找不到 FFmpeg，影片縮圖與播放無法使用"),
        (_, Some(f)) => crate::log::info("ffmpeg", format!("使用 {}", f.display())),
    }
}

/// What deleting a linked item for good does to its original, by the settings.
fn originals_policy(config: &Config) -> Originals {
    match (config.delete_linked_originals, config.originals_to_recycle_bin) {
        (false, _) => Originals::Keep,
        (true, true) => Originals::RecycleBin,
        (true, false) => Originals::Erase,
    }
}

/// The question before deleting `count` items for good, `linked` of them kept
/// in place: it says what happens to those originals.
fn delete_message(count: usize, linked: usize, policy: Originals) -> String {
    let mut msg = format!("永久刪除 {count} 個項目？此動作無法復原。");
    if linked > 0 {
        msg += &match policy {
            Originals::Keep => format!("\n\n其中 {linked} 個是「保留在原位置」的檔案：只會從資源庫移除，硬碟上的原始檔案會保留。"),
            Originals::RecycleBin => format!("\n\n⚠ 其中 {linked} 個是「保留在原位置」的檔案：原始檔案會被移到 Windows 資源回收筒。"),
            Originals::Erase => format!("\n\n⚠ 其中 {linked} 個是「保留在原位置」的檔案：原始檔案會從硬碟上直接刪除，無法還原！"),
        };
    }
    msg
}

/// Width and left edge in pixels of a full-height 9:16 crop centred at `centre` (0..1 of
/// the width), kept inside the frame. None for a frame that isn't wider than 9:16.
pub fn crop_9x16(width: u32, height: u32, centre: f32) -> Option<(u32, u32)> {
    let w = ((height as f64 * 9.0 / 16.0 / 2.0).round() * 2.0) as u32;
    if width == 0 || w == 0 || w >= width {
        return None;
    }
    let x = (centre.clamp(0.0, 1.0) as f64 * width as f64 - w as f64 / 2.0).round().clamp(0.0, (width - w) as f64) as u32;
    Some((w, x))
}

fn sanitize_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    // Windows drops trailing dots and spaces, so "a ." would become "a ".
    let s = s.trim().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() { "image".into() } else { s }
}

/// `a.png` -> `a (2).png` -> `a (3).png` ... until the path is free.
fn unique_path(path: &std::path::Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

/// The clip the in/out marks make: from the in mark (or the start) to the out mark,
/// or else to the playhead when it is past the in mark, or else to the end.
pub fn clip_from_marks(mark: (Option<f64>, Option<f64>), pos: f64, duration: f64) -> (f64, f64) {
    let start = mark.0.unwrap_or(0.0);
    let end = mark.1.unwrap_or(if pos > start + 0.05 { pos } else { duration });
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deleting_for_good_says_what_happens_to_originals() {
        let c = |linked, recycle| Config { delete_linked_originals: linked, originals_to_recycle_bin: recycle, ..Config::default() };
        assert!(originals_policy(&c(false, true)) == Originals::Keep, "kept unless asked");
        assert!(originals_policy(&c(false, false)) == Originals::Keep);
        assert!(originals_policy(&c(true, true)) == Originals::RecycleBin);
        assert!(originals_policy(&c(true, false)) == Originals::Erase);

        assert_eq!(delete_message(3, 0, Originals::Erase), "永久刪除 3 個項目？此動作無法復原。", "nothing about originals when none is linked");
        assert!(delete_message(3, 2, Originals::Keep).contains("原始檔案會保留"));
        assert!(delete_message(3, 2, Originals::RecycleBin).contains("資源回收筒"));
        let erase = delete_message(3, 2, Originals::Erase);
        assert!(erase.contains('⚠') && erase.contains("其中 2 個") && erase.contains("無法還原"), "{erase}");
    }

    /// A media folder or a typo on the command line doesn't become a library.
    #[test]
    fn the_command_line_never_makes_a_library_unasked() {
        let root = std::env::temp_dir().join(format!("orangelink-launch-{}", crate::model::new_id()));
        let lib = root.join("lib");
        Library::create(&lib).unwrap();
        let media = root.join("media");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("clip.mp4"), "x").unwrap();
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let typo = root.join("typo");
        let file = media.join("clip.mp4");

        assert_eq!(Launch::from_args(vec![]), Launch::Last);
        assert_eq!(Launch::from_args(vec![lib.clone(), file.clone()]), Launch::Open(lib.clone(), vec![file.clone()]));
        assert_eq!(Launch::from_args(vec![media.clone()]), Launch::Import(vec![media.clone()]));
        assert_eq!(Launch::from_args(vec![file.clone(), media.clone()]), Launch::Import(vec![file.clone(), media.clone()]));
        assert_eq!(Launch::from_args(vec![typo.clone(), file.clone()]), Launch::AskCreate(typo.clone(), vec![file]));
        assert_eq!(Launch::from_args(vec![empty.clone()]), Launch::AskCreate(empty, vec![]));
        assert!(!typo.exists() && !media.join("library.json").exists(), "nothing was made");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn split_by_contents_keeps_only_true_copies() {
        let dir = std::env::temp_dir().join(format!("orangelink-split-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in [("a", "xx"), ("b", "xy"), ("c", "xx"), ("d", "xy"), ("e", "zz")] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let files = ["a", "b", "c", "d", "e", "gone"].iter().map(|n| (n.to_string(), dir.join(n))).collect();
        let mut groups = split_by_contents(files, &AtomicBool::new(false));
        groups.sort();
        assert_eq!(groups, vec![vec!["a".to_string(), "c".to_string()], vec!["b".to_string(), "d".to_string()]]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clip_that_cannot_start_does_not_hold_up_the_export_queue() {
        let ctx = egui::Context::default();
        let mut app = App::with_config(&ctx, Config::default());
        let root = std::env::temp_dir().join(format!("orangelink-clipq-{}", crate::model::new_id()));
        app.lib = Some(Library::create(&root).unwrap());
        app.ffmpeg_dir = Some(root.clone());
        let preset = crate::config::default_export_presets().remove(0);
        app.clips.queue = [("deleted", "c1"), ("deleted too", "c2")].map(|(i, c)| (i.to_string(), c.to_string(), preset.clone())).into();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err("FFmpeg 失敗".to_string())).unwrap();
        app.clips.rx = Some(rx);
        app.poll_clip_export(&ctx);
        assert!(app.clips.queue.is_empty() && app.clips.rx.is_none(), "the rest of the queue was left waiting");
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_duplicate_search_is_a_job_that_can_be_cancelled() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = app.new_dup_search();
        app.dupes.exact_rx = Some(rx);
        assert!(app.background_jobs().iter().any(|j| j.kind == jobs::JobKind::Duplicates && j.can_cancel));
        app.cancel_job(jobs::JobKind::Duplicates);
        assert!(cancel.load(Ordering::Relaxed), "the thread is told to stop");
        let _ = tx.send(vec![vec!["a".into(), "b".into()]]);
        app.poll_exact_duplicates();
        assert!(!matches!(app.view, View::Ids { .. }), "a cancelled search shows no results");
        assert!(app.background_jobs().iter().all(|j| j.kind != jobs::JobKind::Duplicates));
    }

    #[test]
    fn closing_stops_a_clip_export_and_waits_for_it() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        let (tx, rx) = std::sync::mpsc::channel::<ClipExported>();
        let cancel = Arc::new(AtomicBool::new(false));
        app.clips.cancel = cancel.clone();
        app.clips.rx = Some(rx);
        let ended = Arc::new(AtomicBool::new(false));
        let ended2 = ended.clone();
        // Stands in for the export thread: it ends FFmpeg once told to, which takes a moment.
        std::thread::spawn(move || {
            while !cancel.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            ended2.store(true, Ordering::Relaxed);
            drop(tx);
        });
        drop(app);
        assert!(ended.load(Ordering::Relaxed), "closed with FFmpeg still running");
    }

    #[test]
    fn arrow_keys_go_on_from_the_item_the_preview_paged_to() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.caret = Some("a".into());
        app.open_viewer_at("c".into());
        assert_eq!(app.caret.as_deref(), Some("c"));
        assert_eq!(app.anchor.as_deref(), Some("c"));
    }

    #[test]
    fn dialogs_check_folders_once_when_they_open() {
        let ctx = egui::Context::default();
        let mut app = App::with_config(&ctx, Config::default());
        let root = std::env::temp_dir().join(format!("orangelink-dlg-{}", crate::model::new_id()));
        let mut lib = Library::create(&root.join("lib")).unwrap();
        let (here, gone) = (root.join("here"), root.join("gone"));
        std::fs::create_dir_all(&here).unwrap();
        lib.add_watch_folder(here.clone(), true);
        lib.add_watch_folder(gone.clone(), true);
        app.lib = Some(lib);
        app.act(Action::WatchFoldersDialog);
        app.process_actions(&ctx);
        let Some(Dialog::WatchFolders { missing, .. }) = &app.dialog else { panic!("no dialog") };
        assert_eq!(missing.iter().collect::<Vec<_>>(), [&gone]);

        app.dialog = None;
        app.config.import_mode = crate::config::ImportMode::Ask;
        app.act(Action::ImportByMode(vec![here.clone(), root.join("file.png")]));
        app.process_actions(&ctx);
        assert!(matches!(&app.dialog, Some(Dialog::ImportMode { dirs: 1, .. })));
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dragging_a_clip_edge_keeps_it_within_the_video() {
        let ctx = egui::Context::default();
        let mut app = App::with_config(&ctx, Config::default());
        let root = std::env::temp_dir().join(format!("orangelink-cliprange-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let it: Item = serde_json::from_value(serde_json::json!({
            "id": "v", "name": "v", "file_name": "v.mp4", "ext": "mp4", "kind": "video", "size": 1, "duration": 10.0, "added_at": 0, "modified_at": 0,
            "clips": [{ "id": "c", "name": "c", "start": 0.0, "end": 0.06 }]
        }))
        .unwrap();
        lib.insert_item(it);
        app.lib = Some(lib);
        app.act(Action::SetClipRange("v".into(), "c".into(), -0.04, 0.06));
        app.act(Action::SetClipRange("v".into(), "c".into(), 9.0, 10.5));
        app.process_actions(&ctx);
        let c = &app.lib.as_ref().unwrap().items["v"].clips[0];
        assert_eq!((c.start, c.end), (9.0, 10.0));
        app.act(Action::SetClipRange("v".into(), "c".into(), -0.04, 0.06));
        app.process_actions(&ctx);
        let c = &app.lib.as_ref().unwrap().items["v"].clips[0];
        assert!(c.start >= 0.0, "{}", c.start);
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn marks_make_a_clip() {
        assert_eq!(clip_from_marks((Some(10.0), Some(20.0)), 50.0, 100.0), (10.0, 20.0));
        assert_eq!(clip_from_marks((Some(10.0), None), 15.0, 100.0), (10.0, 15.0), "to the playhead");
        assert_eq!(clip_from_marks((Some(10.0), None), 5.0, 100.0), (10.0, 100.0), "playhead before in: to the end");
        assert_eq!(clip_from_marks((None, Some(8.0)), 50.0, 100.0), (0.0, 8.0));
    }

    #[test]
    fn searching_everywhere_ignores_the_selected_folder() {
        let mut it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "n", "file_name": "n.png", "ext": "png", "size": 1, "width": 1, "height": 1,
            "added_at": 0, "modified_at": 0
        }))
        .unwrap();
        let other = View::Folder("other".into());
        assert!(!in_view(&other, false, &it));
        assert!(in_view(&other, true, &it));
        it.deleted = true;
        assert!(!in_view(&other, true, &it));
        assert!(in_view(&View::Trash, false, &it));
    }

    /// `cargo test --release large_library_timing -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn large_library_timing() {
        use std::time::Instant;
        const N: usize = 20_000;
        let root = std::env::temp_dir().join(format!("orangelink-big-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let words = ["角色", "場景", "背景", "教學", "直播", "精華", "音效", "BGM", "立繪", "Logo"];
        let subtitle: String = (0..400).map(|i| format!("第 {i} 句台詞，大家好歡迎收看。\n")).collect();
        let t = Instant::now();
        for i in 0..N {
            let mut it: Item = serde_json::from_value(serde_json::json!({
                "id": crate::model::new_id(), "name": format!("素材_{i:05}_{}", words[i % 10]),
                "file_name": format!("f{i}.png"), "ext": if i % 5 == 0 { "mp4" } else { "png" },
                "size": 1000 + i, "width": 1920, "height": if i % 3 == 0 { 1920 } else { 1080 },
                "added_at": i as i64 * 1000, "modified_at": 0, "rating": i % 6,
                "tags": [words[i % 10], words[(i / 10) % 10]],
                "palette": [{ "rgb": [i as u8, 100, 200], "ratio": 0.3 }, { "rgb": [20, 20, 20], "ratio": 0.2 }],
                "source": format!("Z:\\missing\\f{i}.png")
            }))
            .unwrap();
            if i % 10 == 0 {
                it.text = Some(subtitle.clone());
            }
            let id = it.id.clone();
            std::fs::create_dir_all(root.join("images").join(format!("{id}.info"))).unwrap();
            lib.insert_item(it);
            lib.touch(&id);
        }
        lib.flush().unwrap();
        println!("create + write {N} items: {:?}", t.elapsed());

        let t = Instant::now();
        let lib = Library::open(&root).unwrap();
        println!("open (read + parse + missing check): {:?}", t.elapsed());
        assert_eq!(lib.items.len(), N);

        let live: Vec<&Item> = lib.items.values().filter(|i| !i.deleted).collect();
        for q in ["", "角色", "台詞", "不存在的字", "授權:內部", "missing", "Été"] {
            let c = Criteria::new(&SavedFilter { search: q.into(), ..Default::default() });
            let t = Instant::now();
            let n = live.iter().filter(|it| c.matches(it)).count();
            println!("search {q:?}: {n} hits in {:?}", t.elapsed());
        }
        let t = Instant::now();
        let folders: Vec<Criteria> = lib.meta.smart_folders.iter().filter(|s| !s.group).map(|s| Criteria::new(&s.filter)).collect();
        let total: usize = folders.iter().map(|c| live.iter().filter(|it| c.matches(it)).count()).sum();
        println!("smart folder counts ({} folders, {total} matches): {:?}", folders.len(), t.elapsed());
        let t = Instant::now();
        let mut v = live.clone();
        v.sort_by_cached_key(|it| (it.name.to_lowercase(), it.id.clone()));
        println!("sort by name: {:?}", t.elapsed());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn thumbnail_textures_stay_under_the_limit() {
        let used = |n: u64| (0..n).map(|i| (format!("t{i}"), i, 100)).collect::<Vec<_>>();
        assert!(lru_victims(used(10), 10, 1000).is_empty());
        assert_eq!(lru_victims(used(12), 12, 1000), vec!["t0".to_string(), "t1".to_string()], "oldest go first");
        // Counted in bytes: one big tile weighs as much as many small ones.
        let mixed = vec![("big".into(), 1, 900), ("a".into(), 2, 100), ("b".into(), 3, 100)];
        assert_eq!(lru_victims(mixed, 9, 300), vec!["big".to_string()]);
        // On screen this frame: kept even over the limit.
        let mut on_screen = used(3);
        on_screen.iter_mut().for_each(|e| e.1 = 7);
        assert!(lru_victims(on_screen, 7, 200).is_empty());
    }

    #[test]
    fn vertical_crop_stays_inside_the_frame() {
        assert_eq!(crop_9x16(1920, 1080, 0.5), Some((608, 656)));
        assert_eq!(crop_9x16(1920, 1080, 0.0), Some((608, 0)));
        assert_eq!(crop_9x16(1920, 1080, 1.0), Some((608, 1312)));
        assert_eq!(crop_9x16(1080, 1920, 0.5), None, "already portrait");
        assert_eq!(crop_9x16(0, 0, 0.5), None);
    }

    #[test]
    fn search_finds_the_best_matching_line() {
        let cue = |t: f64, text: &str| crate::media::Cue { start: t, end: t + 1.0, text: text.into() };
        let subs = Subtitles {
            id: "a".into(),
            path: None,
            cues: vec![cue(0.0, "大家好"), cue(5.0, "不能拉我啊"), cue(9.0, "拉我 Center 位")],
            search: String::new(),
            last_cue: None,
            focus: None,
            scroll_to_focus: false,
        };
        assert_eq!(subs.find("拉我"), Some(1), "the first of equally good lines");
        assert_eq!(subs.find("拉我 center"), Some(2), "more words matched wins");
        assert_eq!(subs.find("授權:內部 大家"), Some(0), "field terms are ignored");
        assert_eq!(subs.find("不存在"), None);
        assert_eq!(subs.find(""), None);
    }

    #[test]
    fn failures_read_as_errors() {
        assert!(looks_like_error("匯出失敗：拒絕存取"));
        assert!(looks_like_error("無法開啟：找不到程式"));
        assert!(!looks_like_error("已匯出 3 個檔案"));
        // Only the first line counts: a finished import listing one failed file is not an error.
        assert!(!looks_like_error("匯入完成：5 個新項目\n無法讀取 a.png"));
    }

    #[test]
    fn filter_combinations() {
        let item = |id: &str, ext: &str, rating: u8, tags: &[&str], rgb: [u8; 3]| -> Item {
            let mut it: Item = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "file_name": format!("{id}.{ext}"), "ext": ext, "kind": "image", "size": 1,
                "width": 10, "height": 10, "added_at": 0, "modified_at": 0, "rating": rating, "tags": tags
            }))
            .unwrap();
            it.palette = vec![crate::model::PaletteColor { rgb, ratio: 0.5 }];
            it
        };
        let items = [
            item("red-png", "png", 5, &["貓"], [230, 60, 60]),
            item("blue-jpg", "jpg", 3, &["貓", "狗"], [60, 120, 230]),
            item("red-webp", "webp", 1, &[], [225, 70, 55]),
            item("blue-png", "png", 0, &["狗"], [50, 110, 240]),
        ];
        let hits = |f: SavedFilter| -> Vec<&str> {
            let c = Criteria::new(&f);
            items.iter().filter(|it| c.matches(it)).map(|it| it.id.as_str()).collect()
        };
        let exts = |e: &[&str]| e.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(hits(SavedFilter { exts: exts(&["png", "jpg"]), ..Default::default() }), ["red-png", "blue-jpg", "blue-png"], "any of the formats");
        assert_eq!(hits(SavedFilter { exts: exts(&["png"]), min_rating: 1, ..Default::default() }), ["red-png"]);
        assert_eq!(hits(SavedFilter { color: Some([230, 60, 60]), ..Default::default() }), ["red-png", "red-webp"]);
        assert_eq!(hits(SavedFilter { color: Some([60, 120, 230]), tags: vec!["貓".into()], ..Default::default() }), ["blue-jpg"]);
        assert_eq!(
            hits(SavedFilter { exts: exts(&["png", "webp"]), color: Some([230, 60, 60]), min_rating: 2, ..Default::default() }),
            ["red-png"],
            "all conditions together"
        );
        assert!(hits(SavedFilter { exts: exts(&["gif"]), ..Default::default() }).is_empty());
    }

    #[test]
    fn search_and_filters() {
        let mut it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "直播精華", "file_name": "a.mp4", "ext": "mp4", "kind": "video", "size": 1,
            "width": 1920, "height": 1080, "added_at": 0, "modified_at": 0, "rating": 3,
            "tags": ["小橘", "遊戲"], "annotation": "第三場", "url": "https://example.com/v"
        }))
        .unwrap();
        it.text = Some("大家好\n不能拉我啊".into());
        let f = |search: &str| SavedFilter { search: search.into(), ..Default::default() };
        let hit = |it: &Item, filter: SavedFilter| Criteria::new(&filter).matches(it);

        // Every word must match something: name, tag, note, url, extension or dialogue.
        assert!(hit(&it, f("直播 小橘")) && hit(&it, f("第三場")) && hit(&it, f("example.com")));
        assert!(hit(&it, f("mp4")) && hit(&it, f("拉我")));
        assert!(!hit(&it, f("直播 不存在")));
        assert!(hit(&it, f("")), "no search matches everything");

        assert!(hit(&it, SavedFilter { tags: vec!["小橘".into(), "遊戲".into()], ..Default::default() }));
        assert!(!hit(&it, SavedFilter { tags: vec!["小橘".into(), "音樂".into()], ..Default::default() }));
        assert!(hit(&it, SavedFilter { min_rating: 3, ..Default::default() }));
        assert!(!hit(&it, SavedFilter { min_rating: 4, ..Default::default() }));
        assert!(hit(&it, SavedFilter { kind: Some(MediaKind::Video), ext: Some("mp4".into()), ..Default::default() }));
        assert!(!hit(&it, SavedFilter { kind: Some(MediaKind::Audio), ..Default::default() }));
        assert!(!hit(&it, SavedFilter { untagged: true, ..Default::default() }));
        assert!(hit(&it, SavedFilter { uncategorized: true, ..Default::default() }));
        it.folders.push("f".into());
        assert!(!hit(&it, SavedFilter { uncategorized: true, ..Default::default() }));
        assert!(hit(&it, SavedFilter { orientation: Some(crate::model::Orientation::Landscape), ..Default::default() }));
    }

    #[test]
    fn field_search_terms() {
        let mut it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "訪談", "file_name": "a.mp4", "ext": "mp4", "size": 1,
            "width": 0, "height": 0, "added_at": 0, "modified_at": 0
        }))
        .unwrap();
        it.fields.insert("授權".into(), "內部使用".into());
        let hit_in = |it: &Item, q: &str| Criteria::new(&SavedFilter { search: q.into(), ..Default::default() }).matches(it);
        let hit = |q: &str| hit_in(&it, q);
        assert!(hit("授權:內部") && hit("授權：使用") && hit("授權:") && hit("內部"));
        assert!(!hit("授權:公開") && !hit("客戶:") && !hit("客戶:內部"));

        it.name = "Logo_Final ÉTÉ".into();
        let hit = |q: &str| hit_in(&it, q);
        assert!(hit("logo") && hit("LOGO_f") && hit("été") && hit("final") && !hit("logos"));
    }

    #[test]
    fn smart_folder_conditions() {
        let root = std::env::temp_dir().join(format!("orangelink-smart-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let mk = |id: &str, name: &str, ext: &str, kind: &str, dur: Option<f64>| -> Item {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": name, "file_name": format!("{name}.{ext}"), "ext": ext, "kind": kind, "size": 1,
                "width": 1920, "height": 1080, "added_at": 0, "modified_at": 0, "duration": dur
            }))
            .unwrap()
        };
        let mut stream = mk("s", "直播", "mp4", "video", Some(3.0 * 3600.0));
        let mut short = mk("c", "短片", "mp4", "video", Some(40.0));
        short.clips.push(crate::model::Clip { id: "k".into(), name: "候選 01".into(), start: 0.0, end: 5.0, ..Default::default() });
        short.use_count = 2;
        short.last_used = Some(crate::model::now_ms());
        stream.last_used = Some(crate::model::now_ms() - 40 * 86_400_000);
        let srt = mk("t", "短片", "srt", "other", None);
        let song = mk("a", "歌", "mp3", "audio", Some(200.0));
        for it in [stream, short, srt, song] {
            lib.insert_item(it);
        }
        lib.missing.insert("a".into());
        let ids = |f: SavedFilter| {
            let c = Criteria::for_library(&f, &lib);
            let mut v: Vec<&str> = lib.items.values().filter(|i| c.matches(i)).map(|i| i.id.as_str()).collect();
            v.sort();
            v
        };
        let d = SavedFilter::default;
        assert_eq!(ids(SavedFilter { kinds: vec![MediaKind::Video, MediaKind::Audio], ..d() }), ["a", "c", "s"]);
        assert_eq!(ids(SavedFilter { min_duration: Some(60.0), max_duration: Some(3600.0), ..d() }), ["a"]);
        assert_eq!(ids(SavedFilter { has_clips: Some(true), ..d() }), ["c"]);
        assert_eq!(ids(SavedFilter { has_candidates: true, ..d() }), ["c"]);
        assert_eq!(ids(SavedFilter { has_subtitle: Some(true), ..d() }), ["c"], "the library's 短片.srt");
        assert_eq!(ids(SavedFilter { has_subtitle: Some(false), ..d() }), ["a", "s"], "only video and audio");
        assert_eq!(ids(SavedFilter { missing: true, ..d() }), ["a"]);
        assert_eq!(ids(SavedFilter { never_used: true, kind: Some(MediaKind::Video), ..d() }), ["s"]);
        assert_eq!(ids(SavedFilter { used_days: Some(30), ..d() }), ["c"]);
        // The built-in 待處理的直播: long videos without clips.
        let todo = SavedFilter { kind: Some(MediaKind::Video), min_duration: Some(1800.0), has_clips: Some(false), ..d() };
        assert_eq!(ids(todo), ["s"]);
        // Seeding again adds only the groups not there.
        lib.seed_default_smart_folders();
        let n = lib.meta.smart_folders.len();
        lib.seed_default_smart_folders();
        assert_eq!(lib.meta.smart_folders.len(), n);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shift_arrows_extend_the_range_from_the_anchor_and_shrink_it_back() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.visible = ["a", "b", "c", "d", "e", "f"].into_iter().map(String::from).collect();
        app.grid_cols = 2;
        let shift = egui::Modifiers { shift: true, ..Default::default() };
        // The key handler queues a click; the frame applies it before the next key.
        let step = |app: &mut App, delta: i64, mods: egui::Modifiers| {
            app.arrow_step(delta, mods);
            let ctx = app.egui_ctx.clone();
            app.process_actions(&ctx);
        };
        let selected = |app: &App| {
            let mut sel: Vec<String> = app.selected.iter().cloned().collect();
            sel.sort();
            sel.join("")
        };

        // The first arrow with nothing selected picks the first item.
        step(&mut app, 1, egui::Modifiers::NONE);
        assert_eq!((app.anchor.as_deref(), app.caret.as_deref()), (Some("a"), Some("a")));

        // Shift+→ twice: the anchor stays, the caret moves, the range grows.
        step(&mut app, 1, shift);
        step(&mut app, 1, shift);
        assert_eq!((app.anchor.as_deref(), app.caret.as_deref()), (Some("a"), Some("c")));
        assert_eq!(selected(&app), "abc");

        // Shift+← shrinks it instead of getting stuck at the anchor's end.
        step(&mut app, -1, shift);
        assert_eq!(app.caret.as_deref(), Some("b"));
        assert_eq!(selected(&app), "ab");

        // Shift+↓ moves a whole row; past the end it stops at the last item.
        step(&mut app, 2, shift);
        assert_eq!(selected(&app), "abcd");
        step(&mut app, 10, shift);
        assert_eq!(app.caret.as_deref(), Some("f"));
        assert_eq!(selected(&app), "abcdef");

        // A plain arrow drops the range and starts again from where the caret is.
        step(&mut app, -1, egui::Modifiers::NONE);
        assert_eq!((app.anchor.as_deref(), app.caret.as_deref()), (Some("e"), Some("e")));
        assert_eq!(selected(&app), "e");
        assert_eq!(app.scroll_to.as_deref(), Some("e"));
    }

    #[test]
    fn batch_rename_is_undoable_and_skips_missing_ids_safely() {
        let root = std::env::temp_dir().join(format!("orangelink-rename-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        for (id, name) in [("a", "甲"), ("b", "乙")] {
            let it: Item = serde_json::from_value(serde_json::json!({
                "id": id, "name": name, "file_name": format!("{id}.png"), "ext": "png",
                "kind": "image", "size": 1, "width": 10, "height": 10, "added_at": 0, "modified_at": 0
            }))
            .unwrap();
            lib.insert_item(it);
        }
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(lib);

        // Missing ID in the middle must not shift names onto the wrong item.
        app.apply_batch_rename(vec!["a".into(), "gone".into(), "b".into()], "{name}_{n}".into(), 1);
        let lib = app.lib.as_ref().unwrap();
        assert_eq!(lib.items["a"].name, "甲_01");
        assert_eq!(lib.items["b"].name, "乙_02");
        assert!(app.can_undo());

        app.restore_items(false);
        let lib = app.lib.as_ref().unwrap();
        assert_eq!(lib.items["a"].name, "甲");
        assert_eq!(lib.items["b"].name, "乙");
        assert!(app.can_redo());

        app.restore_items(true);
        let lib = app.lib.as_ref().unwrap();
        assert_eq!(lib.items["a"].name, "甲_01");
        assert_eq!(lib.items["b"].name, "乙_02");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ungrouped_tag_rename_is_undoable() {
        let root = std::env::temp_dir().join(format!("orangelink-tagundo-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "file_name": "a.png", "ext": "png",
            "kind": "image", "size": 1, "width": 10, "height": 10,
            "added_at": 0, "modified_at": 0, "tags": ["舊標籤"]
        }))
        .unwrap();
        lib.insert_item(it);
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(lib);
        // Baseline frame captures initial LibraryMeta structure.
        app.track_structure();
        assert!(!app.can_undo());

        // Renaming a tag that belongs to no TagGroup still captures affected items.
        app.lib.as_mut().unwrap().rename_tag("舊標籤", "新標籤");
        app.track_structure();
        assert_eq!(app.lib.as_ref().unwrap().items["a"].tags, vec!["新標籤"]);
        assert!(app.can_undo());

        app.restore_items(false);
        assert_eq!(app.lib.as_ref().unwrap().items["a"].tags, vec!["舊標籤"]);
        app.restore_items(true);
        assert_eq!(app.lib.as_ref().unwrap().items["a"].tags, vec!["新標籤"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    fn undo_test_app(name: &str) -> (App, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("orangelink-{name}-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "a", "file_name": "a.png", "ext": "png",
            "kind": "image", "size": 1, "width": 10, "height": 10,
            "added_at": 0, "modified_at": 0
        }))
        .unwrap();
        lib.insert_item(it);
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(lib);
        app.track_structure();
        (app, root)
    }

    #[test]
    fn a_new_field_and_its_value_are_undone_with_one_ctrl_z() {
        let (mut app, root) = undo_test_app("fieldundo");
        let ctx = egui::Context::default();
        app.apply(&ctx, Action::SetField(vec!["a".into()], "作者".into(), "我".into()));
        app.track_structure();
        let lib = app.lib.as_ref().unwrap();
        assert_eq!(lib.items["a"].fields.get("作者").map(String::as_str), Some("我"));
        assert!(lib.meta.fields.iter().any(|f| f == "作者"));

        app.restore_items(false);
        let lib = app.lib.as_ref().unwrap();
        assert!(lib.items["a"].fields.is_empty(), "the value outlived the undo");
        assert!(!lib.meta.fields.iter().any(|f| f == "作者"), "the field outlived the undo");
        assert!(!app.can_undo(), "setting a field took more than one step");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_in_the_same_frame_as_a_structure_edit_undoes_that_edit() {
        let (mut app, root) = undo_test_app("frameundo");
        app.edit_items(&["a".to_string()], |it| it.name = "b".into());
        // A field added this frame, not yet seen by the end-of-frame tracking.
        app.lib.as_mut().unwrap().add_field("x");
        app.restore_items(false);
        let lib = app.lib.as_ref().unwrap();
        assert!(!lib.meta.fields.iter().any(|f| f == "x"), "the newest edit was not the one undone");
        assert_eq!(lib.items["a"].name, "b");
        app.track_structure();
        assert!(app.can_redo(), "the end of the frame cleared the redo");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_item_sent_back_to_the_trash_by_undo_is_no_longer_selected() {
        let (mut app, root) = undo_test_app("trashundo");
        let mut trashed = app.lib.as_ref().unwrap().items["a"].clone();
        trashed.deleted = true;
        app.record_undo(vec![trashed], false);
        app.selected.insert("a".into());
        app.restore_items(false);
        assert!(app.lib.as_ref().unwrap().items["a"].deleted);
        assert!(app.selected.is_empty(), "a hidden trashed item stayed selected");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn floating_window_blocks_shortcuts_and_closes_topmost_first() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        assert!(!app.has_floating_window());
        assert!(!app.close_top_floating_window());

        app.show_ai_log = true;
        app.show_shortcuts = true;
        app.whisper.open = true;
        assert!(app.has_floating_window());

        // Closes topmost first without wiping underlying floating windows.
        assert!(app.close_top_floating_window());
        assert!(!app.whisper.open && app.show_shortcuts && app.show_ai_log);
        assert!(app.close_top_floating_window());
        assert!(!app.show_shortcuts && app.show_ai_log);
        assert!(app.close_top_floating_window());
        assert!(!app.has_floating_window());
    }

    #[test]
    fn close_viewer_clears_studio_state() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.viewer = Some(Viewer { id: "v1".into(), zoom: 2.0, pan: egui::Vec2::new(10.0, 20.0) });
        app.mark = (Some(1.0), Some(5.0));
        app.crop_frame = Some(0.5);
        app.viewer_tags_open = true;
        app.clip_ui.sel.insert("c1".into());
        app.text_cues_cache = Some(("v1".into(), vec![crate::media::Cue { start: 0.0, end: 1.0, text: "hi".into() }]));

        app.close_viewer(true);
        assert!(app.viewer.is_none());
        assert_eq!(app.scroll_to.as_deref(), Some("v1"));
        assert_eq!(app.mark, (None, None));
        assert_eq!(app.crop_frame, None);
        assert!(!app.viewer_tags_open);
        assert!(app.clip_ui.sel.is_empty());
        assert!(app.text_cues_cache.is_none());
    }

    #[test]
    fn the_subtitle_list_scrolls_to_a_found_line_once_and_then_scrolls_freely() {
        let cue = |t: f64| crate::media::Cue { start: t, end: t + 1.0, text: "x".into() };
        let mut subs = Subtitles {
            id: "a".into(),
            path: None,
            cues: vec![cue(0.0), cue(5.0), cue(9.0)],
            search: String::new(),
            last_cue: None,
            focus: None,
            scroll_to_focus: false,
        };
        subs.focus_on(Some(2));
        assert_eq!(subs.scroll_target(None), Some(2));
        // The playhead is not on the line yet: the line stays marked, the list is free.
        assert_eq!(subs.scroll_target(None), None, "the list was held on the found line");
        assert_eq!(subs.focus, Some(2));
        assert_eq!(subs.scroll_target(Some(1)), Some(1), "following playback still scrolls");
    }

    #[test]
    fn the_multi_selection_summary_is_reused_until_the_selection_or_library_changes() {
        let root = std::env::temp_dir().join(format!("orangelink-multisum-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        for (id, rating, tags) in [("a", 3, vec!["x", "y", "z"]), ("b", 3, vec!["z", "x"]), ("c", 1, vec!["x"])] {
            let it: Item = serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "file_name": "a.png", "ext": "png", "kind": "image", "size": 10,
                "width": 1, "height": 1, "added_at": 0, "modified_at": 0, "rating": rating, "tags": tags
            }))
            .unwrap();
            lib.insert_item(it);
        }
        lib.add_field("作者");
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(lib);
        app.selected.extend(["a".to_string(), "b".to_string()]);
        let first = app.multi_summary();
        assert_eq!(first.common_tags, vec!["x", "z"]);
        assert_eq!(first.rating, Some(3));
        assert_eq!(first.size, 20);
        assert_eq!(first.fields, vec![("作者".to_string(), Some(String::new()))]);
        assert!(std::rc::Rc::ptr_eq(&first, &app.multi_summary()), "recomputed with nothing changed");

        app.selected.insert("c".into());
        let more = app.multi_summary();
        assert_eq!(more.common_tags, vec!["x"]);
        assert_eq!(more.rating, None);

        let lib = app.lib.as_mut().unwrap();
        lib.items.get_mut("c").unwrap().fields.insert("作者".into(), "我".into());
        lib.touch("c");
        assert_eq!(app.multi_summary().fields[0].1, None, "a library edit was not picked up");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cached_stats_count_items_in_missing_folders_and_an_all_trash_library_as_empty() {
        let (mut app, root) = undo_test_app("cachedstats");
        app.lib.as_mut().unwrap().items.get_mut("a").unwrap().folders.push("gone".into());
        app.lib.as_mut().unwrap().touch("a");
        assert_eq!(app.with_sidebar_stats(|s| (s.missing_folder_refs, s.total)), (1, 1));
        app.lib.as_mut().unwrap().items.get_mut("a").unwrap().deleted = true;
        app.lib.as_mut().unwrap().touch("a");
        assert_eq!(app.with_sidebar_stats(|s| s.total), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn paging_the_preview_drops_the_last_videos_9_16_frame() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.viewer = Some(Viewer { id: "v1".into(), zoom: 1.0, pan: egui::Vec2::ZERO });
        app.crop_frame = Some(0.25);
        app.mark = (Some(1.0), Some(5.0));
        app.clip_ui.sel.insert("c1".into());
        app.viewer_tags_open = true;
        app.open_viewer_at("v2".into());
        assert_eq!(app.crop_frame, None, "the next video inherited the 9:16 frame");
        assert_eq!(app.mark, (None, None));
        assert!(app.clip_ui.sel.is_empty());
        assert!(!app.viewer_tags_open);
    }

    #[test]
    fn sidebar_stats_counts_items_kinds_and_trash() {
        let root = std::env::temp_dir().join(format!("orangelink-stats-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let mk = |id: &str, kind: &str, size: u64, deleted: bool, folders: &[&str], tags: &[&str]| -> Item {
            serde_json::from_value(serde_json::json!({
                "id": id, "name": id, "file_name": format!("{id}.bin"), "ext": "bin",
                "kind": kind, "size": size, "width": 0, "height": 0,
                "added_at": 0, "modified_at": 0, "deleted": deleted,
                "folders": folders, "tags": tags
            }))
            .unwrap()
        };
        lib.insert_item(mk("i1", "image", 100, false, &["f1"], &["t1"]));
        lib.insert_item(mk("v1", "video", 250, false, &[], &[]));
        lib.insert_item(mk("a1", "audio", 50, false, &[], &["t1"]));
        lib.insert_item(mk("d1", "other", 999, true, &[], &[]));

        let stats = crate::ui::SidebarStats::compute(&lib);
        assert_eq!(stats.total, 3);
        assert_eq!(stats.trash, 1);
        assert_eq!(stats.total_size, 400);
        assert_eq!(stats.uncategorized, 2);
        assert_eq!(stats.untagged, 1);
        assert_eq!(stats.by_kind, [1, 1, 1, 0]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_check_prompts_instead_of_auto_downloading() {
        let release = || crate::update::Release {
            tag: "v9.9.9".into(),
            asset: Some("OrangeLink-v9.9.9-windows-x64.zip".into()),
            sha256: Some("a".repeat(64)),
            sig: Some("sig".into()),
        };

        // With auto_update enabled, detecting a newer release opens Dialog::UpdatePrompt.
        let mut app = App::with_config(&egui::Context::default(), Config { auto_update: true, ..Config::default() });
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(release())).unwrap();
        app.update = UpdateState::Checking(rx);
        app.poll_update_check();
        assert_eq!(app.update.new_version(), Some("v9.9.9"));
        assert!(matches!(&app.dialog, Some(Dialog::UpdatePrompt { tag }) if tag == "v9.9.9"));

        // With auto_update disabled, the badge state updates without interrupting with a modal.
        let mut quiet = App::with_config(&egui::Context::default(), Config { auto_update: false, ..Config::default() });
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(release())).unwrap();
        quiet.update = UpdateState::Checking(rx);
        quiet.poll_update_check();
        assert_eq!(quiet.update.new_version(), Some("v9.9.9"));
        assert!(quiet.dialog.is_none());
    }

    /// Esc in the search box, a tag or note field, or a settings field.
    #[test]
    fn esc_in_a_text_box_leaves_the_box_and_keeps_the_selection() {
        let esc_after_typing = |typing_was_noted: bool| {
            let ctx = egui::Context::default();
            let mut app = App::with_config(&ctx, Config::default());
            app.visible = vec!["a".into()];
            app.selected.insert("a".into());
            let mut text = String::new();
            let frame = |events: Vec<egui::Event>| egui::RawInput { events, ..Default::default() };
            for _ in 0..2 {
                let _ = ctx.run(frame(Vec::new()), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| ui.text_edit_singleline(&mut text).request_focus());
                });
            }
            assert!(ctx.wants_keyboard_input(), "the box has the keyboard");
            if typing_was_noted {
                app.typing = ctx.wants_keyboard_input();
            }
            let esc = egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
            let _ = ctx.run(frame(vec![esc]), |ctx| {
                app.handle_keys(ctx);
                app.process_actions(ctx);
            });
            app.selected.contains("a")
        };
        assert!(esc_after_typing(true));
        // What happened before: egui has dropped the box's focus by the time the keys
        // are handled, so the Esc also cleared the selection.
        assert!(!esc_after_typing(false), "egui keeps the focus through Esc now: `typing` can go");
    }

    #[test]
    fn a_preview_opened_on_an_item_outside_the_grid_stays_open() {
        let root = std::env::temp_dir().join(format!("orangelink-goto-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        for (id, name) in [("v", "直播"), ("s", "直播字幕")] {
            let it: Item = serde_json::from_value(serde_json::json!({
                "id": id, "name": name, "file_name": format!("{id}.mp4"), "ext": "mp4",
                "kind": "video", "size": 1, "width": 10, "height": 10, "added_at": 0, "modified_at": 0
            }))
            .unwrap();
            lib.insert_item(it);
        }
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(lib);
        // A search that shows only the other item, as when a related item is opened.
        app.search = "字幕".into();
        app.refresh_visible();
        assert_eq!(app.visible, ["s"]);
        app.open_viewer_at("v".into());
        // The next frame.
        app.refresh_visible();
        app.close_viewer_if_hidden();
        assert_eq!(app.viewer.as_ref().map(|v| v.id.as_str()), Some("v"), "the preview stays open");
        assert!(app.visible.contains(&"v".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An import under way, fed by the test instead of an importer thread.
    fn fake_import(app: &mut App) -> std::sync::mpsc::Sender<ImportEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        app.import = Some(ImportRun {
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            silent: false,
            progress: Some((1, 4)),
            imported: 0,
            duplicates: 0,
            failed: 0,
            errors: Vec::new(),
            new: Vec::new(),
        });
        tx
    }

    fn queue_import(app: &mut App, root: &std::path::Path) {
        let ctx = app.egui_ctx.clone();
        app.start_import(&ctx, vec![root.join("missing.png")], false, None, Vec::new(), false);
    }

    #[test]
    fn queued_imports_show_in_the_jobs_list_and_are_cancelled_with_the_running_one() {
        let root = std::env::temp_dir().join(format!("orangelink-importq-{}", crate::model::new_id()));
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(Library::create(&root).unwrap());
        let tx = fake_import(&mut app);
        queue_import(&mut app, &root);
        queue_import(&mut app, &root);
        assert_eq!(app.import_queue.len(), 2, "a second import waits for the first");

        let list = app.background_jobs();
        let job = list.iter().find(|j| j.kind == jobs::JobKind::Import).expect("the import is listed");
        assert!(job.can_cancel);
        assert!(job.detail.contains("另有 2 批排隊"), "{}", job.detail);

        app.cancel_job(jobs::JobKind::Import);
        assert!(app.import.as_ref().unwrap().cancel.load(Ordering::Relaxed));
        assert!(app.import_queue.is_empty());
        // The cancelled importer still sends Finished; nothing queued starts after it.
        tx.send(ImportEvent::Finished).unwrap();
        app.poll_import();
        assert!(app.import.is_none(), "no queued import started after the cancel");
        assert!(app.background_jobs().iter().all(|j| j.kind != jobs::JobKind::Import));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_finished_import_starts_the_next_queued_one() {
        let root = std::env::temp_dir().join(format!("orangelink-importnext-{}", crate::model::new_id()));
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        app.lib = Some(Library::create(&root).unwrap());
        let tx = fake_import(&mut app);
        queue_import(&mut app, &root);
        tx.send(ImportEvent::Finished).unwrap();
        app.poll_import();
        assert!(app.import.is_some(), "the queued import started");
        assert!(app.import_queue.is_empty());
        if let Some(run) = app.import.take() {
            run.cancel.store(true, Ordering::Relaxed);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    /// Names for exported files and captures: never empty, nothing Windows refuses.
    #[test]
    fn random_file_names() {
        for s in crate::fuzz::strings(4, 20_000) {
            let name = sanitize_file_name(&s);
            assert!(!name.is_empty(), "{s:?}");
            assert!(!name.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)), "{s:?} -> {name:?}");
            assert!(!name.ends_with(['.', ' ']), "{s:?} -> {name:?}");
        }
    }
}
