use crate::{
    imaging::{extract_palette, quick_hash, save_thumbnail, THUMB_SIDE},
    library::{item_dir, write_json, META_FILE, THUMB_FILE},
    media::{self, ext_of, is_junk, kind_of},
    model::{new_id, now_ms, Item, MediaKind},
};
use anyhow::Result;
use eframe::egui;
use rayon::prelude::*;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{channel, Receiver},
        Arc, Mutex,
    },
    thread,
};

// One event per file over a channel: boxing the item would buy nothing.
#[allow(clippy::large_enum_variant)]
pub enum ImportEvent {
    Imported(Item),
    Duplicate(PathBuf),
    Failed(PathBuf, String),
    Progress { done: usize, total: usize },
    Finished,
}

/// What is already there, for duplicate detection on import: content hashes and
/// linked source keys, and for a large file's partial hash the files it was taken
/// from, since that hash alone doesn't prove two large files are the same.
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub keys: HashSet<String>,
    pub files: HashMap<String, Vec<PathBuf>>,
}

impl From<HashSet<String>> for Known {
    fn from(keys: HashSet<String>) -> Self {
        Self { keys, files: HashMap::new() }
    }
}

impl Known {
    /// A file with exactly these bytes is known. A partial hash is confirmed by
    /// comparing contents; with nothing to compare against, the hash is trusted.
    pub fn has_copy(&self, hash: &str, path: &Path) -> bool {
        if !self.keys.contains(hash) {
            return false;
        }
        if !crate::imaging::is_partial_hash(hash) {
            return true;
        }
        match self.files.get(hash) {
            Some(files) if !files.is_empty() => files.iter().any(|f| crate::imaging::same_contents(f, path).unwrap_or(false)),
            _ => true,
        }
    }

    fn add(&mut self, hash: &str, key: &str, path: &Path) {
        if crate::imaging::is_partial_hash(hash) {
            self.files.entry(hash.to_string()).or_default().push(path.to_path_buf());
        }
        self.keys.insert(hash.to_string());
        self.keys.insert(key.to_string());
    }

    /// Undoes `add` for a file that failed to import.
    fn forget(&mut self, hash: &str, key: &str, path: &Path) {
        self.keys.remove(key);
        if let Some(files) = self.files.get_mut(hash) {
            files.retain(|f| f != path);
            if !files.is_empty() {
                return; // another file still has this hash
            }
            self.files.remove(hash);
        }
        self.keys.remove(hash);
    }
}

pub struct ImportOptions {
    /// Leave files where they are instead of copying them into the library.
    pub link: bool,
    pub folder: Option<String>,
    /// Tags every new item gets.
    pub tags: Vec<String>,
    pub skip_duplicates: bool,
    pub ffmpeg_dir: Option<PathBuf>,
    /// Set to stop the import; remaining files are skipped.
    pub cancel: Arc<AtomicBool>,
}

/// What a drop would import, for the confirmation before a large one: filled
/// in by `scan` on a background thread.
#[derive(Clone, Debug, Default)]
pub struct ImportScan {
    /// Files found, and their total size, once counted.
    pub counted: bool,
    pub files: usize,
    pub bytes: u64,
    /// Files checked for duplicates so far, and duplicates among them.
    pub checked: usize,
    pub dupes: usize,
}

/// Counts the files under `inputs`, then checks each against the library's
/// `known` paths and content hashes, updating `out` as it goes. Stops when
/// `out` has no other owner left.
pub fn scan(inputs: Vec<PathBuf>, known: Known, out: Arc<Mutex<ImportScan>>, ctx: egui::Context) {
    let files = collect_paths(inputs);
    let bytes = files.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum();
    {
        let mut s = lock(&out);
        (s.counted, s.files, s.bytes) = (true, files.len(), bytes);
    }
    ctx.request_repaint();
    let mut seen = known;
    for (n, p) in files.iter().enumerate() {
        if Arc::strong_count(&out) == 1 {
            return; // the dialog was closed
        }
        let abs = dunce_abs(p).unwrap_or_else(|_| p.clone());
        let key = source_key(&abs);
        let dup = seen.keys.contains(&key)
            || quick_hash(p).is_ok_and(|h| {
                let dup = seen.has_copy(&h, p);
                if !dup {
                    seen.add(&h, &key, &abs);
                }
                dup
            });
        let mut s = lock(&out);
        s.checked = n + 1;
        s.dupes += dup as usize;
        drop(s);
        if n % 20 == 19 {
            ctx.request_repaint();
        }
    }
    ctx.request_repaint();
}

/// Expands directories recursively, dropping system/temp junk files.
fn collect_paths(inputs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in inputs {
        if p.is_dir() {
            let walker = walkdir::WalkDir::new(&p)
                .into_iter()
                .filter_entry(|e| e.depth() == 0 || !is_junk(e.path()));
            for entry in walker.filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    out.push(entry.into_path());
                }
            }
        } else if p.is_file() && !is_junk(&p) {
            out.push(p);
        }
    }
    out
}

/// Imports on a background thread. `existing` holds content hashes and linked
/// source paths already in the library, used for duplicate detection.
pub fn spawn(
    root: PathBuf,
    inputs: Vec<PathBuf>,
    opts: ImportOptions,
    existing: Known,
    ctx: egui::Context,
) -> Receiver<ImportEvent> {
    let (tx, rx) = channel();
    thread::spawn(move || {
        let paths = collect_paths(inputs);
        let total = paths.len();
        let _ = tx.send(ImportEvent::Progress { done: 0, total });
        let seen = Mutex::new(existing);
        let done = AtomicUsize::new(0);
        // Offset timestamps by index so "sort by added" keeps the import order.
        let base = now_ms();
        paths.par_iter().enumerate().for_each_with(tx.clone(), |tx, (i, path)| {
            if opts.cancel.load(Ordering::Relaxed) {
                return;
            }
            let event = match guarded(|| import_one(&root, path, &opts, &seen, base + i as i64)) {
                Ok(Some(item)) => ImportEvent::Imported(item),
                Ok(None) => ImportEvent::Duplicate(path.clone()),
                Err(e) => ImportEvent::Failed(path.clone(), e.to_string()),
            };
            let _ = tx.send(event);
            let d = done.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = tx.send(ImportEvent::Progress { done: d, total });
            ctx.request_repaint();
        });
        let _ = tx.send(ImportEvent::Finished);
        ctx.request_repaint();
    });
    rx
}

