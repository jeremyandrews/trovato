#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Performance tests: what each page costs, in exact counts (BG14).
//!
//! Each test loads one page of a generated site (see `common::perf`) as each
//! kind of visitor it has, with cold and with warm caches, and compares the
//! database statements, cache lookups, cache misses, cache writes, plugin
//! calls, host calls, gathers, renders and the statements' fingerprints with
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

/// Held by a test that adds a comment row for a while (the measured comment
/// post, BG18) and by the test that counts the generated site's comments: the
/// tests in a target run in parallel.
static COMMENT_ROWS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const BOTH: [Caches; 2] = [Caches::Cold, Caches::Warm];

/// Visit `path` as each of `visitors`, cold and warm, check the counts, and
/// check the page is the real one: it shows every one of `shows` and none of
/// `hides`.
async fn measure(
    site: &PerfSite,
    page: &str,
    visitors: &[Visitor],
    path: &str,
    shows: &[&str],
    hides: &[&str],
) {
    for &visitor in visitors {
        for caches in BOTH {
            let visit = site.visit(page, visitor, caches, path).await;
            for wanted in shows {
                assert!(
                    visit.body.contains(wanted),
                    "{} does not show {wanted:?}",
                    visit.key
                );
            }
            for unwanted in hides {
                assert!(
                    !visit.body.contains(unwanted),
                    "{} shows {unwanted:?}",
                    visit.key
                );
            }
            check(&visit);
        }
    }
}

#[test]
fn front_page() {
    run_test(async {
        let site = shared_site();
        // The front page is the blog listing, newest first.
        measure(
            site,
            "front",
            &[Visitor::Anonymous, Visitor::User],
            "/",
            &["Post 1:"],
            &[],
        )
        .await;
    });
}

/// The Dashboard as the administrator. Its panels count pending comments, so
/// it holds the comment rows still for the whole measure.
#[test]
fn admin_dashboard() {
    run_test(async {
        let _rows = COMMENT_ROWS.lock().await;
        let site = shared_site();
        measure(
            site,
            "admin_dashboard",
            &[Visitor::Admin],
            "/admin",
            &["href=\"/admin/content/overview\""],
            &[],
        )
        .await;
    });
}

/// The Content landing page as the administrator; it counts comments too.
#[test]
fn admin_content_landing() {
    run_test(async {
        let _rows = COMMENT_ROWS.lock().await;
        let site = shared_site();
        measure(
            site,
            "admin_content_landing",
            &[Visitor::Admin],
            "/admin/content/overview",
            &["href=\"/admin/content/add\""],
            &[],
        )
        .await;
    });
}

/// The People landing page as the administrator.
#[test]
fn admin_people_landing() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "admin_people_landing",
            &[Visitor::Admin],
            "/admin/people/overview",
            &["href=\"/admin/people/roles\""],
            &[],
        )
        .await;
    });
}

/// The Reports landing page as the administrator.
#[test]
fn admin_reports_landing() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "admin_reports_landing",
            &[Visitor::Admin],
            "/admin/reports/overview",
            &["href=\"/admin/reports/log\""],
            &[],
        )
        .await;
    });
}

#[test]
fn item_page_with_comments() {
    run_test(async {
        let site = shared_site();
        let path = site.item_path.clone();
        // The newest post and its three comments.
        measure(
            site,
            "item",
            &[Visitor::Anonymous, Visitor::User],
            &path,
            &[
                "Post 1:",
                "Comment 1 on post 1.",
                "Comment 41 on post 1.",
                "Comment 81 on post 1.",
            ],
            &[],
        )
        .await;
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
            // The newest post tagged topic-1; post 1 is tagged topic-2 and
            // topic-3, so a listing that ignored the tag would show it.
            &["Post 3:"],
            &["Post 1:"],
        )
        .await;
    });
}

#[test]
fn feed() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "feed",
            // Feed readers do not sign in.
            &[Visitor::Anonymous],
            "/blog/feed.xml",
            &["<item>", "Post 1:"],
            &[],
        )
        .await;
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
            // The one post with "river" in its title.
            &["Post 39:"],
            &[],
        )
        .await;
    });
}

#[test]
fn login_page() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "login",
            &[Visitor::Anonymous],
            "/user/login",
            &["name=\"password\""],
            &[],
        )
        .await;
    });
}

