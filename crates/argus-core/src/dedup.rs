//! Content hashing for near-duplicate detection (M1-6).
//!
//! The *exact*-duplicate defense is the `UNIQUE (url)` constraint on
//! `argus_articles` plus an idempotent upsert (see the store port). This module
//! covers the *near*-duplicate case: two feeds syndicating the same story under
//! different URLs. A stable `content_hash` over normalized title+body lets the
//! store recognize and flag those without a second network or model call.
//!
//! Pure and dependency-light: `sha2` compiles to `wasm32-wasip1`.

use sha2::{Digest, Sha256};

/// Compute a stable hex-encoded SHA-256 over an article's normalized title and
/// body.
///
/// Normalization lowercases and collapses runs of ASCII whitespace to a single
/// space so trivial reformatting (extra newlines, leading indentation) does not
/// change the hash. Two articles with the same normalized text hash equal even
/// when their URLs differ, which is the near-duplicate signal.
#[must_use]
pub fn content_hash(title: &str, body: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize(title).as_bytes());
    hasher.update([0u8]); // field separator so "ab"+"c" != "a"+"bc"
    hasher.update(normalize(body).as_bytes());
    hex::encode(hasher.finalize())
}

/// Lowercase and collapse ASCII whitespace runs to single spaces, trimmed.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_space && !out.is_empty() {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.extend(ch.to_lowercase());
            prev_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_hex() {
        let h = content_hash("Title", "Body");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, content_hash("Title", "Body"));
    }

    #[test]
    fn whitespace_and_case_insensitive() {
        assert_eq!(
            content_hash("Hello  World", "The\n\n Body"),
            content_hash("hello world", "the body")
        );
    }

    #[test]
    fn field_boundary_matters() {
        // "ab"|"c" must not collide with "a"|"bc".
        assert_ne!(content_hash("ab", "c"), content_hash("a", "bc"));
    }

    #[test]
    fn different_content_differs() {
        assert_ne!(content_hash("A", "one"), content_hash("A", "two"));
    }
}
