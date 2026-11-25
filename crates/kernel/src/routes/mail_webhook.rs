//! The mail provider's bounce and complaint reports (BG18 part 2,
//! `docs/design/mail-queue.md` §4).
//!
//! `POST /mail/webhook/mailgun` takes Mailgun's webhook JSON. **No session and
//! no CSRF token, on purpose**: the provider posts with neither. It is
//! authenticated instead by the signature Mailgun puts in every payload:
//! `signature.signature` must be the hex `HMAC-SHA256` of
//! `signature.timestamp` followed by `signature.token`, keyed with
//! `MAIL_WEBHOOK_SIGNING_KEY`, compared in constant time. The timestamp must
//! be within five minutes of now and the token unseen for ten (Redis
//! `SET NX EX`), so a captured request cannot be replayed. With no key
//! configured, or no mail configured at all, the route answers 404. Rate
//! limited per client IP in the `mail_webhook` bucket.
//!
//! A `failed` event of `severity: permanent` is a hard bounce and a
//! `complained` event a complaint ([`mail_log::record_feedback`]); every other
//! event is acknowledged and ignored. The answer is 200 once an event is
//! recorded or ignored, 401 for a bad signature or an old timestamp, 406 for a
//! payload that cannot be read (Mailgun does not retry a 406), and 503 when the
//! replay check or the database fails, so Mailgun tries again.
//!
//! Reference: <https://documentation.mailgun.com/docs/mailgun/user-manual/webhooks/securing-webhooks/>.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

use crate::services::mail_log::{self, Feedback};
use crate::state::AppState;

/// How far a signature's timestamp may be from now, in seconds.
const MAX_SKEW_SECS: i64 = 300;

/// How long a token is remembered against replay, in seconds: longer than the
/// window above, so a token cannot outlive its record.
const REPLAY_TTL_SECS: u64 = 600;

/// Longest token accepted. Mailgun's are 50 characters.
const MAX_TOKEN_LEN: usize = 256;

pub fn router() -> Router<AppState> {
    Router::new().route("/mail/webhook/mailgun", post(mailgun))
}

#[derive(Debug, Deserialize)]
struct Payload {
    signature: Signature,
    #[serde(rename = "event-data")]
    event: EventData,
}

#[derive(Debug, Deserialize)]
struct Signature {
    timestamp: String,
    token: String,
    signature: String,
}

#[derive(Debug, Default, Deserialize)]
struct EventData {
    #[serde(default)]
    event: String,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    recipient: String,
    #[serde(default)]
    reason: String,
    #[serde(default, rename = "delivery-status")]
    delivery_status: DeliveryStatus,
    #[serde(default)]
    message: Message,
}

#[derive(Debug, Default, Deserialize)]
struct DeliveryStatus {
    #[serde(default)]
    code: Option<serde_json::Value>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Default, Deserialize)]
struct Message {
    #[serde(default)]
    headers: Headers,
}

#[derive(Debug, Default, Deserialize)]
struct Headers {
    #[serde(default, rename = "message-id")]
    message_id: String,
}

/// Whether `signature` is the hex HMAC-SHA256 of `timestamp + token` under
/// `key`. Constant time in the comparison (`Mac::verify_slice`).
pub fn signature_valid(key: &str, timestamp: &str, token: &str, signature: &str) -> bool {
    let Ok(expected) = hex::decode(signature.trim()) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key.as_bytes()) else {
        return false;
    };
    mac.update(timestamp.as_bytes());
    mac.update(token.as_bytes());
    mac.verify_slice(&expected).is_ok()
}

/// Whether a signature's `timestamp` (Unix seconds) is close enough to `now`.
fn timestamp_fresh(timestamp: &str, now: i64) -> bool {
    timestamp
        .trim()
        .parse::<i64>()
        .is_ok_and(|t| now.abs_diff(t) <= MAX_SKEW_SECS.unsigned_abs())
}

