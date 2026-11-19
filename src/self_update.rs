//! Installing a new version without an installer. OrangeLink ships as a zip, so:
//! check the release key's signature on the version and SHA-256 in `latest.json`,
//! download the zip it names into `%LOCALAPPDATA%\OrangeLink\update`, check it
//! against that SHA-256, unpack it, and when OrangeLink closes start
//! the new exe with `--apply-update`: it waits for the old one to exit, renames it
//! to `.old`, copies itself into its place and, when asked, starts it again.
//!
//! Only for an installed copy: an exe somewhere under a `target` folder is a
//! development build and is never replaced.
//!
//! The flow follows Prime Files (`crates/nohrs-pages/src/updater.rs`), MIT License,
//! Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// This build's version, kept in the exe where `tools/sign_release` finds it:
/// it refuses to sign a zip whose exe is another version than the one named,
/// which would make every copy reinstall the same update on each start.
/// (`#[used]` is not enough: the MSVC linker still drops it, so `clean_up`,
/// run at every start, refers to it.)
static VERSION_MARKER: &str = concat!("OrangeLink-release-version=", env!("CARGO_PKG_VERSION"), ";");

/// How long `--apply-update` waits for the old OrangeLink to exit.
const WAIT_FOR_EXIT: Duration = Duration::from_secs(60);

/// A development build: the exe sits under a `target` folder.
pub fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
}

/// This copy may replace itself.
pub fn can_self_update() -> bool {
    std::env::current_exe().is_ok_and(|exe| !is_dev_build(&exe))
}

fn staging_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("OrangeLink").join("update"))
}

/// `<exe>.old`, left by the last update.
fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    PathBuf::from(name)
}

/// At start: removes what the last update left behind.
pub fn clean_up() {
    std::hint::black_box(VERSION_MARKER);
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::fs::remove_file(old_path(&exe));
    if let Some(root) = staging_root().filter(|r| !exe.starts_with(r)) {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Shown when a release's signature does not check. Whoever made `latest.json`
/// fail may also have replaced the zip on the download page, so this never
/// suggests installing by hand.
const SIGNATURE_FAILED: &str = "新版的簽章驗證失敗，請勿安裝，並請回報（「說明」→「回報問題」）";

/// Shown when `latest.json` names something that is not a zip asset.
const BAD_RELEASE_INFO: &str = "新版的更新資訊不正確（檔名不合法），請勿安裝，並請回報（「說明」→「回報問題」）";

/// Downloads, checks and unpacks `tag`'s zip (release asset `asset` on this
/// repository's GitHub Releases, SHA-256 `sha256`, release signature `sig`).
/// Returns the new exe, ready for `install_on_exit`. A release whose asset name or
/// signature doesn't check is not downloaded; a zip that does not match is deleted.
pub fn download(tag: &str, asset: &str, sha256: &str, sig: &str, cancel: &AtomicBool) -> Result<PathBuf, String> {
    if !crate::update::is_asset_name(asset) {
        return Err(BAD_RELEASE_INFO.into());
    }
    // Whoever can change latest.json can change its hash too; only the release key
    // can sign one.
    if !signature_ok(&release_keys(), tag, sha256, sig) {
        return Err(SIGNATURE_FAILED.into());
    }
    let root = staging_root().ok_or("找不到 LOCALAPPDATA")?;
    let dir = root.join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立暫存資料夾：{e}"))?;
    let zip = dir.join("OrangeLink.zip");
    let url = crate::update::asset_url(tag, asset);
    let mut cmd = curl(ZIP_MAX)?;
    cmd.args(["-L", "--fail", "-sS", "--max-time", "900", "-o"]).arg(&zip).arg(&url).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| format!("無法執行 curl：{e}"))?;
    // curl has exited on every way out of here, so the folder can be deleted.
    let finished = wait_or_cancel(&mut child, cancel);
    match finished {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(format!("下載更新失敗（curl 結束代碼 {:?}）", status.code()));
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    }
    // A proxy or captive portal may answer with a web page instead of the file.
    let mut magic = [0u8; 2];
    let is_zip = std::fs::File::open(&zip).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut magic)).is_ok() && &magic == b"PK";
    if !is_zip {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("下載到的不是 zip（網路可能被攔截，或需要先登入網路）".into());
    }
    let got = sha256_file(&zip).map_err(|e| format!("讀不到下載的檔案：{e}"))?;
    if !got.eq_ignore_ascii_case(sha256.trim()) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("更新檔的 SHA-256 不符，已丟棄（應為 {}…，實際 {}…）", &sha256.trim()[..sha256.trim().len().min(12)], &got[..12]));
    }
    // Windows 10 and later ship bsdtar, which unpacks zips.
    let out = dir.join("unpacked");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut tar = crate::process::hidden(system_tool("tar")?);
    tar.arg("-xf").arg(&zip).arg("-C").arg(&out).stdout(Stdio::null()).stderr(Stdio::null());
    let ok = tar.status().is_ok_and(|s| s.success());
    if !ok {
        return Err("無法解壓縮更新檔".into());
    }
    walkdir::WalkDir::new(&out)
        .into_iter()
        .filter_map(Result::ok)
        .map(|e| e.into_path())
        .find(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("OrangeLink.exe")))
        .ok_or_else(|| "更新檔裡沒有 OrangeLink.exe".to_string())
}

