use sha2::Digest;
use sha2::Sha256;

/// Stable, non-reversible fingerprint of a token, for keying per-token budget
/// rows.
///
/// Returns the first 16 hex chars of SHA-256 (64 bits — ample to avoid
/// collisions between a handful of tokens, short enough for a DB key).
#[must_use]
pub fn token_fingerprint(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(token_fingerprint("ghp_abc"), token_fingerprint("ghp_abc"));
    }

    #[test]
    fn distinct_tokens_differ() {
        assert_ne!(token_fingerprint("ghp_abc"), token_fingerprint("ghp_def"));
    }

    #[test]
    fn is_16_hex_chars() {
        let fp = token_fingerprint("ghp_abc");
        assert_eq!(fp.len(), 16);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn does_not_leak_token() {
        assert!(!token_fingerprint("ghp_secret").contains("secret"));
    }
}
