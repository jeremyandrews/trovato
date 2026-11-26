#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato import` (B167): items and revisions with the ids and dates the
//! file gives, through the kernel's checks, in one transaction.
//!
//! A port used to write `item` and `item_revision` with SQL and restart the
//! server. These tests hold the importer to what replaces that: ids and dates
//! kept, a rerun that changes nothing, a changed record that adds exactly one
//! revision, a bad record that writes nothing, and a running server that
//! serves the result at once. The last two tests pin the form create and edit
//! paths, which the importer must not have changed.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use serde_json::json;
use trovato_kernel::import::{ImportFailed, ImportReport, import};
use uuid::Uuid;

/// 2014-01-01, 2015-01-01, 2016-01-01, 2017-01-01 (UTC).
const Y2014: i64 = 1_388_534_400;
const Y2015: i64 = 1_420_070_400;
const Y2016: i64 = 1_451_606_400;
const Y2017: i64 = 1_483_228_800;

async fn run(
    app: &TestApp,
    lines: &[serde_json::Value],
    dry_run: bool,
) -> anyhow::Result<ImportReport> {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    import(&app.state, text.as_bytes(), dry_run).await
}

/// A user to author imported content, removed with what it wrote.
async fn author(app: &TestApp, prefix: &str) -> Uuid {
    let name = common::username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = common::user_id_of(app, &name).await;
    // Registered before the items it writes, so it runs after them.
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    id
}

/// Delete `ids` (items) when the test ends, whatever it did.
fn forget_items(app: &TestApp, ids: Vec<Uuid>) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        for id in ids {
            let _ = sqlx::query("UPDATE item SET current_revision_id = NULL WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item_revision WHERE item_id = $1")
                .bind(id)
                .execute(&pool)
                .await;
            let _ = sqlx::query("DELETE FROM item WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        }
    });
}

fn item_line(id: Uuid, author: Uuid, title: &str, changed: i64) -> serde_json::Value {
    json!({
        "type": "item", "id": id, "item_type": "page", "title": title,
        "author_id": author, "created": Y2014, "changed": changed,
        "fields": {"body": {"value": format!("<p>{title}</p>"), "format": "filtered_html"}}
    })
}

fn revision_line(
    id: Uuid,
    item: Uuid,
    author: Uuid,
    title: &str,
    created: i64,
) -> serde_json::Value {
    json!({
        "type": "revision", "id": id, "item_id": item, "author_id": author,
        "title": title, "created": created, "log": format!("saved {created}"),
        "fields": {"body": {"value": "<p>old</p>", "format": "filtered_html"}}
    })
}

/// An item with two past revisions: three revisions in all, dated
/// 2014, 2015 and (the current one) 2016.
fn history(item: Uuid, author: Uuid, title: &str) -> (Vec<serde_json::Value>, Uuid, [Uuid; 2]) {
    let current = Uuid::now_v7();
    let past = [Uuid::now_v7(), Uuid::now_v7()];
    let mut line = item_line(item, author, title, Y2016);
    line["revision_id"] = json!(current);
    let lines = vec![
        line,
        revision_line(past[0], item, author, &format!("{title} (first)"), Y2014),
        revision_line(past[1], item, author, &format!("{title} (second)"), Y2015),
    ];
    (lines, current, past)
}

async fn revisions_of(app: &TestApp, item: Uuid) -> Vec<(Uuid, i64)> {
    sqlx::query_as("SELECT id, created FROM item_revision WHERE item_id = $1 ORDER BY created, id")
        .bind(item)
        .fetch_all(&app.db)
        .await
        .unwrap()
}

async fn dates_of(app: &TestApp, item: Uuid) -> Option<(i64, i64, String)> {
    sqlx::query_as("SELECT created, changed, title FROM item WHERE id = $1")
        .bind(item)
        .fetch_optional(&app.db)
        .await
        .unwrap()
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for(cookies))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

