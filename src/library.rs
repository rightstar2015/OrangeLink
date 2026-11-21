use crate::model::{new_id, now_ms, Folder, Item, LibraryMeta, SavedFilter, SmartFolder, WatchFolder};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub const LIB_FILE: &str = "library.json";
pub const LOCK_FILE: &str = "library.lock";
pub const IMAGES_DIR: &str = "images";
pub const META_FILE: &str = "metadata.json";
pub const THUMB_FILE: &str = "_thumbnail.png";
/// Days of backups kept; each day keeps its first and its newest backup.
const BACKUP_DAYS: usize = 10;
/// Backups made just before a restore, kept apart from the daily ones.
const BEFORE_RESTORE_KEEP: usize = 5;
/// While the library changes, its newest backup is at most this old.
pub const BACKUP_EVERY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// On disk:
/// ```text
/// <root>/library.json
/// <root>/images/<id>.info/{<original file>, metadata.json, _thumbnail.png}
/// ```
pub struct Library {
    pub root: PathBuf,
    pub meta: LibraryMeta,
    pub items: HashMap<String, Item>,
    dirty: HashSet<String>,
    meta_dirty: bool,
    /// Items whose file is missing on disk (see `refresh_missing`).
    pub missing: HashSet<String>,
    /// Bumped on every change, so the UI can cache derived views.
    pub revision: u64,
    /// Bumped when `meta` (folders, smart folders, tag groups, fields…) changes.
    pub meta_rev: u64,
    /// Item states saved just before a structural change edits them (for undo).
    pub captured: Vec<Item>,
    /// Item folders whose metadata.json could not be read when the library was opened.
    pub unreadable: Vec<Unreadable>,
    /// Set when library.json was damaged and its folders, smart folders, tags and
    /// fields came from a backup instead: what to tell the user.
    pub recovered: Option<String>,
    /// Something was saved since the last backup.
    changed_since_backup: bool,
    /// When this window last wrote a backup of the library.
    last_backup: Option<std::time::Instant>,
}

pub struct Unreadable {
    pub id: String,
    pub error: String,
}

/// What permanently deleting a linked item does to the original file.
#[derive(Clone, Copy, PartialEq)]
pub enum Originals {
    Keep,
    RecycleBin,
    Erase,
}

pub fn item_dir(root: &Path, id: &str) -> PathBuf {
    root.join(IMAGES_DIR).join(format!("{id}.info"))
}

/// Write via a temp file so a crash never leaves half-written JSON behind.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_file(path, &serde_json::to_vec_pretty(value)?, false)
}

/// Like `write_json`, and also waits until the data is on the disk before the
/// rename, so a power cut can't leave an empty file in its place. For the files
/// everything depends on and that change now and then: `library.json`, the config.
pub fn write_json_durable<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_file(path, &serde_json::to_vec_pretty(value)?, true)
}

/// `write_json_durable`, except a file that already holds exactly this is left
/// alone, modified time and all. Returns whether it wrote.
pub fn write_json_if_changed<T: Serialize>(path: &Path, value: &T) -> Result<bool> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    write_file(path, &bytes, true)?;
    Ok(true)
}

fn write_file(path: &Path, bytes: &[u8], sync: bool) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// A backup of all metadata: compact (it can be large and is read only by a
/// restore), and on the disk before it counts.
fn write_backup_file(path: &Path, meta: &LibraryMeta, items: &HashMap<String, Item>) -> Result<()> {
    let items: Vec<&Item> = items.values().collect();
    write_file(path, &serde_json::to_vec(&serde_json::json!({ "meta": meta, "items": items }))?, true)
}

/// What a library check found (`check_integrity`).
#[derive(Debug, Default)]
pub struct Integrity {
    /// Item folders with no metadata.json that no item owns: left by an import
    /// that was cut off (a crash, a power cut).
    pub leftovers: Vec<PathBuf>,
    /// Items whose file is no longer what was imported: (id, its hash now).
    pub changed: Vec<(String, String)>,
    /// Files that could not be read to compare.
    pub unreadable_files: usize,
}

impl Integrity {
    pub fn is_clean(&self) -> bool {
        self.leftovers.is_empty() && self.changed.is_empty()
    }
}

/// What `check_integrity` needs, taken from the library on the UI thread.
pub struct IntegrityInput {
    root: PathBuf,
    /// Every item id, the unreadable ones too.
    known: HashSet<String>,
    /// (id, file, hash taken on import) of the items whose file is there.
    files: Vec<(String, PathBuf, String)>,
}

impl IntegrityInput {
    pub fn total(&self) -> usize {
        self.files.len()
    }
}

