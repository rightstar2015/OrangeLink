//! Thumbnail hashes for finding similar items, kept in one cache file in the
//! library so a search after restarting reads only thumbnails that are new or
//! were rebuilt since. One file, written by the search's background thread,
//! keeps the item metadata (and the UI thread's saving) out of it.
//!
//! Videos also get a hash of the frame at 25%, 50% and 75% (with FFmpeg): two
//! cuts of one stream share a thumbnail-like frame less often than they share
//! most of their picture, and different videos can share a title card.

use crate::library::write_json;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::UNIX_EPOCH,
};

/// In the library root.
pub const CACHE_FILE: &str = "similarity-cache.json";

/// Frames hashed per video, at these fractions of its length.
const FRAME_AT: [f64; 3] = [0.25, 0.5, 0.75];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// The thumbnail's modified time (ns) and size when hashed: a rebuilt
    /// thumbnail differs and is hashed again.
    modified: u64,
    size: u64,
    hash: u64,
    /// A video's frame hashes, and the video file's (modified, size) they were taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frames: Option<[u64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<(u64, u64)>,
}

/// One item to hash: its thumbnail, and for a video the file and its length.
#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    pub thumb: PathBuf,
    pub video: Option<(PathBuf, f64)>,
}

/// An item's fingerprint.
#[derive(Clone, Debug, PartialEq)]
pub struct Print {
    pub id: String,
    pub hash: u64,
    pub frames: Option<[u64; 3]>,
}

/// One search writes the file at a time.
static WRITING: Mutex<()> = Mutex::new(());

fn stamp(path: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(path).ok()?;
    let t = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos() as u64;
    Some((t, m.len()))
}

/// dHash of the frame at `t` seconds, FFmpeg scaling it to 9×8 grey itself.
fn frame_hash(ffmpeg_dir: &Path, file: &Path, t: f64) -> Option<u64> {
    let out = crate::media::tool(ffmpeg_dir, "ffmpeg")
        .args(["-v", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-vf", "scale=9:8:flags=area,format=gray", "-f", "rawvideo", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let px = out.stdout;
    if px.len() < 72 {
        return None;
    }
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h = (h << 1) | (px[y * 9 + x] < px[y * 9 + x + 1]) as u64;
        }
    }
    Some(h)
}

fn video_frames(ffmpeg_dir: &Path, file: &Path, duration: f64) -> Option<[u64; 3]> {
    // NaN or infinity (a damaged length) would ask FFmpeg for frames at "NaN" seconds.
    if !(duration.is_finite() && duration > 0.0) {
        return None;
    }
    let mut out = [0u64; 3];
    for (h, at) in out.iter_mut().zip(FRAME_AT) {
        *h = frame_hash(ffmpeg_dir, file, duration * at)?;
    }
    Some(out)
}

/// The fingerprint of each item, from the cache where the thumbnail (and for
/// videos the file) is unchanged and computed otherwise. Items without a
/// readable thumbnail are left out. Without FFmpeg videos get no frames.
/// The cache then holds exactly these items.
/// Stops early, with nothing, when `cancel` is set; the cache is then left as it was.
pub fn hashes(root: &Path, items: &[Source], ffmpeg_dir: Option<&Path>, cancel: &AtomicBool) -> Vec<Print> {
    use rayon::prelude::*;
    let file = root.join(CACHE_FILE);
    let cached: HashMap<String, Entry> = fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let entries: Vec<(String, Entry, bool)> = items
        .par_iter()
        .filter_map(|s| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let (modified, size) = stamp(&s.thumb)?;
            let old = cached.get(&s.id).copied();
            let mut changed = false;
            let mut e = match old {
                Some(e) if e.modified == modified && e.size == size => e,
                _ => {
                    changed = true;
                    Entry { modified, size, hash: crate::imaging::dhash(&s.thumb)?, frames: None, file: None }
                }
            };
            if let (Some((path, duration)), Some(ff)) = (&s.video, ffmpeg_dir) {
                let now = stamp(path);
                let had = old.filter(|o| o.file.is_some() && o.file == now).and_then(|o| o.frames);
                match had {
                    Some(f) => (e.frames, e.file) = (Some(f), now),
                    None if now.is_some() && old.is_none_or(|o| o.file != now) => {
                        e.frames = video_frames(ff, path, *duration);
                        e.file = now;
                        changed = true;
                    }
                    // Tried on this file before and FFmpeg couldn't read it.
                    None => (e.frames, e.file) = (None, now),
                }
            }
            Some((s.id.clone(), e, changed))
        })
        .collect();
    // A partial list would drop the hashes of every item not reached.
    if cancel.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let changed = entries.iter().any(|e| e.2) || entries.len() != cached.len();
    if changed {
        let map: HashMap<&str, Entry> = entries.iter().map(|(id, e, _)| (id.as_str(), *e)).collect();
        let _guard = crate::lock(&WRITING);
        if let Err(e) = write_json(&file, &map) {
            crate::log::error("similarity", format!("{}：{e:#}", file.display()));
        }
    }
    entries.into_iter().map(|(id, e, _)| Print { id, hash: e.hash, frames: e.frames }).collect()
}