/// Starts the downloaded exe to replace this one once it has exited; with
/// `relaunch`, the replaced exe is started again afterwards.
pub fn install_on_exit(new_exe: &Path, relaunch: bool) -> Result<(), String> {
    let target = std::env::current_exe().map_err(|e| e.to_string())?;
    if is_dev_build(&target) {
        return Err("開發版不會自動更新".into());
    }
    let mut cmd = crate::process::hidden(new_exe);
    cmd.arg("--apply-update").arg(&target);
    if relaunch {
        cmd.arg("--relaunch");
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().map(|_| ()).map_err(|e| format!("無法啟動更新程式：{e}"))
}

/// `--apply-update <target> [--relaunch]`, run by the new exe from the staging
/// folder: puts itself in place of `target`.
pub fn apply(target: &Path, relaunch: bool) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if is_dev_build(target) {
        return Err("不會替換開發版".into());
    }
    // A running exe cannot be opened for writing: wait until the old one exits.
    let start = Instant::now();
    while std::fs::OpenOptions::new().write(true).open(target).is_err() {
        if start.elapsed() > WAIT_FOR_EXIT {
            return Err("舊版 OrangeLink 沒有結束".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    replace(&me, target)?;
    if relaunch {
        crate::process::visible(target).spawn().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `target` moves aside to `.old` and `new` is copied in; on failure the old one
/// comes back.
fn replace(new: &Path, target: &Path) -> Result<(), String> {
    let old = old_path(target);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old).map_err(|e| format!("無法移開舊版：{e}"))?;
    if let Err(e) = std::fs::copy(new, target) {
        let _ = std::fs::remove_file(target);
        let _ = std::fs::rename(&old, target);
        return Err(format!("無法放上新版：{e}"));
    }
    Ok(())
}

/// SHA-256 of `data`, as lowercase hex.
#[cfg(test)]
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex(&sha2::Sha256::digest(data))
}

/// SHA-256 of a file, read a piece at a time, as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Waits for `child`, polling `cancel`. On cancel, or when the wait itself
/// fails, the child is killed and waited for: a process that is still exiting
/// keeps its files open, and Windows then refuses to delete them.
fn wait_or_cancel(child: &mut std::process::Child, cancel: &AtomicBool) -> Result<std::process::ExitStatus, String> {
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("已取消下載更新".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("下載更新失敗：{e}"));
            }
        }
    }
}

/// The copy of `curl` or `tar` that ships with Windows (System32). There is no
/// fallback to the bare name: Windows looks for that next to OrangeLink's exe
/// and on PATH first, so any `curl.exe` placed there would run.
pub fn system_tool(name: &str) -> Result<PathBuf, String> {
    system_tool_in(std::env::var_os("SystemRoot").as_deref(), name)
}

fn system_tool_in(system_root: Option<&std::ffi::OsStr>, name: &str) -> Result<PathBuf, String> {
    system_root
        .map(|root| PathBuf::from(root).join("System32").join(format!("{name}.exe")))
        .filter(|p| p.is_file())
        .ok_or_else(|| format!("找不到 Windows 內建的 {name}.exe（System32）"))
}

