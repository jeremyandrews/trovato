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

use common::perf::{Caches, PerfSite, Visitor, check, check_answered, expected, shared_site};
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

/// Every account the generator writes, the administrator too, has exactly one
/// `account.created` event, shaped as the other creation paths shape theirs:
/// `path` `generate`, `by` `operator`, no actor, no address, a v7 id, and
/// `admin` on the administrator's alone.
#[test]
fn generate_records_every_account_it_creates() {
    run_test(async {
        let site = shared_site();
        let db = &site.app.db;
        let options = common::perf::site_options();

        let accounts: Vec<(uuid::Uuid, String, bool)> = sqlx::query_as(
            "SELECT id, name, is_admin FROM users WHERE name LIKE 'gen\\_%' ORDER BY name",
        )
        .fetch_all(db)
        .await
        .unwrap();
        assert_eq!(accounts.len(), usize::try_from(options.users).unwrap() + 1);

        // Every `account.created` a generated account has, whatever its path
        // or shape: a second event of any kind is as wrong as none.
        type Event = (
            uuid::Uuid,
            Option<uuid::Uuid>,
            String,
            Option<uuid::Uuid>,
            Option<String>,
            Option<String>,
            Option<String>,
            serde_json::Value,
        );
        let events: Vec<Event> = sqlx::query_as(
            "SELECT e.id, e.user_id, e.outcome, e.actor_id, e.subject_hash, e.ip_address, \
                    e.user_agent, e.details \
             FROM security_audit_log e JOIN users u ON u.id = e.user_id \
             WHERE e.kind = 'account.created' AND u.name LIKE 'gen\\_%'",
        )
        .fetch_all(db)
        .await
        .unwrap();
        assert_eq!(
            events.len(),
            accounts.len(),
            "one event per generated account"
        );
        // And no `generate` event for an account that is not a generated one.
        let generate_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM security_audit_log \
             WHERE kind = 'account.created' AND details->>'path' = 'generate'",
        )
        .fetch_one(db)
        .await
        .unwrap();
        assert_eq!(generate_events, i64::try_from(accounts.len()).unwrap());

        for (id, name, is_admin) in &accounts {
            let mine: Vec<_> = events.iter().filter(|e| e.1 == Some(*id)).collect();
            assert_eq!(mine.len(), 1, "{name} has {} events", mine.len());
            let (event_id, _, outcome, actor, subject, ip, agent, details) = mine[0];
            assert_eq!(event_id.get_version_num(), 7, "{name}'s event id");
            assert_eq!(outcome, "success", "{name}");
            assert_eq!(
                (actor, subject, ip, agent),
                (&None, &None, &None, &None),
                "{name}: an actor, a subject, an address or a user agent"
            );
            assert_eq!(details["path"], "generate", "{name}: {details}");
            assert_eq!(details["by"], "operator", "{name}: {details}");
            assert_eq!(details["active"], true, "{name}: {details}");
            if *is_admin {
                assert_eq!(name, "gen_admin");
                assert_eq!(details["admin"], true, "{name}: {details}");
            } else {
                assert!(details.get("admin").is_none(), "{name}: {details}");
            }
        }
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

// ── Writes the security log records (B277) ─────────────────────────────────
//
// Each of these writes an account change and the security event that records
// it, in one transaction. The counts pin what that costs; the ignored
// `security_event_write_latency` below times the same requests.

/// The password every account made by these tests starts with, and the one
/// the password change alternates with.
const WRITE_PASSWORDS: [&str; 2] = ["Perf-Write-Password-1", "Perf-Write-Password-2"];

/// A signed-in member of the site of its own: `(id, name, cookies)`. Its
/// session is registered (the page load does that) before anything is
/// measured.
async fn own_member(site: &PerfSite, prefix: &str) -> (uuid::Uuid, String, String) {
    let name = format!("{prefix}_{}", uuid::Uuid::now_v7().simple());
    site.app
        .create_test_user(&name, WRITE_PASSWORDS[0], &format!("{name}@perf.example.test"))
        .await;
    let id = common::user_id_of(&site.app, &name).await;
    let cookies = site.app.login(&name, WRITE_PASSWORDS[0]).await;
    (id, name, cookies)
}

/// The `content` of the page's `csrf-token` meta tag, or nothing when it has
/// none (an anonymous page served from the page cache), as
/// [`PerfSite::submit`] reads it.
fn meta_csrf(html: &str) -> String {
    let marker = "name=\"csrf-token\" content=\"";
    html.find(marker)
        .and_then(|start| html[start + marker.len()..].split('"').next())
        .unwrap_or_default()
        .to_string()
}

/// The value of the first `_token` input on the page.
fn form_token(html: &str) -> String {
    common::input_value_in(html, "_token").expect("a CSRF form field")
}

/// Self registration, as the signup page posts it.
fn register(html: &str, name: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::post("/user/register/json")
        .header("content-type", "application/json")
        .header("x-csrf-token", meta_csrf(html))
        .body(axum::body::Body::from(
            serde_json::json!({
                "username": name,
                "mail": format!("{name}@perf.example.test"),
                "password": WRITE_PASSWORDS[0],
            })
            .to_string(),
        ))
        .unwrap()
}

/// The profile page's password change, from `from` to `to`.
fn change_password(html: &str, from: &str, to: &str) -> axum::http::Request<axum::body::Body> {
    let fields = common::form_fields(html, "user_password_change");
    axum::http::Request::post("/user/password")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(format!(
            "{fields}&current_password={from}&new_password={to}&confirm_password={to}"
        )))
        .unwrap()
}

/// The administrator's edit form for the account `name`, saved with `mail`.
fn admin_edit(html: &str, id: uuid::Uuid, name: &str, mail: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::post(format!("/admin/people/{id}/edit"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(format!(
            "_token={}&_form_build_id=x&name={}&mail={}&status=1",
            urlencoding::encode(&form_token(html)),
            urlencoding::encode(name),
            urlencoding::encode(mail),
        )))
        .unwrap()
}

/// `POST /api/tokens`, as the tokens page sends it.
fn create_token(html: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::post("/api/tokens")
        .header("content-type", "application/json")
        .header("x-csrf-token", meta_csrf(html))
        .body(axum::body::Body::from(
            serde_json::json!({ "name": "perf", "expires_in_days": 30 }).to_string(),
        ))
        .unwrap()
}

/// How many security events of `kind` name `user_id`.
async fn events_of(site: &PerfSite, kind: &str, user_id: uuid::Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM security_audit_log WHERE kind = $1 AND user_id = $2")
        .bind(kind)
        .bind(user_id)
        .fetch_one(&site.app.db)
        .await
        .unwrap()
}

/// A member changing their own password writes the hash and
/// `password.changed`, and commits them together.
#[test]
fn password_change() {
    run_test(async {
        let site = shared_site();
        let (id, _, cookies) = own_member(site, "perf_pw").await;
        let written = site
            .write("password_change.user.post", &cookies, "/user/profile", true, |html| {
                change_password(html, WRITE_PASSWORDS[0], WRITE_PASSWORDS[1])
            })
            .await;
        assert_eq!(events_of(site, "password.changed", id).await, 1);
        check(&written.visit);
    });
}

/// An administrator changing an account's e-mail address writes the account
/// and `user.mail_changed`, and commits them together.
#[test]
fn admin_user_edit() {
    run_test(async {
        let site = shared_site();
        let (id, name, _) = own_member(site, "perf_edit").await;
        let admin = site.cookies_of(Visitor::Admin).to_string();
        let page = format!("/admin/people/{id}/edit");
        let written = site
            .write("admin_user_edit.admin.post", &admin, &page, true, |html| {
                admin_edit(html, id, &name, &format!("{name}@changed.example.test"))
            })
            .await;
        assert_eq!(events_of(site, "user.mail_changed", id).await, 1);
        check_answered(&written.visit, 303);
    });
}

/// Issuing an API token writes the token and `api_token.created`, and
/// commits them together.
#[test]
fn api_token_create() {
    run_test(async {
        let site = shared_site();
        let (id, _, cookies) = own_member(site, "perf_token").await;
        let written = site
            .write("api_token_create.user.post", &cookies, "/user/tokens", true, create_token)
            .await;
        assert_eq!(events_of(site, "api_token.created", id).await, 1);
        check_answered(&written.visit, 201);
    });
}

/// The `q`th quantile of `samples` (sorted here), nearest rank.
fn quantile(samples: &mut [std::time::Duration], q: f64) -> std::time::Duration {
    samples.sort_unstable();
    let rank = (q * samples.len() as f64).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}

/// Time `runs` writes after `warmup` unmeasured ones, and print the median
/// and 95th percentile latency and the statements per request.
async fn time_write<F, Fut>(label: &str, warmup: usize, runs: usize, mut one: F)
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = common::perf::Written>,
{
    let mut samples = Vec::with_capacity(runs);
    let mut statements = std::collections::BTreeMap::<u32, usize>::new();
    for i in 0..warmup + runs {
        let written = one(i).await;
        assert!(
            (200..400).contains(&written.visit.status),
            "{label} answered {}: {}",
            written.visit.status,
            written.visit.body
        );
        if i >= warmup {
            samples.push(written.elapsed);
            *statements.entry(written.visit.counts.queries).or_default() += 1;
        }
    }
    let p50 = quantile(&mut samples, 0.50);
    let p95 = quantile(&mut samples, 0.95);
    println!(
        "B277 {label}: p50 {:.3} ms, p95 {:.3} ms, statements per request {statements:?} ({runs} runs after {warmup} warmup)",
        p50.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
    );
}

/// The latency of the four writes above, on a warm site: 200 requests each
/// after 20 unmeasured ones. Timings differ by machine, so this is not run
/// with the suite; run it on a release build with nothing else running:
///
/// ```sh
/// cargo test --release -p trovato-kernel --test performance_test \
///     security_event_write_latency -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a timing, not a check: run by hand on a quiet machine"]
fn security_event_write_latency() {
    run_test(async {
        const WARMUP: usize = 20;
        const RUNS: usize = 200;
        let site = shared_site();

        time_write("registration", WARMUP, RUNS, |_| async {
            let name = format!("perf_reg_{}", uuid::Uuid::now_v7().simple());
            site.write("bench", "", "/", false, |html| register(html, &name))
                .await
        })
        .await;

        let (pw_id, _, pw_cookies) = own_member(site, "perf_pw_bench").await;
        let cookies = std::sync::Mutex::new(pw_cookies);
        time_write("password change", WARMUP, RUNS, |i| {
            let cookies = &cookies;
            async move {
                site.app
                    .state
                    .rate_limiter()
                    .reset("password", &pw_id.to_string())
                    .await
                    .ok();
                let (from, to) = (WRITE_PASSWORDS[i % 2], WRITE_PASSWORDS[(i + 1) % 2]);
                let current = cookies.lock().unwrap().clone();
                let written = site
                    .write("bench", &current, "/user/profile", false, |html| {
                        change_password(html, from, to)
                    })
                    .await;
                *cookies.lock().unwrap() = written.cookies.clone();
                written
            }
        })
        .await;

        let (edit_id, edit_name, _) = own_member(site, "perf_edit_bench").await;
        let admin = site.cookies_of(Visitor::Admin).to_string();
        let edit_page = format!("/admin/people/{edit_id}/edit");
        time_write("admin user edit", WARMUP, RUNS, |i| {
            let (admin, edit_page, edit_name) = (&admin, &edit_page, &edit_name);
            async move {
                let mail = format!("{edit_name}+{}@perf.example.test", i % 2);
                site.write("bench", admin, edit_page, false, |html| {
                    admin_edit(html, edit_id, edit_name, &mail)
                })
                .await
            }
        })
        .await;

        let (token_id, _, token_cookies) = own_member(site, "perf_token_bench").await;
        time_write("api token create", WARMUP, RUNS, |_| {
            let token_cookies = &token_cookies;
            async move {
                // Under the per-account limit, whatever the run before left.
                sqlx::query("DELETE FROM api_tokens WHERE user_id = $1")
                    .bind(token_id)
                    .execute(&site.app.db)
                    .await
                    .unwrap();
                site.write("bench", token_cookies, "/user/tokens", false, create_token)
                    .await
            }
        })
        .await;
    });
}
