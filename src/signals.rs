//! Where a stream got exciting: chat activity and loudness per second, and the peaks
//! that make good highlight candidates.

use crate::ytdlp::ChatMessage;
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Stdio,
};

/// Upper bound on curve length (48 hours) so corrupt timestamps cannot cause OOM.
const MAX_SECS: usize = 48 * 3600;

/// Chat weight per second of video. A message counts 1, plus 1 for each keyword in it;
/// Super Chats, stickers and memberships count 3.
pub fn chat_activity(messages: &[ChatMessage], keywords: &[String], duration: f64) -> Vec<f32> {
    let secs = (duration.max(0.0).ceil() as usize).min(MAX_SECS);
    let mut out = vec![0.0; secs.max(1)];
    let keywords: Vec<String> = keywords.iter().map(|k| k.trim().to_lowercase()).filter(|k| !k.is_empty()).collect();
    for m in messages {
        let Some(ms) = m.offset_ms.filter(|ms| *ms >= 0) else { continue };
        let i = (ms / 1000) as usize;
        if i >= MAX_SECS {
            continue;
        }
        if i >= out.len() {
            if duration > 0.0 {
                continue; // after the end of this (maybe trimmed) video
            }
            out.resize(i + 1, 0.0);
        }
        let text = m.text.to_lowercase();
        let hits = keywords.iter().filter(|k| text.contains(k.as_str())).count();
        out[i] += if m.special { 3.0 } else { 1.0 + hits as f32 };
    }
    out
}

/// Which keywords chat used when: `(second, index into keywords)` per hit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hits {
    pub at: Vec<(u32, u16)>,
    pub keywords: Vec<String>,
}

/// Keyword hits per second, for telling why a moment was picked.
pub fn keyword_hits(messages: &[ChatMessage], keywords: &[String], duration: f64) -> Hits {
    let words: Vec<String> = keywords.iter().map(|k| k.trim().to_string()).filter(|k| !k.is_empty()).collect();
    let lower: Vec<String> = words.iter().map(|k| k.to_lowercase()).collect();
    let mut at = Vec::new();
    for m in messages {
        let Some(ms) = m.offset_ms.filter(|ms| *ms >= 0) else { continue };
        let s = ms / 1000;
        if s as usize >= MAX_SECS || (duration > 0.0 && s as f64 >= duration.ceil()) {
            continue;
        }
        let text = m.text.to_lowercase();
        for (k, w) in lower.iter().enumerate() {
            if text.contains(w.as_str()) {
                at.push((s as u32, k as u16));
            }
        }
    }
    Hits { at, keywords: words }
}

/// The chat record's weight curve and keyword hits; None when it has no usable times.
pub fn read_chat(file: &Path, keywords: &[String], duration: f64) -> Option<(Vec<f32>, Hits)> {
    let text = std::fs::read_to_string(file).ok()?;
    let msgs = crate::ytdlp::chat_messages(&text);
    let curve = chat_activity(&msgs, keywords, duration);
    curve.iter().any(|v| *v > 0.0).then(|| (curve, keyword_hits(&msgs, keywords, duration)))
}

/// `<video>.live_chat.json` next to the original file, the name yt-dlp gives it.
pub fn chat_file_for(original: &Path) -> Option<PathBuf> {
    let stem = original.file_stem()?.to_string_lossy();
    let dir = original.parent()?;
    [".live_chat.json", ".live_chat.json.part"].iter().map(|ext| dir.join(format!("{stem}{ext}"))).find(|p| p.is_file())
}

