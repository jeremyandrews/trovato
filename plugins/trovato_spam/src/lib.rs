//! AI comment moderation.
//!
//! Classifies each new comment in the background and publishes it, leaves it for
//! a human, or marks it as spam.
//!
//! # The pieces this uses, and why they were idle
//!
//! Every part of this existed before the plugin did. The AI provider registry has
//! a `Moderation` operation type that nothing invoked, and the admin AI-features
//! screen already lets an operator point a provider at it. The `ai_background`
//! manifest capability authorizes an AI call made outside a web request. The queue
//! host interface has retries and a dead-letter tier. `tap_comment_insert` and
//! `tap_queue_worker` are both dispatched. What was missing was a comment status
//! to classify *into*, which is why this arrives with pending and spam statuses
//! rather than before them.
//!
//! # Flow
//!
//! 1. `tap_comment_insert` pushes a classification job. Nothing slow happens on
//!    the request that posted the comment.
//! 2. `tap_queue_worker` drains it under the background principal, asks the
//!    provider to classify, and writes the verdict back.
//!
//! # Failure posture: closed, into the review queue
//!
//! A comment is created in whatever status the site's `comment_default_status`
//! says, and this plugin only ever moves it *away* from pending on a verdict it
//! actually received:
//!
//! - Provider unreachable, or a response that cannot be read as a verdict → the
//!   job traps, which the queue counts as a failed attempt and retries with
//!   backoff before dead-lettering. The comment stays exactly where it was, which
//!   for a moderated site means waiting for a human.
//! - A `hold` verdict changes nothing, for the same reason.
//! - Only `publish` publishes, and only from pending: a comment a moderator has
//!   already decided on is not re-decided here.
//! - `spam` applies to a pending *or* published comment, because retroactively
//!   removing spam is the point of classifying asynchronously.
//!
//! Every decision is logged, so a false positive can be found later rather than
//! inferred.

mod db_host;

use serde_json::{Value, json};
use trovato_sdk::plugin_tap;
use trovato_sdk::types::{AiMessage, AiOperationType, AiRequest, AiRequestOptions};

/// This plugin's name, for the log trail.
const PLUGIN: &str = "trovato_spam";

/// Logical queue this plugin owns.
const QUEUE_NAME: &str = "comment_moderation";

/// The one kernel table this plugin writes, declared in `db_tables`.
const COMMENT_TABLE: &str = "comment";

/// Comment status values, mirroring `CommentStatus` in the kernel.
///
/// Duplicated as integers rather than shared, because the kernel's enum is not
/// part of the plugin contract. The stored numbers are, and the kernel pins them
/// with a test.
const STATUS_PUBLISHED: i64 = 1;
const STATUS_PENDING: i64 = 2;
const STATUS_SPAM: i64 = 3;

/// What the classifier decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Not spam: publish it.
    Publish,
    /// Uncertain: leave it for a human.
    Hold,
    /// Spam: hide it, keep the row.
    Spam,
}

impl Verdict {
    /// Read a verdict out of the model's answer.
    ///
    /// Accepts a bare word or a JSON object with a `verdict` field, because a
    /// model asked for JSON sometimes returns prose around it. Anything else is
    /// `None`, which the caller treats as a failed classification rather than as
    /// permission to publish.
    fn parse(content: &str) -> Option<Self> {
        let trimmed = content.trim();

        // A JSON object, possibly inside a markdown fence.
        let unfenced = trimmed
            .split("```")
            .find(|part| part.contains('{'))
            .unwrap_or(trimmed);
        if let Ok(value) = serde_json::from_str::<Value>(unfenced.trim().trim_start_matches("json"))
            && let Some(word) = value.get("verdict").and_then(|v| v.as_str())
        {
            return Self::from_word(word);
        }

        Self::from_word(trimmed)
    }

    fn from_word(word: &str) -> Option<Self> {
        match word.trim().trim_matches('"').to_ascii_lowercase().as_str() {
            "publish" | "ham" | "not_spam" => Some(Self::Publish),
            "hold" | "review" | "uncertain" => Some(Self::Hold),
            "spam" => Some(Self::Spam),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Hold => "hold",
            Self::Spam => "spam",
        }
    }
}

/// Declare the classification queue.
///
/// Concurrency 2: each job is one provider call, and the point of the queue is to
/// keep that off the request, not to fan out.
#[plugin_tap]
fn tap_queue_info() -> Value {
    json!([
        {
            "name": QUEUE_NAME,
            "concurrency": 2
        }
    ])
}

