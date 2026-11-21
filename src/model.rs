use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    #[default]
    Image,
    Video,
    Audio,
    Other,
}

impl MediaKind {
    pub const ALL: [MediaKind; 4] = [MediaKind::Image, MediaKind::Video, MediaKind::Audio, MediaKind::Other];

    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => t!("圖片"),
            MediaKind::Video => t!("影片"),
            MediaKind::Audio => t!("音訊"),
            MediaKind::Other => t!("其他"),
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            MediaKind::Image => "🖼",
            MediaKind::Video => "🎬",
            MediaKind::Audio => "🎵",
            MediaKind::Other => "📄",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PaletteColor {
    pub rgb: [u8; 3],
    /// Share of the image's (opaque) pixels close to this color, 0..1.
    pub ratio: f32,
}

/// One asset in the library. Stored as `images/<id>.info/metadata.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub file_name: String,
    pub ext: String,
    #[serde(default)]
    pub kind: MediaKind,
    /// Set when the file was left in place ("linked") instead of copied into the library.
    #[serde(default)]
    pub source: Option<PathBuf>,
    /// Seconds, for video/audio.
    #[serde(default)]
    pub duration: Option<f64>,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub annotation: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub rating: u8,
    #[serde(default)]
    pub palette: Vec<PaletteColor>,
    /// Unix milliseconds.
    pub added_at: i64,
    pub modified_at: i64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub hash: String,
    /// Where the file was imported from (for copies), so watch folders and
    /// re-imports can recognise it without hashing again.
    #[serde(default)]
    pub origin: Option<PathBuf>,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub codec: Option<String>,
    /// Leading text of subtitle/text files, for preview and search.
    #[serde(default)]
    pub text: Option<String>,
    /// Times handed to another app (drag-out, copy, open, Premiere export).
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub last_used: Option<i64>,
    /// Marked in/out ranges of a video or audio file.
    #[serde(default)]
    pub clips: Vec<Clip>,
    /// Values of the library's custom fields (see `LibraryMeta::fields`).
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Subtitle file chosen by hand; otherwise one with the same name is looked up.
    #[serde(default)]
    pub subtitle: Option<PathBuf>,
    /// Live chat record (yt-dlp's `.live_chat.json`) chosen by hand; otherwise
    /// one with the same name next to the original is looked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<PathBuf>,
    /// Related items, both ways: a clip exported from a stream and the stream,
    /// a captured frame and its video, files from one download, or linked by hand.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

/// A marked range inside a video/audio item, in seconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: String,
    pub name: String,
    pub start: f64,
    pub end: f64,
    /// Centre of a 9:16 crop across the frame's width (0 = left edge, 1 = right edge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_x: Option<f32>,
    /// Why a highlight candidate was picked, e.g. `聊天 ×3.2、音量 +8 dB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// How strongly a candidate stood out, for sorting by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// A name an extension suggested for a candidate: the clip's name once kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Each time the clip was written to a file, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exported: Vec<ClipExport>,
}

/// One export of a clip: when, with which preset, to where.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipExport {
    /// Unix seconds.
    pub at: i64,
    pub preset: String,
    pub path: PathBuf,
}

impl Clip {
    /// The last export with `preset`, if any.
    pub fn exported_with(&self, preset: &str) -> Option<&ClipExport> {
        self.exported.iter().rev().find(|e| e.preset == preset)
    }
}

impl Item {
    /// The live chat record for the highlight curves: the one picked by hand
    /// while it is there, else `<video>.live_chat.json` next to the original.
    pub fn chat_file(&self) -> Option<PathBuf> {
        self.chat.clone().filter(|p| p.is_file()).or_else(|| self.imported_from().and_then(|p| crate::signals::chat_file_for(p)))
    }