/// curl arguments that keep a download, and every redirect, on HTTPS.
pub const HTTPS_ONLY: [&str; 4] = ["--proto", "=https", "--proto-redir", "=https"];

/// Largest update zip that is downloaded (a release is about 15 MB).
const ZIP_MAX: u64 = 200 * 1024 * 1024;

/// Windows' curl, ready for a download of at most `max_bytes`: `-q` (which only
/// works as the first argument) so no `_curlrc` in the user's folders can add
/// `--insecure`, a proxy or an output format; HTTPS only, also on redirects; at
/// most 5 redirects.
pub fn curl(max_bytes: u64) -> Result<std::process::Command, String> {
    let mut cmd = crate::process::hidden(system_tool("curl")?);
    cmd.arg("-q").args(HTTPS_ONLY).args(["--max-redirs", "5", "--max-filesize"]).arg(max_bytes.to_string());
    Ok(cmd)
}

/// `release_keys.txt` at the repository's root: the public halves of the
/// release keys (`tools/sign_release keygen`), the one list both the app and
/// the signing tool read. A release signed with any one of them is installed.
///
/// A list so a key can be rotated (SECURITY.md): a version that lists both the
/// old and the new key is signed with the old one, and the version after it can
/// drop the old key. Removing a key only protects the copies that have updated
/// past it, so a leaked key stays trusted by every older copy.
const RELEASE_KEYS_TXT: &str = include_str!("../release_keys.txt");

/// The keys in `RELEASE_KEYS_TXT`. A list that does not parse trusts nothing:
/// no update is offered or installed (`the_shipped_key_list_is_valid` keeps the
/// shipped one right).
pub(crate) fn release_keys() -> Vec<[u8; 32]> {
    crate::release_keys::parse(RELEASE_KEYS_TXT).unwrap_or_default()
}

/// What a release signature covers: the version and the zip's hash, so a signed
/// zip can't be offered as another version.
pub(crate) fn signed_message(tag: &str, sha256: &str) -> String {
    format!("OrangeLink {} {}", tag.trim().trim_start_matches('v'), sha256.trim().to_lowercase())
}

