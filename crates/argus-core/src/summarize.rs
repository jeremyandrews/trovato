//! The summarize stage's pure logic (M2): rate limiting, the multi-source
//! prompt, and defensive parsing of the synthesized story.
//!
//! Summarize is the one stage keyed on a *story* rather than an article, and
//! the one stage that can be asked to run far more often than it should — a
//! busy story gains members in bursts, and each join enqueues a summarize. The
//! rate limit in [`due_at`] is what turns that burst into one call.

use crate::error::{CoreError, CoreResult};
use crate::ports::{ChatRequest, LlmProvider};
use crate::provider::parse_lenient_object;

/// Default minimum interval between two summaries of the same story.
pub const DEFAULT_MIN_INTERVAL_SECONDS: i64 = 600;

/// Generation cap for the summarize call: a title plus two to four paragraphs.
const SUMMARIZE_MAX_TOKENS: u32 = 1200;

/// Hard cap on the story title, enforced after parsing.
pub const MAX_TITLE_CHARS: usize = 100;

/// Most members described to the model in one call. A long-running story can
/// accumulate more members than fit a prompt (or the host output buffer that
/// loaded them); the newest are the ones that changed the story.
pub const MAX_MEMBERS_IN_PROMPT: usize = 25;

/// The system prompt. The three explicit obligations — credit sources by name,
/// note disagreement, keep the timeline where it matters — are what separate a
/// synthesis from a concatenation.
const SUMMARIZE_SYSTEM: &str = "You are a news editor synthesizing one story from several \
independent reports. Respond with ONLY a JSON object with keys \"title\" (under 100 characters, \
no source names) and \"summary\" (2-4 paragraphs). In the summary: credit sources by name when \
you use their reporting, state plainly where sources disagree rather than averaging them, and \
give the sequence of events where the timeline matters. Do not invent facts that are not in the \
reports. Do not include any text outside the JSON object.";

/// One article as the summarize prompt describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryMember {
    /// Article id, carried through to the story's `sources` json.
    pub article_id: String,
    /// Headline.
    pub title: String,
    /// The analyze stage's summary of this article.
    pub summary: String,
    /// Human-readable source name (the feed's name).
    pub source: String,
    /// Publication time (unix seconds), if known.
    pub published_at: Option<i64>,
    /// Whether this member is a near-duplicate of another. Duplicates stay in
    /// the source list (the reader should see that three outlets carried it)
    /// but are not described to the model a second time.
    pub is_duplicate: bool,
}

/// A parsed summarize response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorySummary {
    /// Story headline, truncated to [`MAX_TITLE_CHARS`].
    pub title: String,
    /// Synthesized narrative.
    pub summary: String,
}

/// When a story may next be summarized.
///
/// `None` means "now". Returning the timestamp rather than a boolean is what
/// lets the caller re-enqueue with exactly the right delay instead of polling.
#[must_use]
pub fn due_at(last_summarized_at: Option<i64>, min_interval: i64) -> Option<i64> {
    last_summarized_at.map(|last| last + min_interval.max(0))
}

/// Seconds to wait before a story may be summarized again, `0` if it is due.
#[must_use]
pub fn wait_seconds(now: i64, last_summarized_at: Option<i64>, min_interval: i64) -> i64 {
    match due_at(last_summarized_at, min_interval) {
        None => 0,
        Some(due) => (due - now).max(0),
    }
}

/// Whether a story may be summarized at `now`.
#[must_use]
pub fn is_due(now: i64, last_summarized_at: Option<i64>, min_interval: i64) -> bool {
    wait_seconds(now, last_summarized_at, min_interval) == 0
}

/// The members actually described to the model: unique non-duplicates, newest
/// first, capped at [`MAX_MEMBERS_IN_PROMPT`].
#[must_use]
pub fn prompt_members(members: &[StoryMember]) -> Vec<&StoryMember> {
    let mut kept: Vec<&StoryMember> = members.iter().filter(|m| !m.is_duplicate).collect();
    kept.sort_by_key(|m| std::cmp::Reverse(m.published_at.unwrap_or(0)));
    kept.truncate(MAX_MEMBERS_IN_PROMPT);
    kept
}

/// Build the summarize [`ChatRequest`] for one story.
#[must_use]
pub fn build_request(model: Option<String>, members: &[StoryMember]) -> ChatRequest {
    let mut user = String::from("Reports on this story:\n\n");
    for (i, m) in prompt_members(members).iter().enumerate() {
        user.push_str(&format!("[{}] {} — {}\n", i + 1, m.source, m.title));
        if let Some(ts) = m.published_at {
            user.push_str(&format!("Published (unix): {ts}\n"));
        }
        if !m.summary.is_empty() {
            user.push_str(&format!("{}\n", m.summary));
        }
        user.push('\n');
    }
    ChatRequest {
        model,
        system: Some(SUMMARIZE_SYSTEM.to_string()),
        user,
        max_tokens: Some(SUMMARIZE_MAX_TOKENS),
    }
}

