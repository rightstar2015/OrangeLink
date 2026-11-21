//! Search words and filters, matched against items for the grid and smart folders.

use super::*;

/// A `SavedFilter` prepared for fast matching.
/// A lowercased search word and how to compare it without lowercasing every
/// haystack (subtitle excerpts can be tens of KB per item).
struct Term {
    text: String,
    kind: TermKind,
}

enum TermKind {
    /// No cased letters (CJK, digits…): lowercasing the haystack changes nothing it could match.
    Uncased,
    /// ASCII: compare bytes ignoring ASCII case.
    Ascii,
    Other,
}

impl Term {
    fn new(word: &str) -> Self {
        let text = word.to_lowercase();
        let kind = if !text.chars().any(|c| c.is_lowercase() || c.is_uppercase()) {
            TermKind::Uncased
        } else if text.is_ascii() {
            TermKind::Ascii
        } else {
            TermKind::Other
        };
        Term { text, kind }
    }

    /// Case-insensitive `hay.contains(term)`.
    fn found_in(&self, hay: &str) -> bool {
        match self.kind {
            TermKind::Uncased => hay.contains(&self.text),
            TermKind::Ascii => {
                let t = self.text.as_bytes();
                t.len() <= hay.len() && hay.as_bytes().windows(t.len()).any(|w| w.eq_ignore_ascii_case(t))
            }
            TermKind::Other => hay.to_lowercase().contains(&self.text),
        }
    }
}

pub(super) fn in_view(view: &View, everywhere: bool, it: &Item) -> bool {
    match view {
        View::Trash => it.deleted,
        _ if it.deleted => false,
        _ if everywhere => true,
        View::All | View::Smart(_) | View::Ids { .. } => true,
        View::Uncategorized => it.folders.is_empty(),
        View::Untagged => it.tags.is_empty(),
        View::Folder(f) => it.folders.contains(f),
        View::Kind(k) => it.kind == *k,
    }
}

