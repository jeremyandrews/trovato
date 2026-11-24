#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Performance tests: what each page costs, in exact counts (BG14).
//!
//! Each test loads one page of a generated site (see `common::perf`) as each
//! kind of visitor it has, with cold and with warm caches, and compares the
//! database statements, cache lookups, cache misses, cache writes, plugin
//! calls, host calls and the statements' fingerprints with
//! `tests/fixtures/performance_counts.toml`. Counts, not timings, so the result
//! is the same on every machine and every run.
//!
//! A page that starts doing more (or less) work fails here until the expected
//! numbers change in the same commit. `TROVATO_PERF_RECORD=1` rewrites the
//! file from what the pages do now.
//!
//! Requires Postgres + Redis; runs in CI.

mod common;

use common::perf::{Caches, PerfSite, Visitor, check, expected, shared_site};
use common::run_test;

const BOTH: [Caches; 2] = [Caches::Cold, Caches::Warm];

async fn measure(site: &PerfSite, page: &str, visitors: &[Visitor], path: &str) {
    for &visitor in visitors {
        for caches in BOTH {
            check(&site.visit(page, visitor, caches, path).await);
        }
    }
}

#[test]
fn front_page() {
    run_test(async {
        let site = shared_site();
        measure(site, "front", &[Visitor::Anonymous, Visitor::User], "/").await;
    });
}

#[test]
fn item_page_with_comments() {
    run_test(async {
        let site = shared_site();
        let path = site.item_path.clone();
        measure(site, "item", &[Visitor::Anonymous, Visitor::User], &path).await;
    });
}

#[test]
fn category_page() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "category",
            &[Visitor::Anonymous, Visitor::User],
            "/blog/tag/topic-1",
        )
        .await;
    });
}

#[test]
fn feed() {
    run_test(async {
        let site = shared_site();
        measure(site, "feed", &[Visitor::Anonymous], "/blog/feed.xml").await;
    });
}

#[test]
fn search() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "search",
            &[Visitor::Anonymous, Visitor::User],
            "/search?q=river",
        )
        .await;
    });
}

#[test]
fn login_page() {
    run_test(async {
        let site = shared_site();
        measure(site, "login", &[Visitor::Anonymous], "/user/login").await;
    });
}

#[test]
fn admin_content_list() {
    run_test(async {
        let site = shared_site();
        measure(site, "admin_content", &[Visitor::Admin], "/admin/content").await;
    });
}

/// The harness bites: one more statement on the item page is a failure that
/// names the statement. The extra statement comes from the test router's
/// `x-perf-extra-query` probe, inside the request's profile.
#[test]
fn one_more_query_on_the_item_page_fails_its_test() {
    run_test(async {
        let site = shared_site();
        let path = site.item_path.clone();
        let visit = site
            .visit_with("item", Visitor::Anonymous, Caches::Warm, &path, true)
            .await;
        if std::env::var_os("TROVATO_PERF_RECORD").is_some() {
            return;
        }
        let expected = expected(&visit.key).expect("item.anonymous.warm is recorded");
        assert_eq!(visit.counts.queries, expected.queries + 1);
        let diff = expected
            .mismatch(&visit.counts)
            .expect("one more query must not match the recorded counts");
        assert!(
            diff.contains(&format!(
                "queries: expected {}, got {}",
                expected.queries,
                expected.queries + 1
            )),
            "{diff}"
        );
        let select_one = trovato_kernel::telemetry::fingerprint::fingerprint("SELECT 1");
        assert!(
            diff.contains(&format!(
                "{select_one}: expected 0x, got 1x\n      select ?"
            )),
            "{diff}"
        );
        // And the same check the page test makes panics on it.
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&visit)));
        assert!(
            caught.is_err(),
            "check() accepted a page doing one more query"
        );
    });
}

/// The generator is deterministic and does not write twice: the same options
/// on the generated site write nothing, different ones are refused, and the
/// site holds exactly what was asked for, with the ids the seed gives.
#[test]
fn the_generated_site_is_what_was_asked_for() {
    run_test(async {
        let site = shared_site();
        let db = &site.app.db;
        let options = common::perf::site_options();

        let again = trovato_kernel::generate::generate(db, &options, "unused", |_| {})
            .await
            .expect("the same options again");
        assert!(again.already_present, "the same options wrote again");

        let other = trovato_kernel::generate::GenerateOptions {
            items: options.items + 1,
            ..options.clone()
        };
        assert!(
            trovato_kernel::generate::generate(db, &other, "unused", |_| {})
                .await
                .is_err(),
            "different options on a generated site were not refused"
        );

        let count = |sql: &'static str| async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(db)
                .await
                .unwrap()
        };
        assert_eq!(
            count("SELECT count(*) FROM item WHERE type = 'blog'").await,
            i64::try_from(options.items).unwrap()
        );
        assert_eq!(
            count("SELECT count(*) FROM item_revision").await,
            i64::try_from(options.items).unwrap()
        );
        assert_eq!(
            count("SELECT count(*) FROM comment").await,
            i64::try_from(options.comments).unwrap()
        );
        assert_eq!(
            count("SELECT count(*) FROM users WHERE name LIKE 'gen\\_user\\_%'").await,
            i64::try_from(options.users).unwrap()
        );
        // Ids come from the seed and the row number.
        assert_eq!(
            count("SELECT count(*) FROM item WHERE id = md5('1:item:40')::uuid").await,
            1
        );
        // The comment count each post carries matches its comments.
        assert_eq!(
            count(
                "SELECT count(*) FROM item i WHERE i.comment_count <> \
                 (SELECT count(*) FROM comment c WHERE c.item_id = i.id)"
            )
            .await,
            0
        );
    });
}