/// Default largest distance for 可能重複 (grouped) and 相似 (to one item).
pub const DUPES: u32 = 5;
pub const NEAR: u32 = 18;

fn dist(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Summed over the frames: a metric, so the BK-tree works; the limit is
/// three times the per-frame one, i.e. the average must be within it.
fn frames_dist(a: &[u64; 3], b: &[u64; 3]) -> u32 {
    a.iter().zip(b).map(|(x, y)| dist(*x, *y)).sum()
}

/// Two items' distance: the average over the frames when both have them,
/// otherwise the thumbnails'.
pub fn distance(a: &Print, b: &Print) -> u32 {
    match (&a.frames, &b.frames) {
        (Some(x), Some(y)) => (frames_dist(x, y) + 1) / 3,
        _ => dist(a.hash, b.hash),
    }
}

/// Items within `limit` of `target`, closest first (the target itself first), at most `max`.
pub fn near(prints: &[Print], target: &str, limit: u32, max: usize) -> Option<Vec<String>> {
    let t = prints.iter().find(|p| p.id == target)?;
    let mut found: Vec<(u32, bool, &String)> = prints.iter().map(|p| (distance(p, t), p.id != target, &p.id)).filter(|(d, ..)| *d <= limit).collect();
    found.sort();
    Some(found.into_iter().take(max).map(|(.., id)| id.clone()).collect())
}

/// Groups of items within `limit` of each other's first item, two or more each.
/// Videos with frames are grouped among themselves by their frames, the rest
/// by thumbnail. BK-trees find each item's neighbours without comparing every pair.
pub fn groups(prints: &[Print], limit: u32) -> Vec<Vec<String>> {
    let (videos, rest): (Vec<&Print>, Vec<&Print>) = prints.iter().partition(|p| p.frames.is_some());
    let mut out = group_by(&rest, |p| p.hash, |a, b| dist(*a, *b), limit);
    out.extend(group_by(&videos, |p| p.frames.unwrap_or_default(), frames_dist, limit * 3));
    out
}

fn group_by<K: Copy>(prints: &[&Print], key: impl Fn(&Print) -> K, metric: fn(&K, &K) -> u32, limit: u32) -> Vec<Vec<String>> {
    let tree = BkTree::new(prints.iter().map(|p| key(p)), metric);
    let mut used = vec![false; prints.len()];
    let mut out = Vec::new();
    for i in 0..prints.len() {
        if used[i] {
            continue;
        }
        let mut members: Vec<usize> = tree.within(&key(prints[i]), limit).into_iter().filter(|&j| !used[j]).collect();
        members.sort_unstable();
        if members.len() > 1 {
            for &j in &members {
                used[j] = true;
            }
            out.push(members.into_iter().map(|j| prints[j].id.clone()).collect());
        }
    }
    out
}

/// A tree over values by a metric, by index.
struct BkTree<K> {
    nodes: Vec<(K, Vec<(u32, usize)>)>,
    metric: fn(&K, &K) -> u32,
}

impl<K> BkTree<K> {
    fn new(values: impl Iterator<Item = K>, metric: fn(&K, &K) -> u32) -> Self {
        let mut t = BkTree { nodes: Vec::new(), metric };
        for v in values {
            let n = t.nodes.len();
            if n == 0 {
                t.nodes.push((v, Vec::new()));
                continue;
            }
            let mut at = 0;
            loop {
                let d = (t.metric)(&t.nodes[at].0, &v);
                match t.nodes[at].1.iter().find(|(e, _)| *e == d) {
                    Some(&(_, child)) => at = child,
                    None => {
                        t.nodes[at].1.push((d, n));
                        break;
                    }
                }
            }
            t.nodes.push((v, Vec::new()));
        }
        t
    }

    fn within(&self, v: &K, limit: u32) -> Vec<usize> {
        let mut out = Vec::new();
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![0];
        while let Some(at) = stack.pop() {
            let (h, children) = &self.nodes[at];
            let d = (self.metric)(h, v);
            if d <= limit {
                out.push(at);
            }
            for &(e, child) in children {
                if e + limit >= d && e <= d + limit {
                    stack.push(child);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(path: &Path, flip: bool) {
        let img = image::RgbImage::from_fn(64, 64, |x, _| {
            let v = if flip { 255 - x * 4 } else { x * 4 } as u8;
            image::Rgb([v, v, v])
        });
        img.save(path).unwrap();
    }

    fn print(id: &str, hash: u64) -> Print {
        Print { id: id.into(), hash, frames: None }
    }

    #[test]
    fn groups_match_comparing_every_pair() {
        // Pseudo-random hashes plus near copies of some.
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut prints = Vec::new();
        for i in 0..400 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            prints.push(print(&format!("{i}"), x));
            if i % 7 == 0 {
                prints.push(print(&format!("{i}b"), x ^ 0b1011));
            }
        }
        for limit in [3, 5, 12] {
            let brute: usize = (0..prints.len()).map(|i| (0..prints.len()).filter(|&j| dist(prints[i].hash, prints[j].hash) <= limit).count()).sum();
            let tree = BkTree::new(prints.iter().map(|p| p.hash), |a, b| dist(*a, *b));
            let fast: usize = prints.iter().map(|p| tree.within(&p.hash, limit).len()).sum();
            assert_eq!(brute, fast, "limit {limit}");
        }
        let g = groups(&prints, DUPES);
        assert!(g.iter().any(|g| g == &vec!["0".to_string(), "0b".to_string()]));
        assert!(g.iter().all(|g| g.len() > 1));
        assert_eq!(near(&prints, "7", NEAR, 300).unwrap()[0], "7", "itself first");
    }

    #[test]
    fn videos_are_compared_by_their_frames() {
        let v = |id: &str, hash: u64, frames: [u64; 3]| Print { id: id.into(), hash, frames: Some(frames) };
        // a and b: the same stream cut differently, different thumbnails.
        // c: another video with a's thumbnail (a title card).
        let prints = vec![
            v("a", 0, [0xFF00, 0xF0F0, 0x0FF0]),
            v("b", u64::MAX, [0xFF01, 0xF0F0, 0x0FF3]),
            v("c", 0, [!0xFF00, !0xF0F0, !0x0FF0]),
            print("img", 0),
        ];
        assert_eq!(distance(&prints[0], &prints[1]), 1, "average of 1, 0, 2");
        assert!(distance(&prints[0], &prints[2]) > 40);
        assert_eq!(distance(&prints[0], &prints[3]), 0, "an image against a video: the thumbnails");
        let g = groups(&prints, DUPES);
        assert_eq!(g, vec![vec!["a".to_string(), "b".to_string()]], "c is not grouped with a; images only with images");
        assert_eq!(near(&prints, "a", DUPES, 10).unwrap(), ["a", "img", "b"], "closest first, the target before ties");
    }

    #[test]
    fn reuses_hashes_until_the_thumbnail_changes() {
        let root = std::env::temp_dir().join(format!("orangelink-sim-{}", crate::model::new_id()));
        fs::create_dir_all(&root).unwrap();
        let (a, b) = (root.join("a.png"), root.join("b.png"));
        picture(&a, false);
        picture(&b, true);
        let src = |id: &str, thumb: &Path| Source { id: id.into(), thumb: thumb.to_path_buf(), video: None };
        let items = vec![src("a", &a), src("b", &b), src("gone", &root.join("x.png"))];

        let first = hashes(&root, &items, None, &AtomicBool::new(false));
        assert_eq!(first.len(), 2, "no thumbnail, no hash");
        let ha = first.iter().find(|e| e.id == "a").unwrap().hash;
        assert_eq!(Some(ha), crate::imaging::dhash(&a));

        // A cached hash is used without reading the thumbnail.
        let file = root.join(CACHE_FILE);
        let mut map: HashMap<String, Entry> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        map.get_mut("a").unwrap().hash = 42;
        fs::write(&file, serde_json::to_vec(&map).unwrap()).unwrap();
        let again = hashes(&root, &items, None, &AtomicBool::new(false));
        assert_eq!(again.iter().find(|e| e.id == "a").unwrap().hash, 42);

        // A rebuilt thumbnail is hashed again.
        std::thread::sleep(std::time::Duration::from_millis(20));
        picture(&a, true);
        let rebuilt = hashes(&root, &items, None, &AtomicBool::new(false));
        assert_eq!(rebuilt.iter().find(|e| e.id == "a").unwrap().hash, crate::imaging::dhash(&a).unwrap());

        // A cancelled search leaves the cache as it was.
        let before = fs::read(&file).unwrap();
        assert!(hashes(&root, &items[1..], None, &AtomicBool::new(true)).is_empty());
        assert_eq!(fs::read(&file).unwrap(), before);

        // Items no longer asked about leave the cache.
        hashes(&root, &items[1..], None, &AtomicBool::new(false));
        let map: HashMap<String, Entry> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(map.keys().collect::<Vec<_>>(), vec!["b"]);
        let _ = fs::remove_dir_all(&root);
    }

    /// Needs FFmpeg: a video re-encoded smaller matches its original by its
    /// frames, a different one doesn't; frames are cached until the file changes.
    #[test]
    fn video_frames_with_ffmpeg() {
        let Some(ff) = crate::media::ffmpeg_for_test("video_frames_with_ffmpeg") else { return };
        let root = std::env::temp_dir().join(format!("orangelink-simv-{}", crate::model::new_id()));
        fs::create_dir_all(&root).unwrap();
        let make = |name: &str, src: &str, size: &str| {
            let out = root.join(name);
            let ok = crate::media::tool(&ff, "ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("{src}=size={size}:rate=25"), "-t", "4", "-pix_fmt", "yuv420p"])
                .arg(&out)
                .status()
                .unwrap()
                .success();
            assert!(ok);
            out
        };
        let a = make("a.mp4", "testsrc2", "640x360");
        let b = make("b.mp4", "testsrc2", "320x180");
        let c = make("c.mp4", "mandelbrot", "640x360");
        let thumb = root.join("t.png");
        picture(&thumb, false);
        let src = |id: &str, f: &Path| Source { id: id.into(), thumb: thumb.clone(), video: Some((f.to_path_buf(), 4.0)) };
        let items = vec![src("a", &a), src("b", &b), src("c", &c)];
        let p = hashes(&root, &items, Some(&ff), &AtomicBool::new(false));
        assert!(p.iter().all(|p| p.frames.is_some()), "{p:?}");
        let d = |x: usize, y: usize| distance(&p[x], &p[y]);
        assert!(d(0, 1) <= DUPES, "same picture, other size: {}", d(0, 1));
        assert!(d(0, 2) > DUPES, "different picture: {}", d(0, 2));

        // Cached: tampered frames come back as they are.
        let file = root.join(CACHE_FILE);
        let mut map: HashMap<String, Entry> = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        map.get_mut("a").unwrap().frames = Some([1, 2, 3]);
        fs::write(&file, serde_json::to_vec(&map).unwrap()).unwrap();
        let again = hashes(&root, &items, Some(&ff), &AtomicBool::new(false));
        assert_eq!(again[0].frames, Some([1, 2, 3]));
        let _ = fs::remove_dir_all(&root);
    }
}
