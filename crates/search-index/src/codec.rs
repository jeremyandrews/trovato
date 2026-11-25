//! The Pagefind 1.3.0 index file format: the CBOR structs, the gzip wrapping
//! and the content hash file names.
//!
//! The structs mirror `pagefind/src/index/index_metadata.rs`,
//! `index_words.rs` and `index_filter.rs` at 1.3.0 field for field and index
//! for index (`#[n(..)]`). They derive both `Encode` (the native build writes
//! them) and `Decode` (the decoder and the gates read them back). minicbor's
//! default struct encoding is a fixed length array, which is what
//! `pagefind_web`'s hand written decoder reads.
//!
//! At 1.3.0 a posting's page number is the page's plain position in
//! [`MetaIndex::pages`], not a delta from the previous posting. Delta encoding,
//! `meta_locs` and diacritic variants arrive in later releases (1.6.0 has all
//! three), which is one reason this codec is pinned to one release.

use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use minicbor::{Decode, Encode};
use sha1::{Digest, Sha1};

/// What Pagefind writes inside the gzip stream ahead of every compressed file.
pub const SIGNATURE: &[u8] = b"pagefind_dcd";

/// The `pagefind.<lang>_<hash>.pf_meta` file.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaIndex {
    /// The generator version, `1.3.0`.
    #[n(0)]
    pub version: String,
    /// One entry per page slot, in page number order.
    #[n(1)]
    pub pages: Vec<MetaPage>,
    /// The word index chunks, in word order, with the word range each covers.
    #[n(2)]
    pub index_chunks: Vec<MetaChunk>,
    /// The filter index files.
    #[n(3)]
    pub filters: Vec<MetaFilter>,
    /// Pre-sorted page orders, one per sort key.
    #[n(4)]
    pub sorts: Vec<MetaSort>,
}

/// One word index chunk: load `index/<hash>.pf_index` for a word between
/// `from` and `to` (both shortened to the shortest distinguishing prefix of
/// the neighbouring chunk boundary).
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaChunk {
    /// First word, as a prefix.
    #[n(0)]
    pub from: String,
    /// Last word, as a prefix.
    #[n(1)]
    pub to: String,
    /// File name stem.
    #[n(2)]
    pub hash: String,
}

/// One page slot.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaPage {
    /// The fragment file name stem, `<lang>_<short sha1>`.
    #[n(0)]
    pub hash: String,
    /// Words on the page: the BM25 document length.
    #[n(1)]
    pub word_count: u32,
}

/// One filter index file.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaFilter {
    /// The filter name.
    #[n(0)]
    pub filter: String,
    /// File name stem.
    #[n(1)]
    pub hash: String,
}

/// One sort key's page order.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct MetaSort {
    /// The sort key.
    #[n(0)]
    pub sort: String,
    /// Page numbers in sorted order.
    #[n(1)]
    pub pages: Vec<u64>,
}

/// One `index/<hash>.pf_index` file.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct WordIndex {
    /// Words in byte order.
    #[n(0)]
    pub words: Vec<PackedWord>,
}

/// One word's postings.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct PackedWord {
    /// The (stemmed, when the language has a stemmer) word.
    #[n(0)]
    pub word: String,
    /// One entry per page the word is on, in page number order.
    #[n(1)]
    pub pages: Vec<PackedPage>,
}

/// One word's locations on one page.
#[derive(Encode, Decode, Debug, Clone, PartialEq, Eq)]
pub struct PackedPage {
    /// The page's position in [`MetaIndex::pages`]. Upstream holds a `usize`;
    /// CBOR writes an integer in its shortest form whatever its Rust type, so
    /// the bytes are the same.
    #[n(0)]
    pub page_number: u64,
    /// Word positions, with a weight change written inline as `-(weight) - 1`
    /// ahead of the positions it applies to. The default weight (24, Pagefind's
    /// 1.0) is sorted first.
    #[n(1)]
    pub locs: Vec<i32>,
}