/// One condition of a search.
enum Atom {
    /// A word or "quoted phrase" anywhere: name, tags, notes, URL, extension,
    /// subtitles, fields (`欄位:值` too) and clip names.
    Text(Term),
    /// `#標籤`: a tag equal to this (ignoring case).
    Tag(String),
    /// `ext:mp4`
    Ext(String),
    /// `type:影片` / `type:video`
    Kind(MediaKind),
    /// `rating>=3`, `★3`, `dur>10m`, `w>=1920`, `h<1080`
    Number(NumKey, Cmp, f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum NumKey {
    Rating,
    /// Seconds.
    Duration,
    Width,
    Height,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Cmp {
    Eq,
    Gt,
    Ge,
    Lt,
    Le,
}

impl Cmp {
    fn test(self, a: f64, b: f64) -> bool {
        match self {
            Cmp::Eq => (a - b).abs() < 1e-9,
            Cmp::Gt => a > b,
            Cmp::Ge => a >= b,
            Cmp::Lt => a < b,
            Cmp::Le => a <= b,
        }
    }
}

impl Atom {
    fn matches(&self, it: &Item) -> bool {
        match self {
            Atom::Text(term) => {
                field_term_matches(it, &term.text)
                    || term.found_in(&it.name)
                    || it.tags.iter().any(|t| term.found_in(t))
                    || term.found_in(&it.annotation)
                    || term.found_in(&it.url)
                    || it.ext == term.text
                    || it.text.as_ref().is_some_and(|t| term.found_in(t))
                    || it.fields.values().any(|v| term.found_in(v))
                    || it.clips.iter().any(|c| term.found_in(&c.name))
            }
            Atom::Tag(t) => it.tags.iter().any(|x| x.to_lowercase() == *t),
            Atom::Ext(e) => it.ext == *e,
            Atom::Kind(k) => it.kind == *k,
            Atom::Number(key, cmp, v) => {
                let value = match key {
                    NumKey::Rating => Some(it.rating as f64),
                    NumKey::Duration => it.duration,
                    NumKey::Width => (it.width > 0).then_some(it.width as f64),
                    NumKey::Height => (it.height > 0).then_some(it.height as f64),
                };
                value.is_some_and(|x| cmp.test(x, *v))
            }
        }
    }
}

/// `-詞` excludes; several atoms joined by `OR` need any one.
struct Clause {
    any: Vec<(bool, Atom)>,
}

/// A search word: the text, whether it was "quoted", and whether a `-` outside
/// any quotes excludes it.
struct Token {
    word: String,
    quoted: bool,
    neg: bool,
}

/// Splits a search into words, keeping "quoted phrases" whole (quotes dropped).
/// A `-` right before a word or a quote excludes it; one inside the quotes is
/// part of the phrase (`"-14 dB"`).
fn tokens(search: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = search.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        let neg = c == '-';
        if neg {
            chars.next();
        }
        if matches!(chars.peek(), Some('"' | '“' | '「')) {
            let close = match chars.next() {
                Some('“') => '”',
                Some('「') => '」',
                _ => '"',
            };
            for c in chars.by_ref() {
                if c == close {
                    break;
                }
                word.push(c);
            }
            out.push(Token { word, quoted: true, neg });
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                word.push(c);
                chars.next();
            }
            // A lone `-` is a word, not an exclusion of nothing.
            if neg && word.is_empty() {
                out.push(Token { word: "-".into(), quoted: false, neg: false });
            } else {
                out.push(Token { word, quoted: false, neg });
            }
        }
    }
    out
}

/// `10m`, `1h30m`, `90s`, `1:30`, `45` (seconds).
fn parse_duration(s: &str) -> Option<f64> {
    if s.contains(':') {
        return s.split(':').try_fold(0.0, |acc, p| p.trim().parse::<f64>().ok().map(|v| acc * 60.0 + v));
    }
    let (mut total, mut num, mut any) = (0.0, String::new(), false);
    for c in s.chars() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let unit = match c.to_ascii_lowercase() {
            'h' | '時' => 3600.0,
            'm' | '分' => 60.0,
            's' | '秒' => 1.0,
            _ => return None,
        };
        total += num.parse::<f64>().ok()? * unit;
        num.clear();
        any = true;
    }
    // A bare number, or one left after the units, is seconds.
    if !num.is_empty() {
        total += num.parse::<f64>().ok()?;
    } else if !any {
        return None;
    }
    Some(total)
}

/// A kind by its name in any interface language, so `type:` works whichever
/// language the search help is shown in.
fn kind_named(s: &str) -> Option<MediaKind> {
    match s {
        "圖片" | "image" | "img" | "圖" | "图片" | "图" | "画像" | "이미지" | "사진" => Some(MediaKind::Image),
        "影片" | "video" | "視頻" | "视频" | "動画" | "동영상" | "비디오" => Some(MediaKind::Video),
        "音訊" | "audio" | "音樂" | "聲音" | "音频" | "音乐" | "声音" | "音声" | "오디오" | "음악" => Some(MediaKind::Audio),
        "其他" | "other" | "その他" | "기타" => Some(MediaKind::Other),
        _ => None,
    }
}

/// What one unquoted word of a search is.
enum Special {
    Atom(Atom),
    /// A special form without a usable value: `ext:`, `type:`, `w>=`,
    /// `dur>abc`, a lone `#` or `★`. Left out, like an empty "", so a search
    /// being typed doesn't empty the grid and then fill it again.
    Incomplete,
    /// Plain text.
    Plain,
}

