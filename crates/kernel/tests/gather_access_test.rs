#![allow(clippy::unwrap_used, clippy::expect_used)]
//! FR-8 Story 3.4 — gather query access enforcement + over-fetch/backfill.
//!
//! These exercise the gather read path's **item-level** access enforcement at
//! its own boundary (the id-stream surface): a restricted (unpublished) item is
//! absent from ad-hoc gather results for a viewer who cannot see it and present
//! for one who can; the over-fetch/backfill loop fills a page past invisible
//! candidates; and the hard scan cap terminates with the `access_capped` signal
//! when a viewer can see too little.
//!
//! Field-level dropping from projected rows is validated as pure logic in
//! `gather::access` unit tests (`filter_row_fields`) and end-to-end through the
//! real reference plugin in Story 3.8; the full `TestApp` here loads no
//! field-access plugin, so — as these tests also pin — every field of a visible
//! item passes through unchanged (fail-open).
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use std::collections::HashMap;
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, JoinType, PagerConfig, PagerStyle, QueryContext,
    QueryDefinition, QueryDisplay, QueryField, QueryFilter, QueryRelationship,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, Role};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn display(items_per_page: u32) -> QueryDisplay {
    QueryDisplay {
        format: DisplayFormat::Table,
        items_per_page,
        pager: PagerConfig {
            enabled: true,
            style: PagerStyle::Full,
            show_count: true,
        },
        empty_text: None,
        header: None,
        footer: None,
        canonical_url: None,
        routes: Vec::new(),
        feed: None,
    }
}

/// Ad-hoc conference gather, optionally title-filtered, run as `viewer`.
async fn gather_conferences(
    app: &common::TestApp,
    viewer: Option<UserContext>,
    title_eq: Option<&str>,
    per_page: u32,
) -> trovato_kernel::gather::GatherResult {
    let mut filters = Vec::new();
    if let Some(marker) = title_eq {
        // Isolate this test's items by a shared title *marker* via Contains —
        // each conference still has a globally unique title (marker + suffix) so
        // it produces a unique pathauto alias and does not collide with other
        // tests' `conferences/[title]` alias regeneration in the shared test DB.
        filters.push(QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        });
    }
    let definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some("conference".to_string()),
        filters,
        stage_aware: true,
        ..Default::default()
    };
    let ctx = QueryContext {
        current_user_id: viewer
            .as_ref()
            .and_then(|v| v.authenticated.then_some(v.id)),
        viewer,
        url_args: HashMap::new(),
        language: None,
    };
    app.state
        .gather()
        .execute_definition(
            &definition,
            &display(per_page),
            1,
            HashMap::new(),
            LIVE_STAGE_ID,
            &ctx,
        )
        .await
        .expect("gather executes")
}

fn ids(result: &trovato_kernel::gather::GatherResult) -> Vec<String> {
    result
        .items
        .iter()
        .filter_map(|v| v.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect()
}

async fn make_conference(
    app: &common::TestApp,
    admin: &UserContext,
    marker: &str,
    status: i16,
) -> Uuid {
    // Globally-unique title that still contains the shared marker (so the
    // Contains filter isolates the test while pathauto aliases never collide).
    let title = format!("{marker} {}", Uuid::now_v7().simple());
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title,
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_city": { "value": "Barga" } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("3.4 gather access test".to_string()),
            },
            admin,
        )
        .await
        .expect("create")
        .id
}

/// AC-1/AC-3/AC-6 — a restricted (unpublished) item is absent from gather
/// results for an anonymous viewer, and a published one is present. Exercises
/// the mandatory SQL predicate (anon ⇒ status=1) plus the post-fetch pass.
#[test]
fn gather_hides_unpublished_shows_published_to_anon() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

        let marker = format!("GACC-anon-{}", Uuid::now_v7().simple());
        let published = make_conference(app, &admin, &marker, 1).await;
        let _draft = make_conference(app, &admin, &marker, 0).await;

        // Anonymous role viewer (unauthenticated, but carrying the baseline
        // "access content" permission a real anonymous role has).
        let mut anon = UserContext::anonymous();
        anon.permissions = vec!["access content".to_string()];
        let result = gather_conferences(app, Some(anon), Some(&marker), 25).await;
        let got = ids(&result);
        assert_eq!(
            got.len(),
            1,
            "anon sees only the published item, got {got:?}"
        );
        assert_eq!(got[0], published.to_string());
        assert!(!result.access_capped, "not capped — the page filled");
    });
}

