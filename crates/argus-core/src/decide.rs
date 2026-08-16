//! The decide stage's pure logic (M1-7): build the relevance prompt, run one
//! model call, parse a `{score, reason}` decision defensively, and compare to
//! the topic threshold.
//!
//! This is the heart of what M1-7 wants tested — threshold boundaries, malformed
//! output, and *exactly one* AI call per job — so it is expressed as pure
//! functions plus a single [`decide`] entry point over the [`LlmProvider`] port.
//! The plugin's queue worker calls [`decide`] once per job.

use crate::error::{CoreError, CoreResult};
use crate::model::Decision;
use crate::ports::{ChatRequest, LlmProvider};
use crate::provider::parse_lenient_object;

/// The first N words of an article body handed to the model (M1-7: "first 500
/// words").
pub const DECIDE_BODY_WORDS: usize = 500;

/// Generation cap for the decide call — it only needs to emit a small JSON
/// object, so keep it cheap.
const DECIDE_MAX_TOKENS: u32 = 256;

/// The system prompt framing the decide task as strict JSON output.
const DECIDE_SYSTEM: &str = "You are a news relevance filter. Given a topic and an \
article, respond with ONLY a JSON object of the form {\"score\": <integer 0-100>, \
\"reason\": <short string>} where score is how relevant the article is to the topic. \
Do not include any text outside the JSON object.";