/// The special forms; anything else is plain text.
fn special(word: &str) -> Special {
    if let Some(tag) = word.strip_prefix(['#', '＃']) {
        return if tag.is_empty() { Special::Incomplete } else { Special::Atom(Atom::Tag(tag.to_string())) };
    }
    if word == "★" {
        return Special::Incomplete;
    }
    let numeric = [("rating", NumKey::Rating), ("★", NumKey::Rating), ("dur", NumKey::Duration), ("長度", NumKey::Duration), ("长度", NumKey::Duration), ("長さ", NumKey::Duration), ("길이", NumKey::Duration), ("w", NumKey::Width), ("h", NumKey::Height)];
    for (key, nk) in numeric {
        let Some(rest) = word.strip_prefix(key) else { continue };
        let found = [(">=", Cmp::Ge), ("<=", Cmp::Le), (">", Cmp::Gt), ("<", Cmp::Lt), ("=", Cmp::Eq), (":", Cmp::Eq), ("：", Cmp::Eq)]
            .iter()
            .find_map(|(op, c)| rest.strip_prefix(op).map(|v| (*c, v)))
            // `★3` is `★>=3`; `★精選` is just a word.
            .or_else(|| (key == "★" && rest.starts_with(|c: char| c.is_ascii_digit())).then_some((Cmp::Ge, rest)));
        // `wedding`, `height`: a word that only starts like a key.
        let Some((cmp, value)) = found else { return Special::Plain };
        let v = if nk == NumKey::Duration { parse_duration(value) } else { value.parse::<f64>().ok() };
        let Some(v) = v.filter(|v| v.is_finite()) else { return Special::Incomplete };
        // `rating:3` means three stars or more, like the toolbar's filter.
        let cmp = if nk == NumKey::Rating && cmp == Cmp::Eq && !rest.starts_with('=') { Cmp::Ge } else { cmp };
        return Special::Atom(Atom::Number(nk, cmp, v));
    }
    let Some((key, value)) = word.split_once([':', '：']) else { return Special::Plain };
    match key {
        "ext" | "格式" | "形式" | "형식" => match value.trim_start_matches('.') {
            "" => Special::Incomplete,
            ext => Special::Atom(Atom::Ext(ext.to_string())),
        },
        "type" | "類型" | "类型" | "種類" | "종류" if value.is_empty() => Special::Incomplete,
        // An unknown kind may still be a field called "type".
        "type" | "類型" | "类型" | "種類" | "종류" => kind_named(value).map_or(Special::Plain, |k| Special::Atom(Atom::Kind(k))),
        _ => Special::Plain,
    }
}

/// Whether an unquoted word (lowercase) is plain text rather than a special form.
fn is_plain_word(word: &str) -> bool {
    matches!(special(word), Special::Plain)
}

/// The search as clauses that must all hold.
fn parse(search: &str) -> Vec<Clause> {
    let mut clauses: Vec<Clause> = Vec::new();
    let mut join = false;
    for Token { word, quoted, neg } in tokens(search) {
        if word == "OR" && !quoted && !neg {
            join = !clauses.is_empty();
            continue;
        }
        let body = word.as_str();
        if body.is_empty() {
            continue;
        }
        let lower = body.to_lowercase();
        let atom = match if quoted { Special::Plain } else { special(&lower) } {
            Special::Atom(a) => a,
            Special::Incomplete => continue,
            Special::Plain => Atom::Text(Term::new(&lower)),
        };
        match clauses.last_mut() {
            Some(last) if join => last.any.push((neg, atom)),
            _ => clauses.push(Clause { any: vec![(neg, atom)] }),
        }
        join = false;
    }
    clauses
}

/// The plain words and phrases a search looks for (not excluded, not
/// `欄位:值` or the other special forms), e.g. to find them in subtitles.
pub fn search_words(search: &str) -> Vec<String> {
    tokens(search)
        .into_iter()
        .filter(|t| !t.neg && !t.word.is_empty() && t.word != "-")
        .filter(|t| t.quoted || (t.word != "OR" && !t.word.contains([':', '：']) && is_plain_word(&t.word.to_lowercase())))
        .map(|t| t.word.to_lowercase())
        .collect()
}

/// Characters that end a word, for the fuzzy match's word-start bonus.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || "_-.()[]【】（）「」・,，、".contains(c)
}

