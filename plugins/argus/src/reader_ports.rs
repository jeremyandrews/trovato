//! Per-user reader state over the `db` host (M3).
//!
//! The rules are in [`argus_core::reader`]; the SQL is here.
//!
//! # What is writable and what is not
//!
//! [`record_view`] is called from `tap_item_view`, which the kernel dispatches
//! on the story page with the authenticated user context **and** a live services
//! handle, so read state is genuinely maintained.
//!
//! [`apply_reaction`] and [`set_subscription`] had **no caller in 1.0**: no
//! kernel surface let an authenticated reader write a plugin-owned table
//! (`M3-DESIGN.md` Decision 5, `G-NO-PLUGIN-HTTP`). `KERNEL_API_VERSION (0,99)`
//! added one — a `tap_menu` entry with `handler_type = "api"` is dispatched to
//! `tap_api` with the authenticated user and a live services handle — so both
//! are now called from [`crate::reader_api`], and M3 deviation 5 is undone.

use argus_core::CoreResult;
use argus_core::reader::{Reaction, apply_reaction as decide_reaction};
use serde::Deserialize;
use serde_json::json;

use crate::host_ports::{exec, query_rows};

#[derive(Deserialize)]
struct ReactionRow {
    reaction_type: String,
}

/// Record that `user_id` opened `story_id` at `now`.
///
/// One idempotent statement: at-least-once delivery and a reader refreshing the
/// page are the same event as far as this row is concerned, and `first_seen_at`
/// never moves while `last_seen_at` never regresses (the invariants
/// [`argus_core::reader::ReadState`] states and tests).
///
/// # Errors
///
/// Transient [`argus_core::CoreError::Store`] when the write fails.
pub fn record_view(user_id: &str, story_id: &str, now: i64) -> CoreResult<()> {
    exec(
        "INSERT INTO argus_read_state (user_id, story_item_id, first_seen_at, last_seen_at, view_count) \
         VALUES ($1::uuid, $2::uuid, $3::bigint, $3::bigint, 1) \
         ON CONFLICT (user_id, story_item_id) DO UPDATE SET \
             first_seen_at = LEAST(argus_read_state.first_seen_at, EXCLUDED.first_seen_at), \
             last_seen_at  = GREATEST(argus_read_state.last_seen_at, EXCLUDED.last_seen_at), \
             view_count    = argus_read_state.view_count + 1",
        &[json!(user_id), json!(story_id), json!(now)],
    )?;
    Ok(())
}

/// The reactions `user_id` currently holds on `story_id`.
///
/// # Errors
///
/// Transient [`argus_core::CoreError::Store`] when the read fails.
pub fn load_reactions(user_id: &str, story_id: &str) -> CoreResult<Vec<Reaction>> {
    let rows: Vec<ReactionRow> = query_rows(
        "SELECT reaction_type FROM argus_reactions \
         WHERE user_id = $1::uuid AND story_item_id = $2::uuid ORDER BY reaction_type",
        &[json!(user_id), json!(story_id)],
    )?;
    // An unrecognized stored value is skipped rather than failing the read: a
    // row written by a future version must not break this one's page.
    Ok(rows
        .into_iter()
        .filter_map(|r| Reaction::parse(&r.reaction_type).ok())
        .collect())
}

/// Apply one reaction toggle for a reader, and report the resulting set.
///
/// Called from [`crate::reader_api`] on `POST /argus/story/:id/react`. Until
/// `KERNEL_API_VERSION (0,99)` this had no caller at all — there was no surface
/// through which an authenticated reader could write a plugin-owned table
/// (`G-NO-PLUGIN-HTTP`) — and was kept, with its tests, so the semantics were
/// settled rather than improvised the day a write path existed.
///
/// The decision (insert, toggle off, displace the opposite vote) is
/// [`argus_core::reader::apply_reaction`]; this is the storage half. There are
/// no transactions (`G-DB-NO-TX`), so the removes are ordered before the insert:
/// an interruption between them leaves the reader holding nothing, which is a
/// legal state, rather than holding both an upvote and a downvote, which is not.
///
/// # Errors
///
/// Transient [`argus_core::CoreError::Store`] when a read or write fails.
pub fn apply_reaction(
    user_id: &str,
    story_id: &str,
    reaction: Reaction,
    now: i64,
) -> CoreResult<Vec<Reaction>> {
    let current = load_reactions(user_id, story_id)?;
    let change = decide_reaction(&current, reaction);

    for gone in &change.remove {
        exec(
            "DELETE FROM argus_reactions \
             WHERE user_id = $1::uuid AND story_item_id = $2::uuid AND reaction_type = $3",
            &[json!(user_id), json!(story_id), json!(gone.as_str())],
        )?;
    }
    if let Some(added) = change.insert {
        exec(
            "INSERT INTO argus_reactions (user_id, story_item_id, reaction_type, created) \
             VALUES ($1::uuid, $2::uuid, $3, $4::bigint) \
             ON CONFLICT (user_id, story_item_id, reaction_type) DO NOTHING",
            &[
                json!(user_id),
                json!(story_id),
                json!(added.as_str()),
                json!(now),
            ],
        )?;
    }
    load_reactions(user_id, story_id)
}

/// Subscribe or unsubscribe `user_id` to `topic_id`.
///
/// Called from [`crate::reader_api`] on `PUT /argus/topic/:id/subscribe`.
///
/// # Errors
///
/// Transient [`argus_core::CoreError::Store`] when the write fails.
pub fn set_subscription(
    user_id: &str,
    topic_id: &str,
    subscribed: bool,
    now: i64,
) -> CoreResult<()> {
    if subscribed {
        exec(
            "INSERT INTO argus_subscriptions (user_id, topic_item_id, created) \
             VALUES ($1::uuid, $2::uuid, $3::bigint) \
             ON CONFLICT (user_id, topic_item_id) DO NOTHING",
            &[json!(user_id), json!(topic_id), json!(now)],
        )?;
    } else {
        exec(
            "DELETE FROM argus_subscriptions \
             WHERE user_id = $1::uuid AND topic_item_id = $2::uuid",
            &[json!(user_id), json!(topic_id)],
        )?;
    }
    Ok(())
}
