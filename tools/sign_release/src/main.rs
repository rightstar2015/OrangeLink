//! Signs an OrangeLink release for the updater. Run from `tools/sign_release`:
//!
//!   cargo run --release --locked -- keygen [-o <key file>]
//!       Makes a new random release key and writes it straight into a key file
//!       encrypted with a password you type (by default
//!       `%USERPROFILE%\.orangelink\release-signing.key.enc`, never in the
//!       project; `-o` for the backup key's file). The key never touches the
//!       disk unencrypted. Prints the public key as one line for
//!       `release_keys.txt`.
//!   cargo run --release --locked -- sign <zip> <version> -o <latest.json> [--key <key file>]
//!       Checks that the zip's `OrangeLink/OrangeLink.exe` is that version, asks
//!       for the password and writes the signed latest.json (UTF-8, no BOM). The
//!       zip's file name is the release asset's name: sign it as it is uploaded.
//!   cargo run --release --locked -- pubkey [<key file>]
//!       Prints the public key in an encrypted key file, as release_keys.txt has it.
//!       It only reads the `public` line: use `check` to know a file works.
//!   cargo run --release --locked -- check <key file>
//!       Asks for the file's password, decrypts it, makes sure the key inside
//!       signs for the public key the file names (a damaged seed line fails),
//!       and prints that public key and whether release_keys.txt lists it.
//!       Use it on every backup before relying on it.
//!   cargo run --release --locked -- verify <latest.json> [<zip>]
//!       Checks a signed latest.json (and its zip) exactly as the app will read
//!       it, against release_keys.txt (the list the app builds in). Needs no key or
//!       password, and does not build the app.
//!
//! The signature covers `OrangeLink <version> <sha256>`, so a zip can't be passed
//! off as another version. The key file is encrypted with a key derived from the
//! password (PBKDF2-HMAC-SHA256, random salt); the password is asked for each
//! time and never stored. Keep a copy of the key file and remember the password:
//! without both, no further update can be installed automatically.
//!
//! The tool depends on ed25519-compact and sha2 only; with their own
//! dependencies that is 12 small crates on Windows (two with build scripts,
//! generic-array and getrandom; no proc-macros), not the app's ~600. Build and
//! run it in a Windows account kept for signing, with its own CARGO_HOME
//! (docs/RELEASING.md, "在獨立的簽章帳號簽章"): what is built there can reach the key and password.

mod latest;

use ed25519_compact::{KeyPair, PublicKey, Seed};
use latest::{is_asset_name, signed_message};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const HEADER: &str = "orangelink-release-key 2";
/// PBKDF2 rounds for a new key file (OWASP's 2023 figure for PBKDF2-HMAC-SHA256).
const ROUNDS: u32 = 600_000;
/// Fewest rounds a key file may name: a file can't talk the tool into a weak key.
const MIN_ROUNDS: u32 = 100_000;
/// Most rounds a key file may name: a damaged or planted file can't make the
/// tool spin for hours.
const MAX_ROUNDS: u32 = 10_000_000;
/// What the app's executable carries (`VERSION_MARKER` in src/self_update.rs).
const VERSION_MARKER: &[u8] = b"OrangeLink-release-version=";

type Result<T> = std::result::Result<T, String>;

/// The app's list of release public keys, the one the app builds in.
const RELEASE_KEYS_TXT: &str = include_str!("../../../release_keys.txt");

/// The app's parser for that list, the same code.
#[path = "../../../src/release_keys.rs"]
mod release_keys;

/// The keys every copy of the app trusts.
fn app_keys() -> Result<Vec<[u8; 32]>> {
    release_keys::parse(RELEASE_KEYS_TXT)
}

fn key_dir() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE").ok_or("USERPROFILE is not set")?;
    Ok(PathBuf::from(home).join(".orangelink"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `text` as bytes, checked to be nothing but hex digits (of even length) before
/// any of it is sliced, so a damaged key file gives an error, not a panic that
/// would print its contents.
fn from_hex(text: &str) -> Result<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("not hex".into());
    }
    Ok((0..text.len() / 2).map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect())
}

fn hex32(text: &str) -> Result<[u8; 32]> {
    from_hex(text)?.try_into().map_err(|_| "not 32 bytes".to_string())
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// PBKDF2-HMAC-SHA256 (RFC 8018), one 32-byte block.
fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut u = hmac_sha256(password, &[salt, &1u32.to_be_bytes()]);
    let mut out = u;
    for _ in 1..rounds {
        u = hmac_sha256(password, &[&u]);
        for (o, x) in out.iter_mut().zip(u) {
            *o ^= x;
        }
    }
    out
}