#[test]
fn an_imported_item_keeps_its_id_its_dates_and_its_revision_history() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_author").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (lines, current, past) = history(item, writer, "B167 kept dates");

        let report = run(app, &lines, false).await.expect("imports");
        assert_eq!(report.of("item").created, 1);
        assert_eq!(report.of("revision").created, 2);

        let (created, changed, title) = dates_of(app, item).await.expect("the item, by its id");
        assert_eq!((created, changed), (Y2014, Y2016));
        assert_eq!(title, "B167 kept dates");
        assert_eq!(
            revisions_of(app, item).await,
            vec![(past[0], Y2014), (past[1], Y2015), (current, Y2016)],
            "every revision keeps its id and its date"
        );
        let pointed: Option<Uuid> =
            sqlx::query_scalar("SELECT current_revision_id FROM item WHERE id = $1")
                .bind(item)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(pointed, Some(current));
        let vector: bool = sqlx::query_scalar(
            "SELECT search_vector @@ plainto_tsquery('simple', 'kept') FROM item WHERE id = $1",
        )
        .bind(item)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert!(vector, "the search trigger indexed the imported title");
    });
}

#[test]
fn a_rerun_of_the_same_file_changes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_rerun").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (lines, _, _) = history(item, writer, "B167 rerun");

        run(app, &lines, false).await.expect("first run");
        let before = revisions_of(app, item).await;
        let again = run(app, &lines, false).await.expect("second run");

        assert_eq!(again.changes(), 0, "{:?}", again.counts);
        assert_eq!(again.of("item").unchanged, 1);
        assert_eq!(again.of("revision").unchanged, 2);
        assert!(
            again.cache_note.is_none(),
            "nothing changed, nothing to tell"
        );
        assert_eq!(revisions_of(app, item).await, before);
        assert_eq!(
            dates_of(app, item).await.unwrap().1,
            Y2016,
            "changed is not bumped"
        );
    });
}

#[test]
fn a_changed_item_gains_one_revision_dated_as_the_record_says() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_change").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (mut lines, _, _) = history(item, writer, "B167 change");
        run(app, &lines, false).await.expect("first run");

        lines[0] = item_line(item, writer, "B167 change, edited", Y2017);
        let report = run(app, &lines, false).await.expect("second run");
        assert_eq!(report.changes(), 1, "{:?}", report.counts);
        assert_eq!(report.of("item").updated, 1);
        assert_eq!(report.of("revision").unchanged, 2);

        let revisions = revisions_of(app, item).await;
        assert_eq!(revisions.len(), 4, "one new revision");
        assert_eq!(revisions[3].1, Y2017, "dated as the record says");
        let (created, changed, title) = dates_of(app, item).await.unwrap();
        assert_eq!((created, changed), (Y2014, Y2017));
        assert_eq!(title, "B167 change, edited");

        let third = run(app, &lines, false).await.expect("third run");
        assert_eq!(third.changes(), 0, "the edit, rerun, is unchanged");
    });
}

#[test]
fn a_record_that_fails_validation_writes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_invalid").await;
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        forget_items(app, vec![first, second]);
        let mut bad = item_line(second, writer, "B167 bad format", Y2016);
        bad["fields"]["body"]["format"] = json!("b167_no_such_format");
        let lines = vec![item_line(first, writer, "B167 good", Y2016), bad];

        let error = run(app, &lines, false).await.expect_err("refused");
        let failed = error
            .downcast_ref::<ImportFailed>()
            .expect("an ImportFailed");
        assert_eq!(failed.line, 2, "{failed}");
        assert!(failed.message.contains("format"), "{failed}");
        assert!(
            dates_of(app, first).await.is_none(),
            "the good line was not kept"
        );
        assert!(dates_of(app, second).await.is_none());

        // Each kind of refusal names its line, and writes nothing either.
        let cases = vec![
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "b167_no_such_type", "title": "x", "author_id": writer, "created": Y2014, "changed": Y2015}),
                "content type",
            ),
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "page", "title": "x", "author_id": Uuid::now_v7(), "created": Y2014, "changed": Y2015}),
                "not a user",
            ),
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "page", "title": " ", "author_id": writer, "created": Y2014, "changed": Y2015}),
                "title",
            ),
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "page", "title": "x", "author_id": writer, "created": Y2015, "changed": Y2014}),
                "before",
            ),
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "page", "title": "x", "author_id": writer, "created": Y2014, "changed": Y2015, "status": 7}),
                "status",
            ),
            (
                json!({"type": "item", "id": Uuid::now_v7(), "item_type": "page", "title": "x", "author_id": writer, "created": Y2014, "chnaged": Y2015}),
                "chnaged",
            ),
            (
                revision_line(Uuid::now_v7(), Uuid::now_v7(), writer, "orphan", Y2014),
                "does not exist",
            ),
        ];
        for (line, expected) in cases {
            let error = run(
                app,
                &[item_line(first, writer, "B167 good", Y2016), line.clone()],
                false,
            )
            .await
            .expect_err("refused");
            let failed = error
                .downcast_ref::<ImportFailed>()
                .expect("an ImportFailed");
            assert_eq!(failed.line, 2, "{line}: {failed}");
            assert!(failed.message.contains(expected), "{line}: {failed}");
            assert!(
                dates_of(app, first).await.is_none(),
                "{line}: nothing written"
            );
        }
    });
}