/// Take the first `DECIDE_BODY_WORDS` whitespace-delimited words of `body`.
#[must_use]
pub fn first_words(body: &str) -> String {
    body.split_whitespace()
        .take(DECIDE_BODY_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the decide [`ChatRequest`] for a topic + article.
///
/// `model` routes the call (decide uses a cheap model; see M1-4). The user turn
/// carries the topic's relevance prompt, the article title, and the first 500
/// words of the body.
#[must_use]
pub fn build_request(
    model: Option<String>,
    topic_prompt: &str,
    title: &str,
    body: &str,
) -> ChatRequest {
    let user = format!(
        "Topic relevance criteria:\n{topic_prompt}\n\nArticle title: {title}\n\nArticle body (first {DECIDE_BODY_WORDS} words):\n{}",
        first_words(body)
    );
    ChatRequest {
        model,
        system: Some(DECIDE_SYSTEM.to_string()),
        user,
        max_tokens: Some(DECIDE_MAX_TOKENS),
    }
}

/// Parse a model response into a [`Decision`], tolerating sloppy output.
///
/// Recovers a balanced JSON object from fenced/prefixed/suffixed text, then
/// reads `score` (clamped to `0..=100`, accepting an integer, a float, or a
/// numeric string) and `reason` (optional). A response with no recoverable
/// object or no usable score is a permanent [`CoreError::DecisionParse`] — the
/// article is discarded, never retried.
///
/// # Errors
///
/// [`CoreError::DecisionParse`] when no `{...}` is present or `score` is absent
/// / non-numeric.
pub fn parse_decision(raw: &str) -> CoreResult<Decision> {
    let obj = parse_lenient_object(raw)?;
    let score_val = obj
        .get("score")
        .ok_or_else(|| CoreError::DecisionParse(format!("no `score` field in {obj}")))?;
    let score = coerce_score(score_val)
        .ok_or_else(|| CoreError::DecisionParse(format!("`score` not numeric: {score_val}")))?;
    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Ok(Decision { score, reason })
}

/// Coerce a JSON value into a `0..=100` score, accepting int, float, or string.
fn coerce_score(v: &serde_json::Value) -> Option<u8> {
    let n: f64 = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    if !n.is_finite() {
        return None;
    }
    Some(n.round().clamp(0.0, 100.0) as u8)
}

/// Whether a score keeps an article: `score >= threshold` (the boundary is
/// inclusive — a score exactly at the threshold is kept, M1-7).
#[must_use]
pub fn keeps(score: u8, threshold: u8) -> bool {
    score >= threshold
}

/// Run one decide call and return `(decision, keep, cost_estimate)`.
///
/// Makes **exactly one** `chat` call on the provider (asserted in tests via the
/// mock's call counter). A provider failure propagates as a transient
/// [`CoreError::Provider`] (the worker retries); a malformed-but-received
/// response propagates as a permanent [`CoreError::DecisionParse`] (the worker
/// discards without retry). `cost_estimate` is the host's per-call cost
/// (G-COST-OPAQUE, p11j), `None` when the model is unpriced.
///
/// # Errors
///
/// Propagates provider (transient) and parse (permanent) errors; callers use
/// [`CoreError::is_transient`] to decide retry vs discard.
pub fn decide<P: LlmProvider + ?Sized>(
    provider: &P,
    model: Option<String>,
    topic_prompt: &str,
    threshold: u8,
    title: &str,
    body: &str,
) -> CoreResult<(Decision, bool, Option<f64>)> {
    let req = build_request(model, topic_prompt, title, body);
    let resp = provider.chat(&req)?;
    let decision = parse_decision(&resp.content)?;
    let keep = keeps(decision.score, threshold);
    Ok((decision, keep, resp.cost_estimate))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::provider::{MockProvider, Scripted};

    #[test]
    fn first_words_caps_at_limit() {
        let body = (0..600)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let fw = first_words(&body);
        assert_eq!(fw.split_whitespace().count(), DECIDE_BODY_WORDS);
    }

    #[test]
    fn parses_clean_decision() {
        let d = parse_decision(r#"{"score": 73, "reason": "on topic"}"#).unwrap();
        assert_eq!(d.score, 73);
        assert_eq!(d.reason, "on topic");
    }

    #[test]
    fn parses_sloppy_decision() {
        let d = parse_decision("Sure!\n```json\n{\"score\": \"88\"}\n```\nDone.").unwrap();
        assert_eq!(d.score, 88);
        assert_eq!(d.reason, "");
    }

    #[test]
    fn clamps_and_rounds_score() {
        assert_eq!(parse_decision(r#"{"score": 150}"#).unwrap().score, 100);
        assert_eq!(parse_decision(r#"{"score": 49.6}"#).unwrap().score, 50);
        assert_eq!(parse_decision(r#"{"score": -5}"#).unwrap().score, 0);
    }

    #[test]
    fn malformed_output_is_permanent() {
        let err = parse_decision("the article seems relevant").unwrap_err();
        assert!(!err.is_transient());
        let err2 = parse_decision(r#"{"reason": "no score here"}"#).unwrap_err();
        assert!(matches!(err2, CoreError::DecisionParse(_)));
    }

    #[test]
    fn threshold_boundary_is_inclusive() {
        assert!(keeps(50, 50)); // exactly at threshold keeps
        assert!(keeps(51, 50));
        assert!(!keeps(49, 50));
        assert!(keeps(0, 0)); // threshold 0 keeps everything
    }

    #[test]
    fn decide_makes_exactly_one_call() {
        let mock = MockProvider::chat_once(r#"{"score": 80, "reason": "yes"}"#);
        let (d, keep, _cost) =
            decide(&mock, Some("cheap".into()), "AI news", 50, "T", "b b b").unwrap();
        assert_eq!(d.score, 80);
        assert!(keep);
        assert_eq!(mock.chat_calls(), 1, "exactly one AI call per decide");
    }

    #[test]
    fn decide_discard_still_one_call() {
        let mock = MockProvider::chat_once(r#"{"score": 10, "reason": "off topic"}"#);
        let (d, keep, _cost) = decide(&mock, None, "AI news", 50, "T", "body").unwrap();
        assert_eq!(d.score, 10);
        assert!(!keep);
        assert_eq!(mock.chat_calls(), 1);
    }

    #[test]
    fn decide_provider_error_is_transient_one_call() {
        let mock = MockProvider::new(vec![Scripted::ChatError("rate limit".into())]);
        let err = decide(&mock, None, "t", 50, "T", "b").unwrap_err();
        assert!(err.is_transient());
        assert_eq!(mock.chat_calls(), 1);
    }

    #[test]
    fn decide_malformed_is_permanent_one_call() {
        let mock = MockProvider::chat_once("I think it is relevant, yes.");
        let err = decide(&mock, None, "t", 50, "T", "b").unwrap_err();
        assert!(!err.is_transient());
        assert_eq!(mock.chat_calls(), 1);
    }
}