/// How well `word` (lowercase) matches `text` as a subsequence: every character
/// in order, more for runs, word starts and the very start. None if it doesn't.
/// Hand-written rather than nucleo-matcher: names are short and few enough.
pub fn fuzzy_score(word: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let word: Vec<char> = word.chars().collect();
    if word.is_empty() {
        return Some(0);
    }
    // Most names don't have the letters in order at all: a quick pass first.
    let mut rest = text.iter();
    if !word.iter().all(|w| rest.any(|t| t == w)) {
        return None;
    }
    // Best score for matching word[..=i] with word[i] at text[j]: a small
    // dynamic program, so `ab` in `a_xab` takes the run, not the first `a`.
    const NONE: i32 = i32::MIN / 2;
    let mut prev: Vec<i32> = vec![NONE; text.len()];
    for (i, &wc) in word.iter().enumerate() {
        let mut cur = vec![NONE; text.len()];
        for j in 0..text.len() {
            if text[j] != wc {
                continue;
            }
            let start = j == 0 || is_separator(text[j - 1]) || (!text[j - 1].is_ascii() && !wc.is_ascii());
            let here = 1 + if start { 6 } else { 0 } + if j == 0 { 4 } else { 0 };
            if i == 0 {
                cur[j] = here - (j as i32).min(5);
                continue;
            }
            // From the previous letter at k < j: a run, or a gap that costs.
            let best = (0..j).filter(|&k| prev[k] > NONE).map(|k| if k + 1 == j { prev[k] + 8 } else { prev[k] - ((j - k - 1) as i32).min(6) }).max();
            if let Some(b) = best {
                cur[j] = b + here;
            }
        }
        prev = cur;
    }
    prev.into_iter().filter(|s| *s > NONE).max()
}

/// A search with no special forms (`-`, quotes, OR, `#`, `欄位:值`, `★>=3`…),
/// which the shortcut search may match loosely.
pub fn is_plain_search(search: &str) -> bool {
    tokens(search)
        .iter()
        .all(|t| !t.quoted && !t.neg && t.word != "OR" && !t.word.starts_with('#') && !t.word.contains([':', '：']) && is_plain_word(&t.word.to_lowercase()))
}

/// The fuzzy score of an item for every word of `search`: each word scored on
/// the name, file name or a tag (best of them), None if any word misses.
pub fn fuzzy_item_score(search: &str, it: &Item) -> Option<i32> {
    let mut total = 0;
    for word in search.split_whitespace().map(str::to_lowercase) {
        let best = std::iter::once(it.name.as_str())
            .chain(std::iter::once(it.file_name.as_str()))
            .chain(it.tags.iter().map(String::as_str))
            .filter_map(|t| fuzzy_score(&word, t))
            .max()?;
        total += best;
    }
    Some(total)
}

pub struct Criteria {
    clauses: Vec<Clause>,
    filter: SavedFilter,
    added_after: Option<i64>,
    used_after: Option<i64>,
    /// From the library, when the filter asks about them.
    missing: HashSet<String>,
    subtitled: HashSet<String>,
}

impl Criteria {
    /// Without the library: `missing` and `has_subtitle` match nothing.
    pub fn new(filter: &SavedFilter) -> Self {
        let day = |d: u32| crate::model::now_ms() - d as i64 * 86_400_000;
        Self {
            clauses: parse(&filter.search),
            filter: filter.clone(),
            added_after: filter.added_days.map(day),
            used_after: filter.used_days.map(day),
            missing: HashSet::new(),
            subtitled: HashSet::new(),
        }
    }

    /// With what the library knows about missing files and subtitles.
    pub fn for_library(filter: &SavedFilter, lib: &crate::library::Library) -> Self {
        let mut c = Self::new(filter);
        if filter.missing {
            c.missing = lib.missing.iter().cloned().collect();
        }
        if filter.has_subtitle.is_some() {
            c.subtitled = lib.subtitled_ids();
        }
        c
    }