/// Admin bypasses item access and sees both the published and the unpublished
/// item; fields pass through (fail-open, no field-access plugin).
#[test]
fn gather_shows_all_and_keeps_fields_for_admin() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

        let marker = format!("GACC-admin-{}", Uuid::now_v7().simple());
        make_conference(app, &admin, &marker, 1).await;
        make_conference(app, &admin, &marker, 0).await;

        let result = gather_conferences(app, Some(admin), Some(&marker), 25).await;
        assert_eq!(result.items.len(), 2, "admin sees published + unpublished");
        // Fail-open: the item.* rows keep their fields object intact.
        for row in &result.items {
            let city = row
                .get("fields")
                .and_then(|f| f.get("field_city"))
                .and_then(|c| c.get("value"))
                .and_then(|v| v.as_str());
            assert_eq!(
                city,
                Some("Barga"),
                "fail-open keeps the field on visible rows"
            );
        }
    });
}

/// AC-4/AC-6 — the over-fetch/backfill loop fills the page past invisible
/// candidates. An authenticated viewer without a "view any" permission cannot
/// see unpublished items by another author; the visible published ones are
/// still returned (not starved by the interleaved drafts), and the page is not
/// access-capped because the candidate source is exhausted well within the cap.
#[test]
fn gather_backfills_past_invisible_for_authenticated_viewer() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);

        let marker = format!("GACC-mix-{}", Uuid::now_v7().simple());
        // Interleave drafts and published items.
        let mut published = Vec::new();
        for i in 0..3 {
            make_conference(app, &admin, &marker, 0).await; // draft (invisible)
            published.push(make_conference(app, &admin, &marker, 1).await); // published
            let _ = i;
        }

        // Authenticated, non-admin, only "access content": sees published on the
        // live stage (fast-path) but none of the drafts (not author, no view-any).
        let viewer = UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        let result = gather_conferences(app, Some(viewer), Some(&marker), 25).await;
        let got = ids(&result);
        assert_eq!(
            got.len(),
            3,
            "all published visible, drafts filtered, got {got:?}"
        );
        for p in &published {
            assert!(
                got.contains(&p.to_string()),
                "published {p} must be present"
            );
        }
        assert!(!result.access_capped, "source exhausted, not capped");
    });
}

/// AC-5/AC-6 — the hard scan cap terminates the backfill and sets the
/// `access_capped` signal. With more invisible candidates than the geometric
/// backfill scans for a one-item page, an under-filled page is returned as
/// correct behaviour (not an error) carrying `access_capped = true`.
#[test]
fn gather_access_capped_signal_when_starved() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;

        // Bulk-insert 130 unpublished conferences under a unique title marker —
        // more than the default backfill scans (2+4+8+16+32+64 = 126 over 6
        // rounds) for a page_size of 1.
        let marker = format!("GACC-cap-{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO item (id, type, title, author_id, status, created, changed, stage_id) \
             SELECT gen_random_uuid(), 'conference', $1 || ' ' || gs::text, \
                    '00000000-0000-0000-0000-000000000000', 0, 0, 0, $2 \
             FROM generate_series(1, 130) AS gs",
        )
        .bind(&marker)
        .bind(LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .expect("bulk insert drafts");

        // Authenticated viewer with no unpublished-granting permission: no SQL
        // predicate applies, so all 130 drafts are scanned candidates that the
        // post-fetch pass denies — the loop hits its round/scan cap.
        let viewer = UserContext::authenticated(Uuid::now_v7(), vec!["access content".to_string()]);
        let result = gather_conferences(app, Some(viewer), Some(&marker), 1).await;
        assert!(result.items.is_empty(), "viewer can see none of the drafts");
        assert!(
            result.access_capped,
            "under-filled page beyond the scan cap must signal access_capped"
        );
    });
}