/// An image takes about 4 bytes a pixel once decoded: a 100-megapixel scan is
/// 400 MB. Images over this many pixels, or files over `BIG_FILE`, are decoded
/// at most `BIG_AT_ONCE` at a time, so an import of many huge TIFFs or scans
/// doesn't decode one per core at once and run out of memory. Smaller files
/// still go one per core.
const BIG_PIXELS: u64 = 40_000_000;
const BIG_FILE: u64 = 64 << 20;
const BIG_AT_ONCE: usize = 2;
static BIG_IMAGES: Gate = Gate::new(BIG_AT_ONCE);

/// Lets at most `max` holders of a `Pass` through at once; the others wait.
struct Gate {
    used: Mutex<usize>,
    freed: std::sync::Condvar,
    max: usize,
}

struct Pass<'a>(&'a Gate);

impl Gate {
    const fn new(max: usize) -> Self {
        Self { used: Mutex::new(0), freed: std::sync::Condvar::new(), max }
    }

    fn enter(&self) -> Pass<'_> {
        let mut used = lock(&self.used);
        while *used >= self.max {
            used = self.freed.wait(used).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *used += 1;
        Pass(self)
    }
}

impl Drop for Pass<'_> {
    // Also when the decoder panics: the slot is never lost.
    fn drop(&mut self) {
        *lock(&self.0.used) -= 1;
        self.0.freed.notify_one();
    }
}

/// Whether decoding `path` takes enough memory to go through `BIG_IMAGES`.
/// Reads only the header for the size.
fn is_big_image(path: &Path, size: u64) -> bool {
    size > BIG_FILE
        || image::ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.into_dimensions().ok())
            .is_some_and(|(w, h)| w as u64 * h as u64 > BIG_PIXELS)
}

/// Runs `f`, turning a panic (say, a decoder choking on a damaged file) into an error,
/// so one bad file is reported as failed instead of stopping the whole import.
fn guarded<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let why = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
        Err(anyhow::anyhow!("處理檔案時發生內部錯誤 {why}"))
    })
}

use crate::lock;

/// Key used to recognise an already-linked original.
pub fn source_key(path: &Path) -> String {
    format!("src:{}", path.to_string_lossy().to_lowercase())
}

/// `source_key` for a file not imported yet, matching the key of its imported item.
pub fn path_key(path: &Path) -> Option<String> {
    dunce_abs(path).ok().map(|abs| source_key(&abs))
}

/// Names the library uses in an item's folder: metadata.json and what is written
/// beside it (metadata.json.tmp, .broken copies), and the thumbnail.
fn is_reserved_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(META_FILE) || lower.starts_with(THUMB_FILE)
}

fn import_one(
    root: &Path,
    path: &Path,
    opts: &ImportOptions,
    seen: &Mutex<Known>,
    added_at: i64,
) -> Result<Option<Item>> {
    let abs = dunce_abs(path)?;
    let key = source_key(&abs);
    // Cheap path check first, so rescanning a watch folder doesn't re-hash everything.
    if opts.skip_duplicates && lock(seen).keys.contains(&key) {
        return Ok(None);
    }
    let source = opts.link.then(|| abs.clone());
    let hash = quick_hash(path)?;
    let mut added_to_seen = false;
    if opts.skip_duplicates {
        // Snapshot candidate paths for partial hashes so multi-GB byte comparisons
        // run outside the mutex; re-check any files added concurrently before inserting.
        let (is_partial, initial) = {
            let s = lock(seen);
            if !s.keys.contains(&hash) {
                (false, None)
            } else if !crate::imaging::is_partial_hash(&hash) {
                return Ok(None);
            } else {
                match s.files.get(&hash) {
                    Some(files) if !files.is_empty() => (true, Some(files.clone())),
                    _ => return Ok(None),
                }
            }
        };
        let checked_len = if let Some(files) = &initial {
            if files.iter().any(|f| crate::imaging::same_contents(f, path).unwrap_or(false)) {
                return Ok(None);
            }
            files.len()
        } else {
            0
        };
        let mut s = lock(seen);
        if is_partial {
            let extra_dup = s
                .files
                .get(&hash)
                .is_some_and(|files| files.iter().skip(checked_len).any(|f| crate::imaging::same_contents(f, path).unwrap_or(false)));
            if extra_dup {
                return Ok(None);
            }
        } else if s.has_copy(&hash, path) {
            return Ok(None);
        }
        s.add(&hash, &key, &abs);
        added_to_seen = true;
    }

    let id = new_id();
    let dir = item_dir(root, &id);
    // Guarded here too, so a panic still cleans up the half-made item folder below.
    let result = guarded(|| -> Result<Item> {
        fs::create_dir_all(&dir)?;
        let mut file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        // Never let a copied file collide with metadata.json, _thumbnail.png or the
        // files made beside them (metadata.json.tmp while saving, .broken when kept).
        if source.is_none() && is_reserved_name(&file_name) {
            file_name = format!("item_{file_name}");
        }
        let size = fs::metadata(path)?.len();
        if source.is_none() {
            fs::copy(path, dir.join(&file_name))?;
        }

        let kind = kind_of(path);
        let thumb_path = dir.join(THUMB_FILE);
        let (mut width, mut height, mut duration, mut palette) = (0, 0, None, Vec::new());
        let (mut fps, mut codec) = (None, None);
        let mut have_thumb = false;
        if kind == MediaKind::Image {
            let _pass = is_big_image(path, size).then(|| BIG_IMAGES.enter());
            if let Ok(img) = fs::read(path).map_err(anyhow::Error::from).and_then(|b| Ok(image::load_from_memory(&b)?)) {
                width = img.width();
                height = img.height();
                let shown = crate::imaging::representative_gif_frame(path).unwrap_or(img);
                palette = extract_palette(&shown);
                have_thumb = save_thumbnail(&shown, &thumb_path).is_ok();
            }
        }
        if !have_thumb && kind != MediaKind::Other {
            if let Some(ff) = &opts.ffmpeg_dir {
                let probe = media::probe(ff, path).unwrap_or_default();
                if width == 0 {
                    (width, height) = (probe.width, probe.height);
                }
                if kind != MediaKind::Image {
                    duration = probe.duration;
                    codec = probe.codec;
                }
                if kind == MediaKind::Video {
                    fps = probe.fps;
                }
                if media::thumbnail(ff, path, &thumb_path, duration, THUMB_SIDE) {
                    if let Ok(t) = crate::imaging::decode_file(&thumb_path) {
                        palette = extract_palette(&t);
                    }
                } else if kind == MediaKind::Audio {
                    media::waveform(ff, path, &thumb_path, THUMB_SIDE);
                }
            }
        }

        let item = Item {
            id: id.clone(),
            name: path
                .file_stem()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| file_name.clone()),
            ext: ext_of(path),
            kind,
            source: source.clone(),
            duration,
            file_name,
            size,
            width,
            height,
            tags: opts.tags.clone(),
            folders: opts.folder.iter().cloned().collect(),
            annotation: String::new(),
            url: String::new(),
            rating: 0,
            palette,
            added_at,
            modified_at: added_at,
            deleted: false,
            hash: hash.clone(),
            origin: (!opts.link).then(|| abs.clone()),
            fps,
            codec,
            text: match kind {
                MediaKind::Video | MediaKind::Audio => ["srt", "vtt"].iter().find_map(|e| media::subtitle_search_text(&path.with_extension(e))),
                _ => media::text_excerpt(path),
            },
            use_count: 0,
            last_used: None,
            clips: Vec::new(),
            fields: Default::default(),
            subtitle: None,
            links: Vec::new(),
        };
        write_json(&dir.join(META_FILE), &item)?;
        Ok(item)
    });

    match result {
        Ok(item) => Ok(Some(item)),
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            if added_to_seen {
                lock(seen).forget(&hash, &key, &abs);
            }
            Err(e)
        }
    }
}

