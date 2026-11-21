//! Windows' curl, and the install step of the old self-update.
//!
//! Since 0.1.8 OrangeLink only tells the user about a new release
//! (`update.rs`); it no longer downloads or installs one. Copies 0.1.6 and 0.1.7
//! downloaded a signed zip into `%LOCALAPPDATA%\OrangeLink\update` and, when they
//! closed, started the new exe with `--apply-update <target>`, which waits for
//! the old one to exit, renames it to `.old`, copies itself into its place and,
//! when asked, starts it again. Releases are no longer signed, so those copies
//! no longer update themselves; `apply` stays for a download one of them made
//! before, and `clean_up` removes what it left. An exe under a `target` folder
//! is a development build and is never replaced.
//!
//! The flow follows Prime Files (`crates/nohrs-pages/src/updater.rs`), MIT License,
//! Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long `--apply-update` waits for the old OrangeLink to exit.
const WAIT_FOR_EXIT: Duration = Duration::from_secs(60);

/// A development build: the exe sits under a `target` folder.
pub fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str().eq_ignore_ascii_case("target"))
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
    let Ok(exe) = std::env::current_exe() else { return };
    let _ = std::fs::remove_file(old_path(&exe));
    if let Some(root) = staging_root().filter(|r| !exe.starts_with(r)) {
        let _ = std::fs::remove_dir_all(root);
    }
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

/// Windows' curl, ready for a download of at most `max_bytes`: `-q` (which only
/// works as the first argument) so no `_curlrc` in the user's folders can add
/// `--insecure`, a proxy or an output format; HTTPS only, also on redirects; at
/// most 5 redirects.
pub fn curl(max_bytes: u64) -> Result<std::process::Command, String> {
    let mut cmd = crate::process::hidden(system_tool("curl")?);
    cmd.arg("-q").args(HTTPS_ONLY).args(["--max-redirs", "5", "--max-filesize"]).arg(max_bytes.to_string());
    Ok(cmd)
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
        let args: Vec<String> = curl(200 * 1024 * 1024).unwrap().get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        let after = |flag: &str| args.iter().position(|a| a == flag).map(|i| args[i + 1].as_str());
        assert_eq!(after("--max-filesize"), Some("209715200"));
        assert_eq!(after("--max-redirs"), Some("5"));
        assert_eq!(after("--proto"), Some("=https"));
        assert_eq!(after("--proto-redir"), Some("=https"));
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