    /// Conditions beyond the toolbar's (used by the built-in smart folders).
    fn matches_extra(&self, it: &Item) -> bool {
        use crate::model::Orientation;
        let f = &self.filter;
        (f.exts.is_empty() || f.exts.contains(&it.ext))
            && self.added_after.is_none_or(|t| it.added_at >= t)
            && f.orientation.is_none_or(|o| {
                let (w, h) = (it.width as f32, it.height as f32);
                if w == 0.0 || h == 0.0 {
                    return false;
                }
                let r = w / h;
                match o {
                    Orientation::Landscape => r > 1.05,
                    Orientation::Portrait => r < 0.95,
                    Orientation::Square => (0.95..=1.05).contains(&r),
                }
            })
            && f.min_long_side.is_none_or(|s| it.width.max(it.height) >= s)
            && f.min_size_mb.is_none_or(|mb| it.size >= mb * 1024 * 1024)
            && (!f.unrated || it.rating == 0)
            && (!f.untagged || it.tags.is_empty())
            && (!f.uncategorized || it.folders.is_empty())
            && f.linked.is_none_or(|l| it.source.is_some() == l)
            && (f.kinds.is_empty() || f.kinds.contains(&it.kind))
            && f.min_duration.is_none_or(|d| it.duration.is_some_and(|x| x >= d))
            && f.max_duration.is_none_or(|d| it.duration.is_some_and(|x| x <= d))
            && f.has_clips.is_none_or(|h| it.clips.is_empty() != h)
            && (!f.has_candidates || it.clips.iter().any(|c| is_candidate(&c.name)))
            && f.has_subtitle.is_none_or(|h| matches!(it.kind, MediaKind::Video | MediaKind::Audio) && self.subtitled.contains(&it.id) == h)
            && (!f.missing || self.missing.contains(&it.id))
            && (!f.never_used || it.use_count == 0)
            && self.used_after.is_none_or(|t| it.last_used.is_some_and(|u| u >= t))
    }

    pub(super) fn matches(&self, it: &Item) -> bool {
        let f = &self.filter;
        if !self.clauses.iter().all(|c| c.any.iter().any(|(neg, a)| a.matches(it) != *neg)) {
            return false;
        }
        self.matches_extra(it)
            && f.tags.iter().all(|t| it.tags.contains(t))
            && f.ext.as_ref().is_none_or(|e| it.ext == *e)
            && it.rating >= f.min_rating
            && f.kind.is_none_or(|k| it.kind == k)
            && f.color.is_none_or(|c| it.palette.iter().any(|p| p.ratio >= 0.04 && color_distance(p.rgb, c) < 110.0))
    }
}