#[test]
fn a_stored_revision_is_never_rewritten_and_never_newer_than_the_item() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_immutable").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (mut lines, _, past) = history(item, writer, "B167 immutable");
        run(app, &lines, false).await.expect("first run");

        lines[1] = revision_line(past[0], item, writer, "rewritten", Y2014);
        let error = run(app, &lines, false).await.expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().unwrap();
        assert_eq!(failed.line, 2);
        assert!(failed.message.contains("never rewritten"), "{failed}");

        let newer = revision_line(Uuid::now_v7(), item, writer, "from the future", Y2017);
        let error = run(app, &[newer], false).await.expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().unwrap();
        assert!(failed.message.contains("newer"), "{failed}");
        assert_eq!(revisions_of(app, item).await.len(), 3);
    });
}

#[test]
fn a_dry_run_reports_and_writes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_dry").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (lines, _, _) = history(item, writer, "B167 dry run");

        let report = run(app, &lines, true).await.expect("checks");
        assert!(report.dry_run);
        assert_eq!(report.of("item").created, 1);
        assert_eq!(report.of("revision").created, 2);
        assert!(report.cache_note.is_none());
        assert!(dates_of(app, item).await.is_none(), "nothing kept");
    });
}

#[test]
fn the_running_server_serves_imported_content_without_a_restart() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_serve").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (lines, _, _) = history(item, writer, "B167 served at once");

        let (status, _) = get(app, "", &format!("/item/{item}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        run(app, &lines, false).await.expect("imports");
        let (status, html) = get(app, "", &format!("/item/{item}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("B167 served at once"));

        // The item is now in this server's item cache; a changed record must
        // reach the page all the same.
        let mut edited = lines.clone();
        edited[0] = item_line(item, writer, "B167 served at once, edited", Y2017);
        run(app, &edited, false).await.expect("imports the change");
        let (status, html) = get(app, "", &format!("/item/{item}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("B167 served at once, edited"),
            "the cached copy was dropped"
        );
    });
}

// =============================================================================
// Comments, aliases and redirects (part 2)
// =============================================================================

fn forget_rows(app: &TestApp, table: &'static str, ids: Vec<Uuid>) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        for id in ids {
            let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE id = $1"
            )))
            .bind(id)
            .execute(&pool)
            .await;
        }
    });
}

fn comment_line(
    id: Uuid,
    item: Uuid,
    parent: Option<Uuid>,
    author: Uuid,
    body: &str,
    created: i64,
) -> serde_json::Value {
    json!({
        "type": "comment", "id": id, "item_id": item, "parent_id": parent,
        "author_id": author, "subject": format!("Re: {body}"), "body": format!("<p>{body}</p>"),
        "created": created, "changed": created
    })
}

