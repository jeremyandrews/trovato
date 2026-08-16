//! Did this story actually change? (M4)
//!
//! A story is re-summarized every time it gains a member, and most of those
//! syntheses say the same thing in different words. Notifying on every one of
//! them is how a news app becomes something people mute. This module decides
//! whether a re-summarized story is *materially* different from what the reader
//! was already told.
//!
//! Two routes, and which one runs is an operator's decision:
//!
//! - **The judge** (`argus.notify_judge = on`, the default) — one cheap model
//!   call comparing the two summaries. Budget-gated and budget-counted under
//!   [`crate::model::Stage::Notify`], exactly like analyze and summarize: M2's
//!   fence says notification spend is spend.
//! - **The fallback** ([`change_ratio`]) — token-overlap distance between the
//!   two texts, with no call at all. Deterministic, free, and blunter. Turning
//!   the judge off degrades the decision rather than removing the feature.

use crate::embed::tokenize;
use crate::error::{CoreError, CoreResult};
use crate::ports::{ChatRequest, LlmProvider};
use crate::provider::parse_lenient_object;

/// Generation cap for the judge call. The answer is one word and one clause;
/// anything longer is the model ignoring its instructions.
const JUDGE_MAX_TOKENS: u32 = 200;

/// The judge's system prompt.
///
/// The three examples of "not material" are the failure mode this exists to
/// catch: a re-synthesis that adds a corroborating source and rewords a
/// paragraph is not news to someone who read the first one.
const JUDGE_SYSTEM: &str = "You decide whether an updated news summary tells a reader something \
they did not already know from the previous version. Respond with ONLY a JSON object with keys \
\"material\" (true or false) and \"reason\" (one short clause). Material means: a new fact, a \
correction, a significant development, or a change in what is known. NOT material: rewording, \
reordering, an added source that corroborates what was already said, or added detail that does \
not change the substance. Do not include any text outside the JSON object.";

/// What the judge decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeVerdict {
    /// Whether the reader should be told again.
    pub material: bool,
    /// The judge's one-clause justification, carried into the job result and the
    /// outbox row so an operator can see why a story did or did not notify.
    pub reason: String,
}

/// Build the judge [`ChatRequest`].
#[must_use]
pub fn build_request(model: Option<String>, previous: &str, current: &str) -> ChatRequest {
    ChatRequest {
        model,
        system: Some(JUDGE_SYSTEM.to_string()),
        user: format!("PREVIOUS SUMMARY:\n{previous}\n\nUPDATED SUMMARY:\n{current}\n"),
        max_tokens: Some(JUDGE_MAX_TOKENS),
    }
}

/// Parse a judge response.
///
/// # Errors
///
/// [`CoreError::DecisionParse`] when no JSON object can be recovered or it
/// carries no readable `material` value. Permanent — the caller falls back to
/// [`change_ratio`] rather than retrying the same prompt at the same model.
pub fn parse_verdict(raw: &str) -> CoreResult<JudgeVerdict> {
    let obj = parse_lenient_object(raw)
        .map_err(|e| CoreError::DecisionParse(format!("no judge object: {e}")))?;

    let material = match obj.get("material").or_else(|| obj.get("changed")) {
        Some(serde_json::Value::Bool(b)) => *b,
        // Models substitute a string for a boolean often enough that refusing
        // one would cost real notifications.
        Some(serde_json::Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "material" => true,
            "false" | "no" | "not material" => false,
            other => {
                return Err(CoreError::DecisionParse(format!(
                    "judge said {other:?}, which is neither true nor false"
                )));
            }
        },
        _ => {
            return Err(CoreError::DecisionParse(format!(
                "judge object carried no material verdict: {obj}"
            )));
        }
    };

    let reason = obj
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();

    Ok(JudgeVerdict { material, reason })
}

/// Run one judge call and return `(verdict, cost_estimate)`.
///
/// Makes **exactly one** `chat` call, holding the one-AI-call-per-job rule the
/// 150 s background epoch imposes.
///
/// # Errors
///
/// Propagates provider errors (transient) and parse errors (permanent).
pub fn judge<P: LlmProvider + ?Sized>(
    provider: &P,
    model: Option<String>,
    previous: &str,
    current: &str,
) -> CoreResult<(JudgeVerdict, Option<f64>)> {
    let resp = provider.chat(&build_request(model, previous, current))?;
    let verdict = parse_verdict(&resp.content)?;
    Ok((verdict, resp.cost_estimate))
}

/// How different two texts are, as `1 - Jaccard(tokens)` in `0.0..=1.0`.
///
/// `0.0` is "the same words", `1.0` is "nothing in common". Uses the same
/// tokenizer as the embed stage, so the notion of a word is the one the rest of
/// Argus already uses rather than a second opinion.
///
/// Two empty texts are identical (`0.0`); one empty text against a non-empty one
/// is a total change (`1.0`) — which is the right answer for a story that had no
/// summary and now has one.
#[must_use]
pub fn change_ratio(previous: &str, current: &str) -> f64 {
    let a: std::collections::BTreeSet<String> = tokenize(previous).into_iter().collect();
    let b: std::collections::BTreeSet<String> = tokenize(current).into_iter().collect();
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(&b).count();
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    1.0 - (intersection as f64 / union as f64)
}

