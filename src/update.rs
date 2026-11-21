//! New-version check and the bug report link. Releases are published on this
//! repository's GitHub Releases, each with a `latest.json` naming its version.
//! A newer version is only announced: the notice opens this repository's release
//! page (an address built here, not taken from the file) and the user downloads
//! it there. `releases/latest/download/latest.json` always answers with
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

fn parts(v: &str) -> Option<Vec<u32>> {
    v.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok()).collect()
}

fn is_newer(tag: &str, current: &str) -> bool {
    matches!((parts(tag), parts(current)), (Some(a), Some(b)) if a > b)
}

/// A published version, e.g. `v0.2.0`.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
}

/// `{"version": "0.2.0", ...}`; other fields (the zip's name and hash) are not used.
/// Anything that is not such a JSON object (an error page, an empty answer) gives nothing.
pub(crate) fn release_in_latest(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let version = v.get("version")?.as_str()?.trim().trim_start_matches('v');
    parts(version).filter(|p| p.len() == 3)?;
    Some(Release { tag: format!("v{version}") })
}

/// Largest `latest.json` that is read (it is a few hundred bytes).
pub const LATEST_JSON_MAX: usize = 64 * 1024;

/// The text of a downloaded `latest.json`. A UTF-8 byte order mark is skipped
/// (Windows PowerShell 5.1's `>` and some editors add one). UTF-16, which
/// PowerShell can also write, is an error that says so instead of a file that
/// silently reads as "no new version"; so are other non-UTF-8 bytes and a file
/// over [`LATEST_JSON_MAX`].
pub fn latest_text(bytes: &[u8]) -> Result<&str, String> {
    if bytes.len() > LATEST_JSON_MAX {
        return Err(t!("latest.json 超過 {n} KB", n = LATEST_JSON_MAX / 1024));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) || bytes.contains(&0) {
        return Err(t!("latest.json 是 UTF-16 編碼，必須是 UTF-8（不要用 PowerShell 的 > 產生）").into());
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
    std::str::from_utf8(bytes).map_err(|_| t!("latest.json 不是 UTF-8 文字").to_string())
}

/// The release in `answer` (the body of `latest.json`, or `None` when it could not be
/// read: offline, timed out, no release yet) when it is newer than `current`.
/// Releases are no longer signed: the notice only links to this
/// repository's release page, so there is nothing a signature would add.
fn newer_in(answer: Option<&[u8]>, current: &str) -> Option<Release> {
    release_in_latest(latest_text(answer?).ok()?).filter(|r| is_newer(&r.tag, current))
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
    newer_in(Some(&body), env!("CARGO_PKG_VERSION"))
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// New-issue page with the version filled in. Holds no paths or library data.
pub fn report_url() -> String {
    let body = t!("## 發生了什麼事\n\n\n## 怎麼重現\n1. \n\n## 環境\n- OrangeLink {version}\n- {os}\n\n## 記錄\n（可在「…」→「複製最近的記錄」後貼在這裡；記錄已隱藏使用者資料夾與 token）\n", version = env!("CARGO_PKG_VERSION"), os = std::env::consts::OS);
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
        // Fields from the signed releases (0.1.6 to 0.1.9) are ignored.
        let sha = "a".repeat(64);
        let r = release_in_latest(&format!(r#"{{"version": "0.2.0", "asset": "a.zip", "sha256": "{sha}", "sig": "c2ln"}}"#));
        assert_eq!(r, Some(Release { tag: "v0.2.0".into() }));
    }

    /// Every version has its CHANGELOG section, written before the tag is pushed.
    #[test]
    fn the_changelog_has_a_section_for_this_version() {
        let heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
        assert!(include_str!("../CHANGELOG.md").lines().any(|l| l.trim_end() == heading), "CHANGELOG.md has no {heading} section");
    }

    #[test]
    fn an_unsigned_newer_release_is_announced() {
        assert_eq!(newer_in(Some(br#"{"version": "99.0.0"}"#), "0.1.5").map(|r| r.tag).as_deref(), Some("v99.0.0"));
        let sha = "a".repeat(64);
        let full = format!(r#"{{"version": "99.0.0", "asset": "a.zip", "sha256": "{sha}"}}"#);
        assert_eq!(newer_in(Some(full.as_bytes()), "0.1.5").map(|r| r.tag).as_deref(), Some("v99.0.0"));
    }

    #[test]
    fn a_failed_or_odd_answer_offers_nothing() {
        assert_eq!(newer_in(None, "0.1.5"), None, "offline or timed out");
        assert_eq!(newer_in(Some("".as_bytes()), "0.1.5"), None, "empty answer");
        assert_eq!(newer_in(Some("Not Found".as_bytes()), "0.1.5"), None, "no release yet");
        assert_eq!(newer_in(Some("<!DOCTYPE html><html>".as_bytes()), "0.1.5"), None, "an error page");
        assert_eq!(newer_in(Some(r#"{"version": "0.1.5"}"#.as_bytes()), "0.1.5"), None, "the same version");
        assert_eq!(newer_in(Some(r#"{"version": "0.1.4"}"#.as_bytes()), "0.1.5"), None, "an older version");
        assert_eq!(newer_in(Some("[1, 2]".as_bytes()), "0.1.5"), None);
    }

    #[test]
    fn the_version_is_read_from_this_repositorys_release() {
        assert!(latest_json_url().starts_with("https://github.com/rightstar2015/OrangeLink/") && releases_url().starts_with("https://github.com/"));
    }

    #[test]
    fn a_latest_json_with_a_bom_still_offers_the_update() {
        let body = r#"{"version": "9.0.0"}"#;
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), body.as_bytes()].concat();
        assert_eq!(newer_in(Some(&with_bom), "0.1.5").map(|r| r.tag).as_deref(), Some("v9.0.0"), "UTF-8 with a BOM, as PowerShell 5.1 writes it");
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
        let padded = format!(r#"{{"version": "9.0.0", "notes": "{}"}}"#, "x".repeat(LATEST_JSON_MAX));
        assert!(latest_text(padded.as_bytes()).is_err());
        assert_eq!(newer_in(Some(padded.as_bytes()), "0.1.5"), None);
        let fits = format!(r#"{{"version": "9.0.0", "notes": "{}"}}"#, "x".repeat(LATEST_JSON_MAX - 300));
        assert!(newer_in(Some(fits.as_bytes()), "0.1.5").is_some());
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
