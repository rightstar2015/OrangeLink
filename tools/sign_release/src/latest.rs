//! Reading `latest.json` by the app's rules (`src/update.rs`: `latest_text` and
//! `release_in_latest`), so `verify` accepts exactly what the updater will.
//! The app uses serde_json; this tool keeps to ed25519-compact and sha2, so it
//! has a small strict JSON reader of its own. `testdata/latest_json_cases.txt`
//! holds the cases both sides are tested against.

/// Largest `latest.json` the app reads (`update::LATEST_JSON_MAX`).
pub const LATEST_JSON_MAX: usize = 64 * 1024;
/// Longest `sig` the app reads (`update::SIG_MAX`).
const SIG_MAX: usize = 128;

/// The text of a `latest.json` file, as `update::latest_text` reads it.
pub fn latest_text(bytes: &[u8]) -> Result<&str, String> {
    if bytes.len() > LATEST_JSON_MAX {
        return Err(format!("latest.json is over {} KB", LATEST_JSON_MAX / 1024));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) || bytes.contains(&0) {
        return Err("latest.json is UTF-16; it must be UTF-8".into());
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| "latest.json is not UTF-8 text".to_string())
}

/// What the updater takes from `latest.json` (`update::Release`).
#[derive(Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub asset: Option<String>,
    pub sha256: Option<String>,
    pub sig: Option<String>,
}

impl Release {
    /// One line, the same on both sides of the shared test cases.
    #[cfg(test)]
    pub fn describe(r: Option<&Release>) -> String {
        match r {
            None => "none".into(),
            Some(r) => {
                let f = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
                format!("tag={} | asset={} | sha256={} | sig={}", r.tag, f(&r.asset), f(&r.sha256), f(&r.sig))
            }
        }
    }
}

