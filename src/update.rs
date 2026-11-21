//! New-version check and the bug report link. Releases are published on this
//! repository's GitHub Releases, each with a `latest.json` (version, zip asset name,
//! SHA-256, signature). `releases/latest/download/latest.json` always answers with
//! the newest published release's copy (drafts and pre-releases are skipped). It is
//! a plain download, not the REST API, so it has no hourly rate limit, and it needs
//! no account or stored credentials.

use std::process::Stdio;

const REPO: &str = "rightstar2015/OrangeLink";

/// The download page, opened from the version badge and the help menu.
pub fn releases_url() -> String {
    format!("https://github.com/{REPO}/releases")
}

/// The GitHub release page for a validated version tag (e.g. `v0.1.8`),
/// falling back to the releases list if the tag doesn't look like `vX.Y.Z`.
pub fn release_tag_url(tag: &str) -> String {
    let clean = tag.trim().trim_start_matches('v');
    if parts(clean).is_some_and(|p| p.len() == 3) {
        format!("https://github.com/{REPO}/releases/tag/v{clean}")
    } else {
        releases_url()
    }
}

fn latest_json_url() -> String {
    format!("https://github.com/{REPO}/releases/latest/download/latest.json")
}

/// Where a release's zip is downloaded from: always this repository's release for
/// `tag`, whatever else `latest.json` might say.
#[cfg(test)]
pub fn asset_url(tag: &str, asset: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{asset}")
}

/// A release asset's file name: a plain `.zip` name, with no folder, URL, query or `..`.
pub fn is_asset_name(name: &str) -> bool {
    name.len() <= 128
        && name.to_ascii_lowercase().ends_with(".zip")
        && !name.starts_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn parts(v: &str) -> Option<Vec<u32>> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(tag: &str, current: &str) -> bool {
    matches!((parts(tag), parts(current)), (Some(a), Some(b)) if a > b)
}

/// A published version: its tag, and when `latest.json` names them, the zip asset,
/// its SHA-256 and the release signature (what the updater needs).
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    pub asset: Option<String>,
    pub sha256: Option<String>,
    pub sig: Option<String>,
}

/// `{"version": "0.1.6", "asset": "OrangeLink-v0.1.6-windows-x64.zip", "sha256": "<hex>", "sig": "<base64>", ...}`.
/// Anything that is not such a JSON object (an error page, an empty answer) gives nothing;
/// an asset or hash that is not one is dropped, which leaves only the notice.
pub(crate) fn release_in_latest(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = v.get("version")?.as_str()?.trim().trim_start_matches('v');
    parts(version).filter(|p| p.len() == 3)?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let sha256 = text("sha256").filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()));
    let asset = text("asset").filter(|a| is_asset_name(a));
    Some(Release { tag: format!("v{version}"), asset, sha256, sig: text("sig").filter(|s| s.len() <= SIG_MAX) })
}

/// Longest `sig` that is read: an Ed25519 signature is 88 characters of base64.
const SIG_MAX: usize = 128;

/// Largest `latest.json` that is read (it is a few hundred bytes).
pub const LATEST_JSON_MAX: usize = 64 * 1024;

/// The text of a downloaded `latest.json`. A UTF-8 byte order mark is skipped
/// (Windows PowerShell 5.1's `>` and some editors add one). UTF-16, which
/// PowerShell can also write, is an error that says so instead of a file that
/// silently reads as "no new version"; so are other non-UTF-8 bytes and a file
/// over [`LATEST_JSON_MAX`].
pub fn latest_text(bytes: &[u8]) -> Result<&str, String> {
    if bytes.len() > LATEST_JSON_MAX {
        return Err(format!("latest.json 超過 {} KB", LATEST_JSON_MAX / 1024));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) || bytes.contains(&0) {
        return Err("latest.json 是 UTF-16 編碼，必須是 UTF-8（不要用 PowerShell 的 > 產生）".into());
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| "latest.json 不是 UTF-8 文字".to_string())
}

/// The release in `answer` (the body of `latest.json`, or `None` when it could not be
/// read: offline, timed out, no release yet) when it is newer than `current`,
/// signed or not.
fn newer_any(answer: Option<&[u8]>, current: &str) -> Option<Release> {
    release_in_latest(latest_text(answer?).ok()?).filter(|r| is_newer(&r.tag, current))
}