/// `locs` as `pagefind_web` reads them: `(weight, position)` pairs.
pub fn decode_locs(locs: &[i32]) -> Vec<(u8, u32)> {
    let mut weight: u8 = 25;
    let mut out = Vec::with_capacity(locs.len());
    for &loc in locs {
        if loc < 0 {
            let abs = -(i64::from(loc) + 1);
            weight = u8::try_from(abs.min(255)).unwrap_or(u8::MAX);
        } else {
            out.push((weight, loc.unsigned_abs()));
        }
    }
    out
}

/// CBOR encode a value.
pub fn to_cbor<T: Encode<()>>(value: &T) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    minicbor::encode(value, &mut out).map_err(|e| anyhow::anyhow!("CBOR encode: {e}"))?;
    Ok(out)
}

/// Gzip a file's bytes the way Pagefind 1.3.0's `output::write` does: best
/// compression, the signature and then the body written into one stream.
pub fn pack(body: &[u8]) -> Result<Vec<u8>> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    gz.write_all(SIGNATURE).context("gzip the signature")?;
    gz.write_all(body).context("gzip the body")?;
    gz.finish().context("finish the gzip stream")
}

/// Gunzip a Pagefind file and strip its signature.
pub fn unpack(gz: &[u8]) -> Result<Vec<u8>> {
    let mut raw = Vec::new();
    GzDecoder::new(gz)
        .read_to_end(&mut raw)
        .context("gunzip a pagefind file")?;
    match raw.strip_prefix(SIGNATURE) {
        Some(body) => Ok(body.to_vec()),
        None => bail!("file does not start with the pagefind_dcd signature"),
    }
}

/// Pagefind 1.3.0's `utils::full_hash`: the SHA-1, written as `{:x?}` per
/// byte. That is **not zero padded**, so a byte `0x05` is `5`, not `05`, and
/// the string is between 20 and 40 characters long.
pub fn full_hash(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    let mut out = String::with_capacity(40);
    for b in digest.iter() {
        out.push_str(&format!("{b:x?}"));
    }
    out
}

/// The name stem Pagefind 1.3.0 gives a fragment, word chunk or filter file
/// before any collision: `<lang>_` and the first `lang.len() + 8` characters
/// of `<lang>_<full hash>`, which is 7 characters of the hash.
pub fn short_name(full: &str, language: &str) -> String {
    prefix_chars(full, language.len() + 8)
}

/// The meta file's stem: `<lang>_` and the first `lang.len() + 8` characters
/// of the hash itself (the meta file is cut from the bare hash, not from the
/// prefixed one).
pub fn meta_name(meta: &[u8], language: &str) -> String {
    let hash = full_hash(meta);
    format!("{language}_{}", prefix_chars(&hash, language.len() + 8))
}

/// The first `n` bytes of an ASCII string, or all of it. The hashes and the
/// language keys these are cut from are ASCII.
pub(crate) fn prefix_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locs_decode_as_pagefind_web_reads_them() {
        assert_eq!(
            decode_locs(&[3, 9, -251, 0]),
            vec![(25, 3), (25, 9), (250, 0)]
        );
    }

    #[test]
    fn unpack_refuses_a_file_without_the_signature() {
        let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
        gz.write_all(b"not pagefind").unwrap_or_default();
        let bytes = gz.finish().unwrap_or_default();
        assert!(unpack(&bytes).is_err());
    }

    #[test]
    fn pack_and_unpack_round_trip() {
        let packed = pack(b"hello").unwrap_or_default();
        assert_eq!(unpack(&packed).ok(), Some(b"hello".to_vec()));
    }

    #[test]
    fn hashes_are_not_zero_padded() {
        // SHA-1("") = da39a3ee 5e6b4b0d 3255bfef 95601890 afd80709: the bytes
        // 0x0d, 0x07 and 0x09 are written as one character each.
        assert_eq!(full_hash(b""), "da39a3ee5e6b4bd3255bfef95601890afd879");
        assert_eq!(short_name("en_da39a3ee5e6b", "en"), "en_da39a3e");
        assert_eq!(meta_name(b"", "unknown"), "unknown_da39a3ee5e6b4bd");
    }
}