#[test]
fn imported_comments_keep_their_ids_dates_and_threads() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let writer = author(app, "b167_comment").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let (top, reply) = (Uuid::now_v7(), Uuid::now_v7());
        forget_rows(app, "comment", vec![reply, top]);
        let (mut lines, _, _) = history(item, writer, "B167 comments");
        lines.push(comment_line(
            top,
            item,
            None,
            writer,
            "B167 first comment",
            Y2015,
        ));
        lines.push(comment_line(
            reply,
            item,
            Some(top),
            writer,
            "B167 a reply",
            Y2016,
        ));

        let report = run(app, &lines, false).await.expect("imports");
        assert_eq!(report.of("comment").created, 2);
        let rows: Vec<(Uuid, Option<Uuid>, i64, i64, i16)> = sqlx::query_as(
            "SELECT id, parent_id, created, changed, depth FROM comment WHERE item_id = $1 ORDER BY created",
        )
        .bind(item)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                (top, None, Y2015, Y2015, 0),
                (reply, Some(top), Y2016, Y2016, 1)
            ],
            "ids, dates and the thread kept; the trigger set the depth"
        );
        let count: i32 = sqlx::query_scalar("SELECT comment_count FROM item WHERE id = $1")
            .bind(item)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(count, 2, "the trigger counted them");

        let again = run(app, &lines, false).await.expect("rerun");
        assert_eq!(again.changes(), 0, "{:?}", again.counts);
        assert_eq!(again.of("comment").unchanged, 2);

        let mut edited = lines.clone();
        edited[4]["body"] = json!("<p>B167 a reply, edited</p>");
        edited[4]["changed"] = json!(Y2017);
        let changed = run(app, &edited, false).await.expect("the edit");
        assert_eq!(changed.changes(), 1, "{:?}", changed.counts);
        assert_eq!(changed.of("comment").updated, 1);
        let (body, stamp): (String, i64) =
            sqlx::query_as("SELECT body, changed FROM comment WHERE id = $1")
                .bind(reply)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(
            (body.as_str(), stamp),
            ("<p>B167 a reply, edited</p>", Y2017)
        );

        let (status, html) = get(app, "", &format!("/item/{item}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("B167 a reply, edited"),
            "the page shows the imported comment"
        );
    });
}

#[test]
fn a_comment_that_fails_validation_writes_nothing() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_badcomment").await;
        let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
        forget_items(app, vec![one, two]);
        let elsewhere = Uuid::now_v7();
        let base = vec![
            item_line(one, writer, "B167 comment one", Y2016),
            item_line(two, writer, "B167 comment two", Y2016),
            comment_line(elsewhere, two, None, writer, "on two", Y2015),
        ];
        let cases = vec![
            (
                comment_line(Uuid::now_v7(), one, Some(elsewhere), writer, "x", Y2015),
                "another item",
            ),
            (
                comment_line(
                    Uuid::now_v7(),
                    one,
                    Some(Uuid::now_v7()),
                    writer,
                    "x",
                    Y2015,
                ),
                "does not exist",
            ),
            (
                comment_line(Uuid::now_v7(), Uuid::now_v7(), None, writer, "x", Y2015),
                "does not exist",
            ),
            (
                {
                    let mut c = comment_line(Uuid::now_v7(), one, None, writer, "x", Y2015);
                    c["body"] = json!("  ");
                    c
                },
                "body",
            ),
            (
                {
                    let mut c = comment_line(Uuid::now_v7(), one, None, writer, "x", Y2015);
                    c["subject"] = json!("s".repeat(300));
                    c
                },
                "subject",
            ),
            (
                {
                    let mut c = comment_line(Uuid::now_v7(), one, None, writer, "x", Y2015);
                    c["status"] = json!(9);
                    c
                },
                "status",
            ),
            (
                {
                    let mut c = comment_line(Uuid::now_v7(), one, None, writer, "x", Y2015);
                    c["body_format"] = json!("b167_nope");
                    c
                },
                "text format",
            ),
        ];
        for (line, expected) in cases {
            let mut lines = base.clone();
            lines.push(line.clone());
            let error = run(app, &lines, false).await.expect_err("refused");
            let failed = error
                .downcast_ref::<ImportFailed>()
                .expect("an ImportFailed");
            assert_eq!(failed.line, 4, "{line}: {failed}");
            assert!(failed.message.contains(expected), "{line}: {failed}");
            assert!(
                dates_of(app, one).await.is_none(),
                "{line}: nothing written"
            );
        }
    });
}