// -------------------------------------------------------------------------
// The ad hoc endpoint and the gather table policy
// -------------------------------------------------------------------------

fn username(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

async fn user_id_of(app: &TestApp, name: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(&app.db)
        .await
        .expect("test user should exist")
}

async fn grant_via_role(app: &TestApp, user_id: Uuid, permissions: &[&str]) {
    let role = Role::create(&app.db, &format!("gatheradhoc-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    for permission in permissions {
        Role::add_permission(&app.db, role.id, permission)
            .await
            .expect("add permission to role");
    }
    Role::assign_to_user(&app.db, user_id, role.id)
        .await
        .expect("assign role to user");
    app.state.permissions().invalidate_user(user_id);
}

async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        grant_via_role(app, id, permissions).await;
    }
    let cookies = app.login(&name, "test-password-123").await;
    (id, cookies)
}

/// A published conference whose author is a real account, so a join from the
/// item to `users` has a stored password hash to reach.
async fn conference_by_real_author(app: &TestApp, marker: &str) -> (Uuid, String) {
    let name = username("gather-author");
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let author = user_id_of(app, &name).await;
    let hash: String = sqlx::query_scalar("SELECT pass FROM users WHERE id = $1")
        .bind(author)
        .fetch_one(&app.db)
        .await
        .expect("author hash");
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: "conference".to_string(),
                title: format!("{marker} {}", Uuid::now_v7().simple()),
                author_id: author,
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: None,
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("gather ad hoc lockdown test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create");
    (author, hash)
}

/// Conferences whose title carries `marker`, optionally joined to `users` with
/// the author's `pass` column projected.
fn conference_definition(marker: &str, join_users: bool) -> QueryDefinition {
    let mut definition = QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some("conference".to_string()),
        filters: vec![QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }],
        stage_aware: true,
        ..Default::default()
    };
    if join_users {
        definition.relationships = vec![QueryRelationship {
            name: "u".to_string(),
            target_table: "users".to_string(),
            join_type: JoinType::Left,
            local_field: "author_id".to_string(),
            foreign_field: "id".to_string(),
        }];
        definition.fields = ["id", "type", "author_id", "status", "stage_id", "title"]
            .into_iter()
            .map(|f| QueryField {
                field_name: f.to_string(),
                table_alias: None,
                label: None,
            })
            .chain([QueryField {
                field_name: "pass".to_string(),
                table_alias: Some("u".to_string()),
                label: None,
            }])
            .collect();
    }
    definition
}

/// POST an ad hoc gather as the holder of `cookies` (empty for anonymous).
async fn post_adhoc(
    app: &TestApp,
    definition: &QueryDefinition,
    cookies: &str,
    bucket: &str,
) -> (StatusCode, String) {
    let body = serde_json::json!({
        "definition": definition,
        "display": display(25),
    });
    let response = app
        .request_with_cookies(
            Request::post("/api/gather/query")
                .header("content-type", "application/json")
                .header("x-forwarded-for", test_ip_for(bucket))
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// (a) An anonymous caller cannot run an ad hoc gather at all, and the refusal
/// is a JSON body, not an HTML redirect.
#[test]
fn adhoc_gather_refuses_anonymous() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let marker = format!("GADHOC-anon-{}", Uuid::now_v7().simple());
        let (status, body) = post_adhoc(
            app,
            &conference_definition(&marker, false),
            "",
            &username("gadhoc-anon"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
        let json: serde_json::Value = serde_json::from_str(&body).expect("the 403 body is JSON");
        assert!(json.get("error").is_some(), "JSON error body: {body}");
    });
}

/// (b) An anonymous definition that joins the author's account row is refused,
/// and nothing of the stored hash reaches the response.
#[test]
fn adhoc_gather_anonymous_users_join_leaks_nothing() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let marker = format!("GADHOC-leak-{}", Uuid::now_v7().simple());
        let (_author, hash) = conference_by_real_author(app, &marker).await;
        let (status, body) = post_adhoc(
            app,
            &conference_definition(&marker, true),
            "",
            &username("gadhoc-leak"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
        assert!(!body.contains(&hash), "the author's hash leaked: {body}");
        assert!(!body.contains("$argon2"), "a password hash leaked: {body}");
    });
}