/// Loudest momentary loudness (LUFS) of each second, from FFmpeg's `ebur128` filter. `progress`
/// gets the seconds analysed so far; returning false stops it.
pub fn loudness(ffmpeg_dir: &Path, input: &Path, mut progress: impl FnMut(f64) -> bool) -> Result<Vec<f32>, String> {
    let mut cmd = crate::process::hidden(ffmpeg_dir.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }));
    let mut child = cmd
        .args(["-hide_banner", "-nostats", "-nostdin", "-i"])
        .arg(input)
        .args(["-vn", "-af", "ebur128=metadata=1,ametadata=print:key=lavfi.r128.M", "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            crate::log::command_failed("loudness", &cmd, format!("無法啟動：{e}"));
            t!("無法啟動 FFmpeg：{e}", e)
        })?;
    let stderr = child.stderr.take().ok_or(t!("無法讀取 FFmpeg 輸出"))?;
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    let mut out: Vec<f32> = Vec::new();
    let mut time = None;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        // Each frame prints `pts_time:1.2`, then `lavfi.r128.M=-24.1`.
        if let Some(t) = line.split("pts_time:").nth(1).and_then(|v| v.trim().parse::<f64>().ok()) {
            time = Some(t);
            continue;
        }
        let (Some(t), Some(m)) = (time, line.split("lavfi.r128.M=").nth(1).and_then(|v| v.trim().parse::<f32>().ok())) else { continue };
        let i = t.max(0.0) as usize;
        if i >= MAX_SECS {
            continue;
        }
        if i >= out.len() {
            out.resize(i + 1, f32::NEG_INFINITY);
            if !progress(t) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("已取消".into());
            }
        }
        out[i] = out[i].max(m);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() || out.is_empty() {
        crate::log::command_failed("loudness", &cmd, format!("結束代碼 {:?}，取得 {} 秒", status.code(), out.len()));
        return Err(t!("無法分析這個檔案的聲音").into());
    }
    // Silence reads as -inf or -120; keep the curve in a drawable range.
    Ok(out.into_iter().map(|v| (v.max(-70.0) * 10.0).round() / 10.0).collect())
}

/// How unusual each second is, after a moving average over `window` seconds so a single
/// spike doesn't count as a moment. Measured from the median in units of the median
/// absolute deviation, so a burst like the backlog at 0:00 doesn't flatten the rest.
fn standardized(values: &[f32], window: usize) -> Vec<f32> {
    let n = values.len();
    if n == 0 {
        return Vec::new();
    }
    let half = window / 2;
    let mut prefix = vec![0.0f64; n + 1];
    for (i, v) in values.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *v as f64;
    }
    let smooth: Vec<f64> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            (prefix[b] - prefix[a]) / (b - a) as f64
        })
        .collect();
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let mid = median(&mut smooth.clone());
    let mad = median(&mut smooth.iter().map(|v| (v - mid).abs()).collect()) * 1.4826;
    // Mostly-flat input has no spread around the median; fall back to the plain one.
    let sd = (smooth.iter().map(|v| (v - mid).powi(2)).sum::<f64>() / n as f64).sqrt();
    let scale = if mad > 1e-9 { mad } else { sd };
    smooth.iter().map(|v| if scale > 1e-9 { ((v - mid) / scale) as f32 } else { 0.0 }).collect()
}

/// Chat reacts this many seconds after what it reacts to.
const CHAT_DELAY: usize = 4;

/// A moment that stands out, and how much (chat and loudness z-scores added).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peak {
    pub at: usize,
    pub score: f32,
}

/// Seconds where chat and loudness together stand out the most, at least `gap` apart,
/// strongest first, at most `max`. Chat reacts a few seconds late, so it is moved back.
pub fn peaks(chat: Option<&[f32]>, loud: Option<&[f32]>, gap: usize, max: usize) -> Vec<Peak> {
    let len = chat.map_or(0, |c| c.len()).max(loud.map_or(0, |l| l.len()));
    let mut score = vec![0.0f32; len];
    if let Some(c) = chat {
        for (i, z) in standardized(c, 10).into_iter().enumerate() {
            if let Some(t) = i.checked_sub(CHAT_DELAY) {
                score[t] += z;
            }
        }
    }
    if let Some(l) = loud {
        for (i, z) in standardized(l, 3).into_iter().enumerate() {
            score[i] += z;
        }
    }
    let threshold = if chat.is_some() && loud.is_some() { 2.0 } else { 1.0 };
    let mut order: Vec<usize> = (0..len).filter(|&i| score[i] >= threshold).collect();
    order.sort_by(|a, b| score[*b].total_cmp(&score[*a]));
    let mut picked: Vec<Peak> = Vec::new();
    for i in order {
        if picked.len() >= max {
            break;
        }
        if picked.iter().all(|p| p.at.abs_diff(i) >= gap) {
            picked.push(Peak { at: i, score: score[i] });
        }
    }
    picked
}