/// What an event asks the kernel to record, if anything.
/// Mailgun also reports a permanent failure for an address it suppressed
/// itself (`suppress-complaint` is an earlier complaint; `suppress-unsubscribe`
/// an unsubscribe from Mailgun's own list, not a dead address), for a message
/// it gave up retrying (`old`), and for a block by the receiving provider
/// (`espblock`). None of the last three says the address does not exist.
fn feedback_of(event: &EventData) -> Option<Feedback> {
    match event.event.as_str() {
        "failed" if event.severity == "permanent" => match event.reason.as_str() {
            "suppress-complaint" => Some(Feedback::Complaint),
            "suppress-unsubscribe" | "old" | "espblock" => None,
            _ => Some(Feedback::HardBounce),
        },
        "complained" => Some(Feedback::Complaint),
        _ => None,
    }
}

/// The provider's reason for a bounce, for the outbox row and the list.
fn bounce_detail(event: &EventData) -> String {
    let status = &event.delivery_status;
    let text = [&status.description, &status.message, &event.reason]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("permanent failure");
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match status.code.as_ref() {
        Some(code) if !code.is_null() => format!("{} {text}", code.to_string().trim_matches('"')),
        _ => text,
    }
}

/// POST /mail/webhook/mailgun
async fn mailgun(State(state): State<AppState>, body: Bytes) -> Response {
    let Some(key) = state
        .email()
        .and_then(|email| email.config().webhook_signing_key.clone())
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(payload) = serde_json::from_slice::<Payload>(&body) else {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    };
    let sig = &payload.signature;
    if sig.token.is_empty()
        || sig.token.len() > MAX_TOKEN_LEN
        || !signature_valid(&key.0, &sig.timestamp, &sig.token, &sig.signature)
    {
        tracing::warn!("mail webhook: refused a request with a bad signature");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !timestamp_fresh(&sig.timestamp, chrono::Utc::now().timestamp()) {
        tracing::warn!("mail webhook: refused a signature outside the time window");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match first_sight(&state, &sig.token).await {
        Ok(true) => {}
        // Seen already: this event was handled. 200 so a retry stops.
        Ok(false) => return StatusCode::OK.into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "mail webhook: the replay check failed");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }

    let event = &payload.event;
    let Some(feedback) = feedback_of(event) else {
        return StatusCode::OK.into_response();
    };
    let address = event.recipient.trim();
    if address.is_empty() || address.parse::<lettre::Address>().is_err() {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let outbox_id = mail_log::outbox_id_from_message_id(&event.message.headers.message_id);
    let detail = match feedback {
        Feedback::HardBounce => bounce_detail(event),
        Feedback::Complaint => "reported as spam".to_string(),
    };
    match mail_log::record_feedback(state.db(), feedback, address, outbox_id, &detail).await {
        Ok(()) => {
            tracing::info!(?feedback, ?outbox_id, "mail webhook: recorded");
            StatusCode::OK.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "mail webhook: could not record the event");
            // Let the token be used again: the retry carries the same one.
            forget(&state, &sig.token).await;
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// The Redis key a token is remembered under.
fn replay_key(token: &str) -> String {
    format!("mail_webhook:token:{token}")
}

/// Remember `token`; `true` the first time it is seen.
async fn first_sight(state: &AppState, token: &str) -> anyhow::Result<bool> {
    let mut conn = state.redis().get_multiplexed_async_connection().await?;
    let set: Option<String> = redis::cmd("SET")
        .arg(replay_key(token))
        .arg(1)
        .arg("NX")
        .arg("EX")
        .arg(REPLAY_TTL_SECS)
        .query_async(&mut conn)
        .await?;
    Ok(set.is_some())
}

/// Forget `token`, so the provider's retry of an event that failed is taken.
async fn forget(state: &AppState, token: &str) {
    let forgotten: anyhow::Result<()> = async {
        let mut conn = state.redis().get_multiplexed_async_connection().await?;
        redis::cmd("DEL")
            .arg(replay_key(token))
            .query_async::<()>(&mut conn)
            .await?;
        Ok(())
    }
    .await;
    if let Err(e) = forgotten {
        // The retry will be taken for a replay and acknowledged unrecorded.
        tracing::error!(error = %e, "mail webhook: could not release a token after a failure; the retry of this event will be ignored");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sign(key: &str, timestamp: &str, token: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(token.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn a_signature_over_timestamp_and_token_is_accepted() {
        let sig = sign("key-1", "1700000000", "tok");
        assert!(signature_valid("key-1", "1700000000", "tok", &sig));
        assert!(signature_valid(
            "key-1",
            "1700000000",
            "tok",
            &sig.to_uppercase()
        ));
    }

    #[test]
    fn any_change_is_refused() {
        let sig = sign("key-1", "1700000000", "tok");
        assert!(!signature_valid("key-2", "1700000000", "tok", &sig));
        assert!(!signature_valid("key-1", "1700000001", "tok", &sig));
        assert!(!signature_valid("key-1", "1700000000", "tok2", &sig));
        // The boundary between timestamp and token is not free to move.
        assert!(!signature_valid(
            "key-1",
            "170000000",
            "0tok",
            &sign("key-1", "1700000000", "0tok")
        ));
        assert!(!signature_valid("key-1", "1700000000", "tok", &sig[..62]));
        assert!(!signature_valid("key-1", "1700000000", "tok", "not hex"));
        assert!(!signature_valid("key-1", "1700000000", "tok", ""));
    }

    #[test]
    fn timestamps_outside_five_minutes_are_stale() {
        let now = 1_700_000_000;
        assert!(timestamp_fresh("1700000000", now));
        assert!(timestamp_fresh("1699999700", now));
        assert!(timestamp_fresh("1700000300", now));
        assert!(!timestamp_fresh("1699999699", now));
        assert!(!timestamp_fresh("1700000301", now));
        assert!(!timestamp_fresh("soon", now));
        assert!(!timestamp_fresh("", now));
    }

    #[test]
    fn only_permanent_failures_and_complaints_are_recorded() {
        let event = |event: &str, severity: &str| EventData {
            event: event.to_string(),
            severity: severity.to_string(),
            ..EventData::default()
        };
        assert_eq!(
            feedback_of(&event("failed", "permanent")),
            Some(Feedback::HardBounce)
        );
        assert_eq!(feedback_of(&event("failed", "temporary")), None);
        assert_eq!(
            feedback_of(&event("complained", "")),
            Some(Feedback::Complaint)
        );
        assert_eq!(feedback_of(&event("delivered", "")), None);
        assert_eq!(feedback_of(&event("unsubscribed", "")), None);
        let permanent = |reason: &str| EventData {
            reason: reason.to_string(),
            ..event("failed", "permanent")
        };
        assert_eq!(
            feedback_of(&permanent("bounce")),
            Some(Feedback::HardBounce)
        );
        assert_eq!(
            feedback_of(&permanent("suppress-bounce")),
            Some(Feedback::HardBounce)
        );
        assert_eq!(
            feedback_of(&permanent("suppress-complaint")),
            Some(Feedback::Complaint)
        );
        for reason in ["suppress-unsubscribe", "old", "espblock"] {
            assert_eq!(feedback_of(&permanent(reason)), None, "{reason}");
        }
    }

    #[test]
    fn the_bounce_detail_carries_the_code_and_reason() {
        let event = EventData {
            delivery_status: DeliveryStatus {
                code: Some(serde_json::json!(550)),
                description: String::new(),
                message: "5.1.1 No such\r\nuser".to_string(),
            },
            ..EventData::default()
        };
        assert_eq!(bounce_detail(&event), "550 5.1.1 No such  user");
        assert_eq!(bounce_detail(&EventData::default()), "permanent failure");
    }
}
