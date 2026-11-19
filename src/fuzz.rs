//! Random inputs for the tests of functions that take outside text (browser
//! requests, subtitle files, AI tool arguments): many short strings built from
//! the characters that tend to break parsers. Seeded, so a failure repeats.

/// Characters that matter to the parsers under test, plus multi-byte ones (a
/// byte slice inside them panics) and control characters.
const ALPHABET: &[char] = &[
    'a', 'Z', '0', '1', '9', 'f', 'F', '%', '.', ' ', '/', '\\', ':', '*', '?', '"', '<', '>', '|', '-', ',', '#', '=', '&', '+',
    '\n', '\r', '\t', '\0', '\u{7f}', '字', 'é', '😀', '\u{202e}', '\u{feff}',
];

/// `n` strings of 0 to 40 characters.
pub fn strings(seed: u64, n: usize) -> impl Iterator<Item = String> {
    let mut state = seed | 1;
    let mut next = move || {
        // xorshift64
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..n).map(move |_| {
        let len = (next() % 41) as usize;
        (0..len).map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize]).collect()
    })
}

/// `strings` with some well-formed pieces mixed in (`pieces` are inserted
/// whole), so parsers get past their first check now and then.
pub fn strings_with(seed: u64, n: usize, pieces: &'static [&'static str]) -> impl Iterator<Item = String> {
    strings(seed, n).enumerate().map(move |(i, s)| {
        let mut out = String::new();
        for (j, c) in s.chars().enumerate() {
            if (i + j) % 5 == 0 {
                out.push_str(pieces[(i * 7 + j) % pieces.len()]);
            }
            out.push(c);
        }
        out
    })
}
