//! Lexical feature vectors (M2 embed stage) and cosine similarity.
//!
//! # Why this is not a semantic embedding
//!
//! The frozen `ai` host interface exposes no embedding call: `ai-request`
//! serves every operation — including `Embedding` — as a chat completion, and
//! the kernel's real `/embeddings` client is reachable only from kernel code.
//! `M2-DESIGN.md` argues the decision in full. The fallback is a deterministic
//! lexical vector computed here, with no provider, no cost, and no failure
//! mode.
//!
//! # The recipe
//!
//! A signed hashing-trick projection ("`lex-v1`"): each token is hashed once,
//! the hash picks both a dimension index and a sign, and the token's weight is
//! accumulated there. Term weight is sublinear in frequency (`1 + ln tf`) and
//! scaled by where the token appeared — entity names heaviest, then the title,
//! then the summary. The result is L2-normalized, so [`cosine`] is a plain dot
//! product and every similarity is in `[-1, 1]`.
//!
//! Entity names carrying the heaviest weight is deliberate: it makes cosine
//! over these vectors carry entity overlap, so the cluster stage needs one
//! similarity term rather than a cosine blended with a separate Jaccard.
//!
//! The recipe identifier is stored beside each vector. Two vectors are only
//! comparable if their recipe *and* dimension match; the cluster stage drops
//! candidates that disagree rather than silently comparing nonsense.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

/// Identifier for the vector recipe implemented here.
///
/// Stored per row as `"{VECTOR_RECIPE}/{dim}"` (see [`recipe_id`]). Changing
/// the algorithm in a way that alters output means bumping this, which makes
/// every stored vector visibly incomparable instead of quietly wrong.
pub const VECTOR_RECIPE: &str = "lex-v1";

/// Default vector dimension. Overridable via the `argus.vector_dim` site
/// variable; changing it invalidates every stored vector (the recipe id
/// embeds the dimension, so they are skipped, not misread).
pub const DEFAULT_DIMENSION: usize = 256;

/// Smallest dimension that is not obviously degenerate.
pub const MIN_DIMENSION: usize = 32;

/// Largest dimension accepted from config. A vector is stored as a JSON float
/// array and read back through the 256 KB host output buffer alongside other
/// rows, so an unbounded dimension is a self-inflicted truncation bug.
pub const MAX_DIMENSION: usize = 2048;

/// Weight applied to tokens from an extracted entity name.
const WEIGHT_ENTITY: f32 = 3.0;
/// Weight applied to tokens from the article title.
const WEIGHT_TITLE: f32 = 2.0;
/// Weight applied to tokens from the article summary.
const WEIGHT_SUMMARY: f32 = 1.0;

/// Minimum token length kept. Shorter tokens are overwhelmingly function words
/// and inflate collisions without carrying topic signal.
pub const MIN_TOKEN_LEN: usize = 3;

/// Very common English words dropped before hashing. Deliberately short: the
/// sublinear term weighting already suppresses frequent tokens, and a long
/// hand-maintained stopword list is a liability in a multi-language feed set.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "from", "have", "has", "had", "was", "were",
    "are", "but", "not", "you", "his", "her", "its", "their", "they", "them", "will", "would",
    "can", "could", "should", "been", "into", "over", "after", "than", "then", "who", "what",
    "when", "where", "which", "while", "about", "said", "says",
];

/// The recipe identifier stored beside a vector of dimension `dim`.
#[must_use]
pub fn recipe_id(dim: usize) -> String {
    format!("{VECTOR_RECIPE}/{dim}")
}

/// Identifier prefix for the **semantic** route: a real embedding obtained from
/// a provider through the kernel's `ai-request` host.
///
/// Available from `KERNEL_API_VERSION (0,99)` onward, when the host began
/// routing `operation: Embedding` to an embeddings endpoint instead of posting
/// it to `/chat/completions` (`G-AI-EMBED-UNROUTED`). Before that the vector a
/// plugin got back was a chat completion's empty `content`, which is why M2
/// shipped the lexical route.
pub const SEMANTIC_RECIPE: &str = "sem-v1";

/// The recipe identifier stored beside a semantic embedding.
///
/// The model is the identity, because cosine between two *different* embedding
/// models is meaningless and the whole point of the recipe field is that a
/// vector which cannot be compared is skipped rather than misread. The
/// dimension is deliberately **not** part of it: unlike the lexical route the
/// dimension is the provider's choice, not configuration, so it cannot be known
/// before the call — and a provider that changed it under one model name is
/// already caught, since [`cosine`] returns `0.0` on a length mismatch and the
/// cluster stage reads `0.0` as "not comparable", never as a low score.
#[must_use]
pub fn semantic_recipe_id(model: &str) -> String {
    format!("{SEMANTIC_RECIPE}/{model}")
}