/// `update::is_asset_name`.
pub fn is_asset_name(name: &str) -> bool {
    name.len() <= 128
        && name.to_ascii_lowercase().ends_with(".zip")
        && !name.starts_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// `update::parts`: three or more numbers as Rust's `u32` parses them.
fn parts(v: &str) -> Option<Vec<u32>> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

/// `update::release_in_latest`.
pub fn release_in_latest(json: &str) -> Option<Release> {
    let v = parse_json(json).ok()?;
    let version = v.get("version")?.as_str()?.trim().trim_start_matches('v');
    parts(version).filter(|p| p.len() == 3)?;
    let text = |k: &str| v.get(k).and_then(Json::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let sha256 = text("sha256").filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
    let asset = text("asset").filter(|a| is_asset_name(a));
    Some(Release { tag: format!("v{version}"), asset, sha256, sig: text("sig").filter(|s| s.len() <= SIG_MAX) })
}

/// `self_update::signed_message`.
pub fn signed_message(tag: &str, sha256: &str) -> String {
    format!("OrangeLink {} {}", tag.trim().trim_start_matches('v'), sha256.trim().to_lowercase())
}

#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number,
    Str(String),
    Array(Vec<Json>),
    /// Keys in file order; a repeated key's last value counts, as in serde_json.
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(fields) => fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// serde_json's nesting limit: it starts with 128 levels to spend and fails
/// when opening an array or object leaves none, so 127 levels nest and the
/// 128th is an error.
const MAX_DEPTH: usize = 128;

/// Strict JSON (RFC 8259) as serde_json reads it: one value, whitespace around
/// it, no trailing commas or comments, no control characters or lone
/// surrogates in strings.
pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = Parser { s: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing characters at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at {}", self.i))
    }

    fn eat(&mut self, word: &[u8]) -> bool {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if matches!(self.peek(), Some(b'{' | b'[')) && depth + 1 >= MAX_DEPTH {
            return self.err("nested too deep");
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ if self.eat(b"true") => Ok(Json::Bool(true)),
            _ if self.eat(b"false") => Ok(Json::Bool(false)),
            _ if self.eat(b"null") => Ok(Json::Null),
            _ => self.err("expected a value"),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1;
        let mut fields = Vec::new();
        self.ws();
        if self.eat(b"}") {
            return Ok(Json::Object(fields));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return self.err("expected a key");
            }
            let key = self.string()?;
            self.ws();
            if !self.eat(b":") {
                return self.err("expected ':'");
            }
            self.ws();
            let v = self.value(depth + 1)?;
            fields.push((key, v));
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"}") {
                return Ok(Json::Object(fields));
            }
            return self.err("expected ',' or '}'");
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.i += 1;
        let mut items = Vec::new();
        self.ws();
        if self.eat(b"]") {
            return Ok(Json::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            if self.eat(b",") {
                continue;
            }
            if self.eat(b"]") {
                return Ok(Json::Array(items));
            }
            return self.err("expected ',' or ']'");
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        self.eat(b"-");
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.err("bad number"),
        }
        if self.eat(b".") && self.digits() == 0 {
            return self.err("bad number");
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return self.err("bad number");
            }
        }
        // serde_json reads every number it can't keep as an integer as an f64,
        // and a number that is infinite as one (1e400) is an error.
        let text = std::str::from_utf8(&self.s[start..self.i]).unwrap_or_default();
        if !text.parse::<f64>().is_ok_and(f64::is_finite) {
            return self.err("number out of range");
        }
        Ok(Json::Number)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let h = self.s.get(self.i..self.i + 4).filter(|h| h.iter().all(u8::is_ascii_hexdigit)).ok_or(format!("bad \\u escape at {}", self.i))?;
        self.i += 4;
        Ok(h.iter().fold(0, |n, &c| n * 16 + (c as char).to_digit(16).unwrap_or(0)))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else { return self.err("unterminated string") };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                0..=0x1F => return self.err("control character in string"),
                b'\\' => {
                    self.i += 1;
                    let Some(e) = self.peek() else { return self.err("unterminated string") };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = match hi {
                                0xD800..=0xDBFF => {
                                    if !self.eat(b"\\u") {
                                        return self.err("lone surrogate");
                                    }
                                    let lo = self.hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&lo) {
                                        return self.err("lone surrogate");
                                    }
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                0xDC00..=0xDFFF => return self.err("lone surrogate"),
                                c => c,
                            };
                            out.push(char::from_u32(code).ok_or(format!("bad \\u escape at {}", self.i))?);
                        }
                        _ => return self.err("bad escape"),
                    }
                }
                _ => {
                    // Copy one whole UTF-8 character (the input is a &str).
                    let rest = std::str::from_utf8(&self.s[self.i..]).map_err(|_| "not UTF-8".to_string())?;
                    let ch = rest.chars().next().unwrap_or('\0');
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }
}

/// Base64 as the app's `base64::engine::general_purpose::STANDARD` decodes it:
/// the standard alphabet, padding required, unused bits zero.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (n, chunk) in b.chunks(4).enumerate() {
        let last = n == b.len() / 4 - 1;
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return None;
        }
        let mut acc = 0u32;
        for &c in &chunk[..4 - pad] {
            acc = (acc << 6) | u32::from(val(c)?);
        }
        acc <<= 6 * pad as u32;
        let bytes = [(acc >> 16) as u8, (acc >> 8) as u8, acc as u8];
        let keep = 3 - pad;
        // Bits past the last whole byte must be zero.
        if bytes[keep..].iter().any(|&x| x != 0) {
            return None;
        }
        out.extend_from_slice(&bytes[..keep]);
    }
    Some(out)
}

/// The app's `self_update::signature_ok`.
pub fn signature_ok(keys: &[[u8; 32]], tag: &str, sha256: &str, sig: &str) -> bool {
    let Some(bytes) = base64_decode(sig.trim()) else { return false };
    let Ok(sig) = ed25519_compact::Signature::from_slice(&bytes) else { return false };
    let message = signed_message(tag, sha256);
    keys.iter().any(|key| ed25519_compact::PublicKey::new(*key).verify(&message, &sig).is_ok())
}