/// Absolute path without the `\\?\` prefix that `canonicalize` adds on Windows
/// (other apps such as Premiere don't understand it).
pub(crate) fn dunce_abs(path: &Path) -> Result<PathBuf> {
    let abs = fs::canonicalize(path)?;
    let s = abs.to_string_lossy();
    Ok(match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        Some(rest) => PathBuf::from(format!(r"\\{}", &rest[4..])),
        None => abs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_becomes_a_failed_file() {
        let r: Result<()> = guarded(|| panic!("decoder blew up"));
        assert!(r.unwrap_err().to_string().contains("decoder blew up"));
        let m = Mutex::new(1);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            #[allow(clippy::disallowed_methods)] // poisons the lock on purpose
            let _g = m.lock().unwrap();
            panic!("poisoned");
        }));
        assert_eq!(*lock(&m), 1, "a poisoned lock still works");
    }

    #[test]
    fn damaged_and_deeply_nested_files_do_not_stop_an_import() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let mut deep = root.join("src");
        while deep.as_os_str().len() < 300 {
            deep = deep.join("很長的資料夾名稱_long_folder_name");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("broken.png"), b"not really a png").unwrap();
        fs::write(deep.join("broken.mp4"), b"not a video either").unwrap();
        fs::write(deep.join("notes.txt"), "hello").unwrap();
        let lib_root = root.join("lib");
        fs::create_dir_all(lib_root.join(crate::library::IMAGES_DIR)).unwrap();
        let opts = ImportOptions { link: false, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: crate::media::find_ffmpeg_dir(None), cancel: Default::default() };
        let rx = spawn(lib_root, vec![root.join("src")], opts, Known::default(), egui::Context::default());
        let (mut imported, mut failed, mut finished) = (0, 0, false);
        for ev in rx {
            match ev {
                ImportEvent::Imported(_) => imported += 1,
                ImportEvent::Failed(..) => failed += 1,
                ImportEvent::Finished => finished = true,
                _ => {}
            }
        }
        assert!(finished, "the import always reports that it is done");
        assert_eq!(imported + failed, 3);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_preview_counts_sizes_and_duplicates_and_a_rule_tags() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), "same").unwrap();
        fs::write(src.join("sub").join("b.txt"), "same").unwrap();
        fs::write(src.join("c.txt"), "other!").unwrap();
        fs::write(src.join("Thumbs.db"), "junk").unwrap();
        let known: HashSet<String> = [quick_hash(&src.join("c.txt")).unwrap()].into();
        let out = Arc::new(Mutex::new(ImportScan::default()));
        scan(vec![src.clone()], known.into(), out.clone(), egui::Context::default());
        let s = lock(&out).clone();
        assert!(s.counted);
        assert_eq!((s.files, s.bytes, s.checked), (3, 14, 3), "junk left out");
        assert_eq!(s.dupes, 2, "one copy of a.txt and c.txt already known");

        let lib_root = root.join("lib");
        fs::create_dir_all(lib_root.join(crate::library::IMAGES_DIR)).unwrap();
        let opts = ImportOptions { link: true, folder: Some("f1".into()), tags: vec!["直播".into()], skip_duplicates: false, ffmpeg_dir: None, cancel: Default::default() };
        let rx = spawn(lib_root, vec![src.join("c.txt")], opts, Known::default(), egui::Context::default());
        let item = rx.into_iter().find_map(|ev| if let ImportEvent::Imported(it) = ev { Some(it) } else { None }).unwrap();
        assert_eq!((item.tags, item.folders), (vec!["直播".to_string()], vec!["f1".to_string()]));
        let _ = fs::remove_dir_all(&root);
    }
    use crate::library::{Library, Originals};

    fn run(lib: &mut Library, inputs: Vec<PathBuf>, link: bool) -> (usize, usize) {
        let existing = lib.known_keys(false);
        let opts = ImportOptions { link, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: None, cancel: Default::default() };
        let rx = spawn(lib.root.clone(), inputs, opts, existing, egui::Context::default());
        let (mut imported, mut dups) = (0, 0);
        for ev in rx {
            match ev {
                ImportEvent::Imported(item) => {
                    imported += 1;
                    lib.items.insert(item.id.clone(), item);
                }
                ImportEvent::Duplicate(_) => dups += 1,
                ImportEvent::Failed(p, e) => panic!("{}: {e}", p.display()),
                ImportEvent::Finished => break,
                ImportEvent::Progress { .. } => {}
            }
        }
        (imported, dups)
    }

    #[test]
    fn import_dedupes_and_survives_reopen() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let red = image::RgbImage::from_pixel(800, 600, image::Rgb([220, 30, 30]));
        red.save(src.join("red.png")).unwrap();
        red.save(src.join("red-copy.png")).unwrap();
        image::RgbImage::from_pixel(64, 64, image::Rgb([20, 40, 230])).save(src.join("blue.jpg")).unwrap();
        fs::write(src.join("notes.srt"), "1\n00:00:01,000 --> 00:00:02,000\nhi\n").unwrap();
        fs::write(src.join("desktop.ini"), "junk").unwrap();

        let lib_root = root.join("lib");
        let mut lib = Library::create(&lib_root).unwrap();
        assert_eq!(run(&mut lib, vec![src.clone()], false), (3, 1));

        let srt = lib.items.values().find(|i| i.ext == "srt").unwrap();
        assert_eq!(srt.kind, MediaKind::Other);
        assert!(lib.file_path(srt).is_file());

        let red = lib.items.values().find(|i| i.width == 800).unwrap().clone();
        assert!(lib.thumb_path(&red.id).is_file());
        assert!(red.palette[0].rgb[0] > 180 && red.palette[0].ratio > 0.9);

        let folder = lib.add_folder("參考", None);
        lib.items.get_mut(&red.id).unwrap().tags.push("紅色".into());
        lib.items.get_mut(&red.id).unwrap().folders.push(folder.clone());
        lib.touch(&red.id);
        lib.flush().unwrap();

        let mut reopened = Library::open(&lib_root).unwrap();
        assert_eq!(reopened.items.len(), 3);
        assert_eq!(reopened.items[&red.id].tags, vec!["紅色".to_string()]);
        assert_eq!(reopened.folder_name(&folder), Some("參考"));

        reopened.delete_folder(&folder);
        assert!(reopened.items[&red.id].folders.is_empty());
        reopened.delete_forever(std::slice::from_ref(&red.id), Originals::Keep);
        assert!(!lib_root.join("images").join(format!("{}.info", red.id)).exists());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn linked_import_leaves_files_in_place() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        image::RgbImage::from_pixel(32, 32, image::Rgb([0, 200, 0])).save(src.join("g.png")).unwrap();
        fs::write(src.join("clip.mp4"), vec![7u8; 6 << 20]).unwrap();

        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(run(&mut lib, vec![src.clone()], true), (2, 0));
        // Re-linking the same folder finds nothing new.
        assert_eq!(run(&mut lib, vec![src.clone()], true), (0, 2));

        let clip = lib.items.values().find(|i| i.ext == "mp4").unwrap().clone();
        assert_eq!(clip.kind, MediaKind::Video);
        assert_eq!(lib.file_path(&clip), dunce_abs(&src.join("clip.mp4")).unwrap());
        assert!(!crate::library::item_dir(&lib.root, &clip.id).join("clip.mp4").exists());

        let g = lib.items.values().find(|i| i.ext == "png").unwrap().id.clone();
        lib.delete_forever(&[g], Originals::Keep);
        assert!(src.join("g.png").exists(), "original kept by default");
        lib.delete_forever(std::slice::from_ref(&clip.id), Originals::Erase);
        assert!(!src.join("clip.mp4").exists(), "original deleted when enabled");

        fs::remove_dir_all(&root).unwrap();
    }

    /// Large files with the same size, start and end but a different middle
    /// share a partial hash; only a true copy is a duplicate.
    #[test]
    fn large_files_differing_in_the_middle_are_not_duplicates() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let mut one = vec![0u8; 6 << 20];
        fs::write(a.join("one.wav"), &one).unwrap();
        one[3 << 20] = 1;
        fs::write(a.join("two.wav"), &one).unwrap();
        fs::write(b.join("copy.wav"), vec![0u8; 6 << 20]).unwrap();
        let hashes: Vec<String> = ["one.wav", "two.wav"].iter().map(|f| quick_hash(&a.join(f)).unwrap()).collect();
        assert!(crate::imaging::is_partial_hash(&hashes[0]) && hashes[0] == hashes[1], "the case this is about");
        assert!(!crate::imaging::same_contents(&a.join("one.wav"), &a.join("two.wav")).unwrap());
        assert!(crate::imaging::same_contents(&a.join("one.wav"), &b.join("copy.wav")).unwrap());

        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(run(&mut lib, vec![a.clone()], false), (2, 0), "both imported in one go");
        assert_eq!(run(&mut lib, vec![b.clone()], false), (0, 1), "a real copy is still a duplicate");

        let out = Arc::new(Mutex::new(ImportScan::default()));
        fs::write(b.join("three.wav"), &one).unwrap();
        one[4 << 20] = 2;
        fs::write(b.join("four.wav"), &one).unwrap();
        scan(vec![b.clone()], lib.known_keys(false), out.clone(), egui::Context::default());
        assert_eq!(lock(&out).dupes, 2, "copy.wav and three.wav are in the library, four.wav isn't");
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod library_tests {
    use super::*;
    use crate::library::{Library, Originals};

    #[test]
    fn changes_survive_reopening_and_trash_round_trips() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("b.txt"), "b").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert!(Library::is_library(&lib.root));
        assert_eq!(import(&mut lib, vec![src.join("a.txt"), src.join("b.txt")], false, false), 2);
        let ids: Vec<String> = lib.items.keys().cloned().collect();
        let folder = lib.add_folder("角色", None);
        for it in lib.items.values_mut() {
            it.tags = vec!["標籤".into()];
            it.folders = vec![folder.clone()];
            it.rating = 4;
        }
        for id in &ids {
            lib.touch(id);
        }
        // Trash one: a flag on the item, its files stay.
        lib.items.get_mut(&ids[0]).unwrap().deleted = true;
        lib.touch(&ids[0]);
        assert!(lib.has_unsaved());
        lib.flush().unwrap();
        assert!(!lib.has_unsaved());

        let mut lib = Library::open(&lib.root).unwrap();
        assert_eq!(lib.items.len(), 2);
        assert_eq!(lib.folder_name(&folder), Some("角色"));
        assert!(lib.items[&ids[0]].deleted && !lib.items[&ids[1]].deleted);
        assert_eq!(lib.items[&ids[1]].tags, vec!["標籤".to_string()]);
        assert_eq!(lib.items[&ids[1]].rating, 4);
        assert_eq!(lib.tag_counts(), vec![("標籤".to_string(), 1)], "trashed items don't count");
        assert!(lib.file_path(&lib.items[&ids[0]]).is_file(), "a trashed copy keeps its file");

        // Restore it from the trash.
        lib.items.get_mut(&ids[0]).unwrap().deleted = false;
        lib.touch(&ids[0]);
        lib.flush().unwrap();
        let lib = Library::open(&lib.root).unwrap();
        assert!(!lib.items[&ids[0]].deleted);
        assert_eq!(lib.folder_counts().get(&folder), Some(&2));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deleting_forever_handles_originals_as_chosen() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        for n in ["keep.txt", "erase.txt", "copy.txt"] {
            fs::write(src.join(n), n).unwrap();
        }
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(import(&mut lib, vec![src.join("keep.txt"), src.join("erase.txt")], true, false), 2);
        assert_eq!(import(&mut lib, vec![src.join("copy.txt")], false, false), 1);
        let id_of = |lib: &Library, name: &str| lib.items.values().find(|i| i.file_name == name).unwrap().id.clone();
        let (keep, erase, copy) = (id_of(&lib, "keep.txt"), id_of(&lib, "erase.txt"), id_of(&lib, "copy.txt"));

        assert!(lib.delete_forever(std::slice::from_ref(&keep), Originals::Keep).is_empty());
        assert!(src.join("keep.txt").is_file(), "Keep leaves the linked original alone");
        assert!(!crate::library::item_dir(&lib.root, &keep).exists());
        assert!(lib.delete_forever(&[erase], Originals::Erase).is_empty());
        assert!(!src.join("erase.txt").exists(), "Erase removes the linked original");
        // A copied item has no original to touch; only its library folder goes.
        assert!(lib.delete_forever(std::slice::from_ref(&copy), Originals::Erase).is_empty());
        assert!(src.join("copy.txt").is_file());
        assert!(!crate::library::item_dir(&lib.root, &copy).exists());
        assert!(lib.items.is_empty());

        // Deleted originals that still exist are not brought back by a later import.
        lib.flush().unwrap();
        let mut lib = Library::open(&lib.root).unwrap();
        assert_eq!(import(&mut lib, vec![src.join("keep.txt"), src.join("copy.txt")], true, true), 0);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_item_deleted_while_its_file_is_open_elsewhere_does_not_come_back() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        // Sorts before metadata.json, so a delete that stops at it never gets there.
        fs::write(root.join("a.txt"), "x").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(import(&mut lib, vec![root.join("a.txt")], false, false), 1);
        let id = lib.items.keys().next().unwrap().clone();
        let dir = crate::library::item_dir(&lib.root, &id);
        // Another program has the copy open without sharing (a video player, say).
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(dir.join("a.txt")).unwrap();
        lib.delete_forever(std::slice::from_ref(&id), Originals::Keep);
        lib.flush().unwrap();
        drop(held);
        let lib = Library::open(&lib.root).unwrap();
        assert!(!lib.items.contains_key(&id), "the deleted item is back");

        // metadata.json itself held open: the item stays rather than half-going.
        let mut lib = lib;
        fs::write(root.join("b.txt"), "y").unwrap();
        assert_eq!(import(&mut lib, vec![root.join("b.txt")], false, false), 1);
        let id = lib.items.keys().next().unwrap().clone();
        let meta = crate::library::item_dir(&lib.root, &id).join(crate::library::META_FILE);
        let held = fs::OpenOptions::new().read(true).share_mode(0).open(&meta).unwrap();
        lib.delete_forever(std::slice::from_ref(&id), Originals::Keep);
        drop(held);
        assert!(lib.items.contains_key(&id), "still listed, as it would be on the next open");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Sends a real file to the Recycle Bin, so it only runs when asked for.
    #[test]
    #[ignore]
    fn deleting_forever_to_the_recycle_bin() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("bin.txt"), "x").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(import(&mut lib, vec![root.join("bin.txt")], true, false), 1);
        let id = lib.items.keys().next().unwrap().clone();
        assert!(lib.delete_forever(&[id], Originals::RecycleBin).is_empty());
        assert!(!root.join("bin.txt").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn relinking_a_moved_original() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        fs::create_dir_all(root.join("old")).unwrap();
        fs::create_dir_all(root.join("new")).unwrap();
        fs::write(root.join("old").join("pic.txt"), "picture").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(import(&mut lib, vec![root.join("old").join("pic.txt")], true, false), 1);
        let id = lib.items.keys().next().unwrap().clone();
        fs::rename(root.join("old").join("pic.txt"), root.join("new").join("pic.txt")).unwrap();
        lib.refresh_missing();
        assert!(lib.missing.contains(&id));
        lib.relink(&id, root.join("new").join("pic.txt"));
        assert!(lib.missing.is_empty());
        lib.flush().unwrap();
        let lib = Library::open(&lib.root).unwrap();
        assert!(lib.missing.is_empty(), "the new location is saved");
        fs::remove_dir_all(&root).unwrap();
    }

    fn import(lib: &mut Library, inputs: Vec<PathBuf>, link: bool, with_ignored: bool) -> usize {
        let opts = ImportOptions { link, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: None, cancel: Default::default() };
        let rx = spawn(lib.root.clone(), inputs, opts, lib.known_keys(with_ignored), egui::Context::default());
        let mut n = 0;
        for ev in rx {
            match ev {
                ImportEvent::Imported(item) => {
                    n += 1;
                    lib.insert_item(item);
                }
                ImportEvent::Finished => break,
                _ => {}
            }
        }
        n
    }

    #[test]
    fn subtitle_pairing_and_custom_fields() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("talk.mp4"), b"not really a video").unwrap();
        fs::write(src.join("talk.srt"), "1\n00:00:01,000 --> 00:00:02,000\n你好\n").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        assert_eq!(import(&mut lib, vec![src.join("talk.mp4")], true, false), 1);
        let id = lib.items.keys().next().unwrap().clone();
        // Its dialogue is searchable from the start.
        assert_eq!(lib.items[&id].text.as_deref(), Some("你好"));
        // An item indexed before has nothing left to do; one that isn't lists where to look.
        assert!(lib.subtitle_jobs().is_empty());
        lib.items.get_mut(&id).unwrap().text = None;
        let jobs = lib.subtitle_jobs();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].1.iter().any(|p| p.file_name().is_some_and(|n| n == "talk.srt")), "{jobs:?}");

        // A same-named file next to the original...
        // (Canonical paths: the temp dir may be a short 8.3 name like RUNNER~1.)
        let canon = |p: PathBuf| fs::canonicalize(p).unwrap();
        assert_eq!(lib.find_subtitle(&id).map(canon), Some(canon(src.join("talk.srt"))));
        // ...or, once that is gone, a same-named subtitle item in the library.
        assert_eq!(import(&mut lib, vec![src.join("talk.srt")], false, false), 1);
        fs::remove_file(src.join("talk.srt")).unwrap();
        let found = lib.find_subtitle(&id).expect("library copy");
        assert!(found.starts_with(&lib.root));
        let cues = crate::media::parse_cues(&crate::media::read_text(&found, 1 << 20).unwrap());
        assert_eq!(cues[0].text, "你好");

        assert!(lib.add_field("授權") && !lib.add_field("  "));
        lib.items.get_mut(&id).unwrap().fields.insert("授權".into(), "CC0".into());
        lib.rename_field("授權", "License");
        assert_eq!(lib.meta.fields, vec!["License".to_string()]);
        assert_eq!(lib.items[&id].fields.get("License").map(String::as_str), Some("CC0"));
        lib.delete_field("License");
        assert!(lib.meta.fields.is_empty() && lib.items[&id].fields.is_empty());

        // Structural edits keep each touched item's previous state for undo.
        assert_eq!(lib.captured.len(), 1);
        assert_eq!(lib.captured[0].fields.get("授權").map(String::as_str), Some("CC0"));
        lib.captured.clear();
        let folder = lib.add_folder("素材", None);
        lib.items.get_mut(&id).unwrap().folders.push(folder.clone());
        let before = lib.meta.clone();
        let rev = lib.meta_rev;
        lib.delete_folder(&folder);
        assert!(lib.meta_rev > rev && !lib.meta.same_structure(&before));
        assert_eq!(lib.captured[0].folders, vec![folder]);
        let mut collapsed = before.clone();
        collapsed.folders[0].collapsed = true;
        assert!(collapsed.same_structure(&before));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn watch_relink_and_folder_moves() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("watch");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.srt"), "1\n00:00:01,000 --> 00:00:02,000\n你好世界\n").unwrap();
        fs::write(src.join("b.txt"), "second").unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();

        // Copy mode remembers where files came from, so a rescan imports nothing.
        assert_eq!(import(&mut lib, vec![src.clone()], false, true), 2);
        assert_eq!(import(&mut lib, vec![src.clone()], false, true), 0);
        let srt = lib.items.values().find(|i| i.ext == "srt").unwrap().clone();
        assert!(srt.text.as_deref().unwrap().contains("你好世界"));
        assert_eq!(crate::media::subtitle_lines(srt.text.as_deref().unwrap()), "你好世界");

        // A permanently deleted file is not brought back by the watch folder,
        // but a manual import still works.
        lib.delete_forever(std::slice::from_ref(&srt.id), Originals::Keep);
        assert_eq!(import(&mut lib, vec![src.clone()], false, true), 0);
        assert_eq!(import(&mut lib, vec![src.join("a.srt")], false, false), 1);

        // Relink a linked item whose original moved.
        let linked_dir = root.join("linked");
        fs::create_dir_all(&linked_dir).unwrap();
        fs::write(linked_dir.join("clip.wav"), vec![1u8; 1000]).unwrap();
        assert_eq!(import(&mut lib, vec![linked_dir.clone()], true, false), 1);
        let moved = root.join("moved");
        fs::create_dir_all(&moved).unwrap();
        fs::rename(linked_dir.join("clip.wav"), moved.join("clip.wav")).unwrap();
        lib.refresh_missing();
        assert_eq!(lib.missing.len(), 1);
        // Same name, other size: a different file. Same name and size, other
        // contents: told apart by the hash.
        let decoys = root.join("decoys");
        fs::create_dir_all(decoys.join("x")).unwrap();
        fs::write(decoys.join("clip.wav"), vec![1u8; 999]).unwrap();
        assert_eq!(lib.relink_in(&decoys), 0, "only a file of another size");
        fs::write(decoys.join("x").join("clip.wav"), vec![2u8; 1000]).unwrap();
        fs::write(decoys.join("clip.wav"), vec![2u8; 1000]).unwrap();
        assert_eq!(lib.relink_in(&root), 1);
        assert!(lib.missing.is_empty());
        let linked = lib.items.values().find(|i| i.file_name == "clip.wav").unwrap();
        assert_eq!(fs::read(lib.file_path(linked)).unwrap(), vec![1u8; 1000], "the file with the right contents");

        // Folders can't be moved into their own subtree.
        let a = lib.add_folder("A", None);
        let b = lib.add_folder("B", Some(a.clone()));
        assert!(!lib.move_folder(&a, Some(b.clone())));
        assert!(lib.move_folder(&b, None));

        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod restore_and_similarity_tests {
    use super::*;
    use crate::library::Library;

    #[test]
    fn dhash_finds_resized_copy_and_backup_restores_tags() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let mut grad = image::RgbImage::new(200, 120);
        for (x, y, p) in grad.enumerate_pixels_mut() {
            *p = image::Rgb([(x + y) as u8, (x * 2) as u8, 255 - y as u8]);
        }
        grad.save(root.join("a.png")).unwrap();
        image::imageops::resize(&grad, 100, 60, image::imageops::FilterType::Triangle).save(root.join("a_small.png")).unwrap();
        let mut checker = image::RgbImage::new(200, 120);
        for (x, y, p) in checker.enumerate_pixels_mut() {
            *p = if (x / 10 + y / 10) % 2 == 0 { image::Rgb([0, 0, 0]) } else { image::Rgb([255, 255, 255]) };
        }
        checker.save(root.join("b.png")).unwrap();
        let h = |n: &str| crate::imaging::dhash(&root.join(n)).unwrap();
        assert!((h("a.png") ^ h("a_small.png")).count_ones() <= 5, "resized copy is a near-duplicate");
        assert!((h("a.png") ^ h("b.png")).count_ones() > 18, "different picture is far");

        let mut lib = Library::create(&root.join("lib")).unwrap();
        let rx = spawn(
            lib.root.clone(),
            vec![root.join("a.png")],
            ImportOptions { link: true, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: None, cancel: Default::default() },
            lib.known_keys(false),
            egui::Context::default(),
        );
        for ev in rx {
            match ev {
                ImportEvent::Imported(item) => lib.insert_item(item),
                ImportEvent::Finished => break,
                _ => {}
            }
        }
        let id = lib.items.keys().next().unwrap().clone();
        lib.items.get_mut(&id).unwrap().tags = vec!["原本".into()];
        lib.touch(&id);
        lib.flush().unwrap();
        let reopened = Library::open(&lib.root).unwrap(); // writes today's backup
        drop(reopened);
        let mut lib = Library::open(&lib.root).unwrap();
        lib.items.get_mut(&id).unwrap().tags = vec!["改壞了".into()];
        lib.touch(&id);
        let backup = lib.list_backups().last().unwrap().0.clone();
        assert_eq!(lib.restore_backup(&backup).unwrap(), 1);
        assert_eq!(lib.items[&id].tags, vec!["原本".to_string()]);
        assert!(lib.list_backups().iter().any(|(p, _)| p.to_string_lossy().contains("before-restore")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn damaged_metadata_is_reported_and_restored_from_backup() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        image::RgbImage::new(8, 8).save(src.join("a.png")).unwrap();
        image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0])).save(src.join("b.png")).unwrap();
        let mut lib = Library::create(&root.join("lib")).unwrap();
        let rx = spawn(
            lib.root.clone(),
            vec![src.join("a.png"), src.join("b.png")],
            ImportOptions { link: true, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: None, cancel: Default::default() },
            lib.known_keys(false),
            egui::Context::default(),
        );
        for ev in rx {
            match ev {
                ImportEvent::Imported(item) => lib.insert_item(item),
                ImportEvent::Finished => break,
                _ => {}
            }
        }
        let ids: Vec<String> = lib.items.keys().cloned().collect();
        let broken = ids[0].clone();
        lib.items.get_mut(&broken).unwrap().tags = vec!["要救回來".into()];
        lib.touch(&broken);
        lib.flush().unwrap();
        drop(Library::open(&lib.root).unwrap()); // writes today's backup
        let backups = lib.root.join("backups");
        let today = fs::read_dir(&backups).unwrap().flatten().next().unwrap().path();
        // Make the backup look old, so a new one would be due.
        fs::rename(&today, backups.join("backup-20000101-000000.json")).unwrap();

        let meta = crate::library::item_dir(&lib.root, &broken).join(crate::library::META_FILE);
        fs::write(&meta, b"{ not json").unwrap();
        // A folder with no metadata.json is an interrupted import, not a lost item.
        fs::create_dir_all(crate::library::item_dir(&lib.root, "leftover")).unwrap();
        let mut lib = Library::open(&lib.root).unwrap();
        assert_eq!(lib.items.len(), 1, "the other item still loads");
        assert_eq!(lib.unreadable.len(), 1);
        assert_eq!(lib.unreadable[0].id, broken);
        assert!(!lib.unreadable[0].error.is_empty());
        assert_eq!(lib.list_backups().len(), 1, "no backup that leaves the item out");

        assert_eq!(lib.restore_unreadable().unwrap(), (1, 0));
        assert!(lib.unreadable.is_empty());
        assert_eq!(lib.items[&broken].tags, vec!["要救回來".to_string()]);
        assert!(meta.with_extension("json.broken").is_file(), "damaged file kept");
        let reopened = Library::open(&lib.root).unwrap();
        assert_eq!(reopened.items.len(), 2);
        assert!(reopened.unreadable.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn one_window_per_library() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let lib = Library::create(&root.join("lib")).unwrap();
        let first = crate::library::LibraryLock::acquire(&lib.root).unwrap();
        assert!(first.is_some());
        let second = crate::library::LibraryLock::acquire(&lib.root);
        assert!(second.err().unwrap().to_string().contains("另一個 OrangeLink"));
        drop(first);
        assert!(crate::library::LibraryLock::acquire(&lib.root).unwrap().is_some(), "free again once closed");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn damaged_library_json_comes_back_from_a_backup() {
        let root = std::env::temp_dir().join(format!("orangelink-test-{}", new_id()));
        let mut lib = Library::create(&root.join("lib")).unwrap();
        let folder = lib.add_folder("參考", None);
        lib.flush().unwrap();
        let lib_file = lib.root.join("library.json");

        fs::write(&lib_file, b"{ broken").unwrap();
        assert!(Library::open(&lib.root).is_err(), "no backup yet: still an error");
        assert!(lib_file.is_file(), "and the damaged file is left alone");

        lib.flush().unwrap();
        lib.touch_meta();
        lib.flush().unwrap();
        drop(Library::open(&lib.root).unwrap()); // writes today's backup
        fs::write(&lib_file, b"{ broken").unwrap();
        let mut back = Library::open(&lib.root).unwrap();
        assert_eq!(back.folder_name(&folder), Some("參考"));
        assert!(back.recovered.take().unwrap().contains("library.json.broken"));
        assert_eq!(fs::read(root.join("lib").join("library.json.broken")).unwrap(), b"{ broken");
        let again = Library::open(&lib.root).unwrap();
        assert!(again.recovered.is_none(), "the restored library.json was written");
        assert_eq!(again.folder_name(&folder), Some("參考"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_named_like_the_librarys_own_are_copied_under_another_name() {
        for name in ["metadata.json", "METADATA.JSON.tmp", "metadata.json.broken", "metadata.json.broken-1", "_thumbnail.png", "_thumbnail.png.tmp"] {
            assert!(is_reserved_name(name), "{name}");
        }
        for name in ["metadata.jsonl.txt", "my metadata.json", "thumbnail.png", "photo.png"] {
            assert!(!is_reserved_name(name), "{name}");
        }
        let root = std::env::temp_dir().join(format!("orangelink-meta-tmp-{}", new_id()));
        fs::create_dir_all(&root).unwrap();
        let src = root.join("metadata.json.tmp");
        fs::write(&src, b"user's file").unwrap();
        let mut lib = crate::library::Library::create(&root.join("lib")).unwrap();
        let opts = ImportOptions { link: false, folder: None, tags: Vec::new(), skip_duplicates: true, ffmpeg_dir: None, cancel: Arc::new(AtomicBool::new(false)) };
        let item = import_one(&lib.root, &src, &opts, &Mutex::new(Known::default()), 0).unwrap().unwrap();
        let id = item.id.clone();
        lib.insert_item(item);
        // Saving the item writes metadata.json through metadata.json.tmp.
        lib.items.get_mut(&id).unwrap().tags.push("x".into());
        lib.touch(&id);
        lib.flush().unwrap();
        let item = &lib.items[&id];
        assert_eq!(fs::read(lib.file_path(item)).unwrap(), b"user's file", "the copy was overwritten while saving");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn copy_import_of_metadata_json_does_not_overwrite_original() {
        let root = std::env::temp_dir().join(format!("orangelink-meta-clash-{}", new_id()));
        let src_dir = root.join("src");
        fs::create_dir_all(&src_dir).unwrap();
        let src = src_dir.join("metadata.json");
        fs::write(&src, b"{\"user\":\"original\"}").unwrap();
        let lib = crate::library::Library::create(&root.join("lib")).unwrap();
        let seen = Mutex::new(Known::default());
        let opts = ImportOptions {
            link: false,
            folder: None,
            tags: Vec::new(),
            skip_duplicates: true,
            ffmpeg_dir: None,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let item = import_one(&lib.root, &src, &opts, &seen, 0).unwrap().unwrap();
        assert_eq!(item.name, "metadata");
        assert_ne!(item.file_name, META_FILE);
        assert_eq!(fs::read(lib.file_path(&item)).unwrap(), b"{\"user\":\"original\"}");
        // Reopening the library still reads the item's own metadata.json cleanly.
        let reopened = crate::library::Library::open(&lib.root).unwrap();
        assert!(reopened.items.contains_key(&item.id));
        fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Never more than `max` inside at once, and a holder that panics frees its place.
    #[test]
    fn big_images_go_a_few_at_a_time() {
        let gate = Gate::new(2);
        let (inside, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        (0..16).into_par_iter().for_each(|_| {
            let _pass = gate.enter();
            let n = inside.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(5));
            inside.fetch_sub(1, Ordering::SeqCst);
        });
        assert!(most.load(Ordering::SeqCst) <= 2);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _pass = gate.enter();
            panic!("decoder");
        }));
        assert_eq!(*lock(&gate.used), 0);

        let dir = std::env::temp_dir().join(format!("orangelink-big-{}", crate::model::new_id()));
        fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.png");
        image::RgbImage::new(8, 8).save(&small).unwrap();
        assert!(!is_big_image(&small, 100));
        assert!(is_big_image(&small, BIG_FILE + 1), "a big file counts without reading it");
        let _ = fs::remove_dir_all(&dir);
    }
}