/// A candidate clip: one peak, or several close ones merged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    /// The strongest peak's score.
    pub score: f32,
    pub peaks: usize,
}

/// Merged candidates are kept under this length.
pub const MAX_MERGED: f64 = 180.0;

/// `before` and `after` seconds around each peak, in time order, a segment
/// joined with the next when less than `merge` seconds lie between them.
pub fn segments(peaks: &[Peak], before: f64, after: f64, merge: f64, end: f64) -> Vec<Segment> {
    let mut sorted = peaks.to_vec();
    sorted.sort_by_key(|p| p.at);
    let mut out: Vec<Segment> = Vec::new();
    for p in sorted {
        let (start, stop) = ((p.at as f64 - before).max(0.0), (p.at as f64 + after).min(end));
        if let Some(last) = out.last_mut().filter(|l| start - l.end < merge && stop - l.start <= MAX_MERGED) {
            last.end = last.end.max(stop);
            last.score = last.score.max(p.score);
            last.peaks += 1;
            continue;
        }
        out.push(Segment { start, end: stop, score: p.score, peaks: 1 });
    }
    out.retain(|s| s.end - s.start > 1.0);
    out
}

/// Why `start..end` was picked, e.g. `聊天 ×3.2、音量 +8 dB、關鍵字：草 ×14`.
pub fn describe(chat: Option<&[f32]>, hits: Option<&Hits>, loud: Option<&[f32]>, start: f64, end: f64) -> Option<String> {
    let range = |len: usize, delay: usize| {
        // Saturating: a damaged clip can end at 1e30 s, which casts to usize::MAX.
        let a = (start.max(0.0) as usize).saturating_add(delay).min(len);
        let b = (end.max(0.0).ceil() as usize).saturating_add(delay).min(len);
        a..b.max(a)
    };
    let mut parts = Vec::new();
    if let Some(c) = chat.filter(|c| !c.is_empty()) {
        let all = c.iter().sum::<f32>() / c.len() as f32;
        let r = range(c.len(), CHAT_DELAY);
        if all > 0.0 && !r.is_empty() {
            let here = c[r.clone()].iter().sum::<f32>() / r.len() as f32;
            let ratio = here / all;
            if ratio >= 1.1 {
                parts.push(t!("聊天 ×{ratio}", ratio = format!("{:.1}", ratio)));
            }
        }
    }
    if let Some(l) = loud.filter(|l| !l.is_empty()) {
        let r = range(l.len(), 0);
        if !r.is_empty() {
            let mut sorted: Vec<f32> = l.iter().copied().filter(|v| *v > -70.0).collect();
            sorted.sort_by(f32::total_cmp);
            let max = l[r].iter().copied().fold(f32::MIN, f32::max);
            if let Some(&median) = sorted.get(sorted.len() / 2) {
                let up = (max - median).round();
                if up >= 1.0 {
                    parts.push(t!("音量 +{up} dB", up));
                }
            }
        }
    }
    if let Some(h) = hits {
        let (a, b) = ((start.max(0.0) as u32).saturating_add(CHAT_DELAY as u32), (end.max(0.0).ceil() as u32).saturating_add(CHAT_DELAY as u32));
        let mut counts = vec![0usize; h.keywords.len()];
        for &(s, k) in &h.at {
            if (a..b).contains(&s) {
                if let Some(c) = counts.get_mut(k as usize) {
                    *c += 1;
                }
            }
        }
        let mut top: Vec<(usize, &String)> = counts.into_iter().zip(&h.keywords).filter(|(n, _)| *n > 0).collect();
        top.sort_by_key(|t| std::cmp::Reverse(t.0));
        if !top.is_empty() {
            let words: Vec<String> = top.iter().take(3).map(|(n, w)| format!("{w} ×{n}")).collect();
            parts.push(t!("關鍵字：{words}", words = words.join(" ")));
        }
    }
    (!parts.is_empty()).then(|| parts.join(t!("、")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clip_with_a_huge_end_is_described_without_overflowing() {
        let hits = Hits { at: vec![(5, 0)], keywords: vec!["草".into()] };
        let chat = [1.0f32; 10];
        let loud = [-20.0f32; 10];
        for (start, end) in [(0.0, 1e30), (1e30, 1e30), (f64::MAX, f64::INFINITY)] {
            let _ = describe(Some(&chat), Some(&hits), Some(&loud), start, end);
        }
    }

    fn msg(ms: i64, text: &str, special: bool) -> ChatMessage {
        ChatMessage { offset_ms: Some(ms), time: String::new(), author: "a".into(), text: text.into(), paid: None, special }
    }

    #[test]
    fn chat_is_weighted_per_second() {
        let msgs = [msg(500, "晚安", false), msg(1200, "草 www", false), msg(1900, "加油", true), msg(99_000, "late", false)];
        let keywords = vec!["草".to_string(), "WWW".to_string(), " ".to_string()];
        let a = chat_activity(&msgs, &keywords, 3.0);
        assert_eq!(a, vec![1.0, 3.0 + 3.0, 0.0], "late messages past the end are dropped");
        assert_eq!(chat_activity(&msgs, &keywords, 0.0).len(), 100, "unknown length grows to fit");
    }

    #[test]
    fn peaks_are_spread_out_and_strongest_first() {
        let mut chat = vec![1.0f32; 600];
        chat[100..110].fill(30.0);
        chat[104..106].fill(50.0);
        chat[400..405].fill(15.0);
        let p = peaks(Some(&chat), None, 30, 10);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!((96..110).contains(&p[0].at), "strongest first, moved back for chat delay: {p:?}");
        assert!((390..405).contains(&p[1].at), "{p:?}");
        assert!(p[0].score > p[1].score);
        assert!(peaks(Some(&vec![2.0; 100]), None, 30, 10).is_empty(), "flat chat has no peaks");
        let mut loud = vec![-30.0f32; 300];
        loud[200] = -5.0;
        loud[201] = -5.0;
        assert_eq!(peaks(None, Some(&loud), 30, 10).len(), 1);
    }

    #[test]
    fn close_peaks_merge_into_one_segment() {
        let p = |at, score| Peak { at, score };
        let peaks = [p(100, 3.0), p(135, 5.0), p(400, 2.0), p(5, 1.0)];
        // 20 s before, 10 after: 80..110 and 115..145 are 5 s apart.
        let s = segments(&peaks, 20.0, 10.0, 10.0, 1000.0);
        assert_eq!(s.len(), 3, "{s:?}");
        assert_eq!((s[0].start, s[0].end), (0.0, 15.0), "clamped at the start");
        assert_eq!((s[1].start, s[1].end, s[1].score, s[1].peaks), (80.0, 145.0, 5.0, 2));
        assert_eq!(segments(&peaks, 20.0, 10.0, 0.0, 1000.0).len(), 4, "0 merges nothing");
        assert_eq!(segments(&peaks, 20.0, 10.0, 10.0, 410.0)[2].end, 410.0, "clamped at the end");
        // Never past MAX_MERGED in one segment.
        let many: Vec<Peak> = (0..20).map(|i| p(100 + i * 31, 1.0)).collect();
        assert!(segments(&many, 20.0, 10.0, 10.0, 9999.0).iter().all(|s| s.end - s.start <= MAX_MERGED));
    }

    #[test]
    fn why_a_moment_was_picked() {
        let mut chat = vec![1.0f32; 100];
        for v in &mut chat[54..64] {
            *v = 5.0;
        }
        let mut loud = vec![-30.0f32; 100];
        loud[55] = -22.0;
        let hits = Hits { at: vec![(56, 0), (57, 0), (58, 1), (90, 1)], keywords: vec!["草".into(), "www".into()] };
        let note = describe(Some(&chat), Some(&hits), Some(&loud), 50.0, 60.0).unwrap();
        assert!(note.starts_with("聊天 ×3."), "{note}");
        assert!(note.contains("音量 +8 dB"), "{note}");
        assert!(note.ends_with("關鍵字：草 ×2 www ×1"), "{note}");
        assert_eq!(describe(Some(&vec![1.0; 100]), None, None, 10.0, 20.0), None, "nothing stands out");
        let msgs = [msg(1500, "草草", false), msg(2500, "WWW 草", false)];
        let h = keyword_hits(&msgs, &["草".into(), "www".into(), " ".into()], 0.0);
        assert_eq!(h.at, vec![(1, 0), (2, 0), (2, 1)]);
        assert_eq!(h.keywords.len(), 2);
    }

    /// Needs FFmpeg; analyses a generated tone that gets louder halfway.
    #[test]
    fn loudness_of_a_generated_clip() {
        let Some(ff) = crate::media::ffmpeg_for_test("loudness_of_a_generated_clip") else { return };
        let dir = std::env::temp_dir().join(format!("orangelink-loud-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("t.wav");
        let ok = crate::process::hidden(ff.join("ffmpeg"))
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=6", "-af", "volume='if(lt(t,3),0.05,0.8)':eval=frame"])
            .arg(&wav)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let curve = loudness(&ff, &wav, |_| true).unwrap();
        assert!(curve.len() >= 5, "{curve:?}");
        assert!(curve[5] > curve[1] + 10.0, "{curve:?}");
        assert_eq!(loudness(&ff, &wav, |_| false), Err("已取消".to_string()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chat_peaks_do_not_pile_up_at_zero_and_corrupt_offsets_are_capped() {
        // Slightly elevated start (0..5s) plus a real highlight at 60..70s: 0:00 must not beat 60s.
        let mut chat = vec![1.0f32; 120];
        for v in &mut chat[0..5] {
            *v = 2.5;
        }
        for v in &mut chat[60..70] {
            *v = 6.0;
        }
        let found = peaks(Some(&chat), None, 20, 3);
        assert!(!found.is_empty());
        assert!(found[0].at >= 50 && found[0].at <= 70, "real peak should win over 0:00: {found:?}");

        // Huge corrupt offset_ms must not allocate gigabytes.
        let huge = [msg(i64::MAX / 2, "corrupt", false), msg(2_000, "ok", false)];
        let curve = chat_activity(&huge, &[], 0.0);
        assert_eq!(curve.len(), 3);
    }
}

/// Prints the highlight peaks found in a real chat record (env OL_CHAT).
#[cfg(test)]
mod real_chat {
    #[test]
    #[ignore]
    fn peaks_of_a_real_record() {
        let text = std::fs::read_to_string(std::env::var("OL_CHAT").unwrap()).unwrap();
        let msgs = crate::ytdlp::chat_messages(&text);
        let keywords = crate::config::Config::default().chat_keywords;
        let chat = super::chat_activity(&msgs, &keywords, 0.0);
        println!("{} messages over {} s", msgs.len(), chat.len());
        let with_time = msgs.iter().filter(|m| m.offset_ms.is_some()).count();
        let busiest = chat.iter().copied().fold(0.0, f32::max);
        let nonzero = chat.iter().filter(|v| **v > 0.0).count();
        println!("with time {with_time}, busiest second {busiest}, seconds with chat {nonzero}");
        let z = super::standardized(&chat, 10);
        println!("max z {:?}", z.iter().copied().fold(f32::MIN, f32::max));
        println!("first offsets {:?}", msgs.iter().take(5).map(|m| m.offset_ms).collect::<Vec<_>>());
        for t in super::peaks(Some(&chat), None, 30, 10).into_iter().map(|p| p.at) {
            let around: f32 = chat[t.saturating_sub(5)..(t + 15).min(chat.len())].iter().sum();
            println!("peak at {}:{:02}  (weight {around} in the 20 s after)", t / 60, t % 60);
        }
    }
}
