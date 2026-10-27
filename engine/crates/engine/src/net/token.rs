// SPDX-License-Identifier: GPL-3.0-or-later

//! The session token: random bytes from the OS CSPRNG, compared in constant time.

use super::NetError;

/// Bytes of entropy in a token (256 bits).
const TOKEN_BYTES: usize = 32;

const HEX: [u8; 16] = *b"0123456789abcdef";

/// A fresh token as lower-case hex (64 characters), from the operating system's CSPRNG.
pub fn generate_token() -> Result<String, NetError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| NetError::Entropy(e.to_string()))?;
    let mut text = String::with_capacity(TOKEN_BYTES * 2);
    for b in bytes {
        text.push(char::from(HEX[usize::from(b >> 4)]));
        text.push(char::from(HEX[usize::from(b & 15)]));
    }
    Ok(text)
}

/// Equality that takes the same time wherever the inputs differ (it always walks the longer of
/// the two), so a local process cannot learn the token byte by byte. The inputs are bounded by
/// the 4 KiB hello frame.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_64_hex_characters_and_differ() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, b);
    }

    #[test]
    fn comparison_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"abcd", b"abc"));
        assert!(!constant_time_eq(b"", b"a"));
        // Zero padding must not make lengths irrelevant.
        assert!(!constant_time_eq(b"a", b"a\0"));
    }
}