/// `r`'s signature checks with one of `keys` (no `sig` or `sha256`: it doesn't).
fn signed_by(keys: &[[u8; 32]], r: &Release) -> bool {
    match (&r.sha256, &r.sig) {
        (Some(sha256), Some(sig)) => crate::self_update::signature_ok(keys, &r.tag, sha256, sig),
        _ => false,
    }
}

/// The newer release in `answer` that is offered: only one whose signature
/// checks with `keys`. Unsigned, or with a signature that does not check, it is
/// not offered at all: whoever broke the signature may have replaced the zip on
/// the download page too, so there is no badge and no link to it.
fn newer_signed(keys: &[[u8; 32]], answer: Option<&[u8]>, current: &str) -> Option<Release> {
    newer_any(answer, current).filter(|r| signed_by(keys, r))
}

/// [`newer_signed`] with the app's release keys.
fn newer_in(answer: Option<&[u8]>, current: &str) -> Option<Release> {
    newer_signed(&crate::self_update::release_keys(), answer, current)
}

fn fetch(url: &str) -> Option<Vec<u8>> {
    let mut cmd = crate::self_update::curl(LATEST_JSON_MAX as u64).ok()?;
    cmd.args(["-L", "--fail", "-sS", "--max-time", "15"]).arg(url).stderr(Stdio::null());
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(out.stdout)
}