/// The key file: the seed XORed with a key derived from the password and a
/// fresh random salt (one secret, encrypted once, so the derived key is a
/// one-time pad), and the public key to tell a wrong password from a right one.
fn encrypt(seed: &[u8; 32], password: &str, salt: [u8; 32], rounds: u32) -> String {
    let pad = pbkdf2(password.as_bytes(), &salt, rounds);
    let sealed: Vec<u8> = seed.iter().zip(pad).map(|(s, p)| s ^ p).collect();
    let pk = KeyPair::from_seed(Seed::new(*seed)).pk;
    format!("{HEADER}\nrounds {rounds}\nsalt {}\nseed {}\npublic {}\n", hex(&salt), hex(&sealed), hex(pk.as_ref()))
}

struct KeyFile {
    rounds: u32,
    salt: [u8; 32],
    sealed: [u8; 32],
    public: [u8; 32],
}

fn parse(text: &str) -> Result<KeyFile> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if lines.next() != Some(HEADER) {
        return Err("not an encrypted release key file".into());
    }
    let mut field = |name: &str| -> Result<String> {
        let line = lines.next().ok_or(format!("key file: {name} is missing"))?;
        line.strip_prefix(name).and_then(|v| v.strip_prefix(' ')).map(str::to_string).ok_or(format!("key file: expected {name}"))
    };
    let rounds: u32 = field("rounds")?.parse().map_err(|_| "key file: rounds is not a number")?;
    if rounds < MIN_ROUNDS {
        return Err(format!("key file: {rounds} rounds is too few"));
    }
    if rounds > MAX_ROUNDS {
        return Err(format!("key file: {rounds} rounds is more than {MAX_ROUNDS}"));
    }
    let salt = hex32(&field("salt")?).map_err(|e| format!("key file: salt is {e}"))?;
    let sealed = hex32(&field("seed")?).map_err(|e| format!("key file: seed is {e}"))?;
    let public = hex32(&field("public")?).map_err(|e| format!("key file: public is {e}"))?;
    if lines.next().is_some() {
        return Err("key file: unexpected lines after public".into());
    }
    Ok(KeyFile { rounds, salt, sealed, public })
}

fn decrypt(text: &str, password: &str) -> Result<KeyPair> {
    let f = parse(text)?;
    let pad = pbkdf2(password.as_bytes(), &f.salt, f.rounds);
    let mut seed = [0u8; 32];
    for (i, s) in seed.iter_mut().enumerate() {
        *s = f.sealed[i] ^ pad[i];
    }
    let kp = KeyPair::from_seed(Seed::new(seed));
    if kp.pk.as_ref() != f.public.as_slice() {
        // The two can't be told apart: either way the key that comes out is not
        // the one the file names.
        return Err("wrong password, or the key file's salt, seed or public line is damaged".into());
    }
    Ok(kp)
}

/// `check`: a key file really opens with `password`: it decrypts, the key that
/// comes out has the public key the file names, and it signs a test message
/// that public key verifies. Returns that public key and whether the app
/// trusts it (it is in `keys`). Unlike `pubkey`, a damaged seed line fails here.
fn check_key(text: &str, password: &str, keys: &[[u8; 32]]) -> Result<([u8; 32], bool)> {
    let f = parse(text)?;
    let kp = decrypt(text, password)?;
    let message = b"OrangeLink key file check";
    let sig = kp.sk.sign(message, None);
    PublicKey::new(f.public).verify(message, &sig).map_err(|_| "the key file's key does not sign for its public key")?;
    Ok((f.public, keys.contains(&f.public)))
}

/// A version as the app reads it: three numbers, no `v`, no suffix.
fn plain_version(v: &str) -> Result<String> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let ok = v.split('.').count() == 3 && v.split('.').all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| v.to_string()).ok_or(format!("version {v:?} is not x.y.z"))
}

/// The version an OrangeLink exe was built as: every marker in it must agree.
fn exe_version(exe: &[u8]) -> Result<String> {
    let mut found: Option<String> = None;
    let mut rest = exe;
    while let Some(at) = rest.windows(VERSION_MARKER.len()).position(|w| w == VERSION_MARKER) {
        let tail = &rest[at + VERSION_MARKER.len()..];
        let end = tail.iter().take(40).position(|&b| b == b';').ok_or("the version marker in the exe has no end")?;
        let v = std::str::from_utf8(&tail[..end]).map_err(|_| "the version marker in the exe is not text")?.to_string();
        if found.as_ref().is_some_and(|f| *f != v) {
            return Err(format!("the exe names two versions, {} and {v}", found.unwrap_or_default()));
        }
        found = Some(v);
        rest = &tail[end..];
    }
    found.ok_or_else(|| "the exe has no version marker (built before 0.1.6, or not OrangeLink)".to_string())
}