/// Looks for leftovers of interrupted imports and for files that changed since
/// they were imported. Counts the files compared in `done`; stops early when
/// `cancel` is set.
pub fn check_integrity(input: IntegrityInput, cancel: &AtomicBool, done: &AtomicUsize) -> Integrity {
    let mut leftovers: Vec<PathBuf> = fs::read_dir(input.root.join(IMAGES_DIR))
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    leftovers.retain(|p| {
        let id = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        p.is_dir() && p.extension().is_some_and(|e| e == "info") && !input.known.contains(&id) && !p.join(META_FILE).exists()
    });
    leftovers.sort();
    let unreadable_files = AtomicUsize::new(0);
    let mut changed: Vec<(String, String)> = input
        .files
        .par_iter()
        .filter_map(|(id, path, hash)| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let now = crate::imaging::quick_hash(path);
            done.fetch_add(1, Ordering::Relaxed);
            match now {
                Ok(h) if h != *hash => Some((id.clone(), h)),
                Ok(_) => None,
                Err(_) => {
                    unreadable_files.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();
    changed.sort();
    Integrity { leftovers, changed, unreadable_files: unreadable_files.into_inner() }
}

/// Something kept in a tree by its parent's id: folders and smart folders.
trait TreeNode {
    fn id(&self) -> &str;
    fn parent_mut(&mut self) -> &mut Option<String>;
}

impl TreeNode for Folder {
    fn id(&self) -> &str {
        &self.id
    }
    fn parent_mut(&mut self) -> &mut Option<String> {
        &mut self.parent
    }
}

impl TreeNode for crate::model::SmartFolder {
    fn id(&self) -> &str {
        &self.id
    }
    fn parent_mut(&mut self) -> &mut Option<String> {
        &mut self.parent
    }
}

/// Makes the folders (or smart folders) a tree again after a hand edit or a
/// restore: a parent that doesn't exist, or one that leads back round to the
/// folder itself, becomes the top level. Without this, a folder in a loop is
/// never reached from the top and never shown. Returns whether anything changed.
fn repair_folder_tree<T: TreeNode>(folders: &mut [T]) -> bool {
    let ids: HashSet<String> = folders.iter().map(|f| f.id().to_string()).collect();
    let mut changed = false;
    for f in folders.iter_mut() {
        let id = f.id().to_string();
        let parent = f.parent_mut();
        if parent.as_ref().is_some_and(|p| *p == id || !ids.contains(p)) {
            *parent = None;
            changed = true;
        }
    }
    for i in 0..folders.len() {
        let start = folders[i].id().to_string();
        let mut seen = HashSet::new();
        let mut up = folders[i].parent_mut().clone();
        while let Some(p) = up {
            if p == start {
                *folders[i].parent_mut() = None;
                changed = true;
                break;
            }
            // A loop further up, without this folder: it is cut when one of its own folders' turn comes.
            if !seen.insert(p.clone()) {
                break;
            }
            up = folders.iter_mut().find(|f| f.id() == p).and_then(|f| f.parent_mut().clone());
        }
    }
    changed
}

/// Held while a window has the library open: `library.lock` opened with no
/// sharing, so a second OrangeLink can't open the same library (each would
/// overwrite the other's saves). Windows lets go of it when the process ends,
/// even after a crash, so a stale lock can't keep the library closed.
pub struct LibraryLock {
    _file: fs::File,
}

impl LibraryLock {
    /// Err when another OrangeLink has the library open. None when the lock
    /// file can't be made at all (a read-only folder): the library still opens.
    pub fn acquire(root: &Path) -> Result<Option<Self>> {
        use std::os::windows::fs::OpenOptionsExt;
        const SHARING_VIOLATION: i32 = 32;
        let path = root.join(LOCK_FILE);
        match fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).share_mode(0).open(&path) {
            Ok(file) => Ok(Some(Self { _file: file })),
            Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) => {
                anyhow::bail!("這個資源庫已經在另一個 OrangeLink 視窗開啟（同時開啟會互相覆寫存檔），請切換到那個視窗")
            }
            Err(e) => {
                crate::log::error("library", format!("無法建立 {}：{e}，這次不鎖定資源庫", path.display()));
                Ok(None)
            }
        }
    }
}

/// The library's backup files, newest first.
fn backup_files(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(root.join("backups"))
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

/// The date in a backup's name (`backup-YYYYMMDD-…`) and whether it was made
/// just before a restore. None for files named otherwise: those are left alone.
fn backup_kind(p: &Path) -> Option<(String, bool)> {
    let name = p.file_stem()?.to_str()?;
    let date = name.strip_prefix("backup-")?.get(..8)?;
    date.bytes().all(|b| b.is_ascii_digit()).then(|| (date.to_string(), name.ends_with("-before-restore")))
}

/// Keeps the backups of the last `BACKUP_DAYS` days (of each day its first and
/// its newest) and the last `BEFORE_RESTORE_KEEP` made before a restore, which
/// don't count as a day's backup.
fn rotate_backups(root: &Path) {
    let mut days: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    let mut before_restore = Vec::new();
    for p in backup_files(root) {
        match backup_kind(&p) {
            Some((_, true)) => before_restore.push(p),
            Some((date, false)) => days.entry(date).or_default().push(p),
            None => {}
        }
    }
    let mut remove: Vec<PathBuf> = before_restore.into_iter().skip(BEFORE_RESTORE_KEEP).collect();
    // Newest day first; within a day, newest file first.
    for (n, (_, files)) in days.into_iter().rev().enumerate() {
        if n >= BACKUP_DAYS {
            remove.extend(files);
        } else if files.len() > 2 {
            remove.extend(files[1..files.len() - 1].iter().cloned());
        }
    }
    for p in remove {
        let _ = fs::remove_file(p);
    }
}

/// The library structure from the newest backup that has a readable one.
fn meta_from_backup(root: &Path) -> Option<(LibraryMeta, PathBuf)> {
    #[derive(serde::Deserialize)]
    struct MetaOnly {
        meta: LibraryMeta,
    }
    backup_files(root).into_iter().find_map(|p| {
        let b = fs::read(&p).ok()?;
        serde_json::from_slice::<MetaOnly>(&b).ok().map(|m| (m.meta, p))
    })
}

/// Renames a damaged file to `<name>.broken` (numbered when that is taken).
fn keep_aside(path: &Path) -> Result<PathBuf> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for n in 0..100 {
        let to = path.with_file_name(if n == 0 { format!("{name}.broken") } else { format!("{name}.broken-{n}") });
        if !to.exists() {
            fs::rename(path, &to)?;
            return Ok(to);
        }
    }
    anyhow::bail!("太多損壞的備份檔")
}

impl Library {
    pub fn is_library(path: &Path) -> bool {
        path.join(LIB_FILE).is_file()
    }

    pub fn create(root: &Path) -> Result<Self> {
        fs::create_dir_all(root.join(IMAGES_DIR))?;
        let meta = LibraryMeta { version: 1, ..Default::default() };
        write_json_durable(&root.join(LIB_FILE), &meta)?;
        Ok(Self {
            unreadable: Vec::new(),
            recovered: None,
            root: root.to_path_buf(),
            meta,
            items: HashMap::new(),
            dirty: HashSet::new(),
            meta_dirty: false,
            missing: HashSet::new(),
            revision: 0,
            meta_rev: 0,
            captured: Vec::new(),
            changed_since_backup: false,
            last_backup: None,
        })
    }

    pub fn open(root: &Path) -> Result<Self> {
        let lib_file = root.join(LIB_FILE);
        let bytes = fs::read(&lib_file).context("讀取 library.json 失敗")?;
        // Damaged (a disk error, a hand edit): the folders, smart folders, tags and
        // fields come from the newest backup that has them, and the damaged file
        // is kept. Items live in their own files and are read as usual.
        let (meta, recovered) = match serde_json::from_slice::<LibraryMeta>(&bytes) {
            Ok(meta) => (meta, None),
            Err(e) => {
                let Some((meta, from)) = meta_from_backup(root) else {
                    anyhow::bail!("library.json 格式錯誤（{e}），也沒有可以還原的備份");
                };
                let kept = keep_aside(&lib_file).context("無法保留損壞的 library.json")?;
                let note = format!(
                    "library.json 損壞，資料夾、智慧資料夾、標籤群組和自訂欄位已從備份 {} 還原；損壞的檔案保留為 {}",
                    from.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                    kept.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                );
                crate::log::error("library", format!("{}：{e}；{note}", root.display()));
                (meta, Some(note))
            }
        };
        let images = root.join(IMAGES_DIR);
        fs::create_dir_all(&images)?;
        let dirs: Vec<PathBuf> = fs::read_dir(&images)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        // A folder without metadata.json is left over from an interrupted import; a
        // metadata.json that can't be read is an item the user would lose without notice.
        let results: Vec<std::result::Result<Item, Unreadable>> = dirs
            .par_iter()
            .filter(|d| d.join(META_FILE).is_file())
            .map(|d| {
                let id = d.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                fs::read(d.join(META_FILE))
                    .map_err(|e| e.to_string())
                    .and_then(|b| serde_json::from_slice::<Item>(&b).map_err(|e| e.to_string()))
                    .map_err(|error| Unreadable { id, error })
            })
            .collect();
        let mut items = HashMap::new();
        let mut unreadable = Vec::new();
        for r in results {
            match r {
                Ok(item) => {
                    items.insert(item.id.clone(), item);
                }
                Err(u) => unreadable.push(u),
            }
        }
        unreadable.sort_by(|a, b| a.id.cmp(&b.id));
        let mut lib = Self {
            root: root.to_path_buf(),
            meta,
            items,
            dirty: HashSet::new(),
            meta_dirty: false,
            missing: HashSet::new(),
            revision: 0,
            meta_rev: 0,
            captured: Vec::new(),
            unreadable,
            recovered,
            changed_since_backup: false,
            last_backup: None,
        };
        lib.refresh_missing();
        if repair_folder_tree(&mut lib.meta.folders) {
            crate::log::error("library", "資料夾的上層不存在或形成循環，已移到最上層");
            lib.meta_dirty = true;
        }
        if repair_folder_tree(&mut lib.meta.smart_folders) {
            crate::log::error("library", "智慧資料夾的上層不存在或形成循環，已移到最上層");
            lib.meta_dirty = true;
        }
        if lib.recovered.is_some() {
            lib.meta_dirty = true;
            lib.flush().context("無法寫入還原的 library.json")?;
        }
        if !lib.meta.smart_defaults_seeded {
            lib.seed_default_smart_folders();
            let _ = lib.flush();
        }
        // After the seeding, so the backup has the default smart folders too.
        lib.backup_if_due();
        Ok(lib)
    }

    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.display().to_string())
    }

    /// The actual file: the original for linked items, else the copy in the library.
    pub fn file_path(&self, item: &Item) -> PathBuf {
        match &item.source {
            Some(src) => src.clone(),
            None => item_dir(&self.root, &item.id).join(&item.file_name),
        }
    }

    /// Re-checks which items' files are gone (e.g. a linked original was moved).
    pub fn refresh_missing(&mut self) {
        self.missing = self
            .items
            .par_iter()
            .filter(|(_, i)| !self.file_path(i).is_file())
            .map(|(id, _)| id.clone())
            .collect();
        self.revision += 1;
    }

    pub fn insert_item(&mut self, item: Item) {
        self.items.insert(item.id.clone(), item);
        self.changed_since_backup = true;
        self.revision += 1;
    }

    pub fn touch_meta(&mut self) {
        self.meta_dirty = true;
        self.meta_rev += 1;
        self.revision += 1;
    }

    /// Points a missing/linked item at a new file location, spelled as an import
    /// would (no `..`, 8.3 names or `\\?\`), so duplicate checks and watch
    /// folders recognise the file.
    pub fn relink(&mut self, id: &str, path: PathBuf) {
        let path = crate::importer::dunce_abs(&path).unwrap_or(path);
        if let Some(item) = self.items.get_mut(id) {
            item.source = Some(path);
            self.missing.remove(id);
            self.touch(id);
        }
    }

    /// Searches `dir` recursively for missing items' files by name and size; when
    /// several have both, by the content hash taken on import. A file of another
    /// size is a different file and is never linked. Returns how many items were
    /// relinked.
    pub fn relink_in(&mut self, dir: &Path) -> usize {
        if self.missing.is_empty() {
            return 0;
        }
        let mut by_name: HashMap<String, Vec<(PathBuf, u64)>> = HashMap::new();
        for e in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
            if e.file_type().is_file() {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                let name = e.file_name().to_string_lossy().to_lowercase();
                by_name.entry(name).or_default().push((e.into_path(), size));
            }
        }
        let missing: Vec<String> = self.missing.iter().cloned().collect();
        let mut fixed = 0;
        for id in missing {
            let Some(item) = self.items.get(&id) else { continue };
            let same_size: Vec<&PathBuf> = by_name
                .get(&item.file_name.to_lowercase())
                .map(|c| c.iter().filter(|(_, size)| *size == item.size).map(|(p, _)| p).collect())
                .unwrap_or_default();
            let found = match same_size.as_slice() {
                [] => None,
                [one] => Some((*one).clone()),
                many => many
                    .iter()
                    .find(|p| !item.hash.is_empty() && crate::imaging::quick_hash(p).is_ok_and(|h| h == item.hash))
                    .map(|p| (*p).clone()),
            };
            if let Some(path) = found {
                self.relink(&id, path);
                fixed += 1;
            }
        }
        fixed
    }

    /// Backup files, newest first: (path, item count).
    pub fn list_backups(&self) -> Vec<(PathBuf, usize)> {
        backup_files(&self.root)
            .into_iter()
            .map(|p| {
                let n = fs::read(&p)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|j| j.get("items").and_then(|i| i.as_array()).map(|a| a.len()))
                    .unwrap_or(0);
                (p, n)
            })
            .collect()
    }

    fn write_backup(&self, name: &str) -> Result<()> {
        let dir = self.root.join("backups");
        fs::create_dir_all(&dir)?;
        write_backup_file(&dir.join(name), &self.meta, &self.items)
    }

    /// Restores folders, smart folders, tags etc. and the metadata of items that
    /// still exist. Items added since the backup are kept. The current state is
    /// backed up first. Returns how many items were restored.
    pub fn restore_backup(&mut self, path: &Path) -> Result<usize> {
        #[derive(serde::Deserialize)]
        struct Backup {
            meta: LibraryMeta,
            items: Vec<Item>,
        }
        let backup: Backup = serde_json::from_slice(&fs::read(path)?).context("備份檔格式錯誤")?;
        self.write_backup(&format!("backup-{}-before-restore.json", chrono::Local::now().format("%Y%m%d-%H%M%S")))?;
        rotate_backups(&self.root);
        let affected: Vec<String> = backup.items.iter().filter(|i| self.items.contains_key(&i.id)).map(|i| i.id.clone()).collect();
        self.capture(&affected);
        let mut restored = 0;
        for item in backup.items {
            let id = item.id.clone();
            if self.items.contains_key(&id) {
                self.items.insert(id.clone(), item);
                self.dirty.insert(id);
                restored += 1;
            }
        }
        let ignored = std::mem::take(&mut self.meta.ignored_sources);
        self.meta = backup.meta;
        self.meta.ignored_sources.extend(ignored);
        repair_folder_tree(&mut self.meta.folders);
        repair_folder_tree(&mut self.meta.smart_folders);
        self.drop_missing_folders();
        self.touch_meta();
        self.flush()?;
        // Linked items now point where they did then, which may be gone (or back).
        self.refresh_missing();
        Ok(restored)
    }

    /// How many items name a folder that doesn't exist (see `drop_missing_folders`).
    pub fn missing_folder_refs(&self) -> usize {
        let ids: HashSet<&str> = self.meta.folders.iter().map(|f| f.id.as_str()).collect();
        self.items.values().filter(|i| i.folders.iter().any(|f| !ids.contains(f.as_str()))).count()
    }

    /// The items and files `check_integrity` looks at.
    pub fn integrity_input(&self) -> IntegrityInput {
        IntegrityInput {
            root: self.root.clone(),
            known: self.items.keys().cloned().chain(self.unreadable.iter().map(|u| u.id.clone())).collect(),
            files: self
                .items
                .values()
                .filter(|i| !i.hash.is_empty() && !self.missing.contains(&i.id))
                .map(|i| (i.id.clone(), self.file_path(i), i.hash.clone()))
                .collect(),
        }
    }

    /// (items waiting to be written, whether library.json is too).
    pub fn unsaved(&self) -> (usize, bool) {
        (self.dirty.len(), self.meta_dirty)
    }

    /// Removes folder ids that no folder has any more, e.g. folders made after the
    /// backup that was just restored; an item left in none of them shows under
    /// 未分類 again. Returns how many items changed.
    pub fn drop_missing_folders(&mut self) -> usize {
        let ids: HashSet<&str> = self.meta.folders.iter().map(|f| f.id.as_str()).collect();
        let mut changed = 0;
        for item in self.items.values_mut() {
            let before = item.folders.len();
            item.folders.retain(|f| ids.contains(f.as_str()));
            if item.folders.len() != before {
                self.dirty.insert(item.id.clone());
                changed += 1;
            }
        }
        if changed > 0 {
            self.revision += 1;
        }
        changed
    }

    /// Brings back unreadable items from the newest backup that has each of them. The
    /// damaged file is kept as `metadata.json.broken` (numbered when an earlier one is
    /// there). Returns (restored, not in any backup).
    pub fn restore_unreadable(&mut self) -> Result<(usize, usize)> {
        let mut wanted: HashSet<String> = self.unreadable.iter().map(|u| u.id.clone()).collect();
        let mut found: Vec<Item> = Vec::new();
        for (path, _) in self.list_backups() {
            if wanted.is_empty() {
                break;
            }
            let Some(items) = fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|mut j| j.get_mut("items").map(serde_json::Value::take))
                .and_then(|v| serde_json::from_value::<Vec<Item>>(v).ok())
            else {
                continue;
            };
            for item in items {
                if wanted.remove(&item.id) {
                    found.push(item);
                }
            }
        }
        let restored = found.len();
        for item in found {
            let meta = item_dir(&self.root, &item.id).join(META_FILE);
            keep_aside(&meta).context("無法保留損壞的檔案")?;
            let id = item.id.clone();
            self.insert_item(item);
            self.dirty.insert(id.clone());
            self.unreadable.retain(|u| u.id != id);
        }
        // The backup can be older than a folder's deletion.
        self.drop_missing_folders();
        self.flush()?;
        self.refresh_missing();
        Ok((restored, wanted.len()))
    }

    /// Whether a backup now is safe. Not with unreadable items (the backup would
    /// leave them out, and rotating could drop the older backups that still have
    /// them), and not while library.json has just been recovered from a backup
    /// and the user hasn't been told yet.
    fn may_back_up(&self) -> bool {
        self.unreadable.is_empty() && self.recovered.is_none()
    }

    /// The day's first backup, on opening, when today has none yet.
    fn backup_if_due(&mut self) {
        if !self.may_back_up() {
            return;
        }
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        let has_today = backup_files(&self.root).iter().any(|p| backup_kind(p).is_some_and(|(day, before)| day == today && !before));
        if !has_today {
            self.write_daily_backup();
        }
    }

    /// A fresh backup when something was saved since the last one and `every`
    /// has passed, so the day's work is in a backup too, not only the state the
    /// library was opened in. The day's first backup stays; the newer ones of
    /// the day replace each other. Called every few seconds with `BACKUP_EVERY`,
    /// and with zero when the library is closed.
    pub fn backup_if_changed(&mut self, every: std::time::Duration) {
        if !self.changed_since_backup || !self.may_back_up() || self.last_backup.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        self.write_daily_backup();
    }

    fn write_daily_backup(&mut self) {
        let name = format!("backup-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S"));
        // Tried again at the next chance either way: a failing disk shows up as a failing save too.
        self.last_backup = Some(std::time::Instant::now());
        match self.write_backup(&name) {
            Ok(()) => {
                self.changed_since_backup = false;
                rotate_backups(&self.root);
            }
            Err(e) => crate::log::error("library", format!("無法寫入備份 {name}：{e:#}")),
        }
    }

    pub fn thumb_path(&self, id: &str) -> PathBuf {
        item_dir(&self.root, id).join(THUMB_FILE)
    }

    /// Keeps the current state of `ids` in `captured` before they are edited.
    fn capture(&mut self, ids: &[String]) {
        for id in ids {
            if !self.captured.iter().any(|c| c.id == *id) {
                if let Some(it) = self.items.get(id) {
                    self.captured.push(it.clone());
                }
            }
        }
    }

    /// Marks an item as changed; it is written on the next `flush`.
    pub fn touch(&mut self, id: &str) {
        if let Some(item) = self.items.get_mut(id) {
            item.modified_at = now_ms();
            self.dirty.insert(id.to_string());
            self.revision += 1;
        }
    }

    pub fn has_unsaved(&self) -> bool {
        self.meta_dirty || !self.dirty.is_empty()
    }

    /// Writes every changed item and `library.json`. One file failing does not
    /// stop the rest: what could not be written stays dirty for the next flush,
    /// and the error names how many failed and the first reason.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty.is_empty() || self.meta_dirty {
            self.changed_since_backup = true;
        }
        let mut failed: Vec<(String, anyhow::Error)> = Vec::new();
        for id in std::mem::take(&mut self.dirty) {
            if let Some(item) = self.items.get(&id) {
                if let Err(e) = write_json(&item_dir(&self.root, &id).join(META_FILE), item) {
                    failed.push((id, e));
                }
            }
        }
        let mut meta_error = None;
        if self.meta_dirty {
            self.meta.ignored_sources.sort();
            self.meta.ignored_sources.dedup();
            match write_json_durable(&self.root.join(LIB_FILE), &self.meta) {
                Ok(()) => self.meta_dirty = false,
                Err(e) => meta_error = Some(e),
            }
        }
        if failed.is_empty() && meta_error.is_none() {
            return Ok(());
        }
        let mut parts = Vec::new();
        if let Some(e) = &meta_error {
            parts.push(format!("資源庫設定（library.json）：{e:#}"));
        }
        if let Some((id, e)) = failed.first() {
            let name = self.items.get(id).map(|i| i.name.as_str()).unwrap_or(id);
            let more = if failed.len() > 1 { format!("等 {} 個素材", failed.len()) } else { String::new() };
            parts.push(format!("「{name}」{more}：{e:#}"));
        }
        for (id, e) in &failed {
            crate::log::error("save", format!("{id}: {e:#}"));
        }
        if let Some(e) = &meta_error {
            crate::log::error("save", format!("library.json: {e:#}"));
        }
        self.dirty.extend(failed.into_iter().map(|(id, _)| id));
        anyhow::bail!("{}", parts.join("；"))
    }

    /// Content hashes plus imported-from paths, for duplicate detection on import.
    /// `with_ignored` adds permanently deleted paths (used for watch folders).
    /// A large file's partial hash also lists the item files it came from, so a
    /// match can be confirmed by content.
    pub fn known_keys(&self, with_ignored: bool) -> crate::importer::Known {
        let mut known = crate::importer::Known::default();
        for it in self.items.values() {
            if !it.hash.is_empty() {
                if crate::imaging::is_partial_hash(&it.hash) {
                    known.files.entry(it.hash.clone()).or_default().push(self.file_path(it));
                }
                known.keys.insert(it.hash.clone());
            }
            if let Some(p) = it.imported_from() {
                known.keys.insert(crate::importer::source_key(p));
            }
        }
        if with_ignored {
            known.keys.extend(self.meta.ignored_sources.iter().cloned());
        }
        known
    }

    /// Takes changed files as they are now: each item's hash becomes the one the
    /// check found. Items gone since are skipped. Returns how many it updated.
    pub fn accept_hashes(&mut self, changed: Vec<(String, String)>) -> usize {
        let mut n = 0;
        for (id, hash) in changed {
            if let Some(item) = self.items.get_mut(&id) {
                item.hash = hash;
                self.touch(&id);
                n += 1;
            }
        }
        n
    }

    /// Removes items from the library. What happens to linked originals depends
    /// on `originals`. Returns original files that could not be deleted. An item
    /// whose metadata.json can't be deleted stays in the library.
    pub fn delete_forever(&mut self, ids: &[String], originals: Originals) -> Vec<PathBuf> {
        let mut failed = Vec::new();
        for id in ids {
            // metadata.json goes first: it is what brings an item back on the next
            // open, while a file another program holds open can stop the rest of
            // the folder going (that becomes a leftover the library check clears).
            // When even metadata.json can't go, the item stays.
            let dir = item_dir(&self.root, id);
            if self.items.contains_key(id) {
                match fs::remove_file(dir.join(META_FILE)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                        crate::log::error("delete", format!("無法刪除 {}：{e}", dir.display()));
                        continue;
                    }
                    _ => {}
                }
            }
            if let Some(item) = self.items.remove(id) {
                if let Some(from) = item.imported_from() {
                    self.meta.ignored_sources.push(crate::importer::source_key(from));
                    self.touch_meta();
                }
                if let Some(src) = item.source.as_ref().filter(|s| s.exists()) {
                    let result = match originals {
                        Originals::Keep => Ok(()),
                        Originals::RecycleBin => trash::delete(src).map_err(|e| e.to_string()),
                        Originals::Erase => fs::remove_file(src).map_err(|e| e.to_string()),
                    };
                    if result.is_err() {
                        failed.push(src.clone());
                    }
                }
                if let Err(e) = fs::remove_dir_all(&dir) {
                    crate::log::error("delete", format!("無法刪除 {}：{e}", dir.display()));
                }
                self.revision += 1;
            }
            self.dirty.remove(id);
            self.missing.remove(id);
        }
        failed
    }

    /// Tags of non-deleted items with their usage counts, sorted by name.
    pub fn tag_counts(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for item in self.items.values().filter(|i| !i.deleted) {
            for t in &item.tags {
                *counts.entry(t.as_str()).or_default() += 1;
            }
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().map(|(t, n)| (t.to_string(), n)).collect();
        v.sort_by_key(|a| a.0.to_lowercase());
        v
    }

    pub fn folder_counts(&self) -> HashMap<String, usize> {
        let mut counts = HashMap::new();
        for item in self.items.values().filter(|i| !i.deleted) {
            for f in &item.folders {
                *counts.entry(f.clone()).or_default() += 1;
            }
        }
        counts
    }

    pub fn folder_name(&self, id: &str) -> Option<&str> {
        self.meta.folders.iter().find(|f| f.id == id).map(|f| f.name.as_str())
    }

    pub fn child_folders(&self, parent: Option<&str>) -> Vec<&Folder> {
        let mut v: Vec<&Folder> = self
            .meta
            .folders
            .iter()
            .filter(|f| f.parent.as_deref() == parent)
            .collect();
        v.sort_by_key(|a| a.name.to_lowercase());
        v
    }

    /// Depth-first list of (id, name, depth), for menus.
    pub fn folder_flat(&self) -> Vec<(String, String, usize)> {
        fn walk(lib: &Library, parent: Option<&str>, depth: usize, out: &mut Vec<(String, String, usize)>) {
            for f in lib.child_folders(parent) {
                out.push((f.id.clone(), f.name.clone(), depth));
                walk(lib, Some(&f.id), depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(self, None, 0, &mut out);
        out
    }

    pub fn add_folder(&mut self, name: &str, parent: Option<String>) -> String {
        let id = new_id();
        self.meta.folders.push(Folder {
            id: id.clone(),
            name: name.to_string(),
            parent,
            created_at: now_ms(),
            color: None,
            collapsed: false,
        });
        self.touch_meta();
        id
    }

    fn folder_mut(&mut self, id: &str) -> Option<&mut Folder> {
        self.meta.folders.iter_mut().find(|f| f.id == id)
    }

    pub fn set_folder_color(&mut self, id: &str, color: Option<[u8; 3]>) {
        if let Some(f) = self.folder_mut(id) {
            f.color = color;
            self.touch_meta();
        }
    }

    pub fn toggle_folder_collapsed(&mut self, id: &str) {
        if let Some(f) = self.folder_mut(id) {
            f.collapsed = !f.collapsed;
            self.touch_meta();
        }
    }

    /// Moves a folder under `parent` (None = top level). Refuses to create cycles.
    pub fn move_folder(&mut self, id: &str, parent: Option<String>) -> bool {
        if let Some(p) = &parent {
            if self.folder_with_descendants(id).contains(p) {
                return false;
            }
        }
        match self.folder_mut(id) {
            Some(f) => {
                f.parent = parent;
                self.touch_meta();
                true
            }
            None => false,
        }
    }

    pub fn add_smart_folder(&mut self, name: &str, filter: SavedFilter, parent: Option<String>, group: bool) -> String {
        let id = new_id();
        self.meta.smart_folders.push(SmartFolder {
            id: id.clone(),
            name: name.to_string(),
            filter,
            parent,
            group,
            collapsed: false,
        });
        self.touch_meta();
        id
    }

    /// Smart folders under `parent`, groups first, in creation order.
    pub fn child_smart_folders(&self, parent: Option<&str>) -> Vec<&SmartFolder> {
        let mut v: Vec<&SmartFolder> = self.meta.smart_folders.iter().filter(|s| s.parent.as_deref() == parent).collect();
        v.sort_by_key(|s| !s.group);
        v
    }

    fn smart_with_descendants(&self, id: &str) -> HashSet<String> {
        let mut out = HashSet::from([id.to_string()]);
        loop {
            let before = out.len();
            for s in &self.meta.smart_folders {
                if s.parent.as_ref().is_some_and(|p| out.contains(p)) {
                    out.insert(s.id.clone());
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }

    pub fn toggle_smart_collapsed(&mut self, id: &str) {
        if let Some(s) = self.meta.smart_folders.iter_mut().find(|s| s.id == id) {
            s.collapsed = !s.collapsed;
            self.touch_meta();
        }
    }

    pub fn move_smart_folder(&mut self, id: &str, parent: Option<String>) -> bool {
        if parent.as_ref().is_some_and(|p| self.smart_with_descendants(id).contains(p)) {
            return false;
        }
        match self.meta.smart_folders.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.parent = parent;
                self.touch_meta();
                true
            }
            None => false,
        }
    }

    pub fn update_smart_filter(&mut self, id: &str, filter: SavedFilter) {
        if let Some(s) = self.meta.smart_folders.iter_mut().find(|s| s.id == id) {
            s.filter = filter;
            self.touch_meta();
        }
    }

    /// Creates the built-in smart-folder tree (by type, rating, date, size, status).
    pub fn seed_default_smart_folders(&mut self) {
        use crate::model::{MediaKind, Orientation};
        let f = SavedFilter::default;
        let tree: Vec<(&str, Vec<(&str, SavedFilter)>)> = vec![
            (
                "依類型",
                vec![
                    ("圖片", SavedFilter { kind: Some(MediaKind::Image), ..f() }),
                    ("影片", SavedFilter { kind: Some(MediaKind::Video), ..f() }),
                    ("音訊", SavedFilter { kind: Some(MediaKind::Audio), ..f() }),
                    (
                        "字幕與文字",
                        SavedFilter { exts: ["srt", "vtt", "ass", "ssa", "lrc", "txt"].map(String::from).to_vec(), ..f() },
                    ),
                ],
            ),
            (
                "依評分",
                vec![
                    ("★★★★★", SavedFilter { min_rating: 5, ..f() }),
                    ("★4 以上", SavedFilter { min_rating: 4, ..f() }),
                    ("未評分", SavedFilter { unrated: true, ..f() }),
                ],
            ),
            (
                "依加入時間",
                vec![
                    ("今天", SavedFilter { added_days: Some(1), ..f() }),
                    ("最近 7 天", SavedFilter { added_days: Some(7), ..f() }),
                    ("最近 30 天", SavedFilter { added_days: Some(30), ..f() }),
                ],
            ),
            (
                "依尺寸",
                vec![
                    ("橫向", SavedFilter { orientation: Some(Orientation::Landscape), ..f() }),
                    ("直向", SavedFilter { orientation: Some(Orientation::Portrait), ..f() }),
                    ("正方形", SavedFilter { orientation: Some(Orientation::Square), ..f() }),
                    ("4K 以上", SavedFilter { min_long_side: Some(3840), ..f() }),
                    ("大檔案（100 MB 以上）", SavedFilter { min_size_mb: Some(100), ..f() }),
                ],
            ),
            (
                "整理狀態",
                vec![
                    ("未分類", SavedFilter { uncategorized: true, ..f() }),
                    ("未標籤", SavedFilter { untagged: true, ..f() }),
                    ("保留在原位置", SavedFilter { linked: Some(true), ..f() }),
                    ("找不到原始檔", SavedFilter { missing: true, ..f() }),
                    ("從未使用", SavedFilter { never_used: true, ..f() }),
                ],
            ),
            (
                "剪輯",
                vec![
                    (
                        "待處理的直播",
                        SavedFilter { kind: Some(MediaKind::Video), min_duration: Some(30.0 * 60.0), has_clips: Some(false), ..f() },
                    ),
                    ("有精華候選", SavedFilter { has_candidates: true, ..f() }),
                    ("有片段", SavedFilter { has_clips: Some(true), ..f() }),
                    ("沒有字幕的影音", SavedFilter { has_subtitle: Some(false), ..f() }),
                ],
            ),
        ];
        // Again from the menu: only the groups not there (by name).
        let have: Vec<String> = self.meta.smart_folders.iter().filter(|s| s.group && s.parent.is_none()).map(|s| s.name.clone()).collect();
        for (group, children) in tree.into_iter().filter(|(g, _)| !have.iter().any(|h| h == g)) {
            let gid = self.add_smart_folder(group, f(), None, true);
            for (name, filter) in children {
                self.add_smart_folder(name, filter, Some(gid.clone()), false);
            }
        }
        self.meta.smart_defaults_seeded = true;
        self.touch_meta();
    }

    pub fn add_tag_group(&mut self, name: &str) -> String {
        let id = new_id();
        self.meta.tag_groups.push(crate::model::TagGroup { id: id.clone(), name: name.to_string(), tags: Vec::new(), collapsed: false });
        self.touch_meta();
        id
    }

    pub fn rename_tag_group(&mut self, id: &str, name: &str) {
        if let Some(g) = self.meta.tag_groups.iter_mut().find(|g| g.id == id) {
            g.name = name.to_string();
            self.touch_meta();
        }
    }

    pub fn delete_tag_group(&mut self, id: &str) {
        self.meta.tag_groups.retain(|g| g.id != id);
        self.touch_meta();
    }

    pub fn toggle_tag_group(&mut self, id: &str) {
        if let Some(g) = self.meta.tag_groups.iter_mut().find(|g| g.id == id) {
            g.collapsed = !g.collapsed;
            self.touch_meta();
        }
    }

    /// Puts `tag` in group `id` (None = ungrouped). A tag belongs to at most one group.
    pub fn set_tag_group(&mut self, tag: &str, id: Option<&str>) {
        for g in &mut self.meta.tag_groups {
            g.tags.retain(|t| t != tag);
            if Some(g.id.as_str()) == id {
                g.tags.push(tag.to_string());
                g.tags.sort_by_key(|t| t.to_lowercase());
            }
        }
        self.touch_meta();
    }

    pub fn rename_smart_folder(&mut self, id: &str, name: &str) {
        if let Some(s) = self.meta.smart_folders.iter_mut().find(|s| s.id == id) {
            s.name = name.to_string();
            self.touch_meta();
        }
    }

    pub fn delete_smart_folder(&mut self, id: &str) {
        let gone = self.smart_with_descendants(id);
        self.meta.smart_folders.retain(|s| !gone.contains(&s.id));
        self.touch_meta();
    }

    pub fn smart_folder(&self, id: &str) -> Option<&SmartFolder> {
        self.meta.smart_folders.iter().find(|s| s.id == id)
    }

    pub fn add_watch_folder(&mut self, path: PathBuf, link: bool) {
        if !self.meta.watch_folders.iter().any(|w| w.path == path) {
            self.meta.watch_folders.push(WatchFolder { path, link, folder: None, tags: Vec::new() });
            self.touch_meta();
        }
    }

    /// Relates two items both ways. False if either is missing or they are the same.
    pub fn link_items(&mut self, a: &str, b: &str) -> bool {
        if a == b || !self.items.contains_key(a) || !self.items.contains_key(b) {
            return false;
        }
        for (x, y) in [(a, b), (b, a)] {
            if let Some(it) = self.items.get_mut(x) {
                if !it.links.iter().any(|l| l == y) {
                    it.links.push(y.to_string());
                    self.touch(x);
                }
            }
        }
        true
    }

    /// Related items that are still there (not deleted for good or in the trash).
    pub fn linked(&self, id: &str) -> Vec<&Item> {
        self.items.get(id).map_or_else(Vec::new, |it| it.links.iter().filter_map(|l| self.items.get(l)).filter(|i| !i.deleted).collect())
    }

    /// Where a watch folder's new files go and which tags they get.
    pub fn set_watch_rule(&mut self, path: &Path, folder: Option<String>, tags: Vec<String>) {
        if let Some(w) = self.meta.watch_folders.iter_mut().find(|w| w.path == path) {
            if w.folder != folder || w.tags != tags {
                (w.folder, w.tags) = (folder, tags);
                self.touch_meta();
            }
        }
    }

    /// Every folder with its depth, in tree order (for pickers).
    pub fn folder_tree(&self) -> Vec<(usize, &Folder)> {
        fn walk<'a>(lib: &'a Library, parent: Option<&str>, depth: usize, out: &mut Vec<(usize, &'a Folder)>) {
            for f in lib.child_folders(parent) {
                out.push((depth, f));
                walk(lib, Some(&f.id), depth + 1, out);
            }
        }
        let mut out = Vec::new();
        walk(self, None, 0, &mut out);
        out
    }

    pub fn remove_watch_folder(&mut self, path: &Path) {
        self.meta.watch_folders.retain(|w| w.path != path);
        self.touch_meta();
    }

    pub fn rename_folder(&mut self, id: &str, name: &str) {
        if let Some(f) = self.meta.folders.iter_mut().find(|f| f.id == id) {
            f.name = name.to_string();
            self.touch_meta();
        }
    }

    fn folder_with_descendants(&self, id: &str) -> HashSet<String> {
        let mut out = HashSet::from([id.to_string()]);
        loop {
            let before = out.len();
            for f in &self.meta.folders {
                if f.parent.as_ref().is_some_and(|p| out.contains(p)) {
                    out.insert(f.id.clone());
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }

    /// Deletes the folder and its subfolders. Items stay in the library.
    pub fn delete_folder(&mut self, id: &str) {
        let gone = self.folder_with_descendants(id);
        self.meta.folders.retain(|f| !gone.contains(&f.id));
        self.touch_meta();
        let affected: Vec<String> = self
            .items
            .values()
            .filter(|i| i.folders.iter().any(|f| gone.contains(f)))
            .map(|i| i.id.clone())
            .collect();
        self.capture(&affected);
        for iid in affected {
            if let Some(item) = self.items.get_mut(&iid) {
                item.folders.retain(|f| !gone.contains(f));
            }
            self.touch(&iid);
        }
    }

    pub fn rename_tag(&mut self, old: &str, new: &str) {
        for g in &mut self.meta.tag_groups {
            if let Some(t) = g.tags.iter_mut().find(|t| *t == old) {
                *t = new.to_string();
            }
            g.tags.sort_by_key(|t| t.to_lowercase());
            g.tags.dedup();
        }
        self.touch_meta();
        let affected: Vec<String> = self
            .items
            .values()
            .filter(|i| i.tags.iter().any(|t| t == old))
            .map(|i| i.id.clone())
            .collect();
        self.capture(&affected);
        for id in affected {
            if let Some(item) = self.items.get_mut(&id) {
                item.tags.retain(|t| t != old);
                if !item.tags.iter().any(|t| t == new) {
                    item.tags.push(new.to_string());
                }
            }
            self.touch(&id);
        }
    }

    pub fn delete_tag(&mut self, tag: &str) {
        for g in &mut self.meta.tag_groups {
            g.tags.retain(|t| t != tag);
        }
        self.touch_meta();
        let affected: Vec<String> = self
            .items
            .values()
            .filter(|i| i.tags.iter().any(|t| t == tag))
            .map(|i| i.id.clone())
            .collect();
        self.capture(&affected);
        for id in affected {
            if let Some(item) = self.items.get_mut(&id) {
                item.tags.retain(|t| t != tag);
            }
            self.touch(&id);
        }
    }

    /// Adds a custom field name (no-op if it exists). Returns false for an empty name.
    pub fn add_field(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        if !self.meta.fields.iter().any(|f| f == name) {
            self.meta.fields.push(name.to_string());
            self.touch_meta();
        }
        true
    }

    pub fn rename_field(&mut self, old: &str, new: &str) {
        let new = new.trim();
        if new.is_empty() || old == new {
            return;
        }
        self.meta.fields.retain(|f| f != new);
        if let Some(f) = self.meta.fields.iter_mut().find(|f| *f == old) {
            *f = new.to_string();
        }
        self.touch_meta();
        let affected: Vec<String> = self.items.values().filter(|i| i.fields.contains_key(old)).map(|i| i.id.clone()).collect();
        self.capture(&affected);
        for id in affected {
            if let Some(item) = self.items.get_mut(&id) {
                if let Some(v) = item.fields.remove(old) {
                    item.fields.insert(new.to_string(), v);
                }
            }
            self.touch(&id);
        }
    }

    pub fn delete_field(&mut self, name: &str) {
        self.meta.fields.retain(|f| f != name);
        self.touch_meta();
        let affected: Vec<String> = self.items.values().filter(|i| i.fields.contains_key(name)).map(|i| i.id.clone()).collect();
        self.capture(&affected);
        for id in affected {
            if let Some(item) = self.items.get_mut(&id) {
                item.fields.remove(name);
            }
            self.touch(&id);
        }
    }

    /// Video/audio items whose dialogue isn't searchable yet, each with the files that may
    /// hold their subtitles, in `find_subtitle`'s order. Touches no files, so it is quick.
    pub fn subtitle_jobs(&self) -> Vec<(String, Vec<PathBuf>)> {
        use crate::model::MediaKind;
        let stem = |s: &str| s.rsplit_once('.').map(|(a, _)| a).unwrap_or(s).to_lowercase();
        let mut in_lib: HashMap<String, PathBuf> = HashMap::new();
        for s in self.items.values().filter(|s| !s.deleted && matches!(s.ext.as_str(), "srt" | "vtt")) {
            for key in [s.name.to_lowercase(), stem(&s.file_name)] {
                in_lib.entry(key).or_insert_with(|| self.file_path(s));
            }
        }
        let mut jobs: Vec<(String, Vec<PathBuf>)> = self
            .items
            .values()
            .filter(|it| !it.deleted && it.text.is_none() && matches!(it.kind, MediaKind::Video | MediaKind::Audio))
            .map(|it| {
                let mut c: Vec<PathBuf> = it.subtitle.iter().cloned().collect();
                c.extend([it.name.to_lowercase(), stem(&it.file_name)].iter().filter_map(|k| in_lib.get(k).cloned()));
                if let Some(orig) = it.imported_from() {
                    c.extend(["srt", "vtt"].map(|e| orig.with_extension(e)));
                }
                (it.id.clone(), c)
            })
            .filter(|(_, c)| !c.is_empty())
            .collect();
        jobs.sort();
        jobs
    }

    /// Video and audio items with a subtitle picked by hand or a library subtitle
    /// item of the same name. Like `find_subtitle` without looking at the disk, so
    /// a file next to the original that was never imported doesn't count.
    pub fn subtitled_ids(&self) -> std::collections::HashSet<String> {
        let stem = |s: &str| s.rsplit_once('.').map(|(a, _)| a).unwrap_or(s).to_lowercase();
        let names: std::collections::HashSet<String> = self
            .items
            .values()
            .filter(|s| !s.deleted && ["srt", "vtt"].contains(&s.ext.as_str()))
            .flat_map(|s| [s.name.to_lowercase(), stem(&s.file_name)])
            .collect();
        self.items
            .values()
            .filter(|it| matches!(it.kind, crate::model::MediaKind::Video | crate::model::MediaKind::Audio))
            .filter(|it| it.subtitle.is_some() || names.contains(&it.name.to_lowercase()) || names.contains(&stem(&it.file_name)))
            .map(|it| it.id.clone())
            .collect()
    }

    /// The subtitle file for a video/audio item: the one picked by hand, a library
    /// subtitle item with the same name, or a file with the same name next to the original.
    pub fn find_subtitle(&self, id: &str) -> Option<PathBuf> {
        const EXTS: [&str; 2] = ["srt", "vtt"];
        let it = self.items.get(id)?;
        if let Some(p) = it.subtitle.as_ref().filter(|p| p.is_file()) {
            return Some(p.clone());
        }
        let stem = |s: &str| s.rsplit_once('.').map(|(a, _)| a).unwrap_or(s).to_lowercase();
        let want = [it.name.to_lowercase(), stem(&it.file_name)];
        let in_lib = self.items.values().find(|s| {
            !s.deleted
                && EXTS.contains(&s.ext.as_str())
                && (want.contains(&s.name.to_lowercase()) || want.contains(&stem(&s.file_name)))
        });
        if let Some(s) = in_lib {
            let p = self.file_path(s);
            if p.is_file() {
                return Some(p);
            }
        }
        let orig = it.imported_from()?;
        EXTS.iter().map(|e| orig.with_extension(e)).find(|p| p.is_file())
    }

    /// The video or audio a subtitle item goes with: the reverse of
    /// `find_subtitle` (picked for it, or the same name).
    pub fn media_for_subtitle(&self, sub_id: &str) -> Option<String> {
        let s = self.items.get(sub_id)?;
        let path = self.file_path(s);
        let stem = |s: &str| s.rsplit_once('.').map(|(a, _)| a).unwrap_or(s).to_lowercase();
        let names = [s.name.to_lowercase(), stem(&s.file_name)];
        let mut found: Vec<&Item> = self
            .items
            .values()
            .filter(|it| !it.deleted && matches!(it.kind, crate::model::MediaKind::Video | crate::model::MediaKind::Audio))
            .filter(|it| it.subtitle.as_ref() == Some(&path) || names.contains(&it.name.to_lowercase()) || names.contains(&stem(&it.file_name)))
            .collect();
        // Picked by hand first, then a video before its audio; the same answer every frame.
        found.sort_by_key(|it| (it.subtitle.as_ref() != Some(&path), it.kind != crate::model::MediaKind::Video, it.id.clone()));
        found.first().map(|it| it.id.clone())
    }
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
    fn a_subtitle_finds_its_video() {
        use crate::model::MediaKind;
        let root = std::env::temp_dir().join(format!("orangelink-subfor-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        let mut add = |id: &str, name: &str, ext: &str, kind: MediaKind| {
            let mut it = item(id);
            it.name = name.into();
            it.file_name = format!("{name}.{ext}");
            it.ext = ext.into();
            it.kind = kind;
            lib.items.insert(id.into(), it);
        };
        add("v", "直播 0301", "mp4", MediaKind::Video);
        add("a", "直播 0301", "m4a", MediaKind::Audio);
        add("s", "直播 0301", "srt", MediaKind::Other);
        add("x", "別的", "srt", MediaKind::Other);
        assert_eq!(lib.media_for_subtitle("s").as_deref(), Some("v"), "the video before its audio");
        assert_eq!(lib.media_for_subtitle("x"), None);
        // Picked by hand wins over the name.
        let path = lib.file_path(&lib.items["x"]);
        lib.items.get_mut("a").unwrap().subtitle = Some(path);
        assert_eq!(lib.media_for_subtitle("x").as_deref(), Some("a"));
        lib.items.get_mut("v").unwrap().deleted = true;
        assert_eq!(lib.media_for_subtitle("s").as_deref(), Some("a"));
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn links_go_both_ways_and_skip_what_is_gone() {
        let root = std::env::temp_dir().join(format!("orangelink-links-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        for id in ["stream", "clip", "frame"] {
            lib.insert_item(item(id));
        }
        assert!(lib.link_items("clip", "stream") && lib.link_items("frame", "stream"));
        assert!(lib.link_items("clip", "stream"), "again changes nothing");
        assert!(!lib.link_items("clip", "clip") && !lib.link_items("clip", "nope"));
        let names = |lib: &Library, id: &str| lib.linked(id).iter().map(|i| i.id.clone()).collect::<Vec<_>>();
        assert_eq!(names(&lib, "stream"), ["clip", "frame"]);
        assert_eq!(names(&lib, "clip"), ["stream"]);
        lib.items.get_mut("frame").unwrap().deleted = true;
        assert_eq!(names(&lib, "stream"), ["clip"], "trashed items are not listed");
        lib.items.remove("clip");
        assert!(names(&lib, "stream").is_empty(), "nor ones deleted for good");

        // Merging keeps the other item's links.
        let (mut keep, mut other) = (item("a"), item("b"));
        other.links = vec!["a".into(), "c".into()];
        keep.links = vec!["c".into()];
        crate::model::merge_into(&mut keep, &other);
        assert_eq!(keep.links, ["c"], "not itself, not twice");
        let _ = fs::remove_dir_all(&root);
    }

    /// One item that cannot be written must not drop the others' changes,
    /// and is written by a later flush once the problem is gone.
    #[test]
    fn a_failed_write_keeps_the_rest_and_retries() {
        let root = std::env::temp_dir().join(format!("orangelink-flush-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        for id in ["a", "b", "c"] {
            fs::create_dir_all(item_dir(&root, id)).unwrap();
            lib.insert_item(item(id));
            lib.touch(id);
        }
        lib.meta_dirty = true;
        // A folder where the temporary file goes makes writing "b" fail.
        let blocker = item_dir(&root, "b").join(META_FILE).with_extension("json.tmp");
        fs::create_dir_all(&blocker).unwrap();

        let err = lib.flush().unwrap_err().to_string();
        assert!(err.contains("「b」"), "{err}");
        assert!(item_dir(&root, "a").join(META_FILE).is_file());
        assert!(item_dir(&root, "c").join(META_FILE).is_file());
        assert!(!item_dir(&root, "b").join(META_FILE).exists());
        assert!(!lib.meta_dirty, "library.json is still written");
        assert!(lib.has_unsaved(), "b waits for the next flush");

        fs::remove_dir(&blocker).unwrap();
        lib.flush().unwrap();
        assert!(item_dir(&root, "b").join(META_FILE).is_file());
        assert!(!lib.has_unsaved());
        let _ = fs::remove_dir_all(&root);
    }

    /// A hand-edited library.json with a loop or a missing parent still opens,
    /// and every folder shows up.
    #[test]
    fn smart_folders_in_a_loop_are_brought_back_to_the_top() {
        let smart = |id: &str, parent: &str| -> crate::model::SmartFolder {
            serde_json::from_value(serde_json::json!({ "id": id, "name": id, "filter": {}, "parent": parent })).unwrap()
        };
        let mut folders = vec![smart("a", "b"), smart("b", "a"), smart("self", "self")];
        assert!(repair_folder_tree(&mut folders));
        let tops = folders.iter().filter(|f| f.parent.is_none()).count();
        assert_eq!(tops, 2, "one of the loop and the one that was its own parent");
    }

    #[test]
    fn folder_loops_and_lost_parents_are_repaired() {
        let folder = |id: &str, parent: Option<&str>| Folder { id: id.into(), name: id.into(), parent: parent.map(Into::into), created_at: 0, color: None, collapsed: false };
        let mut folders = vec![
            folder("a", Some("c")),
            folder("b", Some("a")),
            folder("c", Some("b")),
            folder("self", Some("self")),
            folder("lost", Some("gone")),
            folder("ok", None),
            folder("child", Some("ok")),
        ];
        assert!(repair_folder_tree(&mut folders));
        assert!(!repair_folder_tree(&mut folders), "a tree stays as it is");
        let parent = |id: &str| folders.iter().find(|f| f.id == id).unwrap().parent.clone();
        assert_eq!(parent("a"), None, "the loop is cut once");
        assert_eq!(parent("b").as_deref(), Some("a"));
        assert_eq!(parent("c").as_deref(), Some("b"));
        assert_eq!((parent("self"), parent("lost")), (None, None));
        assert_eq!(parent("child").as_deref(), Some("ok"));

        let root = std::env::temp_dir().join(format!("orangelink-loop-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        lib.meta.folders = vec![folder("x", Some("y")), folder("y", Some("x"))];
        write_json(&root.join(LIB_FILE), &lib.meta).unwrap();
        let lib = Library::open(&root).unwrap();
        assert_eq!(lib.folder_flat().len(), 2);
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    /// Leftover item folders and changed files are found; items, unreadable ones
    /// and their folders are not leftovers.
    #[test]
    fn the_check_finds_leftovers_and_changed_files() {
        let root = std::env::temp_dir().join(format!("orangelink-check-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        for (id, body) in [("same", "one"), ("edited", "two")] {
            let dir = item_dir(&root, id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("f.png"), body).unwrap();
            let mut it = item(id);
            it.hash = crate::imaging::quick_hash(&dir.join("f.png")).unwrap();
            lib.insert_item(it);
            lib.touch(id);
        }
        lib.flush().unwrap();
        fs::write(item_dir(&root, "edited").join("f.png"), "changed").unwrap();
        let leftover = item_dir(&root, "cut-off");
        fs::create_dir_all(&leftover).unwrap();
        fs::write(leftover.join("f.png"), "half").unwrap();
        // Not ours to judge: an unreadable item, and a folder that isn't an item folder.
        fs::create_dir_all(item_dir(&root, "broken")).unwrap();
        lib.unreadable.push(Unreadable { id: "broken".into(), error: String::new() });
        fs::create_dir_all(root.join(IMAGES_DIR).join("other")).unwrap();

        let (cancel, done) = (AtomicBool::new(false), AtomicUsize::new(0));
        let found = check_integrity(lib.integrity_input(), &cancel, &done);
        assert_eq!(found.leftovers, [leftover]);
        assert_eq!(found.changed.len(), 1);
        assert_eq!(found.changed[0].0, "edited");
        assert_eq!(done.load(Ordering::Relaxed), 2);
        assert!(!found.is_clean());

        // Taking the edited file as it is now: the next check finds it unchanged.
        let mut changed = found.changed.clone();
        changed.push(("deleted-since".into(), "x".into()));
        assert_eq!(lib.accept_hashes(changed), 1, "an item gone since is skipped");
        assert!(check_integrity(lib.integrity_input(), &cancel, &done).changed.is_empty());

        lib.items.get_mut("same").unwrap().folders = vec!["gone".into()];
        assert_eq!(lib.missing_folder_refs(), 1);
        assert_eq!(lib.drop_missing_folders(), 1);
        assert_eq!(lib.missing_folder_refs(), 0);
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    /// The day's work is in a backup, not only the state the library was
    /// opened in, so a damaged item imported today, or a damaged library.json with
    /// folders made today, comes back. And a new library's backup has the default
    /// smart folders.
    #[test]
    fn todays_work_is_in_a_backup() {
        let root = std::env::temp_dir().join(format!("orangelink-backup-{}", new_id()));
        Library::create(&root).unwrap();
        let mut lib = Library::open(&root).unwrap();
        assert!(!lib.meta.smart_folders.is_empty());
        assert_eq!(lib.list_backups().len(), 1, "the day's first backup, with the default smart folders");
        assert!(fs::read_to_string(&lib.list_backups()[0].0).unwrap().contains(&lib.meta.smart_folders[0].id));
        let folder = lib.add_folder("精華素材", None);
        fs::create_dir_all(item_dir(&root, "a")).unwrap();
        lib.insert_item(item("a"));
        lib.items.get_mut("a").unwrap().folders = vec![folder.clone()];
        lib.touch("a");
        lib.flush().unwrap();
        lib.backup_if_changed(BACKUP_EVERY);
        assert!(lib.changed_since_backup, "not again within BACKUP_EVERY");
        std::thread::sleep(std::time::Duration::from_millis(1100)); // a new name (seconds)
        lib.backup_if_changed(std::time::Duration::ZERO);
        let backups = lib.list_backups();
        assert_eq!(backups.len(), 2, "the day's first and newest");
        assert_eq!(backups[0].1, 1, "the newest has today's item");
        lib.backup_if_changed(std::time::Duration::ZERO);
        assert_eq!(lib.list_backups().len(), 2, "nothing changed since");
        let seeded = lib.meta.smart_folders.len();
        assert!(seeded > 0);
        drop(lib);

        fs::write(item_dir(&root, "a").join(META_FILE), "{ broken").unwrap();
        let mut lib = Library::open(&root).unwrap();
        assert_eq!(lib.unreadable.len(), 1);
        assert_eq!(lib.restore_unreadable().unwrap(), (1, 0), "today's item is in the backup");
        drop(lib);

        fs::write(root.join(LIB_FILE), "{ broken").unwrap();
        let lib = Library::open(&root).unwrap();
        assert!(lib.recovered.is_some());
        assert!(lib.meta.folders.iter().any(|f| f.id == folder), "the folder made today is back");
        assert_eq!(lib.meta.smart_folders.len(), seeded, "the smart folders are back, not seeded again");
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    /// Each day keeps its first and newest backup for 10 days, and
    /// backups made before a restore are kept apart (the last 5).
    #[test]
    fn backups_rotate_by_day() {
        let root = std::env::temp_dir().join(format!("orangelink-rotate-{}", new_id()));
        let dir = root.join("backups");
        fs::create_dir_all(&dir).unwrap();
        let mut names: Vec<String> = (1..=12).map(|d| format!("backup-202601{d:02}-120000.json")).collect();
        names.extend(["080000", "120000", "180000"].map(|t| format!("backup-20260113-{t}.json")));
        names.extend((0..7).map(|n| format!("backup-20260113-13000{n}-before-restore.json")));
        names.push("backup-test.json".into());
        for n in &names {
            fs::write(dir.join(n), "{}").unwrap();
        }
        rotate_backups(&root);
        let mut left: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        let mut want: Vec<String> = (4..=12).map(|d| format!("backup-202601{d:02}-120000.json")).collect();
        want.extend(["080000", "180000"].map(|t| format!("backup-20260113-{t}.json")));
        want.extend((2..7).map(|n| format!("backup-20260113-13000{n}-before-restore.json")));
        want.push("backup-test.json".into());
        want.sort();
        assert_eq!(left, want);
        let _ = fs::remove_dir_all(&root);
    }

    /// A backup made before a restore today doesn't stand in for the
    /// day's backup.
    #[test]
    fn a_before_restore_backup_is_not_the_days_backup() {
        let root = std::env::temp_dir().join(format!("orangelink-before-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        fs::create_dir_all(item_dir(&root, "a")).unwrap();
        lib.insert_item(item("a"));
        lib.touch("a");
        lib.flush().unwrap();
        let now = chrono::Local::now().format("%Y%m%d-%H%M%S");
        lib.write_backup(&format!("backup-{now}-before-restore.json")).unwrap();
        lib.backup_if_due();
        let daily = lib.list_backups().iter().filter(|(p, _)| backup_kind(p).is_some_and(|(_, before)| !before)).count();
        assert_eq!(daily, 1);
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    /// Restoring a backup from before a folder existed leaves no item pointing at it.
    #[test]
    fn a_restore_drops_folders_that_no_longer_exist() {
        let root = std::env::temp_dir().join(format!("orangelink-restore-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        let old = lib.add_folder("old", None);
        fs::create_dir_all(item_dir(&root, "a")).unwrap();
        lib.insert_item(item("a"));
        lib.items.get_mut("a").unwrap().folders = vec![old.clone()];
        lib.touch("a");
        lib.flush().unwrap();
        lib.write_backup("backup-test.json").unwrap();
        let backup = root.join("backups").join("backup-test.json");
        assert!(!fs::read_to_string(&backup).unwrap().contains('\n'), "backups are compact");

        let new = lib.add_folder("new", None);
        fs::create_dir_all(item_dir(&root, "b")).unwrap();
        lib.insert_item(item("b"));
        lib.items.get_mut("b").unwrap().folders = vec![old.clone(), new.clone()];
        lib.touch("b");
        lib.flush().unwrap();

        lib.restore_backup(&backup).unwrap();
        assert!(lib.meta.folders.iter().all(|f| f.id != new));
        assert_eq!(lib.items["b"].folders, std::slice::from_ref(&old), "the folder made after the backup is gone");
        assert_eq!(lib.items["a"].folders, [old]);
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn after_a_restore_linked_files_that_are_gone_show_as_missing() {
        let root = std::env::temp_dir().join(format!("orangelink-restore-missing-{}", new_id()));
        let mut lib = Library::create(&root.join("lib")).unwrap();
        fs::create_dir_all(item_dir(&lib.root, "a")).unwrap();
        fs::write(root.join("here.png"), "x").unwrap();
        lib.insert_item(item("a"));
        lib.items.get_mut("a").unwrap().source = Some(root.join("old place.png"));
        lib.touch("a");
        lib.flush().unwrap();
        lib.write_backup("backup-20000101-000000.json").unwrap();
        lib.relink("a", root.join("here.png"));
        lib.refresh_missing();
        assert!(lib.missing.is_empty());
        lib.restore_backup(&lib.root.join("backups").join("backup-20000101-000000.json")).unwrap();
        assert!(lib.missing.contains("a"), "the restored path is gone but the item isn't marked missing");
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_relinked_file_is_spelled_as_an_import_would() {
        let root = std::env::temp_dir().join(format!("orangelink-relink-{}", new_id()));
        let mut lib = Library::create(&root.join("lib")).unwrap();
        fs::create_dir_all(root.join("moved").join("sub")).unwrap();
        fs::write(root.join("moved").join("f.png"), "x").unwrap();
        lib.insert_item(item("a"));
        lib.relink("a", root.join("moved").join("sub").join("..").join("f.png"));
        let want = crate::importer::dunce_abs(&root.join("moved").join("f.png")).unwrap();
        assert_eq!(lib.items["a"].source.as_deref(), Some(want.as_path()));
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_item_restored_from_a_backup_leaves_folders_deleted_since() {
        let root = std::env::temp_dir().join(format!("orangelink-restore-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        let gone = lib.add_folder("gone", None);
        fs::create_dir_all(item_dir(&root, "a")).unwrap();
        lib.insert_item(item("a"));
        lib.items.get_mut("a").unwrap().folders = vec![gone.clone()];
        lib.touch("a");
        lib.flush().unwrap();
        lib.write_backup("backup-20000101-000000.json").unwrap();
        lib.delete_folder(&gone);
        lib.flush().unwrap();
        fs::write(item_dir(&root, "a").join(META_FILE), b"{ not json").unwrap();

        let mut lib = Library::open(&root).unwrap();
        assert_eq!(lib.unreadable.len(), 1);
        assert_eq!(lib.restore_unreadable().unwrap(), (1, 0));
        assert!(lib.items["a"].folders.is_empty(), "points at a folder that no longer exists");
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_damaged_copy_of_an_item_does_not_replace_the_first() {
        let root = std::env::temp_dir().join(format!("orangelink-restore-{}", new_id()));
        let mut lib = Library::create(&root).unwrap();
        fs::create_dir_all(item_dir(&root, "a")).unwrap();
        lib.insert_item(item("a"));
        lib.touch("a");
        lib.flush().unwrap();
        lib.write_backup("backup-20000101-000000.json").unwrap();
        let meta = item_dir(&root, "a").join(META_FILE);
        for damage in ["first damage", "second damage"] {
            fs::write(&meta, damage).unwrap();
            let mut lib = Library::open(&root).unwrap();
            assert_eq!(lib.restore_unreadable().unwrap(), (1, 0));
        }
        let dir = item_dir(&root, "a");
        assert_eq!(fs::read_to_string(dir.join("metadata.json.broken")).unwrap(), "first damage");
        assert_eq!(fs::read_to_string(dir.join("metadata.json.broken-1")).unwrap(), "second damage");
        drop(lib);
        let _ = fs::remove_dir_all(&root);
    }
}