/// The judge-free verdict: material when the texts have drifted past `threshold`.
#[must_use]
pub fn verdict_without_judge(previous: &str, current: &str, threshold: f64) -> JudgeVerdict {
    let ratio = change_ratio(previous, current);
    JudgeVerdict {
        material: ratio >= threshold,
        reason: format!("token distance {ratio:.2} against threshold {threshold:.2} (judge off)"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::provider::{MockProvider, Scripted};

    const BEFORE: &str = "Reuters reported that the chip maker's datacenter revenue set a record \
        this quarter, driven by demand for accelerator hardware.";
    const AFTER_REWORDED: &str = "The chip maker's datacenter revenue set a record this quarter, \
        Reuters reported, driven by demand for accelerator hardware.";
    const AFTER_MATERIAL: &str = "The chip maker has withdrawn its guidance after regulators \
        opened an antitrust investigation into its accelerator supply agreements.";

    // ---- the prompt ------------------------------------------------------

    #[test]
    fn the_prompt_carries_both_summaries_and_the_material_definition() {
        let req = build_request(Some("cheap".into()), BEFORE, AFTER_MATERIAL);
        assert_eq!(req.model.as_deref(), Some("cheap"));
        assert!(req.user.contains(BEFORE));
        assert!(req.user.contains(AFTER_MATERIAL));
        let system = req.system.unwrap();
        assert!(system.contains("rewording"));
        assert!(system.contains("ONLY a JSON object"));
        assert_eq!(req.max_tokens, Some(JUDGE_MAX_TOKENS));
    }

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn parses_a_clean_verdict() {
        let v = parse_verdict(r#"{"material": true, "reason": "guidance withdrawn"}"#).unwrap();
        assert!(v.material);
        assert_eq!(v.reason, "guidance withdrawn");
    }

    #[test]
    fn parses_a_fenced_verdict_and_the_alternate_key() {
        let v =
            parse_verdict("```json\n{\"changed\": false, \"reason\": \"reworded\"}\n```").unwrap();
        assert!(!v.material);
    }

    #[test]
    fn accepts_a_stringly_typed_boolean() {
        for (raw, expected) in [
            (r#"{"material": "true"}"#, true),
            (r#"{"material": "YES"}"#, true),
            (r#"{"material": "false"}"#, false),
            (r#"{"material": " no "}"#, false),
        ] {
            assert_eq!(parse_verdict(raw).unwrap().material, expected, "{raw}");
        }
    }

    #[test]
    fn a_verdict_with_no_material_key_is_a_permanent_parse_error() {
        let err = parse_verdict(r#"{"reason": "hmm"}"#).unwrap_err();
        assert!(!err.is_transient());
        assert!(matches!(err, CoreError::DecisionParse(_)));
    }

    #[test]
    fn an_uninterpretable_answer_is_a_permanent_parse_error() {
        assert!(parse_verdict(r#"{"material": "maybe"}"#).is_err());
        assert!(parse_verdict("I cannot help with that.").is_err());
    }

    #[test]
    fn a_missing_reason_is_tolerated() {
        assert_eq!(parse_verdict(r#"{"material": true}"#).unwrap().reason, "");
    }

    // ---- the call --------------------------------------------------------

    #[test]
    fn judge_makes_exactly_one_call_and_threads_cost() {
        let mock = MockProvider::chat_once(r#"{"material": true, "reason": "new fact"}"#)
            .with_chat_cost(0.0004);
        let (v, cost) = judge(&mock, Some("cheap".into()), BEFORE, AFTER_MATERIAL).unwrap();
        assert!(v.material);
        assert_eq!(cost, Some(0.0004));
        assert_eq!(mock.chat_calls(), 1);
    }

    #[test]
    fn a_provider_failure_is_transient() {
        let mock = MockProvider::new(vec![Scripted::ChatError("rate limited".into())]);
        let err = judge(&mock, None, BEFORE, AFTER_MATERIAL).unwrap_err();
        assert!(err.is_transient());
    }

    // ---- the fallback ----------------------------------------------------

    #[test]
    fn identical_text_has_no_distance() {
        assert!(change_ratio(BEFORE, BEFORE).abs() < 1e-9);
        assert!(change_ratio("", "").abs() < 1e-9);
    }

    #[test]
    fn disjoint_text_is_a_total_change() {
        assert!((change_ratio("alpha bravo charlie", "delta echo foxtrot") - 1.0).abs() < 1e-9);
        assert!((change_ratio("", "something entirely new") - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rewording_scores_below_a_real_development() {
        let reworded = change_ratio(BEFORE, AFTER_REWORDED);
        let material = change_ratio(BEFORE, AFTER_MATERIAL);
        assert!(
            reworded < material,
            "reworded {reworded:.3} should be closer than material {material:.3}"
        );
        assert!(reworded < 0.2, "reordering barely moves the token set");
        assert!(material > 0.5, "a new development replaces most of it");
    }

    #[test]
    fn the_ratio_stays_inside_its_range() {
        for (a, b) in [
            (BEFORE, AFTER_MATERIAL),
            ("", BEFORE),
            (BEFORE, ""),
            ("!!! ???", "..."),
        ] {
            let r = change_ratio(a, b);
            assert!((0.0..=1.0).contains(&r), "{r} out of range for {a:?}/{b:?}");
        }
    }

    #[test]
    fn the_judge_free_verdict_uses_the_threshold_and_explains_itself() {
        let quiet = verdict_without_judge(BEFORE, AFTER_REWORDED, 0.35);
        assert!(!quiet.material);
        assert!(quiet.reason.contains("judge off"));

        let loud = verdict_without_judge(BEFORE, AFTER_MATERIAL, 0.35);
        assert!(loud.material);
    }

    #[test]
    fn a_zero_threshold_makes_every_resummary_material() {
        assert!(verdict_without_judge(BEFORE, BEFORE, 0.0).material);
    }
}