/// `OrangeLink/OrangeLink.exe` out of the zip, unpacked with Windows' tar.
fn exe_in_zip(zip: &Path) -> Result<Vec<u8>> {
    let tar = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("tar.exe"))
        .filter(|p| p.is_file())
        .ok_or("Windows' tar.exe (System32) is missing")?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("orangelink-sign-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp folder: {e}"))?;
    #[allow(clippy::disallowed_methods)] // a console tool: no hidden window to keep
    let status = std::process::Command::new(tar).arg("-xf").arg(zip).arg("-C").arg(&dir).status();
    let exe = match status {
        Ok(s) if s.success() => std::fs::read(dir.join("OrangeLink").join("OrangeLink.exe")).map_err(|e| format!("OrangeLink/OrangeLink.exe in the zip: {e}")),
        Ok(s) => Err(format!("tar could not unpack the zip ({s})")),
        Err(e) => Err(format!("tar: {e}")),
    };
    let _ = std::fs::remove_dir_all(&dir);
    exe
}

/// The signed latest.json for `zip`, after checking that its exe is `version`.
fn sign(zip: &Path, version: &str, kp: &KeyPair, keys: &[[u8; 32]]) -> Result<String> {
    not_listed(kp.pk.as_ref(), keys)?;
    let version = plain_version(version)?;
    let asset = zip.file_name().and_then(|n| n.to_str()).filter(|n| is_asset_name(n)).ok_or("the zip's file name is not a valid release asset name")?;
    let built = exe_version(&exe_in_zip(zip)?)?;
    if built != version {
        return Err(format!("the zip's OrangeLink.exe is version {built}, not {version}"));
    }
    let bytes = std::fs::read(zip).map_err(|e| format!("read zip: {e}"))?;
    let sha = hex(&Sha256::digest(&bytes));
    let message = signed_message(&version, &sha);
    let sig = kp.sk.sign(&message, None);
    kp.pk.verify(&message, &sig).map_err(|_| "the signature does not check")?;
    // Every value is checked above to need no escaping in JSON.
    let latest = format!("{{\n  \"version\": \"{version}\",\n  \"asset\": \"{asset}\",\n  \"sha256\": \"{sha}\",\n  \"sig\": \"{}\",\n  \"notes\": \"\"\n}}\n", base64(sig.as_ref()));
    // What is written must read back the way the app reads it, with its keys.
    verify(latest.as_bytes(), Some(zip), keys)?;
    Ok(latest)
}

/// An error, and a warning on the console, when `pk` is not one of the app's
/// release_keys.txt: no copy of OrangeLink would install what it signs (the wrong
/// key file, say, in the middle of a rotation).
fn not_listed(pk: &[u8], keys: &[[u8; 32]]) -> Result<()> {
    if keys.iter().any(|k| k.as_slice() == pk) {
        return Ok(());
    }
    eprintln!("WARNING: this release key is not in release_keys.txt; nothing is written.");
    Err(format!("the key {} is not in release_keys.txt", release_keys::hex(pk)))
}

/// Checks a signed latest.json, and the zip it names when given, by the app's
/// rules: the same text and JSON rules, all three of asset, sha256 and sig
/// present (or the app offers nothing), a signature by one of `keys`, the
/// zip's name and SHA-256, and the version of the exe inside it.
fn verify(latest: &[u8], zip: Option<&Path>, keys: &[[u8; 32]]) -> Result<String> {
    let text = latest::latest_text(latest)?;
    let r = latest::release_in_latest(text).ok_or("latest.json has no version the app reads (not a JSON object, or version is not x.y.z)")?;
    let asset = r.asset.ok_or("asset is missing or not a valid zip name")?;
    let sha256 = r.sha256.ok_or("sha256 is missing or not 64 hex digits")?;
    let sig = r.sig.ok_or("sig is missing (not signed) or too long")?;
    if !latest::signature_ok(keys, &r.tag, &sha256, &sig) {
        return Err(format!("the signature on {} does not check with the keys in release_keys.txt", r.tag));
    }
    if let Some(zip) = zip {
        let name = zip.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name != asset {
            return Err(format!("the zip is named {name}, latest.json names {asset}"));
        }
        let bytes = std::fs::read(zip).map_err(|e| format!("read zip: {e}"))?;
        let got = hex(&Sha256::digest(&bytes));
        if !got.eq_ignore_ascii_case(&sha256) {
            return Err(format!("the zip's SHA-256 is {got}, latest.json says {sha256}"));
        }
        let built = exe_version(&exe_in_zip(zip)?)?;
        let version = r.tag.trim().trim_start_matches('v');
        if built != version {
            return Err(format!("the zip's OrangeLink.exe is version {built}, latest.json says {version}"));
        }
    }
    Ok(format!("{} {asset}: signature good{}", r.tag, if zip.is_some() { ", zip matches" } else { "" }))
}

