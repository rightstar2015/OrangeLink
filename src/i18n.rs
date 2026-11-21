//! Interface languages. The strings in the code are written in Traditional
//! Chinese and double as keys: `t!("設定")` shows 設定, or its entry in
//! `locales/<language>.json` when another language is picked. A key with no
//! entry shows the Chinese. `every_key_is_translated` checks the files against
//! the `t!` / `tk!` keys in the code.
//!
//! Not translated on purpose: what is saved in a library or the config (folder
//! and smart folder names, clip names), the search syntax (`type:影片` keeps
//! working in every language), the log, and MCP replies (read by an AI).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// `t!("文字")` is the text in the current language. With arguments, `{name}`
/// in the text is replaced: `t!("已匯入 {n} 個", n)` or `t!("…{n}…", n = list.len())`.
/// The arguments are shown with `Display`; format anything else first.
macro_rules! t {
    (@val $name:ident) => {
        $name
    };
    (@val $name:ident = $val:expr) => {
        $val
    };
    ($key:literal) => {
        $crate::i18n::tr($key)
    };
    ($key:literal, $($name:ident $(= $val:expr)?),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($key), &[$((stringify!($name), &(t!(@val $name $(= $val)?)) as &dyn ::std::fmt::Display)),+])
    };
}

/// A key that is translated where it is shown (`crate::i18n::tr`), for tables
/// in `const`s. It is the text itself.
macro_rules! tk {
    ($key:literal) => {
        $key
    };
}

/// A language the interface can be shown in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    ZhTw,
    ZhCn,
    En,
    Ja,
    Ko,
}

impl Lang {
    pub const ALL: [Lang; 5] = [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko];

    /// The language's name in itself, the same in every language.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::ZhTw => "繁體中文",
            Lang::ZhCn => "简体中文",
            Lang::En => "English",
            Lang::Ja => "日本語",
            Lang::Ko => "한국어",
        }
    }

    /// The translation file; Traditional Chinese is the code itself.
    fn source(self) -> Option<&'static str> {
        match self {
            Lang::ZhTw => None,
            Lang::ZhCn => Some(include_str!("../locales/zh-CN.json")),
            Lang::En => Some(include_str!("../locales/en.json")),
            Lang::Ja => Some(include_str!("../locales/ja.json")),
            Lang::Ko => Some(include_str!("../locales/ko.json")),
        }
    }

    /// From a BCP 47 tag such as `zh-TW`, `zh-Hans-CN` or `ja-JP`: Chinese by
    /// script or region, otherwise by language. Anything else is None.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let tag = tag.replace('_', "-").to_ascii_lowercase();
        let mut parts = tag.split(['-', '.']);
        match parts.next()? {
            "zh" => {
                let rest: Vec<&str> = parts.collect();
                // The script decides when there is one (zh-Hans-HK is simplified).
                let simplified = if rest.contains(&"hans") {
                    true
                } else if rest.contains(&"hant") {
                    false
                } else {
                    rest.iter().any(|p| matches!(*p, "cn" | "sg" | "my"))
                };
                Some(if simplified { Lang::ZhCn } else { Lang::ZhTw })
            }
            "en" => Some(Lang::En),
            "ja" => Some(Lang::Ja),
            "ko" => Some(Lang::Ko),
            _ => None,
        }
    }

    /// Windows' display language; English when it is none of these.
    pub fn system() -> Lang {
        system_tag().as_deref().and_then(Lang::from_tag).unwrap_or(Lang::En)
    }
}