/// The newest published version when it is newer than this build. Any failure
/// (offline, GitHub unreachable, no release yet, a malformed file) means no notice;
/// a `latest.json` in the wrong encoding is also written to the log.
pub fn newer_release() -> Option<Release> {
    let body = fetch(&latest_json_url())?;
    if let Err(e) = latest_text(&body) {
        crate::log::error("update", format!("讀不懂最新版本資訊：{e}"));
    }
    if let Some(r) = newer_any(Some(&body), env!("CARGO_PKG_VERSION")).filter(|r| !signed_by(&crate::self_update::release_keys(), r)) {
        let why = if r.sig.is_none() { "沒有簽章" } else { "簽章驗證失敗" };
        crate::log::error("update", format!("最新版本資訊（{}）{why}，不提示這個版本", r.tag));
    }
    newer_in(Some(&body), env!("CARGO_PKG_VERSION"))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// New-issue page with the version filled in. Holds no paths or library data.
pub fn report_url() -> String {
    let body = format!(
        "## 發生了什麼事\n\n\n## 怎麼重現\n1. \n\n## 環境\n- OrangeLink {}\n- {}\n\n## 記錄\n（可在「…」→「複製最近的記錄」後貼在這裡；記錄已隱藏使用者資料夾與 token）\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS
    );
    format!("https://github.com/{REPO}/issues/new?body={}", encode(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.10", "0.1.9"));
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(!is_newer("v0.1.2", "0.1.2"));
        assert!(!is_newer("v0.1.1", "0.1.2"));
        assert!(!is_newer("", "0.1.2"));
        assert!(!is_newer("null", "0.1.2"));
        assert!(!is_newer("v0.2.0-beta", "0.1.2"), "pre-release tags are not offered");
    }

    #[test]
    fn reads_latest_json() {
        let tag = |j: &str| release_in_latest(j).map(|r| r.tag);
        assert_eq!(tag(r#"{"version": "0.1.4", "notes": "y"}"#).as_deref(), Some("v0.1.4"));
        assert_eq!(tag(r#"{"version": "v1.2.3"}"#).as_deref(), Some("v1.2.3"));
        assert_eq!(tag(r#"{"version": "soon"}"#), None);
        assert_eq!(tag(r#"{"version": "1.2"}"#), None);
        assert_eq!(tag(r#"{"version": 3}"#), None);
        // The updater's fields; a hash that is not one is ignored.
        let sha = "a".repeat(64);
        let zip = "OrangeLink-v0.2.0-windows-x64.zip";
        let r = release_in_latest(&format!(r#"{{"version": "0.2.0", "asset": " {zip} ", "sha256": "{sha}", "sig": "c2ln"}}"#)).unwrap();
        assert_eq!(r, Release { tag: "v0.2.0".into(), asset: Some(zip.into()), sha256: Some(sha), sig: Some("c2ln".into()) });
        let r = release_in_latest(r#"{"version": "0.2.0", "asset": "", "sha256": "xyz"}"#).unwrap();
        assert_eq!((r.asset, r.sha256), (None, None));
        // The old Drive format names a file id, not an asset: only the notice.
        let r = release_in_latest(&format!(r#"{{"version": "0.2.0", "zip": "1AbC", "sha256": "{}"}}"#, "b".repeat(64))).unwrap();
        assert_eq!(r.asset, None);
    }

    use crate::sign_release_latest as tool;

    const CASES: &str = include_str!("../tools/sign_release/testdata/latest_json_cases.txt");

    /// One line per outcome, written the way the shared case file writes it.
    fn describe(r: Option<Release>) -> String {
        match r {
            None => "none".to_string(),
            Some(r) => {
                let f = |o: Option<String>| o.unwrap_or_else(|| "-".into());
                format!("tag={} | asset={} | sha256={} | sig={}", r.tag, f(r.asset), f(r.sha256), f(r.sig))
            }
        }
    }

    /// What the updater makes of a whole downloaded latest.json.
    fn outcome(bytes: &[u8]) -> String {
        match latest_text(bytes) {
            Err(_) => "unreadable".into(),
            Ok(text) => describe(release_in_latest(text)),
        }
    }

    /// tools/sign_release's `verify` reads latest.json with its own JSON reader;
    /// both are held to this one list of cases (read by the tool's own reader of
    /// the file, so neither side can skip a case).
    #[test]
    fn reads_latest_json_like_the_signing_tool() {
        let cases = tool::case_list(CASES);
        assert!(cases.len() > 40, "only {} cases read", cases.len());
        for (input, want) in &cases {
            assert_eq!(&outcome(input), want, "{}", String::from_utf8_lossy(&input[..input.len().min(300)]));
            assert_eq!(&tool::outcome(input), want);
        }
        let raw = CASES.lines().filter(|l| l.starts_with("raw ")).count();
        assert!(raw >= 8, "BOM, UTF-16 and size cases are byte cases");
    }

    /// The signing tool's JSON reader against serde_json on 10,000 random
    /// mutations of the shared cases (a fixed seed, so a failure repeats):
    /// whether the text is JSON at all, and what the updater takes from it.
    #[test]
    fn the_signing_tool_and_the_app_agree_on_random_json() {
        let corpus: Vec<Vec<char>> = tool::case_list(CASES)
            .into_iter()
            .filter_map(|(bytes, _)| String::from_utf8(bytes).ok())
            .filter(|s| s.len() < 4096)
            .map(|s| s.chars().collect())
            .collect();
        const PIECES: &[char] = &['{', '}', '[', ']', '"', ':', ',', '.', '-', '+', '0', '1', '9', 'e', 'E', 't', 'r', 'u', 'f', 'a', 'l', 's', 'n', 'v', '\\', '/', ' ', '\n', '\t', '\u{feff}', 'é', '😀', '\u{1}'];
        let mut state: u64 = 0x0A11_CE5E_ED00_0001;
        let mut next = move |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n.max(1) as u64) as usize
        };
        for round in 0..10_000 {
            let mut s = corpus[next(corpus.len())].clone();
            for _ in 0..1 + next(4) {
                let at = next(s.len() + 1);
                match next(5) {
                    0 => s.insert(at, PIECES[next(PIECES.len())]),
                    1 if at < s.len() => {
                        s.remove(at);
                    }
                    2 if at < s.len() => s[at] = PIECES[next(PIECES.len())],
                    3 => {
                        let from = next(s.len() + 1);
                        let piece: Vec<char> = s[from..(from + next(20)).min(s.len())].to_vec();
                        s.splice(at..at, piece);
                    }
                    _ => {
                        let end = (at + next(8)).min(s.len());
                        s.drain(at..end);
                    }
                }
            }
            let text: String = s.into_iter().collect();
            let serde_reads = serde_json::from_str::<serde_json::Value>(&text).is_ok();
            assert_eq!(tool::parse_json(&text).is_ok(), serde_reads, "round {round}: {text}");
            assert_eq!(tool::Release::describe(tool::release_in_latest(&text).as_ref()), describe(release_in_latest(&text)), "round {round}: {text}");
        }
    }

    /// Every version has its CHANGELOG section, written before the tag is pushed.
    #[test]
    fn the_changelog_has_a_section_for_this_version() {
        let heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
        assert!(include_str!("../CHANGELOG.md").lines().any(|l| l.trim_end() == heading), "CHANGELOG.md has no {heading} section");
    }

    /// .gitattributes keeps *.txt as LF in every checkout, so the shared cases
    /// (and release_keys.txt) have the same bytes for the app, the tool and CI.
    #[test]
    fn text_files_the_tests_read_are_checked_out_with_lf() {
        let cases = include_bytes!("../tools/sign_release/testdata/latest_json_cases.txt");
        assert!(!cases.contains(&b'\r'), "CR in latest_json_cases.txt: is *.txt in .gitattributes?");
        let attributes = include_str!("../.gitattributes");
        assert!(attributes.lines().any(|l| l.split_whitespace().collect::<Vec<_>>() == ["*.txt", "text", "eol=lf"]));
    }

    /// The test key (seed 7s), and latest.json for `version` signed with `seed`'s
    /// key; `extra` goes in as more fields.
    const TEST_KEY: u8 = 7;
    fn test_keys() -> [[u8; 32]; 1] {
        [*ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([TEST_KEY; 32])).pk]
    }
    fn signed_json(seed: u8, version: &str, extra: &str) -> String {
        use base64::Engine;
        let sha = "a".repeat(64);
        let kp = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([seed; 32]));
        let sig = base64::engine::general_purpose::STANDARD.encode(kp.sk.sign(crate::self_update::signed_message(version, &sha), None).as_ref());
        format!(r#"{{"version": "{version}", "asset": "a.zip", "sha256": "{sha}", "sig": "{sig}"{extra}}}"#)
    }

    #[test]
    fn a_release_with_a_bad_signature_is_not_offered_as_an_update() {
        let keys = test_keys();
        let good = signed_json(TEST_KEY, "99.0.0", "");
        assert_eq!(newer_signed(&keys, Some(good.as_bytes()), "0.1.5").map(|r| r.tag).as_deref(), Some("v99.0.0"));
        let sha = "a".repeat(64);
        for bad in [
            // Base64 of "sig": any string used to bring the badge back.
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}", "sig": "c2ln"}}"#),
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}", "sig": "{}=="}}"#, "A".repeat(86)),
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}", "sig": "{}"}}"#, "A".repeat(128)),
            // Signed, but by another key.
            signed_json(8, "99.0.0", ""),
            // Signed for another version, or another zip.
            signed_json(TEST_KEY, "98.0.0", "").replace("98.0.0", "99.0.0"),
            good.replace(&sha, &"b".repeat(64)),
        ] {
            assert_eq!(newer_signed(&keys, Some(bad.as_bytes()), "0.1.5"), None, "{bad}");
            assert!(newer_any(Some(bad.as_bytes()), "0.1.5").is_some(), "seen, so it can be logged");
        }
        // The app's own keys don't accept the test key's signature either.
        assert_eq!(newer_in(Some(good.as_bytes()), "0.1.5"), None);
    }

    #[test]
    fn an_unsigned_release_is_not_offered_as_an_update() {
        let sha = "a".repeat(64);
        let signed = signed_json(TEST_KEY, "99.0.0", "");
        assert_eq!(newer_signed(&test_keys(), Some(signed.as_bytes()), "0.1.5").map(|r| r.tag).as_deref(), Some("v99.0.0"));
        for unsigned in [
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}"}}"#),
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}", "sig": ""}}"#),
            format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}", "sig": "{}"}}"#, "A".repeat(200)),
            r#"{"version": "99.0.0"}"#.to_string(),
        ] {
            assert_eq!(newer_in(Some(unsigned.as_bytes()), "0.1.5"), None, "{unsigned}");
            assert!(newer_any(Some(unsigned.as_bytes()), "0.1.5").is_some(), "seen, so it can be logged");
        }
    }

    #[test]
    fn a_failed_or_odd_answer_offers_nothing() {
        let newer = signed_json(TEST_KEY, "99.0.0", "");
        assert_eq!(newer_signed(&test_keys(), Some(newer.as_bytes()), "0.1.5").map(|r| r.tag).as_deref(), Some("v99.0.0"));
        assert_eq!(newer_in(None, "0.1.5"), None, "offline or timed out");
        assert_eq!(newer_in(Some("".as_bytes()), "0.1.5"), None, "empty answer");
        assert_eq!(newer_in(Some("Not Found".as_bytes()), "0.1.5"), None, "no release yet");
        assert_eq!(newer_in(Some("<!DOCTYPE html><html>".as_bytes()), "0.1.5"), None, "an error page");
        assert_eq!(newer_in(Some(r#"{"version": "0.1.5"}"#.as_bytes()), "0.1.5"), None, "the same version");
        assert_eq!(newer_in(Some(r#"{"version": "0.1.4"}"#.as_bytes()), "0.1.5"), None, "an older version");
        assert_eq!(newer_in(Some("[1, 2]".as_bytes()), "0.1.5"), None);
    }

    #[test]
    fn the_zip_is_only_fetched_from_this_repositorys_release() {
        assert!(is_asset_name("OrangeLink-v0.2.0-windows-x64.zip"));
        assert!(is_asset_name("orangelink.ZIP"));
        for bad in ["", ".zip", "../x.zip", "a/b.zip", r"a\b.zip", "https://example.com/x.zip", "x.zip?y=1", "x.exe", "x.zip.exe", "x..zip", "名稱.zip", &format!("{}.zip", "a".repeat(130))] {
            assert!(!is_asset_name(bad), "{bad}");
            let json = format!(r#"{{"version": "9.0.0", "asset": {}}}"#, serde_json::to_string(bad).unwrap());
            assert_eq!(release_in_latest(&json).unwrap().asset, None, "{bad}");
        }
        assert_eq!(asset_url("v0.2.0", "OrangeLink.zip"), format!("https://github.com/{REPO}/releases/download/v0.2.0/OrangeLink.zip"));
        assert!(latest_json_url().starts_with("https://github.com/") && releases_url().starts_with("https://github.com/"));
    }

    #[test]
    fn a_latest_json_with_a_bom_still_offers_the_update() {
        let body = signed_json(TEST_KEY, "9.0.0", "");
        let body = body.as_str();
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), body.as_bytes()].concat();
        assert_eq!(newer_signed(&test_keys(), Some(&with_bom), "0.1.5").map(|r| r.tag).as_deref(), Some("v9.0.0"), "UTF-8 with a BOM, as PowerShell 5.1 writes it");
        assert_eq!(latest_text(&with_bom), Ok(body));
        // UTF-16 (PowerShell's other `>` encoding), with or without a BOM, says what is wrong.
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(body.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain(body.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        let bare: Vec<u8> = body.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for utf16 in [le, be, bare] {
            assert!(latest_text(&utf16).unwrap_err().contains("UTF-16"));
            assert_eq!(newer_in(Some(&utf16), "0.1.5"), None);
        }
        assert!(latest_text(b"{\"version\": \"9.0.0\xFF\"}").unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn oversized_latest_json_fields_are_ignored() {
        // A file over 64 KB is not read at all, however it starts.
        let padded = signed_json(TEST_KEY, "9.0.0", &format!(r#", "notes": "{}""#, "x".repeat(LATEST_JSON_MAX)));
        assert!(latest_text(padded.as_bytes()).is_err());
        assert_eq!(newer_in(Some(padded.as_bytes()), "0.1.5"), None);
        let fits = signed_json(TEST_KEY, "9.0.0", &format!(r#", "notes": "{}""#, "x".repeat(LATEST_JSON_MAX - 300)));
        assert!(newer_signed(&test_keys(), Some(fits.as_bytes()), "0.1.5").is_some());
        // A signature longer than any real one is dropped: the notice stays, no download.
        let sig = |s: &str| release_in_latest(&format!(r#"{{"version": "9.0.0", "sig": "{s}"}}"#)).unwrap().sig;
        assert_eq!(sig(&"A".repeat(88)).map(|s| s.len()), Some(88));
        assert_eq!(sig(&"A".repeat(128)).map(|s| s.len()), Some(128));
        assert_eq!(sig(&"A".repeat(129)), None);
        assert_eq!(sig(&"A".repeat(20_000)), None);
    }

    /// Reads the real latest.json from GitHub (there must be a published release).
    #[test]
    #[ignore]
    fn reads_the_real_release() {
        let latest = fetch(&latest_json_url()).unwrap();
        let latest = latest_text(&latest).unwrap();
        println!("latest.json: {:?}", release_in_latest(latest));
        assert!(release_in_latest(latest).is_some());
    }

    #[test]
    fn report_link_is_encoded() {
        let url = report_url();
        assert!(url.contains("?body=%23%23%20"));
        assert!(!url[url.find("body=").unwrap()..].contains(' '));
    }

    #[test]
    fn release_tag_url_points_to_validated_github_tag() {
        assert_eq!(release_tag_url("v0.1.8"), format!("https://github.com/{REPO}/releases/tag/v0.1.8"));
        assert_eq!(release_tag_url("0.2.0"), format!("https://github.com/{REPO}/releases/tag/v0.2.0"));
        assert_eq!(release_tag_url("../evil"), releases_url());
        assert_eq!(release_tag_url("v0.2.0?x=1"), releases_url());
    }
}