/// The shared case file (testdata/latest_json_cases.txt) as `(file bytes,
/// expected outcome)`. Read by this tool's tests and, through `#[path]`, the
/// app's, so both use the same reader.
///
/// - `in <text>`: the file is that one line of text (`in` alone: empty).
/// - `raw <tokens>`: the file's bytes, each token hex bytes with an optional
///   `*<count>` repeat (`efbbbf`, `20*65536`), for BOMs, UTF-16 and sizes.
/// - `out <outcome>`: what the updater makes of the file before it: `unreadable`
///   (the text is refused: size, UTF-16, not UTF-8), `none` (no release), or
///   the fields taken (`tag=… | asset=… | sha256=… | sig=…`).
#[cfg(test)]
pub fn case_list(text: &str) -> Vec<(Vec<u8>, String)> {
    let mut out = Vec::new();
    let mut input: Option<Vec<u8>> = None;
    for (n, line) in text.lines().enumerate() {
        if let Some(i) = line.strip_prefix("in ").or((line == "in").then_some("")) {
            input = Some(i.as_bytes().to_vec());
        } else if let Some(tokens) = line.strip_prefix("raw ") {
            let mut bytes = Vec::new();
            for token in tokens.split_whitespace() {
                let (hex, count) = token.split_once('*').map_or((token, 1), |(h, c)| (h, c.parse().expect("repeat count")));
                assert!(hex.len() % 2 == 0 && hex.bytes().all(|b| b.is_ascii_hexdigit()), "line {}: bad hex {token}", n + 1);
                let chunk: Vec<u8> = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
                for _ in 0..count {
                    bytes.extend_from_slice(&chunk);
                }
            }
            input = Some(bytes);
        } else if let Some(o) = line.strip_prefix("out ") {
            out.push((input.take().unwrap_or_else(|| panic!("line {}: out without in", n + 1)), o.to_string()));
        }
    }
    out
}

/// What this reader makes of a whole `latest.json` file, as `case_list` writes it.
#[cfg(test)]
pub fn outcome(bytes: &[u8]) -> String {
    match latest_text(bytes) {
        Err(_) => "unreadable".into(),
        Ok(text) => Release::describe(release_in_latest(text).as_ref()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &str = include_str!("../testdata/latest_json_cases.txt");

    #[test]
    fn reads_latest_json_like_the_app() {
        let all = case_list(CASES);
        assert!(all.len() > 40);
        for (input, want) in all {
            assert_eq!(outcome(&input), want, "{}", String::from_utf8_lossy(&input[..input.len().min(300)]));
        }
    }

    #[test]
    fn decodes_the_text_like_the_app() {
        let body = r#"{"version": "9.0.0"}"#;
        assert_eq!(latest_text(&[b"\xEF\xBB\xBF".as_slice(), body.as_bytes()].concat()), Ok(body));
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(body.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert!(latest_text(&le).unwrap_err().contains("UTF-16"));
        let bare: Vec<u8> = body.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(latest_text(&bare).unwrap_err().contains("UTF-16"));
        assert!(latest_text(b"{\"v\xFF\"}").is_err());
        assert!(latest_text(&vec![b' '; LATEST_JSON_MAX + 1]).is_err());
        assert!(latest_text(&vec![b' '; LATEST_JSON_MAX]).is_ok());
    }

    #[test]
    fn base64_is_as_strict_as_the_apps() {
        for (enc, raw) in [("", ""), ("Zg==", "f"), ("Zm8=", "fo"), ("Zm9v", "foo"), ("Zm9vYmFy", "foobar")] {
            assert_eq!(base64_decode(enc).as_deref(), Some(raw.as_bytes()), "{enc}");
        }
        for bad in ["Zg", "Zg=", "Zh==", "Zm9=", "Zg==Zg==", "Z===", "Zm9v!", "Zm 9v"] {
            assert_eq!(base64_decode(bad), None, "{bad}");
        }
    }
}