/// Clamp a configured dimension into the supported range.
#[must_use]
pub fn clamp_dimension(dim: usize) -> usize {
    dim.clamp(MIN_DIMENSION, MAX_DIMENSION)
}

/// Split `text` into lowercase alphanumeric tokens, dropping stopwords and
/// tokens shorter than [`MIN_TOKEN_LEN`].
///
/// Splitting on "not alphanumeric" rather than on whitespace means URLs,
/// hyphenated compounds and punctuation-attached words all decompose the same
/// way in every feed.
#[must_use]
pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= MIN_TOKEN_LEN)
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// Hash a token to a `(dimension index, sign)` pair.
///
/// One SHA-256 over the token supplies both: the low 4 bytes pick the index,
/// and one bit of the next byte picks the sign. The signed variant of the
/// hashing trick keeps collisions from systematically inflating similarity —
/// two unrelated tokens landing in the same slot cancel half the time instead
/// of always adding.
fn hash_slot(token: &str, dim: usize) -> (usize, f32) {
    let digest = Sha256::digest(token.as_bytes());
    let raw = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let index = (raw as usize) % dim;
    let sign = if digest[4] & 1 == 0 { 1.0 } else { -1.0 };
    (index, sign)
}

/// One weighted text section fed into a feature vector.
#[derive(Debug, Clone, Copy)]
struct Section<'a> {
    text: &'a str,
    weight: f32,
}

/// Build the L2-normalized feature vector for an article.
///
/// `entities` are the canonical names extracted by the analyze stage. An
/// article with no usable tokens at all yields an all-zero vector, which
/// [`cosine`] scores as `0.0` against everything — the cluster stage treats
/// that as "no signal" rather than "no match".
#[must_use]
pub fn feature_vector(title: &str, summary: &str, entities: &[String], dim: usize) -> Vec<f32> {
    let dim = clamp_dimension(dim);
    let joined_entities = entities.join(" ");
    let sections = [
        Section {
            text: &joined_entities,
            weight: WEIGHT_ENTITY,
        },
        Section {
            text: title,
            weight: WEIGHT_TITLE,
        },
        Section {
            text: summary,
            weight: WEIGHT_SUMMARY,
        },
    ];

    // Accumulate weighted term frequencies first, so the sublinear damping is
    // applied per token across the whole document rather than per section.
    let mut weighted_tf: HashMap<String, f32> = HashMap::new();
    for section in sections {
        for token in tokenize(section.text) {
            *weighted_tf.entry(token).or_insert(0.0) += section.weight;
        }
    }

    let mut vector = vec![0.0f32; dim];
    for (token, tf) in &weighted_tf {
        let weight = 1.0 + tf.ln();
        let (index, sign) = hash_slot(token, dim);
        vector[index] += sign * weight;
    }
    l2_normalize(&mut vector);
    vector
}

/// Scale `v` to unit length in place. A zero vector is left as-is.
pub fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Cosine similarity between two vectors.
///
/// Returns `0.0` when the vectors have different lengths or either is a zero
/// vector — both mean "not comparable", and the cluster stage must not read
/// that as a match. Inputs are not assumed normalized.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(-1.0, 1.0)
}

