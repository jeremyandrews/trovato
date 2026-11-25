//! Trovato Search plugin — Pagefind client-side search index.
//!
//! Installing this plugin is what switches the static search index on: its
//! `pagefind_index_status` table is the kernel's signal that the site wants
//! the index, and carries an operator's request for a full rebuild.
//!
//! The index itself is kept up to date by the kernel. Every write that changes
//! what it holds queues the items it touched (the item and comment services,
//! the content type and search field admin, workspace publishing, URL alias
//! writes, and plugins through `host::search_index_enqueue`), and the kernel's
//! cron rewrites only those items' part of the index.
//!
//! This plugin used to poll `MAX(changed)` over published items each cron run
//! and ask for a full rebuild when it moved. With every writer queuing its own
//! items that poll would turn each edit back into a full rebuild, so its
//! `tap_cron` now only reports how many items are waiting.

use trovato_sdk::host;
use trovato_sdk::prelude::*;

/// Report how many items are queued for the search index.
#[plugin_tap]
pub fn tap_cron(_input: CronInput) -> serde_json::Value {
    let queued = host::query_raw("SELECT count(*) AS queued FROM search_index_queue", &[])
        .ok()
        .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(&json).ok())
        .and_then(|rows| {
            rows.first()
                .and_then(|r| r.get("queued"))
                .and_then(|v| v.as_i64())
        });
    match queued {
        Some(queued) => serde_json::json!({"queued": queued}),
        None => serde_json::json!({"error": "failed to read the search index queue"}),
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
            result.get("error").is_some() || result.get("queued").is_some(),
            "Should return either error or the queue length"
        );
    }
}