    /// The path this item was imported from, if known.
    pub fn imported_from(&self) -> Option<&PathBuf> {
        self.source.as_ref().or(self.origin.as_ref())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub collapsed: bool,
}

/// A saved set of filters shown as a folder.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedFilter {
    pub search: String,
    pub tags: Vec<String>,
    pub ext: Option<String>,
    pub min_rating: u8,
    pub color: Option<[u8; 3]>,
    pub kind: Option<MediaKind>,
    /// Any of these extensions.
    pub exts: Vec<String>,
    /// Added within the last N days.
    pub added_days: Option<u32>,
    pub orientation: Option<Orientation>,
    /// Longest side at least this many pixels.
    pub min_long_side: Option<u32>,
    pub min_size_mb: Option<u64>,
    pub unrated: bool,
    pub untagged: bool,
    pub uncategorized: bool,
    /// Some(true) = only "left in place" items, Some(false) = only copies.
    pub linked: Option<bool>,
    /// Any of these kinds (besides `kind`, which allows one).
    pub kinds: Vec<MediaKind>,
    /// Length in seconds, at least / at most (items without a length don't match).
    pub min_duration: Option<f64>,
    pub max_duration: Option<f64>,
    /// Some(true) = has clips, Some(false) = has none.
    pub has_clips: Option<bool>,
    /// Has highlight candidates not kept or deleted yet.
    pub has_candidates: bool,
    /// Video and audio with (true) or without (false) subtitles.
    pub has_subtitle: Option<bool>,
    /// The original file can't be found.
    pub missing: bool,
    pub never_used: bool,
    /// Used (opened, dragged out, exported) within the last N days.
    pub used_days: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl SavedFilter {
    pub fn is_empty(&self) -> bool {
        *self == SavedFilter::default()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmartFolder {
    pub id: String,
    pub name: String,
    pub filter: SavedFilter,
    #[serde(default)]
    pub parent: Option<String>,
    /// A group only holds other smart folders; clicking it expands/collapses.
    #[serde(default)]
    pub group: bool,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TagGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub collapsed: bool,
}

/// A folder rescanned periodically; new files are imported automatically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WatchFolder {
    pub path: PathBuf,
    /// Leave files in place instead of copying.
    pub link: bool,
    /// New files go into this library folder and get these tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Library-wide data, stored as `library.json` at the library root.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LibraryMeta {
    pub version: u32,
    #[serde(default)]
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub smart_folders: Vec<SmartFolder>,
    #[serde(default)]
    pub watch_folders: Vec<WatchFolder>,
    /// Source keys of items deleted permanently, so watch folders don't re-import them.
    #[serde(default)]
    pub ignored_sources: Vec<String>,
    #[serde(default)]
    pub tag_groups: Vec<TagGroup>,
    /// The default smart-folder tree is created once per library.
    #[serde(default)]
    pub smart_defaults_seeded: bool,
    /// Names of custom fields, in display order.
    #[serde(default)]
    pub fields: Vec<String>,
}

impl LibraryMeta {
    /// Whether `other` differs in structure, ignoring expanded/collapsed state.
    pub fn same_structure(&self, other: &LibraryMeta) -> bool {
        fn plain(m: &LibraryMeta) -> LibraryMeta {
            let mut m = m.clone();
            m.folders.iter_mut().for_each(|f| f.collapsed = false);
            m.smart_folders.iter_mut().for_each(|f| f.collapsed = false);
            m.tag_groups.iter_mut().for_each(|g| g.collapsed = false);
            m
        }
        plain(self) == plain(other)
    }
}

/// The invisible characters that change the direction text runs in: U+202E
/// makes "evil\u{202E}gpj.exe" read "evilexe.jpg".
pub fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Merging duplicates: `keep` takes on what `other` has that it lacks — tags,
/// folders, notes, URL, rating (the higher), clips, custom fields, subtitle and
/// use. Its own file and name stay.
pub fn merge_into(keep: &mut Item, other: &Item) {
    for t in &other.tags {
        if !keep.tags.contains(t) {
            keep.tags.push(t.clone());
        }
    }
    for f in &other.folders {
        if !keep.folders.contains(f) {
            keep.folders.push(f.clone());
        }
    }
    let note = other.annotation.trim();
    if !note.is_empty() && !keep.annotation.contains(note) {
        if !keep.annotation.trim().is_empty() {
            keep.annotation.push_str("

");
        }
        keep.annotation.push_str(note);
    }
    if keep.url.is_empty() {
        keep.url = other.url.clone();
    }
    keep.rating = keep.rating.max(other.rating);
    for c in &other.clips {
        if !keep.clips.iter().any(|k| (k.start - c.start).abs() < 0.01 && (k.end - c.end).abs() < 0.01) {
            keep.clips.push(Clip { id: new_id(), ..c.clone() });
        }
    }
    keep.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    for (k, v) in &other.fields {
        let mine = keep.fields.entry(k.clone()).or_default();
        if mine.trim().is_empty() {
            *mine = v.clone();
        }
    }
    if keep.subtitle.is_none() {
        keep.subtitle = other.subtitle.clone();
    }
    for l in &other.links {
        if *l != keep.id && !keep.links.contains(l) {
            keep.links.push(l.clone());
        }
    }
    keep.use_count = keep.use_count.saturating_add(other.use_count);
    keep.last_used = keep.last_used.max(other.last_used);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> Item {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "file_name": "f.png", "ext": "png", "size": 1, "width": 1, "height": 1,
            "added_at": 0, "modified_at": 0
        }))
        .unwrap()
    }