/// Writes `text` to `path` through a temporary file, as UTF-8 without a BOM.
/// The temporary file is flushed to the disk (`sync_all`) before the rename,
/// so a crash or a pulled USB stick leaves the old file or the whole new one.
fn write_file(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("write {}: {e}", tmp.display()));
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Where `sign -o` may write latest.json: never the key file it signs with
/// (compared after `canonicalize`, so `.` or `..` in the path, another case or
/// a short name can't hide it), and never over an existing file that is not
/// `.json`.
fn check_output(out: &Path, key: &Path) -> Result<()> {
    if !out.exists() {
        return Ok(());
    }
    let same = match (std::fs::canonicalize(out), std::fs::canonicalize(key)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Err(format!("-o {} is the key file; refusing to write latest.json over it", out.display()));
    }
    let is_json = out.extension().is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if !is_json {
        return Err(format!("-o {} exists and is not a .json file; refusing to write over it", out.display()));
    }
    Ok(())
}

/// A key file: created with `create_new`, so an existing file (even one made a
/// moment ago by something else) is reported and never replaced; flushed with
/// `sync_all` and read back before it counts as written. A failed write
/// removes only the file it created.
fn write_new_file(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("{} already exists; it is never overwritten", path.display()));
        }
        Err(e) => return Err(format!("create {}: {e}", path.display())),
    };
    let written = file.write_all(text.as_bytes()).and_then(|()| file.sync_all());
    drop(file);
    let read_back = written.and_then(|()| std::fs::read(path));
    match read_back {
        Ok(bytes) if bytes == text.as_bytes() => Ok(()),
        Ok(_) => {
            let _ = std::fs::remove_file(path);
            Err(format!("{} did not read back as written; it was removed", path.display()))
        }
        Err(e) => {
            let _ = std::fs::remove_file(path);
            Err(format!("write {}: {e}; the partial file was removed", path.display()))
        }
    }
}

#[cfg(windows)]
mod console {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> isize;
        fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const ENABLE_ECHO_INPUT: u32 = 0x4;

    /// Turns typing echo off until the guard is dropped. `None` when input is
    /// not a console: the password is only ever typed, never piped in.
    pub struct NoEcho(isize, u32);

    pub fn no_echo() -> Option<NoEcho> {
        // SAFETY: plain Win32 calls on this process's own stdin handle.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(h, &mut mode) == 0 || SetConsoleMode(h, mode & !ENABLE_ECHO_INPUT) == 0 {
                return None;
            }
            Some(NoEcho(h, mode))
        }
    }

    impl Drop for NoEcho {
        fn drop(&mut self) {
            // SAFETY: restores the mode read in `no_echo`.
            unsafe {
                SetConsoleMode(self.0, self.1);
            }
        }
    }
}

fn ask_password(prompt: &str) -> Result<String> {
    use std::io::Write;
    eprint!("{prompt}: ");
    let _ = std::io::stderr().flush();
    #[cfg(windows)]
    let _guard = console::no_echo().ok_or("type the password in a console window (input is not a console)")?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    eprintln!();
    let pw = line.trim_end_matches(['\r', '\n']).to_string();
    if pw.is_empty() {
        return Err("empty password".into());
    }
    Ok(pw)
}

fn new_password() -> Result<String> {
    let pw = ask_password("new password for the release key (use a random one from a password manager, 20+ characters)")?;
    if pw.chars().count() < 12 {
        return Err("the password is shorter than 12 characters".into());
    }
    if ask_password("type it again")? != pw {
        return Err("the two passwords differ".into());
    }
    Ok(pw)
}

/// Writes a new encrypted key file, never over an existing one.
/// The key file `keygen`, `pubkey` and `sign` use unless told another.
fn default_key_file() -> Result<PathBuf> {
    Ok(key_dir()?.join("release-signing.key.enc"))
}

/// Writes `seed`, encrypted with `password`, as a new key file at `path`, and
/// returns the public key. Only the encrypted text is ever written (a temporary
/// file holding it, then the rename); an existing file is never overwritten.
fn save_new(seed: &[u8; 32], password: &str, path: &Path) -> Result<[u8; 32]> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = encrypt(seed, password, *Seed::generate(), ROUNDS);
    let kp = decrypt(&text, password)?;
    write_new_file(path, &text)?;
    Ok(*kp.pk)
}

