#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The rate-limit categories comment writes and AI search endpoints resolve to.
//!
//! `categorize_path` sent every `/api/...` path to the `api` category at 100 a
//! minute. That covered comment posts — nobody writes a hundred comments a
//! minute, but a spammer with an account does — and the three AI search
//! endpoints, which spend provider tokens on every call.
//!
//! The mapping itself is unit-tested in `middleware::rate_limit`. What is pinned
//! here is that the mapping and the configured limits meet: a category is only
//! useful if the limiter actually enforces its number, and the previous defect
//! was precisely a category resolving to someone else's limit.
//!
//! Requires Redis (through the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::middleware::categorize_path;
use uuid::Uuid;

/// Exhaust `category` for a fresh identifier and return how many requests were
/// allowed before the limiter refused.
///
/// The identifier is unique per call, so this counts only its own requests: the
/// limiter's counters live in a Redis instance every test binary shares.
async fn allowed_before_refusal(app: &TestApp, category: &str, ceiling: u32) -> u32 {
    let identifier = format!("test:{}", Uuid::now_v7().simple());
    let mut allowed = 0;

    for _ in 0..ceiling {
        match app.state.rate_limiter().check(category, &identifier).await {
            Ok(()) => allowed += 1,
            Err(_) => return allowed,
        }
    }

    allowed
}

/// A comment post is bounded by the comment category, not by the generic api
/// bucket it used to share.
#[test]
fn comment_writes_are_limited_well_below_the_api_bucket() {
    run_test(async {
        let app = shared_app().await;

        let category = categorize_path(&format!("/api/item/{}/comments", Uuid::now_v7()), "POST");
        assert_eq!(category, "comment");

        // Ceiling well above the comment limit and well below the api limit, so
        // this both proves the limit exists and would fail loudly if the category
        // ever resolved back to `api`.
        let allowed = allowed_before_refusal(app, category, 20).await;

        assert!(
            allowed < 20,
            "comment writes must be refused inside 20 requests a minute, {allowed} were allowed"
        );
        assert!(
            allowed > 0,
            "the first comment of the minute must be allowed"
        );
    });
}

/// Each AI search endpoint enforces its own limit, and the cheapest allows more
/// than the most expensive. That ordering is the reason they are three categories
/// rather than one.
#[test]
fn the_three_ai_search_endpoints_enforce_separate_limits() {
    run_test(async {
        let app = shared_app().await;

        let expand = categorize_path("/api/v1/search/expand", "POST");
        let summarize = categorize_path("/api/v1/search/summarize", "POST");
        let followup = categorize_path("/api/v1/search/followup", "POST");
        assert_eq!(
            (expand, summarize, followup),
            ("search_expand", "search_summarize", "search_followup")
        );

        // 40 is above every AI search limit and below the api limit of 100, so a
        // category that fell back to `api` would exhaust the ceiling instead of
        // being refused.
        let expand_allowed = allowed_before_refusal(app, expand, 40).await;
        let summarize_allowed = allowed_before_refusal(app, summarize, 40).await;
        let followup_allowed = allowed_before_refusal(app, followup, 40).await;

        for (category, allowed) in [
            (expand, expand_allowed),
            (summarize, summarize_allowed),
            (followup, followup_allowed),
        ] {
            assert!(
                allowed > 0 && allowed < 40,
                "{category} must enforce a limit inside 40 requests, {allowed} were allowed"
            );
        }

        assert!(
            expand_allowed > summarize_allowed,
            "expansion is the cheapest call and must allow the most: \
             expand {expand_allowed}, summarize {summarize_allowed}"
        );
        assert!(
            summarize_allowed > followup_allowed,
            "follow-up is the most expensive call and must allow the fewest: \
             summarize {summarize_allowed}, followup {followup_allowed}"
        );
    });
}

/// Reading is not writing. A page loading a comment thread must not spend the
/// budget for posting one.
#[test]
fn reading_comments_stays_in_the_api_bucket() {
    run_test(async {
        let app = shared_app().await;

        let category = categorize_path(&format!("/api/item/{}/comments", Uuid::now_v7()), "GET");
        assert_eq!(category, "api");

        // Twenty reads is nothing to the api bucket, and would be refused by the
        // comment one.
        let allowed = allowed_before_refusal(app, category, 20).await;

        assert_eq!(
            allowed, 20,
            "reads must not be bounded by the comment write limit"
        );
    });
}