/// Fold a new member vector into a story centroid holding `count` members.
///
/// The centroid is the L2-normalized mean of its members. Storing it
/// normalized (rather than storing the running sum) keeps [`cosine`] cheap and
/// keeps the stored magnitude from drifting with story size; `count` is passed
/// in so the incremental update still weights an established story's centroid
/// against a single new article correctly.
#[must_use]
pub fn fold_centroid(centroid: &[f32], count: u32, member: &[f32]) -> Vec<f32> {
    if centroid.len() != member.len() || centroid.is_empty() {
        return member.to_vec();
    }
    let n = count.max(1) as f32;
    let mut out: Vec<f32> = centroid
        .iter()
        .zip(member.iter())
        .map(|(c, m)| c * n + m)
        .collect();
    l2_normalize(&mut out);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn tokenize_drops_short_tokens_and_stopwords() {
        let toks = tokenize("The quick brown fox, and a dog!");
        assert!(!toks.contains(&"the".to_string()));
        assert!(!toks.contains(&"and".to_string()));
        assert!(!toks.iter().any(|t| t.chars().count() < MIN_TOKEN_LEN));
        assert!(toks.contains(&"quick".to_string()));
        assert!(toks.contains(&"brown".to_string()));
    }

    #[test]
    fn tokenize_splits_on_punctuation_and_urls() {
        let toks = tokenize("https://example.test/ai-safety_report");
        assert!(toks.contains(&"example".to_string()));
        assert!(toks.contains(&"safety".to_string()));
        assert!(toks.contains(&"report".to_string()));
    }

    #[test]
    fn vector_is_normalized_and_right_length() {
        let v = feature_vector("Rust compiler release", "A new compiler ships", &[], 128);
        assert_eq!(v.len(), 128);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(approx(norm, 1.0), "expected unit norm, got {norm}");
    }

    #[test]
    fn vector_is_deterministic() {
        let a = feature_vector("Title", "Body text here", &["OpenAI".into()], 64);
        let b = feature_vector("Title", "Body text here", &["OpenAI".into()], 64);
        assert_eq!(a, b);
    }

    #[test]
    fn empty_input_yields_zero_vector_that_matches_nothing() {
        let v = feature_vector("", "", &[], 64);
        assert!(v.iter().all(|x| *x == 0.0));
        let other = feature_vector("Rust compiler", "release notes", &[], 64);
        assert_eq!(cosine(&v, &other), 0.0);
    }

    #[test]
    fn same_story_scores_far_above_unrelated() {
        let dim = 256;
        let a = feature_vector(
            "OpenAI releases new reasoning model",
            "OpenAI announced a reasoning model with improved benchmark scores.",
            &["OpenAI".into()],
            dim,
        );
        let b = feature_vector(
            "OpenAI announces reasoning model release",
            "The reasoning model from OpenAI posts improved benchmark scores.",
            &["OpenAI".into()],
            dim,
        );
        let c = feature_vector(
            "Flooding closes coastal highway",
            "Heavy rain closed the coastal highway for a second day.",
            &["Pacific Coast Highway".into()],
            dim,
        );
        let same = cosine(&a, &b);
        let different = cosine(&a, &c);
        assert!(same > 0.6, "related articles should score high, got {same}");
        assert!(
            different < 0.2,
            "unrelated articles should score low, got {different}"
        );
        assert!(same > different + 0.4);
    }

    #[test]
    fn shared_entities_pull_similarity_up() {
        let dim = 256;
        let base = ("Chip maker posts record quarter", "Revenue rose sharply.");
        let other = (
            "Semiconductor firm beats forecast",
            "Sales climbed this period.",
        );
        let without = cosine(
            &feature_vector(base.0, base.1, &[], dim),
            &feature_vector(other.0, other.1, &[], dim),
        );
        let with = cosine(
            &feature_vector(base.0, base.1, &["Nvidia".into()], dim),
            &feature_vector(other.0, other.1, &["Nvidia".into()], dim),
        );
        assert!(
            with > without,
            "a shared entity must raise similarity ({with} vs {without})"
        );
    }

    #[test]
    fn cosine_handles_mismatched_and_zero_vectors() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
    }

    #[test]
    fn cosine_of_identical_normalized_vectors_is_one() {
        let mut v = vec![0.3, -0.7, 0.1, 0.9];
        l2_normalize(&mut v);
        assert!(approx(cosine(&v, &v), 1.0));
    }

    #[test]
    fn fold_centroid_moves_toward_the_new_member() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let folded = fold_centroid(&a, 1, &b);
        // One member plus one new article: halfway, renormalized.
        assert!(approx(folded[0], folded[1]));
        assert!(approx(cosine(&folded, &a), cosine(&folded, &b)));
    }

    #[test]
    fn fold_centroid_weights_an_established_story() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let small = fold_centroid(&a, 1, &b);
        let large = fold_centroid(&a, 20, &b);
        assert!(
            cosine(&large, &a) > cosine(&small, &a),
            "a 20-member centroid must move less than a 1-member one"
        );
    }

    #[test]
    fn fold_centroid_on_a_mismatched_vector_adopts_the_member() {
        let folded = fold_centroid(&[1.0, 0.0], 3, &[0.0, 1.0, 0.0]);
        assert_eq!(folded, vec![0.0, 1.0, 0.0]);
    }

    #[test]
    fn dimension_is_clamped() {
        assert_eq!(clamp_dimension(0), MIN_DIMENSION);
        assert_eq!(clamp_dimension(1_000_000), MAX_DIMENSION);
        assert_eq!(clamp_dimension(256), 256);
        assert_eq!(feature_vector("a title", "", &[], 4).len(), MIN_DIMENSION);
    }

    #[test]
    fn recipe_id_embeds_the_dimension() {
        assert_eq!(recipe_id(256), "lex-v1/256");
        assert_ne!(recipe_id(256), recipe_id(512));
    }
}