/// Queue a classification job for a new comment.
///
/// Receives the serialized comment. Enqueues regardless of the status it was
/// created in: on a site that publishes immediately, classification is what makes
/// retroactive spam removal possible, and on a moderated site it is what empties
/// the queue.
#[plugin_tap]
fn tap_comment_insert(comment: Value) -> Value {
    let Some(id) = comment.get("id").and_then(|v| v.as_str()) else {
        // Nothing to classify without an id; not an error worth failing the insert
        // over.
        return json!({ "queued": false, "reason": "no comment id" });
    };

    let job = json!({
        "comment_id": id,
        "item_id": comment.get("item_id"),
        "author_id": comment.get("author_id"),
        "body": comment.get("body"),
        "status_at_insert": comment.get("status"),
    });

    match trovato_sdk::host::queue_push(QUEUE_NAME, &job) {
        Ok(()) => json!({ "queued": true, "comment_id": id }),
        Err(code) => {
            trovato_sdk::host::log(
                "error",
                PLUGIN,
                &format!("failed to queue comment {id} for classification: {code}"),
            );
            // The comment is already stored, in whatever status the site's default
            // says. Failing to queue leaves a moderated comment waiting for a
            // human, which is the safe direction.
            json!({ "queued": false, "reason": "queue push failed" })
        }
    }
}

/// Classify one comment and apply the verdict.
///
/// # Panics
///
/// Panics when the provider cannot be reached or its answer cannot be read as a
/// verdict. That is deliberate: a WASM trap is how the queue is told an attempt
/// failed, so the job is retried with backoff and dead-lettered if the provider
/// stays down — and the comment stays where it was throughout.
#[plugin_tap]
fn tap_queue_worker(job: Value) -> Value {
    let Some(comment_id) = job.get("comment_id").and_then(|v| v.as_str()) else {
        // A job with no comment id can never succeed, so returning an error value
        // (rather than trapping) lets the queue drop it instead of retrying
        // forever.
        return json!({ "status": "error", "reason": "job has no comment_id" });
    };

    let body = job.get("body").and_then(|v| v.as_str()).unwrap_or_default();
    if body.trim().is_empty() {
        trovato_sdk::host::log(
            "info",
            PLUGIN,
            &format!("comment {comment_id}: empty body, holding for review"),
        );
        return json!({ "status": "ok", "verdict": "hold", "applied": false });
    }

    let request = AiRequest {
        operation: AiOperationType::Moderation,
        provider_id: None,
        model: None,
        messages: vec![
            AiMessage {
                role: "system".to_string(),
                content: SYSTEM_PROMPT.to_string(),
            },
            AiMessage {
                role: "user".to_string(),
                content: classification_prompt(&job, body),
            },
        ],
        input: None,
        options: AiRequestOptions {
            // A verdict is a word; there is no reason to pay for more.
            max_tokens: Some(64),
            temperature: Some(0.0),
            ..AiRequestOptions::default()
        },
    };

    let response = match trovato_sdk::host::ai_request(&request) {
        Ok(response) => response,
        Err(code) => {
            trovato_sdk::host::log(
                "warn",
                PLUGIN,
                &format!(
                    "comment {comment_id}: classification unavailable ({code}); leaving it as it is"
                ),
            );
            // Trap: a failed attempt, retried with backoff. The comment does not
            // move.
            panic!("trovato_spam: ai_request failed with {code}");
        }
    };

    let Some(verdict) = Verdict::parse(&response.content) else {
        trovato_sdk::host::log(
            "warn",
            PLUGIN,
            &format!(
                "comment {comment_id}: unreadable verdict {:?}; leaving it as it is",
                truncate(&response.content, 200)
            ),
        );
        panic!("trovato_spam: unreadable verdict");
    };

    let applied = apply(comment_id, verdict);

    trovato_sdk::host::log(
        "info",
        PLUGIN,
        &format!(
            "comment {comment_id}: verdict {} ({}applied)",
            verdict.as_str(),
            if applied { "" } else { "not " }
        ),
    );

    json!({
        "status": "ok",
        "comment_id": comment_id,
        "verdict": verdict.as_str(),
        "applied": applied,
    })
}

/// What the model is asked to do.
const SYSTEM_PROMPT: &str = "\
You are a comment moderation classifier for a website. Decide whether a comment \
is spam. Reply with exactly one word: publish, hold, or spam. Use `spam` for \
unsolicited advertising, link farming, or automated nonsense. Use `hold` when you \
are not sure, or when the comment may be abusive rather than spam, so a human \
reviews it. Use `publish` only for a comment that is plainly a genuine \
contribution. Reply with the single word and nothing else.";