/// `欄位:值` (or `欄位：值`) matches items whose custom field contains the value;
/// `欄位:` alone matches items where the field is filled in.
fn field_term_matches(it: &Item, term: &str) -> bool {
    let Some((key, value)) = term.split_once([':', '：']) else { return false };
    it.fields
        .iter()
        .any(|(k, v)| k.to_lowercase() == key && !v.trim().is_empty() && v.to_lowercase().contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, ext: &str, tags: &[&str], rating: u8, dur: Option<f64>, w: u32, h: u32) -> Item {
        let mut it: Item = serde_json::from_value(serde_json::json!({
            "id": name, "name": name, "file_name": format!("{name}.{ext}"), "ext": ext, "size": 1,
            "width": w, "height": h, "added_at": 0, "modified_at": 0
        }))
        .unwrap();
        it.kind = crate::media::kind_of(std::path::Path::new(&it.file_name));
        it.tags = tags.iter().map(|t| t.to_string()).collect();
        it.rating = rating;
        it.duration = dur;
        it
    }

    fn hits(items: &[Item], q: &str) -> Vec<String> {
        let c = Criteria::new(&SavedFilter { search: q.into(), ..Default::default() });
        items.iter().filter(|it| c.matches(it)).map(|it| it.name.clone()).collect()
    }

    #[test]
    fn fuzzy_matching() {
        assert!(fuzzy_score("opd", "Opening_Draft").is_some());
        assert!(fuzzy_score("xyz", "Opening_Draft").is_none());
        assert!(fuzzy_score("dop", "Opening_Draft").is_none(), "order matters");
        // A run and a word start beat scattered letters.
        assert!(fuzzy_score("draft", "Opening_Draft").unwrap() > fuzzy_score("draft", "d_r_a_f_t").unwrap());
        assert!(fuzzy_score("ab", "xxab").unwrap() > fuzzy_score("ab", "xaxb").unwrap(), "a run beats a gap");
        assert!(fuzzy_score("op", "opening").unwrap() > fuzzy_score("op", "loop").unwrap(), "the start counts");
        assert!(fuzzy_score("直精", "直播精華").is_some());

        let it: Item = serde_json::from_value(serde_json::json!({
            "id": "a", "name": "小橘 直播精華 第三場", "file_name": "demo_stream_03.mp4", "ext": "mp4", "size": 1,
            "width": 1, "height": 1, "added_at": 0, "modified_at": 0, "tags": ["遊戲"]
        }))
        .unwrap();
        assert!(fuzzy_item_score("mst 精華", &it).is_some(), "mst from the file name, 精華 from the name");
        assert!(fuzzy_item_score("遊戲", &it).is_some(), "tags too");
        assert!(fuzzy_item_score("精華 zzz", &it).is_none(), "every word");
        assert!(is_plain_search("demo 精華") && !is_plain_search("demo -草稿") && !is_plain_search("ext:mp4") && !is_plain_search("#遊戲"));
    }

    #[test]
    fn a_dash_inside_quotes_and_a_star_before_a_word_are_searched_for() {
        let named = |name: &str| -> Item {
            serde_json::from_value(serde_json::json!({
                "id": name, "name": name, "file_name": "a.png", "ext": "png", "size": 1, "width": 1, "height": 1, "added_at": 0, "modified_at": 0, "rating": 4
            }))
            .unwrap()
        };
        let items = vec![named("音量 -14 dB 版本"), named("★精選 合輯"), named("一般")];
        assert_eq!(hits(&items, r#""-14 dB""#), ["音量 -14 dB 版本"], "the phrase, not everything but it");
        assert_eq!(hits(&items, r#"-"-14 dB""#), ["★精選 合輯", "一般"], "a dash before the quotes still excludes");
        assert_eq!(hits(&items, "★精選"), ["★精選 合輯"]);
        assert_eq!(hits(&items, "★4").len(), 3, "★ and a number is still a rating");
        assert_eq!(search_words(r#""-14 dB" -草稿"#), ["-14 db"]);
    }

    #[test]
    fn search_syntax() {
        let items = vec![
            item("開場動畫 草稿", "mp4", &["直播"], 2, Some(1800.0), 1920, 1080),
            item("開場 動畫", "png", &["直播精華"], 4, None, 1080, 1920),
            item("貓咪", "jpg", &["貓"], 5, None, 4000, 3000),
            item("狗狗", "mp3", &["狗"], 0, Some(20.0), 0, 0),
        ];
        let h = |q: &str| hits(&items, q);
        // Plain words still all have to match.
        assert_eq!(h("開場 動畫"), ["開場動畫 草稿", "開場 動畫"]);
        assert_eq!(h("-草稿"), ["開場 動畫", "貓咪", "狗狗"]);
        assert_eq!(h("開場 -草稿"), ["開場 動畫"]);
        assert_eq!(h("\"開場 動畫\""), ["開場 動畫"], "a phrase keeps its space");
        assert_eq!(h("「開場 動畫」"), ["開場 動畫"]);
        assert_eq!(h("-\"開場 動畫\" 開場"), ["開場動畫 草稿"]);
        assert_eq!(h("貓 OR 狗"), ["貓咪", "狗狗"]);
        assert_eq!(h("貓 OR 狗 OR 草稿"), ["開場動畫 草稿", "貓咪", "狗狗"]);
        assert_eq!(h("貓 or 狗"), Vec::<String>::new(), "lowercase or is a word");
        assert_eq!(h("#直播"), ["開場動畫 草稿"], "a tag equal, not containing");
        assert_eq!(h("直播"), ["開場動畫 草稿", "開場 動畫"]);
        assert_eq!(h("ext:mp4"), ["開場動畫 草稿"]);
        assert_eq!(h("type:影片"), ["開場動畫 草稿"]);
        // The type names and keys of every interface language.
        for q in ["type:video", "type:视频", "類型:動画", "종류:동영상", "種類:動画", "类型:视频"] {
            assert_eq!(h(q), ["開場動畫 草稿"], "{q}");
        }
        assert_eq!(h("長さ>10m"), h("dur>10m"));
        assert_eq!(h("길이>10m"), h("dur>10m"));
        assert_eq!(h("形式:mp4"), h("ext:mp4"));
        assert_eq!(h("type:audio"), ["狗狗"]);
        assert_eq!(h("★>=4"), ["開場 動畫", "貓咪"]);
        assert_eq!(h("★4"), ["開場 動畫", "貓咪"]);
        assert_eq!(h("rating:4"), ["開場 動畫", "貓咪"]);
        assert_eq!(h("rating=4"), ["開場 動畫"]);
        assert_eq!(h("rating<1"), ["狗狗"]);
        assert_eq!(h("dur>10m"), ["開場動畫 草稿"]);
        assert_eq!(h("dur<30s"), ["狗狗"]);
        assert_eq!(h("dur>=1:00"), ["開場動畫 草稿"]);
        assert_eq!(h("w>=1920"), ["開場動畫 草稿", "貓咪"]);
        assert_eq!(h("h>1080"), ["開場 動畫", "貓咪"]);
        assert_eq!(h("type:影片 OR type:音訊 -#直播"), ["狗狗"]);
        assert_eq!(h("ext:jpg OR ext:png ★>=5"), ["貓咪"]);
        // Words that only start like a key are text.
        assert_eq!(h("w"), Vec::<String>::new());
        assert_eq!(h("狗狗 h"), Vec::<String>::new());
    }

    /// A special form without a usable value is left out, like an empty "",
    /// whatever is wrong with it (it is how a search looks while being typed).
    #[test]
    fn incomplete_forms_are_left_out() {
        let items = vec![
            item("開場動畫", "mp4", &["直播"], 2, Some(1800.0), 1920, 1080),
            item("貓咪", "jpg", &["貓"], 5, None, 4000, 3000),
        ];
        let h = |q: &str| hits(&items, q);
        let all = ["開場動畫", "貓咪"];
        for q in ["\"", "\"\"", "ext:", "ext:.", "格式：", "type:", "dur>abc", "dur>", "w>=", "h<", "rating:", "★", "#", "＃", "-ext:", "-w>=", "w=nan"] {
            assert_eq!(h(q), all, "{q:?}");
        }
        // Next to complete conditions they change nothing.
        assert_eq!(h("貓 ext:"), ["貓咪"]);
        assert_eq!(h("ext:mp4 w>="), ["開場動畫"]);
        assert_eq!(h("dur>abc OR 貓"), ["貓咪"]);
        // An unknown kind stays text (it may be a field called "type").
        assert_eq!(h("type:xyz"), Vec::<String>::new());
        assert!(!is_plain_search("ext:") && !is_plain_search("w>="));
        assert_eq!(search_words("ext: w>= 貓"), ["貓"]);
    }

    #[test]
    fn words_for_subtitles() {
        assert_eq!(search_words("開場 -草稿 \"大家 好\" #角色 ext:mp4 授權:內部 OR 貓"), ["開場", "大家 好", "貓"]);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m"), Some(600.0));
        assert_eq!(parse_duration("1h30m"), Some(5400.0));
        assert_eq!(parse_duration("1:30"), Some(90.0));
        assert_eq!(parse_duration("45"), Some(45.0));
        assert_eq!(parse_duration("2分30秒"), Some(150.0));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
    }
}