#[cfg(windows)]
fn system_tag() -> Option<String> {
    use windows::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName, LOCALE_ALLOW_NEUTRAL_NAMES};
    let mut buf = [0u16; 85];
    #[allow(unsafe_code)]
    // SAFETY: the buffer is LOCALE_NAME_MAX_LENGTH (85) wide, as the call expects.
    let n = unsafe { LCIDToLocaleName(u32::from(GetUserDefaultUILanguage()), Some(&mut buf), LOCALE_ALLOW_NEUTRAL_NAMES) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

#[cfg(not(windows))]
fn system_tag() -> Option<String> {
    std::env::var("LANG").ok()
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Shows the interface in `lang` from the next frame on.
pub fn set(lang: Lang) {
    CURRENT.store(Lang::ALL.iter().position(|l| *l == lang).unwrap_or(0) as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    Lang::ALL.get(CURRENT.load(Ordering::Relaxed) as usize).copied().unwrap_or_default()
}

type Table = HashMap<&'static str, &'static str>;

/// A language's entries, read once. The strings are kept for the whole run (a
/// few hundred KB per language used), so lookups can hand out `&'static str`.
fn table(lang: Lang) -> Option<&'static Table> {
    static TABLES: [OnceLock<Table>; 5] = [const { OnceLock::new() }; 5];
    let i = Lang::ALL.iter().position(|l| *l == lang)?;
    let source = lang.source()?;
    Some(TABLES[i].get_or_init(|| parse(source).unwrap_or_default()))
}

fn parse(source: &str) -> Option<Table> {
    let map: HashMap<String, String> = serde_json::from_str(source).ok()?;
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    Some(map.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (leak(k), leak(v))).collect())
}

/// `key` in the current language.
pub fn tr(key: &'static str) -> &'static str {
    tr_in(current(), key)
}

/// `key` in `lang`.
pub fn tr_in(lang: Lang, key: &'static str) -> &'static str {
    match table(lang) {
        Some(t) => t.get(key).copied().unwrap_or(key),
        None => key,
    }
}

/// `template` with each `{name}` replaced by its argument; `{{` and `}}` are
/// braces. A name with no argument is left as it is.
pub fn fill(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(end) = tail.find('}').filter(|_| tail.starts_with('{')) {
            let name = &tail[1..end];
            if let Some((_, v)) = args.iter().find(|(n, _)| *n == name) {
                let _ = write!(out, "{v}");
                rest = &tail[end + 1..];
                continue;
            }
        }
        out.push_str(&tail[..1]);
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn fills_named_arguments_and_braces() {
        let n = 3;
        assert_eq!(fill("已匯入 {n} 個，{name}", &[("n", &n), ("name", &"貓.png")]), "已匯入 3 個，貓.png");
        assert_eq!(fill("{{n}} {n}", &[("n", &1)]), "{n} 1");
        assert_eq!(fill("{missing} {", &[]), "{missing} {");
        assert_eq!(fill("é{n}é", &[("n", &"ü")]), "éüé");
        let files = 2;
        assert_eq!(t!("{files} 個", files), "2 個");
        assert_eq!(t!("{n} 個", n = files + 1), "3 個");
    }

    #[test]
    fn reads_the_language_from_a_tag() {
        assert_eq!(Lang::from_tag("zh-TW"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_tag("zh-HK"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_tag("zh-Hant-TW"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_tag("zh-CN"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_tag("zh-Hans-HK"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_tag("zh_CN.UTF-8"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_tag("zh"), Some(Lang::ZhTw));
        assert_eq!(Lang::from_tag("en-US"), Some(Lang::En));
        assert_eq!(Lang::from_tag("ja-JP"), Some(Lang::Ja));
        assert_eq!(Lang::from_tag("ko-KR"), Some(Lang::Ko));
        assert_eq!(Lang::from_tag("de-DE"), None);
        assert_eq!(Lang::from_tag(""), None);
    }

    /// The text of each `t!("…")` / `tk!("…")` key in the code outside tests.
    fn keys_in_code() -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"))];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("i18n.rs") {
                    let src = std::fs::read_to_string(&path).unwrap();
                    let code = src.split("#[cfg(test)]\nmod tests").next().unwrap_or_default();
                    let ident = |c: char| c.is_alphanumeric() || c == '_';
                    for (i, _) in code.match_indices("t!(\"").chain(code.match_indices("tk!(\"")) {
                        if code[..i].chars().next_back().is_some_and(ident) {
                            continue;
                        }
                        let start = code[i..].find('"').unwrap() + i;
                        let key: String = serde_json::Deserializer::from_str(&code[start..]).into_iter::<String>().next().unwrap().unwrap();
                        keys.insert(key);
                    }
                }
            }
        }
        keys
    }

    fn placeholders(s: &str) -> BTreeSet<&str> {
        s.match_indices('{')
            .filter_map(|(i, _)| {
                let name = &s[i + 1..i + 1 + s[i + 1..].find('}')?];
                (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).then_some(name)
            })
            .collect()
    }

    #[test]
    fn every_key_is_translated() {
        let keys = keys_in_code();
        assert!(keys.len() > 100, "found only {} keys", keys.len());
        for lang in Lang::ALL {
            let Some(source) = lang.source() else { continue };
            let map: HashMap<String, String> = serde_json::from_str(source).unwrap_or_else(|e| panic!("{lang:?}: {e}"));
            let missing: Vec<&String> = keys.iter().filter(|k| map.get(*k).is_none_or(|v| v.trim().is_empty())).collect();
            assert!(missing.is_empty(), "{lang:?} has no entry for {} keys: {missing:#?}", missing.len());
            let unused: Vec<&String> = map.keys().filter(|k| !keys.contains(*k)).collect();
            assert!(unused.is_empty(), "{lang:?} has entries no code uses: {unused:#?}");
            for (k, v) in &map {
                assert_eq!(placeholders(k), placeholders(v), "{lang:?}: {k:?} → {v:?}");
            }
        }
    }

    #[test]
    fn a_translated_key_shows_in_the_picked_language() {
        // Not through `set`: other tests run at the same time and check Chinese text.
        assert_eq!(tr_in(Lang::En, "設定"), "Settings");
        assert_eq!(tr_in(Lang::Ja, "設定"), "設定");
        assert_eq!(tr_in(Lang::ZhTw, "設定"), "設定");
    }
}