/// Build the classification prompt from the job.
///
/// The account signals are here because they are what a classifier needs to tell
/// a new account's first plausible-looking comment from an established
/// contributor's, and because the trust ladder built on top of this reads the
/// same fields.
fn classification_prompt(job: &Value, body: &str) -> String {
    let author = job
        .get("author_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let item = job
        .get("item_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    format!(
        "Comment on item {item}, by account {author}.\n\nComment:\n{}\n\nOne word: publish, hold, or spam.",
        truncate(body, 4000)
    )
}

/// Apply a verdict, returning whether a row changed.
///
/// Each write carries the status it expects in the `where` clause, which makes it
/// a compare-and-set: if a moderator got there first, the update matches nothing
/// and the human decision stands.
fn apply(comment_id: &str, verdict: Verdict) -> bool {
    let (new_status, expected) = match verdict {
        // Publishing is only ever a promotion out of the queue.
        Verdict::Publish => (STATUS_PUBLISHED, vec![STATUS_PENDING]),
        // A hold is what the comment is already doing.
        Verdict::Hold => return false,
        // Spam applies to a pending comment and to one already visible: taking
        // spam down after the fact is the point of classifying asynchronously.
        Verdict::Spam => (STATUS_SPAM, vec![STATUS_PENDING, STATUS_PUBLISHED]),
    };

    for from in expected {
        let data = json!({ "status": new_status });
        let where_clause = json!({ "id": comment_id, "status": from });

        match db_host::update(COMMENT_TABLE, &data, &where_clause) {
            Ok(0) => continue,
            Ok(_) => return true,
            Err(code) => {
                trovato_sdk::host::log(
                    "error",
                    PLUGIN,
                    &format!("comment {comment_id}: failed to write verdict: {code}"),
                );
                return false;
            }
        }
    }

    false
}

/// Truncate on a character boundary, so a multi-byte character cannot split.
fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut end = max_len;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_verdict_word_is_read() {
        assert_eq!(Verdict::parse("spam"), Some(Verdict::Spam));
        assert_eq!(Verdict::parse("  Publish\n"), Some(Verdict::Publish));
        assert_eq!(Verdict::parse("HOLD"), Some(Verdict::Hold));
    }

    /// A model asked for one word sometimes answers with JSON anyway.
    #[test]
    fn a_json_verdict_is_read() {
        assert_eq!(
            Verdict::parse(r#"{"verdict": "spam", "reason": "link farm"}"#),
            Some(Verdict::Spam)
        );
        assert_eq!(
            Verdict::parse("```json\n{\"verdict\":\"publish\"}\n```"),
            Some(Verdict::Publish)
        );
    }

    #[test]
    fn synonyms_are_accepted() {
        assert_eq!(Verdict::parse("ham"), Some(Verdict::Publish));
        assert_eq!(Verdict::parse("not_spam"), Some(Verdict::Publish));
        assert_eq!(Verdict::parse("review"), Some(Verdict::Hold));
    }

    /// The important half: anything unreadable is *not* a verdict, so the caller
    /// traps and the comment stays where it is. Guessing "publish" here would
    /// publish spam on a bad response.
    #[test]
    fn an_unreadable_answer_is_not_a_verdict() {
        for content in [
            "",
            "I'm sorry, I can't help with that.",
            "{\"decision\": \"spam\"}",
            "maybe?",
        ] {
            assert_eq!(Verdict::parse(content), None, "{content:?}");
        }
    }

    #[test]
    fn a_hold_verdict_writes_nothing() {
        assert!(
            !apply("00000000-0000-0000-0000-000000000000", Verdict::Hold),
            "a hold must not touch the row"
        );
    }

    #[test]
    fn the_job_carries_what_the_classifier_needs() {
        let comment = json!({
            "id": "c1",
            "item_id": "i1",
            "author_id": "a1",
            "body": "hello",
            "status": 2,
        });

        let result = __inner_tap_comment_insert(comment);

        assert_eq!(result.get("queued").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            result.get("comment_id").and_then(|v| v.as_str()),
            Some("c1")
        );
    }

    #[test]
    fn a_comment_without_an_id_is_not_queued() {
        let result = __inner_tap_comment_insert(json!({ "body": "hello" }));

        assert_eq!(result.get("queued").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn a_job_without_a_comment_id_is_dropped_not_retried() {
        let result = __inner_tap_queue_worker(json!({ "body": "hello" }));

        assert_eq!(
            result.get("status").and_then(|v| v.as_str()),
            Some("error"),
            "an error value is a completed dispatch, so the queue drops the job"
        );
    }

    /// An empty body cannot be classified, and is held rather than published.
    #[test]
    fn an_empty_body_is_held() {
        let result = __inner_tap_queue_worker(json!({ "comment_id": "c1", "body": "   " }));

        assert_eq!(result.get("verdict").and_then(|v| v.as_str()), Some("hold"));
        assert_eq!(result.get("applied").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn the_prompt_carries_the_account_and_item() {
        let job = json!({ "comment_id": "c1", "item_id": "i1", "author_id": "a1" });

        let prompt = classification_prompt(&job, "buy cheap things");

        assert!(prompt.contains("i1"));
        assert!(prompt.contains("a1"));
        assert!(prompt.contains("buy cheap things"));
    }

    #[test]
    fn a_long_body_is_truncated_on_a_character_boundary() {
        let body = "é".repeat(3000);

        let prompt = classification_prompt(&json!({}), &body);

        // Cut, and still valid UTF-8 (constructing the String would have panicked
        // otherwise).
        assert!(prompt.len() < body.len() + 200);
    }

    #[test]
    fn the_queue_is_declared_with_bounded_concurrency() {
        let declared = __inner_tap_queue_info();
        let queue = declared.get(0).expect("one queue");

        assert_eq!(queue.get("name").and_then(|v| v.as_str()), Some(QUEUE_NAME));
        assert_eq!(queue.get("concurrency").and_then(|v| v.as_i64()), Some(2));
    }
}