/// `keygen`: a new random key, straight into an encrypted key file.
fn keygen(path: &Path, password: &str) -> Result<[u8; 32]> {
    save_new(&Seed::generate(), password, path)
}

fn run(args: &[&str]) -> Result<()> {
    match args {
        ["keygen"] | ["keygen", "-o", _] => {
            let path = match args.get(2) {
                Some(p) => PathBuf::from(p),
                None => default_key_file()?,
            };
            if path.exists() {
                return Err(format!("{} already exists; it is never overwritten", path.display()));
            }
            let pw = new_password()?;
            let pk = keygen(&path, &pw)?;
            println!("encrypted key file: {}", path.display());
            println!("public key, one line for release_keys.txt:");
            println!("{}", release_keys::hex(&pk));
            println!("Back it up in at least two places before it is used (docs/RELEASING.md, \"金鑰的保管\").");
        }
        ["check", file] => {
            let path = PathBuf::from(file);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let pw = ask_password("password for this key file")?;
            let (pk, listed) = check_key(&text, &pw, &app_keys()?)?;
            println!("OK: {} opens with this password and signs for its public key", path.display());
            println!("public key: {}", release_keys::hex(&pk));
            if listed {
                println!("in release_keys.txt: yes");
            } else {
                println!("in release_keys.txt: NO (no copy of OrangeLink trusts this key)");
            }
        }
        ["pubkey"] | ["pubkey", _] => {
            let path = match args.get(1) {
                Some(p) => PathBuf::from(p),
                None => default_key_file()?,
            };
            let f = parse(&std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?)?;
            println!("{}", release_keys::hex(PublicKey::new(f.public).as_ref()));
        }
        ["sign", zip, version, "-o", out] | ["sign", zip, version, "-o", out, "--key", _] => {
            let path = match args.get(6) {
                Some(p) => PathBuf::from(p),
                None => default_key_file()?,
            };
            let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} (make one with `keygen`)", path.display()))?;
            // Check what can be checked before asking for the password.
            check_output(Path::new(out), &path)?;
            plain_version(version)?;
            let keys = app_keys()?;
            not_listed(&parse(&text)?.public, &keys)?;
            let pw = ask_password("release key password")?;
            let kp = decrypt(&text, &pw)?;
            let latest = sign(Path::new(zip), version, &kp, &keys)?;
            check_output(Path::new(out), &path)?;
            write_file(Path::new(out), &latest)?;
            println!("wrote {out}");
        }
        ["verify", latest] | ["verify", latest, _] => {
            let zip = args.get(2).map(Path::new);
            let bytes = std::fs::read(latest).map_err(|e| format!("{latest}: {e}"))?;
            println!("OK: {}", verify(&bytes, zip, &app_keys()?)?);
        }
        _ => return Err("usage: sign_release keygen [-o <key file>] | pubkey [<key file>] | sign <zip> <version> -o <latest.json> [--key <key file>] | verify <latest.json> [<zip>] | check <key file>".into()),
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Err(e) = run(&args) {
        eprintln!("sign_release: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_the_published_vectors() {
        // RFC 7914 section 11 (PBKDF2-HMAC-SHA256), first 32 bytes.
        assert_eq!(hex(&pbkdf2(b"passwd", b"salt", 1)), "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc");
        assert_eq!(hex(&pbkdf2(b"Password", b"NaCl", 80000)), "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56");
    }

    #[test]
    fn base64_matches_the_published_vectors() {
        // RFC 4648 section 10.
        for (raw, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
        assert_eq!(base64(&[0u8; 64]).len(), 88, "a signature fits the app's 128-character limit");
    }

    #[test]
    fn the_key_file_opens_only_with_its_password() {
        let seed = [7u8; 32];
        let text = encrypt(&seed, "correct horse battery", [1; 32], MIN_ROUNDS);
        assert!(!text.contains(&hex(&seed)), "the seed is not in the file");
        assert_eq!(decrypt(&text, "correct horse battery").unwrap().sk.seed().as_ref(), &seed);
        assert!(decrypt(&text, "wrong").err().unwrap_or_default().starts_with("wrong password"));
        // Another salt, another file for the same seed and password.
        assert_ne!(encrypt(&seed, "correct horse battery", [2; 32], MIN_ROUNDS), text);
    }

    #[test]
    fn a_damaged_key_file_is_an_error_not_a_panic() {
        let good = encrypt(&[7u8; 32], "pw", [1; 32], MIN_ROUNDS);
        let bad = [
            String::new(),
            "7".repeat(64),
            good.replace("rounds 100000", "rounds 10"),
            good.replace("salt 01", "salt 0g"),
            good.replace("salt 01", "salt é"),
            good.replace("seed ", "seed x"),
            good.lines().take(3).collect::<Vec<_>>().join("\n"),
        ];
        for text in bad {
            assert!(decrypt(&text, "pw").is_err(), "{text:?}");
        }
        for text in ["é".repeat(32), "zz".repeat(32), "7".repeat(63), "7".repeat(66)] {
            assert!(hex32(&text).is_err(), "{text}");
        }
    }

    #[test]
    fn reads_the_version_built_into_the_exe() {
        let exe = |s: &str| [b"MZ\0\0junk".as_slice(), s.as_bytes(), b"\0more"].concat();
        assert_eq!(exe_version(&exe("OrangeLink-release-version=0.1.6;")).unwrap(), "0.1.6");
        assert_eq!(exe_version(&exe("OrangeLink-release-version=0.1.6;..OrangeLink-release-version=0.1.6;")).unwrap(), "0.1.6");
        assert!(exe_version(&exe("OrangeLink-release-version=0.1.6;..OrangeLink-release-version=0.1.7;")).is_err());
        assert!(exe_version(&exe("no marker")).is_err());
        assert!(exe_version(&exe("OrangeLink-release-version=0.1.6")).is_err(), "no end");
    }

    #[test]
    fn sign_release_verify_reads_latest_json_like_the_updater() {
        let dir = std::env::temp_dir().join(format!("sign-release-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let kp = KeyPair::from_seed(Seed::new([7; 32]));
        let pk: [u8; 32] = *kp.pk;
        let zip = make_zip(&dir, "OrangeLink-v0.2.0-windows-x64.zip", b"MZ OrangeLink-release-version=0.2.0; rest");
        let json = sign(&zip, "0.2.0", &kp, &[pk]).unwrap();
        assert!(verify(json.as_bytes(), Some(&zip), &[pk]).unwrap().contains("zip matches"));
        assert!(verify(json.as_bytes(), None, &[[1; 32], pk]).is_ok(), "any listed key");
        let bom = [b"\xEF\xBB\xBF".as_slice(), json.as_bytes()].concat();
        assert!(verify(&bom, Some(&zip), &[pk]).is_ok(), "a BOM, as the app reads it");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(json.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert!(verify(&utf16, None, &[pk]).unwrap_err().contains("UTF-16"));
        assert!(verify(json.as_bytes(), None, &[[1; 32]]).unwrap_err().contains("signature"), "another key");
        let unsigned: String = json.lines().filter(|l| !l.contains("\"sig\"")).collect::<Vec<_>>().join("\n");
        assert!(verify(unsigned.as_bytes(), None, &[pk]).unwrap_err().contains("sig"));
        let other_version = json.replace("\"version\": \"0.2.0\"", "\"version\": \"0.2.1\"");
        assert!(verify(other_version.as_bytes(), None, &[pk]).unwrap_err().contains("signature"));
        let renamed = dir.join("OrangeLink.zip");
        std::fs::copy(&zip, &renamed).unwrap();
        assert!(verify(json.as_bytes(), Some(&renamed), &[pk]).unwrap_err().contains("named"));
        std::fs::write(&zip, b"PK another zip").unwrap();
        assert!(verify(json.as_bytes(), Some(&zip), &[pk]).unwrap_err().contains("SHA-256"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sign_refuses_a_key_not_in_release_keys() {
        let dir = std::env::temp_dir().join(format!("sign-release-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip = make_zip(&dir, "OrangeLink-v0.2.0-windows-x64.zip", b"MZ OrangeLink-release-version=0.2.0; rest");
        let kp = KeyPair::from_seed(Seed::new([7; 32]));
        let others = [[1u8; 32], [2u8; 32]];
        assert!(sign(&zip, "0.2.0", &kp, &others).unwrap_err().contains("release_keys.txt"));
        assert!(not_listed(kp.pk.as_ref(), &others).is_err());
        assert!(not_listed(&others[1], &others).is_ok());
        assert!(sign(&zip, "0.2.0", &kp, &[]).is_err(), "an empty list signs nothing");
        // The app's real list never holds the test key.
        let listed = app_keys().expect("release_keys.txt");
        assert!(listed.len() >= 2 && !listed.contains(&*kp.pk));
        let pk: [u8; 32] = *kp.pk;
        assert!(sign(&zip, "0.2.0", &kp, &[[1; 32], pk]).is_ok(), "listed beside another key");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keygen_writes_only_an_encrypted_key_file() {
        let dir = std::env::temp_dir().join(format!("sign-release-keygen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("backup").join("release-backup.key.enc");
        let pk = keygen(&path, "correct horse battery staple").unwrap();
        // One file in the folder: no plain key, no temporary file left behind.
        let files: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(files, vec![std::ffi::OsString::from("release-backup.key.enc")]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(HEADER));
        let kp = decrypt(&text, "correct horse battery staple").unwrap();
        assert_eq!(*kp.pk, pk);
        assert!(!text.contains(&hex(kp.sk.seed().as_ref())), "the seed is not in the file");
        assert!(text.contains(&release_keys::hex(&pk)), "the public key is, for pubkey");
        // The printed line is what release_keys.txt reads.
        assert_eq!(release_keys::parse(&release_keys::hex(&pk)).unwrap(), vec![pk]);
        // Never over an existing file, and every key is new.
        assert!(keygen(&path, "another password").unwrap_err().contains("already exists"));
        assert_eq!(decrypt(&std::fs::read_to_string(&path).unwrap(), "correct horse battery staple").map(|k| *k.pk).ok(), Some(pk));
        let other = keygen(&dir.join("primary.key.enc"), "correct horse battery staple").unwrap();
        assert_ne!(other, pk);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_reports_a_damaged_seed_line() {
        let seed = [7u8; 32];
        let pk: [u8; 32] = *KeyPair::from_seed(Seed::new(seed)).pk;
        let text = encrypt(&seed, "pw", [1; 32], MIN_ROUNDS);
        assert_eq!(check_key(&text, "pw", &[[1; 32], pk]).unwrap(), (pk, true));
        assert_eq!(check_key(&text, "pw", &[[1; 32]]).unwrap(), (pk, false), "not trusted, said so");
        assert!(check_key(&text, "not it", &[pk]).unwrap_err().starts_with("wrong password"));
        // One digit of the seed line changed: pubkey would still print the
        // public key, check does not pass.
        let seed_line = text.lines().find(|l| l.starts_with("seed ")).unwrap();
        let mut damaged_line = seed_line.to_string();
        let last = damaged_line.pop().unwrap();
        damaged_line.push(if last == '0' { '1' } else { '0' });
        let damaged = text.replace(seed_line, &damaged_line);
        assert_eq!(parse(&damaged).unwrap().public, pk, "the public line is untouched");
        assert!(check_key(&damaged, "pw", &[pk]).unwrap_err().contains("damaged"));
        // A seed line that is not hex, short, or missing.
        let not_hex = text.replace(seed_line, &format!("seed {}", "z".repeat(64)));
        let short = text.replace(seed_line, &seed_line[..seed_line.len() - 2]);
        let missing = text.replace(&format!("{seed_line}\n"), "");
        for bad in [not_hex, short, missing] {
            assert!(check_key(&bad, "pw", &[pk]).is_err(), "{bad}");
        }
    }

    #[test]
    fn sign_refuses_to_write_over_the_key_file() {
        let dir = std::env::temp_dir().join(format!("sign-release-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let key = dir.join("Release.key.enc");
        std::fs::write(&key, "key").unwrap();
        for out in [key.clone(), dir.join("sub").join("..").join("Release.key.enc"), dir.join("RELEASE.KEY.ENC"), dir.join(".").join("release.key.enc")] {
            assert!(check_output(&out, &key).unwrap_err().contains("is the key file"), "{}", out.display());
        }
        // Not the key, but an existing file that is not .json.
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, "x").unwrap();
        assert!(check_output(&notes, &key).unwrap_err().contains("not a .json"));
        let other_key = dir.join("backup.key.enc");
        std::fs::write(&other_key, "other").unwrap();
        assert!(check_output(&other_key, &key).unwrap_err().contains("not a .json"));
        // An existing latest.json (any case) or a new file is fine.
        let latest = dir.join("latest.JSON");
        std::fs::write(&latest, "{}").unwrap();
        assert!(check_output(&latest, &key).is_ok());
        assert!(check_output(&dir.join("new-latest.json"), &key).is_ok());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "key");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keygen_refuses_to_replace_a_file_created_meanwhile() {
        let dir = std::env::temp_dir().join(format!("sign-release-create-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("release.key.enc");
        // Something else made the file between the check in `run` and the write:
        // the write itself refuses, and the file is left as it was.
        std::fs::write(&path, b"someone else's file").unwrap();
        assert!(write_new_file(&path, "key").unwrap_err().contains("already exists"));
        assert!(keygen(&path, "pw").unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read(&path).unwrap(), b"someone else's file");
        // A new file is written whole, with nothing left beside it.
        let fresh = dir.join("fresh.key.enc");
        write_new_file(&fresh, "key text").unwrap();
        assert_eq!(std::fs::read_to_string(&fresh).unwrap(), "key text");
        // latest.json is replaced through a flushed temporary file.
        let out = dir.join("latest.json");
        write_file(&out, "one").unwrap();
        write_file(&out, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "two");
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["fresh.key.enc", "latest.json", "release.key.enc"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_file_with_extra_lines_or_huge_rounds_is_rejected() {
        let good = encrypt(&[7u8; 32], "pw", [1; 32], MIN_ROUNDS);
        assert!(parse(&good).is_ok());
        assert!(parse(&format!("{good}\n\n")).is_ok(), "blank lines are fine");
        assert!(parse(&format!("{good}extra line\n")).err().unwrap_or_default().contains("unexpected"));
        assert!(parse(&format!("{good}public {}\n", "00".repeat(32))).err().unwrap_or_default().contains("unexpected"), "a second public key");
        assert!(parse(&good.replace("rounds 100000", "rounds 10000001")).err().unwrap_or_default().contains("more than"));
        assert!(parse(&good.replace("rounds 100000", "rounds 4294967295")).err().unwrap_or_default().contains("more than"));
        assert!(parse(&good.replace("rounds 100000", "rounds 10000000")).is_ok(), "the limit itself");
    }

    #[test]
    fn versions_and_names_follow_the_apps_rules() {
        assert_eq!(plain_version("v0.1.6").unwrap(), "0.1.6");
        for bad in ["0.1", "0.1.6-beta", "1.2.+3", "v", "", "0.1.6 ", "１.2.3"] {
            assert!(plain_version(bad).is_err(), "{bad}");
        }
        assert!(is_asset_name("OrangeLink-v0.1.6-windows-x64.zip"));
        assert!(!is_asset_name("../x.zip") && !is_asset_name("x.exe"));
        assert_eq!(signed_message("0.1.6", "ab"), "OrangeLink 0.1.6 ab");
    }

    /// A zip made with Windows' tar holding `OrangeLink/OrangeLink.exe` = `exe`.
    fn make_zip(dir: &Path, name: &str, exe: &[u8]) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("OrangeLink")).unwrap();
        std::fs::write(src.join("OrangeLink").join("OrangeLink.exe"), exe).unwrap();
        let zip = dir.join(name);
        let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("tar.exe");
        #[allow(clippy::disallowed_methods)] // test helper
        let ok = std::process::Command::new(tar).arg("-a").arg("-cf").arg(&zip).arg("-C").arg(&src).arg("OrangeLink").status().unwrap().success();
        assert!(ok);
        std::fs::remove_dir_all(&src).unwrap();
        zip
    }

    #[test]
    fn signs_only_a_zip_whose_exe_is_that_version() {
        let dir = std::env::temp_dir().join(format!("sign-release-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let kp = KeyPair::from_seed(Seed::new([7; 32]));
        let zip = make_zip(&dir, "OrangeLink-v0.2.0-windows-x64.zip", b"MZ OrangeLink-release-version=0.2.0; rest");
        let pk: [u8; 32] = *kp.pk;
        let latest = sign(&zip, "v0.2.0", &kp, &[pk]).unwrap();
        let sha = hex(&Sha256::digest(std::fs::read(&zip).unwrap()));
        assert!(latest.starts_with("{\n  \"version\": \"0.2.0\",\n  \"asset\": \"OrangeLink-v0.2.0-windows-x64.zip\""), "{latest}");
        assert!(latest.contains(&format!("\"sha256\": \"{sha}\"")));
        let sig = latest.split("\"sig\": \"").nth(1).unwrap().split('"').next().unwrap();
        assert_eq!(sig, base64(kp.sk.sign(signed_message("0.2.0", &sha), None).as_ref()));
        assert!(sign(&zip, "0.2.1", &kp, &[pk]).unwrap_err().contains("0.2.0"), "another version");
        let other = make_zip(&dir, "OrangeLink-v0.2.1-windows-x64.zip", b"MZ no marker");
        assert!(sign(&other, "0.2.1", &kp, &[pk]).unwrap_err().contains("marker"));
        let renamed = dir.join("OrangeLink-v0.2.0.ZIP.exe");
        std::fs::copy(&zip, &renamed).unwrap();
        assert!(sign(&renamed, "0.2.0", &kp, &[pk]).unwrap_err().contains("asset name"));
        // Written as UTF-8 without a BOM.
        let out = dir.join("latest.json");
        write_file(&out, &latest).unwrap();
        assert_eq!(std::fs::read(&out).unwrap()[0], b'{');
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