#[test]
fn admin_content_list() {
    run_test(async {
        let site = shared_site();
        measure(
            site,
            "admin_content",
            &[Visitor::Admin],
            "/admin/content",
            &["Post 1:"],
            &[],
        )
        .await;
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
        let _rows = COMMENT_ROWS.lock().await;
        let site = shared_site();
        let db = &site.app.db;
        let options = common::perf::site_options();

        let again = trovato_kernel::generate::generate(db, &options, "unused-password-0", |_| {})
            .await
            .expect("the same options again");
        assert!(again.already_present, "the same options wrote again");

        let other = trovato_kernel::generate::GenerateOptions {
            items: options.items + 1,
            ..options.clone()
        };
        let refused = trovato_kernel::generate::generate(db, &other, "unused-password-0", |_| {})
            .await
            .expect_err("different options on a generated site were not refused");
        assert!(
            refused
                .to_string()
                .contains("already holds a generated site"),
            "{refused}"
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

        // Every generated row is what the seed gives, not just its id: a
        // digest of the posts, their revisions and the comments, against the
        // one recorded when the generator was written.
        let digest = |sql: &'static str| async move {
            sqlx::query_scalar::<_, String>(sql)
                .fetch_one(db)
                .await
                .unwrap()
        };
        assert_eq!(
            digest(
                "SELECT md5(string_agg(concat_ws('|', id, current_revision_id, title, author_id, \
                 status, created, changed, fields, comment_count), ',' ORDER BY id)) \
                 FROM item WHERE type = 'blog'"
            )
            .await,
            ITEM_DIGEST
        );
        assert_eq!(
            digest(
                "SELECT md5(string_agg(concat_ws('|', id, item_id, author_id, title, fields, \
                 created), ',' ORDER BY id)) FROM item_revision"
            )
            .await,
            REVISION_DIGEST
        );
        assert_eq!(
            digest(
                "SELECT md5(string_agg(concat_ws('|', id, item_id, author_id, body, status, \
                 created, depth), ',' ORDER BY id)) FROM comment"
            )
            .await,
            COMMENT_DIGEST
        );
    });
}

/// What `site_options()` generates, as digests of the rows (see above). A
/// change to the generator's output changes these in the same commit.
const ITEM_DIGEST: &str = "8cce5aec48e03bb29cf0e599ca71506a";
const REVISION_DIGEST: &str = "520f7c8627fd8bf298b2b9ba47d780a0";
const COMMENT_DIGEST: &str = "e6ec1a320125e00c01e046c88cd65dd6";

/// A database with anything in it the generator did not write is refused
/// before a row is written: a real site is never generated into.
#[test]
fn generate_refuses_a_database_it_did_not_generate() {
    run_test(async {
        let db = trovato_test_utils::ScratchDb::create("genrefuse").await;
        trovato_kernel::db::run_migrations(db.pool()).await.unwrap();
        sqlx::query(
            "INSERT INTO users (id, name, pass, mail, status) \
             VALUES (gen_random_uuid(), 'someone', 'x', 'someone@example.invalid', 1)",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let err = trovato_kernel::generate::generate(
            db.pool(),
            &common::perf::site_options(),
            "perf-refuse-password-0",
            |_| {},
        )
        .await
        .expect_err("a database with a user in it was not refused");
        assert!(err.to_string().contains("did not write"), "{err}");
        let written: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM users WHERE name LIKE 'gen\\_%') \
             + (SELECT count(*) FROM category_tag WHERE slug LIKE 'topic-%')",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(written, 0);
    });
}

/// Signing up writes the account and queues its verification mail in one
/// transaction (BG18): the queue adds one insert to the write, and the counts
/// say so.
#[test]
fn signup() {
    run_test(async {
        // Registration was opened when the site was built (common/perf.rs).
        let site = shared_site();
        let name = format!("perf_{}", uuid::Uuid::now_v7().simple());
        let mail = format!("{name}@perf.example.test");
        let visit = site
            .submit(
                "signup",
                Visitor::Anonymous,
                "/user/register/json",
                serde_json::json!({
                    "username": name,
                    "mail": mail,
                    "password": "Perf-Password-123",
                }),
            )
            .await;
        assert_eq!(visit.status, 200, "{}", visit.body);
        let queued: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM mail_outbox
             WHERE recipient = $1 AND category = 'registration_verify'",
        )
        .bind(&mail)
        .fetch_one(&site.app.db)
        .await
        .unwrap();
        assert_eq!(queued, 1, "the measured request queued the verification");
        check(&visit);
    });
}

/// Posting a visible comment on someone else's post queues the author's
/// notification in the comment's own transaction (BG18).
#[test]
fn comment() {
    run_test(async {
        // Its comment exists between the measured post and the delete below;
        // the generated-site check counts every comment, so it waits.
        let _rows = COMMENT_ROWS.lock().await;
        let site = shared_site();
        // The second newest post: the item page test measures the newest, and
        // must not find a fourth comment on it.
        let item: uuid::Uuid = sqlx::query_scalar("SELECT md5('1:item:2')::uuid")
            .fetch_one(&site.app.db)
            .await
            .unwrap();
        let visit = site
            .submit(
                "comment",
                Visitor::Admin,
                &format!("/api/item/{item}/comments"),
                serde_json::json!({ "body": "A measured comment" }),
            )
            .await;
        assert_eq!(visit.status, 200, "{}", visit.body);
        let comment: serde_json::Value = serde_json::from_str(&visit.body).unwrap();
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM mail_outbox WHERE dedupe_key = $1")
                .bind(format!(
                    "comment-published:{}",
                    comment["id"].as_str().unwrap()
                ))
                .fetch_one(&site.app.db)
                .await
                .unwrap();
        assert_eq!(queued, 1, "the measured request queued the notification");
        // The generated site is counted by another test: leave it as it was.
        sqlx::query("DELETE FROM comment WHERE id = $1::uuid")
            .bind(comment["id"].as_str().unwrap())
            .execute(&site.app.db)
            .await
            .unwrap();
        check(&visit);
    });
}