/// Truncate a title to [`MAX_TITLE_CHARS`], on a character boundary, at a word
/// break where one is close enough to the limit to be worth using.
#[must_use]
pub fn truncate_title(title: &str) -> String {
    let cleaned = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= MAX_TITLE_CHARS {
        return cleaned;
    }
    let cut: String = cleaned.chars().take(MAX_TITLE_CHARS).collect();
    // `rfind` yields a byte index, so the "is this word break close enough to
    // the limit" test is made against the byte length of the same string.
    let keep_from = cut.len() * 3 / 4;
    match cut.rfind(' ') {
        Some(idx) if idx >= keep_from => cut[..idx].to_string(),
        _ => cut,
    }
}

/// Read a value as prose, accepting the shapes a model may substitute for a
/// string (a paragraph list is the common one here).
fn as_prose(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.trim().to_string(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// Parse a summarize response.
///
/// A missing title is recoverable — the first sentence of the summary stands in
/// — but a missing summary is not, because the summary is the entire product of
/// the call.
///
/// # Errors
///
/// [`CoreError::SummaryParse`] when no JSON object can be recovered or the
/// object carries no summary text. Permanent: the story keeps whatever summary
/// it already had.
pub fn parse_summary(raw: &str) -> CoreResult<StorySummary> {
    let obj = parse_lenient_object(raw)
        .map_err(|e| CoreError::SummaryParse(format!("no summary object: {e}")))?;

    let summary = as_prose(obj.get("summary").or_else(|| obj.get("body")));
    if summary.is_empty() {
        return Err(CoreError::SummaryParse(format!(
            "summary object carried no summary text: {obj}"
        )));
    }

    let title = as_prose(obj.get("title").or_else(|| obj.get("headline")));
    let title = if title.is_empty() {
        first_sentence(&summary)
    } else {
        title
    };

    Ok(StorySummary {
        title: truncate_title(&title),
        summary,
    })
}

/// The first sentence of `text`, used as a fallback title.
fn first_sentence(text: &str) -> String {
    match text.find(['.', '!', '?']) {
        Some(i) => text[..=i].trim().to_string(),
        None => text.trim().to_string(),
    }
}

/// Run one summarize call and return `(summary, cost_estimate)`.
///
/// Makes **exactly one** `chat` call.
///
/// # Errors
///
/// Propagates provider (transient) and parse (permanent) errors.
pub fn summarize<P: LlmProvider + ?Sized>(
    provider: &P,
    model: Option<String>,
    members: &[StoryMember],
) -> CoreResult<(StorySummary, Option<f64>)> {
    let req = build_request(model, members);
    let resp = provider.chat(&req)?;
    let summary = parse_summary(&resp.content)?;
    Ok((summary, resp.cost_estimate))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::provider::{MockProvider, Scripted};

    fn member(id: &str, source: &str, at: i64, dup: bool) -> StoryMember {
        StoryMember {
            article_id: id.into(),
            title: format!("Headline {id}"),
            summary: format!("Summary of {id}."),
            source: source.into(),
            published_at: Some(at),
            is_duplicate: dup,
        }
    }

    const CLEAN: &str = r#"{"title": "Chip maker posts record quarter",
        "summary": "Reuters reported record revenue.\n\nBloomberg disagreed on the margin."}"#;

    // ---- rate limiting ---------------------------------------------------

    #[test]
    fn a_story_never_summarized_is_due_immediately() {
        assert!(is_due(1000, None, DEFAULT_MIN_INTERVAL_SECONDS));
        assert_eq!(wait_seconds(1000, None, DEFAULT_MIN_INTERVAL_SECONDS), 0);
    }

    #[test]
    fn a_recent_summary_defers_by_the_remaining_interval() {
        let last = 1000;
        assert_eq!(wait_seconds(1100, Some(last), 600), 500);
        assert!(!is_due(1100, Some(last), 600));
    }

    #[test]
    fn the_interval_boundary_is_due() {
        assert!(is_due(1600, Some(1000), 600));
        assert!(!is_due(1599, Some(1000), 600));
    }

    #[test]
    fn a_stale_last_summary_is_due_and_never_negative() {
        assert_eq!(wait_seconds(99_999, Some(1000), 600), 0);
        assert!(is_due(99_999, Some(1000), 600));
    }

    #[test]
    fn a_zero_or_negative_interval_disables_rate_limiting() {
        assert!(is_due(1000, Some(1000), 0));
        assert!(is_due(1000, Some(1000), -60));
    }

    #[test]
    fn a_burst_of_joins_collapses_into_one_call() {
        // Five joins land inside one interval; only the first is due, and the
        // rest all defer to the same instant — which is what makes the
        // re-enqueue coalesce instead of scheduling five calls.
        let last = 1000;
        let waits: Vec<i64> = [1010, 1100, 1200, 1300, 1400]
            .iter()
            .map(|now| wait_seconds(*now, Some(last), 600))
            .collect();
        assert!(waits.iter().all(|w| *w > 0));
        let due: Vec<i64> = [1010, 1100, 1200, 1300, 1400]
            .iter()
            .zip(&waits)
            .map(|(now, w)| now + w)
            .collect();
        assert!(due.iter().all(|d| *d == 1600), "all defer to one instant");
    }

    // ---- prompt ----------------------------------------------------------

    #[test]
    fn prompt_skips_duplicates_and_orders_newest_first() {
        let members = vec![
            member("a", "Reuters", 100, false),
            member("b", "AP", 300, false),
            member("c", "Syndicated", 200, true),
        ];
        let kept = prompt_members(&members);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].article_id, "b");
        assert_eq!(kept[1].article_id, "a");
    }

    #[test]
    fn prompt_is_capped_at_the_member_limit() {
        let members: Vec<StoryMember> = (0..60)
            .map(|i| member(&format!("a{i}"), "Src", i, false))
            .collect();
        assert_eq!(prompt_members(&members).len(), MAX_MEMBERS_IN_PROMPT);
    }

    #[test]
    fn prompt_credits_each_source_by_name() {
        let members = vec![
            member("a", "Reuters", 100, false),
            member("b", "Bloomberg", 200, false),
        ];
        let req = build_request(Some("strong".into()), &members);
        assert!(req.user.contains("Reuters"));
        assert!(req.user.contains("Bloomberg"));
        assert!(req.system.unwrap().contains("credit sources by name"));
        assert_eq!(req.model.as_deref(), Some("strong"));
    }

    #[test]
    fn an_all_duplicate_story_still_builds_a_request() {
        let req = build_request(None, &[member("a", "Src", 1, true)]);
        assert!(req.user.starts_with("Reports on this story:"));
    }

    // ---- parsing ---------------------------------------------------------

    #[test]
    fn parses_a_clean_summary() {
        let s = parse_summary(CLEAN).unwrap();
        assert_eq!(s.title, "Chip maker posts record quarter");
        assert!(s.summary.contains("Bloomberg"));
    }

    #[test]
    fn parses_fenced_output() {
        let s = parse_summary(&format!("Here:\n```json\n{CLEAN}\n```")).unwrap();
        assert!(!s.summary.is_empty());
    }

    #[test]
    fn accepts_a_paragraph_list_and_alternate_keys() {
        let raw = r#"{"headline": "H", "body": ["Para one.", "Para two."]}"#;
        let s = parse_summary(raw).unwrap();
        assert_eq!(s.title, "H");
        assert_eq!(s.summary, "Para one.\n\nPara two.");
    }

    #[test]
    fn a_missing_title_falls_back_to_the_first_sentence() {
        let s = parse_summary(r#"{"summary": "Rates rose again. Then they fell."}"#).unwrap();
        assert_eq!(s.title, "Rates rose again.");
    }

    #[test]
    fn a_missing_summary_is_permanent() {
        let err = parse_summary(r#"{"title": "just a title"}"#).unwrap_err();
        assert!(!err.is_transient());
        assert!(matches!(err, CoreError::SummaryParse(_)));
    }

    #[test]
    fn no_object_is_permanent() {
        let err = parse_summary("Sorry, I can't help with that.").unwrap_err();
        assert!(matches!(err, CoreError::SummaryParse(_)));
    }

    #[test]
    fn title_is_truncated_at_a_word_break() {
        let long = "word ".repeat(60);
        let t = truncate_title(&long);
        assert!(t.chars().count() <= MAX_TITLE_CHARS);
        assert!(!t.ends_with("wor"), "should cut at a word break");
    }

    #[test]
    fn title_without_a_usable_word_break_is_cut_hard() {
        let t = truncate_title(&"x".repeat(300));
        assert_eq!(t.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn title_truncation_is_char_safe_on_multibyte_input() {
        let t = truncate_title(&"é".repeat(300));
        assert_eq!(t.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn title_whitespace_is_collapsed() {
        assert_eq!(truncate_title("  a   b \n c "), "a b c");
    }

    // ---- the call --------------------------------------------------------

    #[test]
    fn summarize_makes_exactly_one_call_and_threads_cost() {
        let mock = MockProvider::chat_once(CLEAN).with_chat_cost(0.02);
        let (s, cost) = summarize(&mock, None, &[member("a", "Reuters", 1, false)]).unwrap();
        assert!(!s.summary.is_empty());
        assert_eq!(cost, Some(0.02));
        assert_eq!(mock.chat_calls(), 1);
    }

    #[test]
    fn summarize_provider_error_is_transient() {
        let mock = MockProvider::new(vec![Scripted::ChatError("timeout".into())]);
        let err = summarize(&mock, None, &[member("a", "S", 1, false)]).unwrap_err();
        assert!(err.is_transient());
    }
}