/// The `sig` in latest.json is one of the release keys' signature on `tag` and
/// `sha256`.
pub(crate) fn signature_ok(keys: &[[u8; 32]], tag: &str, sha256: &str, sig: &str) -> bool {
    use base64::Engine;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(sig.trim()) else { return false };
    let Ok(sig) = ed25519_compact::Signature::from_slice(&bytes) else { return false };
    let message = signed_message(tag, sha256);
    keys.iter().any(|key| ed25519_compact::PublicKey::new(*key).verify(&message, &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_standard_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Over one block, and a length that pads into a second one.
        assert_eq!(sha256_hex(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
        assert_eq!(sha256_hex(&[0u8; 55]).len(), 64);
    }

    #[test]
    fn hashes_files_in_pieces() {
        let dir = std::env::temp_dir().join(format!("orangelink-sha-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.bin");
        let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&f, &data).unwrap();
        assert_eq!(sha256_file(&f).unwrap(), sha256_hex(&data));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_releases_signed_with_the_key_are_installed() {
        use base64::Engine;
        let kp = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
        let key: [u8; 32] = *kp.pk;
        let sha = "AB".repeat(32);
        let sig = base64::engine::general_purpose::STANDARD.encode(kp.sk.sign(signed_message("0.2.0", &sha), None).as_ref());
        assert!(signature_ok(&[key], "v0.2.0", &sha, &sig), "the tag may carry a v, the hash any case");
        assert!(signature_ok(&[key], "0.2.0", &sha.to_lowercase(), &sig));
        assert!(!signature_ok(&[key], "v0.2.1", &sha, &sig), "another version");
        assert!(!signature_ok(&[key], "v0.2.0", &format!("{}cdcd", "ab".repeat(31)), &sig), "another zip");
        assert!(!signature_ok(&release_keys(), "v0.2.0", &sha, &sig), "another key");
        assert!(!signature_ok(&[key], "v0.2.0", &sha, "not base64!"), "garbage");
        assert!(!signature_ok(&[key], "v0.2.0", &sha, ""), "no signature");
    }

    /// SECURITY.md quotes what the user sees when an update is refused.
    #[test]
    fn security_md_quotes_the_update_refusals_as_shown() {
        let doc = include_str!("../SECURITY.md");
        let signature = "新版的簽章驗證失敗，請勿安裝，並請回報";
        assert!(SIGNATURE_FAILED.starts_with(signature));
        assert!(doc.contains(&format!("「{signature}」")), "the bad-signature message");
        // A zip that does not match its hash is thrown away, not "do not install".
        let hash = "更新檔的 SHA-256 不符，已丟棄";
        assert!(include_str!("self_update.rs").contains(&format!("\"{hash}（")), "the message download() shows");
        assert!(doc.contains(&format!("「{hash}」")), "the hash-mismatch message");
        assert!(!doc.contains("簽章或雜湊不對就不安裝,並提示"), "the old sentence gave one message for both");
    }

    #[test]
    fn the_shipped_key_list_is_valid() {
        let keys = crate::release_keys::parse(RELEASE_KEYS_TXT).expect("release_keys.txt");
        let distinct: std::collections::HashSet<_> = keys.iter().collect();
        assert!(distinct.len() >= 2, "at least two different keys: a primary and a backup");
        assert_eq!(release_keys(), keys);
        assert!(!RELEASE_KEYS_TXT.contains('<'), "a placeholder is left in release_keys.txt");
        // The key from before 0.1.6, which no released copy ever trusted, is not listed.
        let old = "d6c7eb3e47c0e311cd7750e72ac31e0b50fe93c05d919e3331f4f19cb0ecbdf3";
        assert!(keys.iter().all(|k| crate::release_keys::hex(k) != old));
    }

    #[test]
    fn a_release_signed_with_any_listed_key_is_accepted() {
        use base64::Engine;
        let old = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
        let new = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([8; 32]));
        let stranger = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([9; 32]));
        let sha = "ab".repeat(32);
        let sign = |kp: &ed25519_compact::KeyPair| base64::engine::general_purpose::STANDARD.encode(kp.sk.sign(signed_message("0.3.0", &sha), None).as_ref());
        let both: [[u8; 32]; 2] = [*old.pk, *new.pk];
        assert!(signature_ok(&both, "v0.3.0", &sha, &sign(&old)), "the old key, during a rotation");
        assert!(signature_ok(&both, "v0.3.0", &sha, &sign(&new)), "the new key");
        assert!(!signature_ok(&both, "v0.3.0", &sha, &sign(&stranger)), "a key not in the list");
        assert!(!signature_ok(&[*new.pk], "v0.3.0", &sha, &sign(&old)), "a key dropped from the list");
        assert!(!signature_ok(&[], "v0.3.0", &sha, &sign(&old)), "no keys, no update");
    }

    #[test]
    fn a_release_without_a_good_signature_or_name_is_not_downloaded() {
        let cancel = AtomicBool::new(false);
        let sha = "ab".repeat(32);
        // Both refusals come before anything is created or fetched.
        let err = download("v99.0.0", "OrangeLink.zip", &sha, "c2ln", &cancel).unwrap_err();
        assert!(err.contains("簽章"), "{err}");
        let kp = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
        use base64::Engine;
        let other = base64::engine::general_purpose::STANDARD.encode(kp.sk.sign(signed_message("99.0.0", &sha), None).as_ref());
        assert!(download("v99.0.0", "OrangeLink.zip", &sha, &other, &cancel).unwrap_err().contains("簽章"), "signed with another key");
        for bad in ["../OrangeLink.zip", "https://example.com/x.zip", "OrangeLink.exe"] {
            assert!(download("v99.0.0", bad, &sha, "c2ln", &cancel).unwrap_err().contains("檔名"), "{bad}");
        }
    }

    #[test]
    fn the_signature_failure_message_does_not_suggest_a_manual_install() {
        let cancel = AtomicBool::new(false);
        let sha = "ab".repeat(32);
        let bad_sig = download("v99.0.0", "OrangeLink.zip", &sha, "c2ln", &cancel).unwrap_err();
        let bad_name = download("v99.0.0", "../x.zip", &sha, "c2ln", &cancel).unwrap_err();
        for err in [bad_sig, bad_name] {
            assert!(err.contains("請勿安裝") && err.contains("回報"), "{err}");
            for hint in ["手動", "下載頁", "下載頁面"] {
                assert!(!err.contains(hint), "{err} points to a manual install");
            }
        }
    }

    #[test]
    fn curl_ignores_the_users_curlrc() {
        let dir = std::env::temp_dir().join(format!("orangelink-curlrc-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        // curl on Windows reads %CURL_HOME%, %APPDATA% or %USERPROFILE%\_curlrc.
        std::fs::write(dir.join("_curlrc"), "write-out = \"CURLRC_LOADED\\n\"\n").unwrap();
        let run = |cmd: &mut std::process::Command| {
            for k in ["CURL_HOME", "XDG_CONFIG_HOME", "HOME"] {
                cmd.env_remove(k);
            }
            cmd.env("APPDATA", &dir).env("USERPROFILE", &dir).args(["-sS", "--max-time", "5", "https://127.0.0.1:9/"]);
            String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned()
        };
        // Without -q the file is read: the check below would catch nothing otherwise.
        let mut plain = crate::process::hidden(system_tool("curl").unwrap());
        assert!(run(plain.args(HTTPS_ONLY)).contains("CURLRC_LOADED"), "the test's _curlrc is not where curl looks");
        assert!(!run(&mut curl(1024).unwrap()).contains("CURLRC_LOADED"));
        let first: Vec<_> = curl(1024).unwrap().get_args().map(|a| a.to_owned()).collect();
        assert_eq!(first[0], "-q", "-q only works as the first argument");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_system_curl_is_an_error_not_a_path_search() {
        let root = std::env::var_os("SystemRoot").unwrap();
        assert!(system_tool_in(Some(&root), "curl").unwrap().ends_with(r"System32\curl.exe"));
        let nowhere = std::env::temp_dir().join(format!("orangelink-noroot-{}", crate::model::new_id()));
        for root in [Some(nowhere.as_os_str()), None] {
            for tool in ["curl", "tar"] {
                assert!(system_tool_in(root, tool).unwrap_err().contains(tool));
            }
        }
    }

    #[test]
    fn downloads_are_limited_in_size_and_redirects() {
        let args: Vec<String> = curl(ZIP_MAX).unwrap().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(after("--max-filesize"), Some("209715200"));
        assert_eq!(after("--max-redirs"), Some("5"));
        assert_eq!(after("--proto"), Some("=https"));
        assert_eq!(after("--proto-redir"), Some("=https"));
    }

    #[test]
    fn cancel_waits_for_curl_to_exit() {
        let dir = std::env::temp_dir().join(format!("orangelink-cancel-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A child that holds a file open the way curl holds the zip it writes.
        let out = dir.join("held.txt");
        let file = std::fs::File::create(&out).unwrap();
        let mut child = crate::process::hidden("ping").args(["-n", "30", "127.0.0.1"]).stdout(file).spawn().unwrap();
        let cancel = AtomicBool::new(true);
        assert!(wait_or_cancel(&mut child, &cancel).unwrap_err().contains("取消"));
        // Gone, not merely told to go: the file it held can be deleted at once.
        assert!(child.try_wait().unwrap().is_some());
        std::fs::remove_dir_all(&dir).expect("the cancelled child still holds its file");
    }

    #[test]
    fn development_builds_are_never_replaced() {
        assert!(is_dev_build(Path::new(r"C:\src\OrangeLink\target\release\orangelink.exe")));
        assert!(is_dev_build(Path::new(r"C:\x\Target\debug\a.exe")));
        assert!(!is_dev_build(Path::new(r"D:\Tools\OrangeLink\OrangeLink.exe")));
        assert!(!is_dev_build(Path::new(r"D:\targets\OrangeLink.exe")));
    }

    #[test]
    fn a_failed_copy_puts_the_old_exe_back() {
        let dir = std::env::temp_dir().join(format!("orangelink-upd-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("OrangeLink.exe");
        std::fs::write(&target, b"old").unwrap();
        // The new exe is missing: the copy fails, the old one stays.
        assert!(replace(&dir.join("missing.exe"), &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        // A real new one takes its place; the old one waits as .old.
        let new = dir.join("new.exe");
        std::fs::write(&new, b"new").unwrap();
        replace(&new, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&target)).unwrap(), b"old");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