#[test]
fn an_imported_alias_serves_its_item_and_reruns_unchanged() {
    run_test(async {
        let app = shared_app().await;
        let writer = author(app, "b167_alias").await;
        let item = Uuid::now_v7();
        forget_items(app, vec![item]);
        let alias = Uuid::now_v7();
        forget_rows(app, "url_alias", vec![alias]);
        let address = format!("/b167/{}", item.simple());
        let mut lines = vec![item_line(item, writer, "B167 aliased", Y2016)];
        lines.push(json!({
            "type": "alias", "id": alias, "source": format!("/item/{item}"),
            "alias": address, "created": Y2015, "changed": Y2016
        }));

        let report = run(app, &lines, false).await.expect("imports");
        assert_eq!(report.of("alias").created, 1);
        let (created, changed): (i64, i64) =
            sqlx::query_as("SELECT created, changed FROM url_alias WHERE id = $1")
                .bind(alias)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!((created, changed), (Y2015, Y2016));
        let (status, html) = get(app, "", &address).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("B167 aliased"));

        assert_eq!(run(app, &lines, false).await.unwrap().changes(), 0);

        let moved = format!("{address}-moved");
        lines[1]["alias"] = json!(moved);
        let report = run(app, &lines, false).await.expect("the change");
        assert_eq!(report.of("alias").updated, 1);
        let (status, _) = get(app, "", &moved).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the running server serves the new address"
        );

        // An address another alias holds is refused, naming the line.
        let mut clash = lines.clone();
        clash.push(json!({
            "type": "alias", "id": Uuid::now_v7(), "source": "/b167-elsewhere",
            "alias": moved, "created": Y2015, "changed": Y2016
        }));
        let error = run(app, &clash, false).await.expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().unwrap();
        assert_eq!(failed.line, 3);
        assert!(failed.message.contains("already alias"), "{failed}");
    });
}

#[test]
fn an_imported_redirect_redirects_and_reruns_unchanged() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_redirects").await;
        common::ensure_plugin_tables(app, "trovato_redirects");
        let redirect = Uuid::now_v7();
        forget_rows(app, "redirect", vec![redirect]);
        let source = format!("/b167-old/{}", redirect.simple());
        let mut lines = vec![json!({
            "type": "redirect", "id": redirect, "source": source,
            "destination": "/b167-new", "created": Y2015
        })];

        let report = run(app, &lines, false).await.expect("imports");
        assert_eq!(report.of("redirect").created, 1);
        let (status_code, language, created): (i16, String, i64) =
            sqlx::query_as("SELECT status_code, language, created FROM redirect WHERE id = $1")
                .bind(redirect)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!((status_code, language.as_str(), created), (301, "", Y2015));
        let response = through_redirects(app, &source).await;
        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
        assert_eq!(response.headers()["location"], "/b167-new");

        assert_eq!(run(app, &lines, false).await.unwrap().changes(), 0);

        lines[0]["destination"] = json!("javascript:alert(1)");
        let error = run(app, &lines, false).await.expect_err("refused");
        let failed = error.downcast_ref::<ImportFailed>().unwrap();
        assert!(failed.message.contains("destination"), "{failed}");

        lines[0]["destination"] = json!("/b167-newer");
        lines[0]["status_code"] = json!(302);
        let report = run(app, &lines, false).await.expect("the change");
        assert_eq!(report.of("redirect").updated, 1);
        let response = through_redirects(app, &source).await;
        assert_eq!(
            response.status(),
            StatusCode::FOUND,
            "the running server's cache dropped"
        );
        assert_eq!(response.headers()["location"], "/b167-newer");
    });
}

/// `uri` through the redirect middleware alone, as the redirect tests drive
/// it: the shared app's router is built before this test enables the plugin.
async fn through_redirects(app: &TestApp, uri: &str) -> axum::response::Response {
    use tower::ServiceExt;
    let router = axum::Router::new()
        .fallback(|| async { "no redirect" })
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            trovato_kernel::middleware::check_redirect,
        ));
    router
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}
