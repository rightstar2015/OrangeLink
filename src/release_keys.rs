//! The release public keys, read from `release_keys.txt` at the repository's
//! root: one Ed25519 public key per line as 64 hex digits, `#` starting a
//! comment. The app builds the file in with `include_str!`; `tools/sign_release`
//! reads the same file through this same module (`#[path]`), so the signer and
//! the app can never trust different keys.
//!
//! std only: the signing tool has no other dependencies to share.

/// Encodings of the eight Ed25519 points of small order (orders 1, 2, 4 and
/// 8), the canonical ones and those with y >= p or the sign bit set on x = 0
/// that lax decoders accept. A signature "by" one of them proves nothing, so
/// none is ever a release key. (Each was checked to be on the curve with
/// 8P = identity.)
const LOW_ORDER: [&str; 14] = [
    "0100000000000000000000000000000000000000000000000000000000000000",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac037a",
    "0000000000000000000000000000000000000000000000000000000000000080",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc85",
    "0000000000000000000000000000000000000000000000000000000000000000",
    "c7176a703d4dd84fba3c0b760d10670f2a2053fa2c39ccc64ec7fd7792ac03fa",
    "0100000000000000000000000000000000000000000000000000000000000080",
    "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f",
    "eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
];

/// The keys in `text`, in file order. An empty list, a line that is not one
/// key, a low-order point (all zeros among them), or the same key twice is an
/// error naming the line.
pub fn parse(text: &str) -> Result<Vec<[u8; 32]>, String> {
    let mut keys: Vec<[u8; 32]> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let key = line.split('#').next().unwrap_or_default().trim();
        if key.is_empty() {
            continue;
        }
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("release_keys.txt line {n}: not a 64-hex-digit public key"));
        }
        let mut bytes = [0u8; 32];
        for (j, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&key[j * 2..j * 2 + 2], 16).map_err(|_| format!("release_keys.txt line {n}: not hex"))?;
        }
        if LOW_ORDER.contains(&hex(&bytes).as_str()) {
            return Err(format!("release_keys.txt line {n}: a low-order point, not a usable public key"));
        }
        if keys.contains(&bytes) {
            return Err(format!("release_keys.txt line {n}: the same key is listed twice"));
        }
        keys.push(bytes);
    }
    if keys.is_empty() {
        return Err("release_keys.txt lists no keys".into());
    }
    Ok(keys)
}

/// `key` as release_keys.txt writes it (tools/sign_release keygen prints it).
pub fn hex(key: &[u8]) -> String {
    key.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";
    const B: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    #[test]
    fn reads_one_key_per_line_with_comments() {
        let text = format!("# release keys\n\n{A}  # primary\n  {}\t\n# {B} retired\n", B.to_uppercase());
        let keys = parse(&text).unwrap();
        assert_eq!(keys.len(), 2, "the commented-out line is not a key");
        assert_eq!(hex(&keys[0]), A);
        assert_eq!(keys[1], [1u8; 32]);
        assert_eq!(parse(&format!("{A}\r\n{B}\r\n")).unwrap().len(), 2, "CRLF");
    }

    #[test]
    fn an_empty_list_is_an_error() {
        for text in ["", "\n\n", "# only a comment\n", "# <PUB_PRIMARY>\n# <PUB_BACKUP>\n"] {
            assert!(parse(text).unwrap_err().contains("no keys"), "{text:?}");
        }
    }

    #[test]
    fn a_key_listed_twice_is_an_error() {
        let err = parse(&format!("{A}\n{B}\n{}\n", A.to_uppercase())).unwrap_err();
        assert!(err.contains("line 3") && err.contains("twice"), "{err}");
    }

    #[test]
    fn release_keys_rejects_a_low_order_point() {
        for bad in LOW_ORDER {
            for spelled in [bad.to_string(), bad.to_uppercase()] {
                let err = parse(&format!("{B}\n{spelled}  # looks like a key\n")).unwrap_err();
                assert!(err.contains("line 2") && err.contains("low-order"), "{spelled}: {err}");
            }
        }
        assert!(parse(&"0".repeat(64)).unwrap_err().contains("low-order"), "all zeros");
        // The keys in use are not among them.
        assert_eq!(parse(include_str!("../release_keys.txt")).unwrap().len(), 2);
    }

    #[test]
    fn a_malformed_line_is_an_error() {
        for bad in [&A[..63], &format!("{A}0"), &format!("0x{}", &A[2..]), &A.replace('d', "g"), "<PUB_PRIMARY>", &format!("{} {}", &A[..32], &A[32..]), "é".repeat(32).as_str(), "[214, 199, 235]"] {
            let err = parse(&format!("{B}\n{bad}\n")).unwrap_err();
            assert!(err.contains("line 2"), "{bad}: {err}");
        }
    }
}
