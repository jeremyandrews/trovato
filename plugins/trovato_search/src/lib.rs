//! Trovato Search plugin — Pagefind client-side search index.
//!
//! Detects content changes via `tap_cron` and signals the kernel to
//! rebuild the Pagefind search index when published live-stage content
//! has been modified since the last index build.

use trovato_sdk::host;
use trovato_sdk::prelude::*;
use trovato_sdk::types::LIVE_STAGE_UUID;

/// Check for content changes and request a Pagefind index rebuild if needed.
///
/// Compares `MAX(changed)` of published live-stage items against the
/// stored `last_indexed_at` timestamp. If content is newer, sets
/// `rebuild_requested = true` so the kernel cron task picks it up.
#[plugin_tap]
pub fn tap_cron(_input: CronInput) -> serde_json::Value {
    // Get the most recent change timestamp for published live-stage items
    let max_changed_json = host::query_raw(
        "SELECT COALESCE(MAX(changed), 0) as max_changed \
         FROM item WHERE status = 1 AND stage_id = $1::uuid",
        &[serde_json::json!(LIVE_STAGE_UUID)],
    );

    let max_changed: i64 = match max_changed_json {
        Ok(json_str) => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
            rows.first()
                .and_then(|r| r.get("max_changed"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
        }
        Err(_) => return serde_json::json!({"error": "failed to query max changed"}),
    };

    // Get the last indexed timestamp
    let status_json = host::query_raw(
        "SELECT last_indexed_at, rebuild_requested \
         FROM pagefind_index_status WHERE id = 1",
        &[],
    );

    let (last_indexed_at, already_requested) = match status_json {
        Ok(json_str) => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
            let row = rows.first();
            let ts = row
                .and_then(|r| r.get("last_indexed_at"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let req = row
                .and_then(|r| r.get("rebuild_requested"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (ts, req)
        }
        Err(_) => return serde_json::json!({"error": "failed to query index status"}),
    };

    // If content is newer than last index and no rebuild already pending
    if max_changed > last_indexed_at && !already_requested {
        let _ = host::execute_raw(
            "UPDATE pagefind_index_status SET rebuild_requested = true WHERE id = 1",
            &[],
        );
        serde_json::json!({"rebuild_requested": true, "max_changed": max_changed, "last_indexed_at": last_indexed_at})
    } else {
        serde_json::json!({"rebuild_requested": false, "max_changed": max_changed, "last_indexed_at": last_indexed_at})
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn tap_cron_returns_status() {
        let input = CronInput {
            timestamp: 1_700_000_000,
        };
        let result = __inner_tap_cron(input);
        // Stub host functions return errors, so we get the error path
        assert!(
            result.get("error").is_some() || result.get("rebuild_requested").is_some(),
            "Should return either error or rebuild status"
        );
    }
}