    #[test]
    fn a_chat_record_picked_by_hand_is_used_and_the_same_name_one_otherwise() {
        let dir = std::env::temp_dir().join(format!("orangelink-chat-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("stream.mp4");
        let (same_name, renamed) = (dir.join("stream.live_chat.json"), dir.join("chat 改過名.json"));
        for f in [&video, &same_name, &renamed] {
            std::fs::write(f, "{}").unwrap();
        }
        let mut it = item("a");
        it.source = Some(video);
        assert_eq!(it.chat_file(), Some(same_name.clone()));
        it.chat = Some(renamed.clone());
        assert_eq!(it.chat_file(), Some(renamed.clone()));
        std::fs::remove_file(&renamed).unwrap();
        assert_eq!(it.chat_file(), Some(same_name), "the picked one is gone: back to the same name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_two_heavily_used_items_does_not_overflow_the_use_count() {
        let mut a = item("a");
        a.use_count = u32::MAX;
        let mut b = item("b");
        b.use_count = 5;
        merge_into(&mut a, &b);
        assert_eq!(a.use_count, u32::MAX);
    }

    #[test]
    fn merging_keeps_everything_once() {
        let mut a = item("a");
        a.tags = vec!["貓".into()];
        a.annotation = "第一版".into();
        a.rating = 2;
        a.use_count = 1;
        a.clips = vec![Clip { id: "c1".into(), name: "開場".into(), start: 0.0, end: 5.0, crop_x: None, ..Default::default() }];
        let mut b = item("b");
        b.tags = vec!["貓".into(), "橘".into()];
        b.folders = vec!["f".into()];
        b.annotation = "授權到 2027".into();
        b.url = "https://example.com".into();
        b.rating = 4;
        b.use_count = 3;
        b.last_used = Some(9);
        b.clips = vec![Clip { id: "c2".into(), name: "開場".into(), start: 0.0, end: 5.0, crop_x: None, ..Default::default() }, Clip { id: "c3".into(), name: "精華".into(), start: 9.0, end: 12.0, crop_x: None, ..Default::default() }];
        b.fields.insert("授權".into(), "內部".into());
        merge_into(&mut a, &b);
        assert_eq!(a.tags, ["貓", "橘"]);
        assert_eq!(a.folders, ["f"]);
        assert_eq!(a.annotation, "第一版

授權到 2027");
        assert_eq!((a.url.as_str(), a.rating, a.use_count, a.last_used), ("https://example.com", 4, 4, Some(9)));
        assert_eq!(a.clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["開場", "精華"]);
        assert_ne!(a.clips[1].id, "c3", "copied clips get their own id");
        assert_eq!(a.fields["授權"], "內部");
        assert_eq!(a.name, "a");
        // Merging the same again changes nothing.
        let once = a.clone();
        merge_into(&mut a, &b);
        assert_eq!(a.tags, once.tags);
        assert_eq!(a.annotation, once.annotation);
        assert_eq!(a.clips.len(), once.clips.len());
    }
}