/// (c) Logging in is not enough; the endpoint takes `administer site`.
#[test]
fn adhoc_gather_refuses_user_without_permission() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let (_id, cookies) = user_holding(app, "gadhoc-norole", &[]).await;
        let marker = format!("GADHOC-norole-{}", Uuid::now_v7().simple());
        let (status, body) = post_adhoc(
            app,
            &conference_definition(&marker, false),
            &cookies,
            &username("gadhoc-norole"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    });
}

/// (d) A role that grants `administer site` runs a plain content gather.
#[test]
fn adhoc_gather_allows_administer_site() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let marker = format!("GADHOC-site-{}", Uuid::now_v7().simple());
        let conference = make_conference(app, &admin, &marker, 1).await;
        let (_id, cookies) = user_holding(app, "gadhoc-site", &["administer site"]).await;
        let (status, body) = post_adhoc(
            app,
            &conference_definition(&marker, false),
            &cookies,
            &username("gadhoc-site"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(
            body.contains(&conference.to_string()),
            "the conference is listed: {body}"
        );
    });
}

/// (e) A superuser runs a plain content gather.
#[test]
fn adhoc_gather_allows_superuser() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let name = username("gadhoc-super");
        let cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let marker = format!("GADHOC-super-{}", Uuid::now_v7().simple());
        let (status, body) =
            post_adhoc(app, &conference_definition(&marker, false), &cookies, &name).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    });
}

/// (f) The table policy does not depend on who asks: the service refuses a
/// join to `users` even for a superuser viewer.
#[test]
fn gather_service_refuses_users_join_for_superuser() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let marker = format!("GADHOC-svc-{}", Uuid::now_v7().simple());
        conference_by_real_author(app, &marker).await;
        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let ctx = QueryContext {
            current_user_id: Some(Uuid::nil()),
            viewer: Some(admin),
            url_args: HashMap::new(),
            language: None,
        };
        let result = app
            .state
            .gather()
            .execute_definition(
                &conference_definition(&marker, true),
                &display(25),
                1,
                HashMap::new(),
                LIVE_STAGE_ID,
                &ctx,
            )
            .await;
        assert!(result.is_err(), "a users join must be refused: {result:?}");
    });
}

/// (g) A named gather stored without passing through `register_query`, the way
/// a plugin migration seeds one, is refused when it runs.
#[test]
fn named_gather_seeded_by_sql_cannot_read_users() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_conference_type().await;
        let marker = format!("GADHOC-named-{}", Uuid::now_v7().simple());
        let (_author, hash) = conference_by_real_author(app, &marker).await;
        let query_id = format!("gadhoc_named_{}", Uuid::now_v7().simple());
        let definition = serde_json::to_value(conference_definition(&marker, true)).unwrap();
        let display = serde_json::to_value(display(25)).unwrap();
        sqlx::query(
            "INSERT INTO gather_query \
             (query_id, label, description, definition, display, plugin, created, changed) \
             VALUES ($1, 'seeded', NULL, $2, $3, 'test', 0, 0)",
        )
        .bind(&query_id)
        .bind(&definition)
        .bind(&display)
        .execute(&app.db)
        .await
        .expect("seed the named gather");
        app.state.gather().reload_from_db().await.expect("reload");

        let response = app
            .request(
                Request::get(format!("/api/query/{query_id}/execute"))
                    .header("x-forwarded-for", test_ip_for(&query_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&bytes).into_owned();

        sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(&query_id)
            .execute(&app.db)
            .await
            .expect("remove the seeded gather");
        app.state.gather().reload_from_db().await.expect("reload");

        assert_ne!(status, StatusCode::OK, "body: {body}");
        assert!(!body.contains(&hash), "the author's hash leaked: {body}");
        assert!(!body.contains("$argon2"), "a password hash leaked: {body}");
    });
}
