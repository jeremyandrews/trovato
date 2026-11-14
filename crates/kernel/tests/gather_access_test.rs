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
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
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

async fn user_holding(app: &TestApp, prefix: &str, permissions: &[&str]) -> (Uuid, String) {
    let name = username(prefix);
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let id = user_id_of(app, &name).await;
    if !permissions.is_empty() {
        common::grant_via_role(app, id, permissions).await;
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

// -------------------------------------------------------------------------
// A projection of the whole `fields` column is field-access filtered
// -------------------------------------------------------------------------
//
// `SELECT item.*` drops the fields `tap_field_access` denies from the nested
// `fields` object. A definition that projects the column named exactly
// `fields` returns that same object, and must drop the same fields.
//
// The shared app loads no field-access plugin, so this builds its own app with
// the reference plugin (`plugins/trovato_field_access_ref`) loaded, the way
// `recovery_plugin_flow_test` builds one with its fixtures: its default rules
// deny `ssn` on type `person` to a viewer without `view pii` and leave `bio`
// ungoverned (visible).

const FIELD_ACCESS_PLUGIN: &str = "trovato_field_access_ref";
const PERSON_TYPE: &str = "person";

/// The app with the field-access reference plugin loaded. Built once.
static FIELD_ACCESS_APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app_with_field_access() -> &'static TestApp {
    FIELD_ACCESS_APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app_with_field_access()))
            .join()
            .expect("field-access fixture app init thread panicked")
    })
}

/// Enable the reference plugin, build an app that loads it and nothing else,
/// then put the plugin's status back so no later app anywhere picks it up.
///
/// `AppState` resolves its enabled plugin set at construction, so the row has to
/// say "enabled" while the app is built and nothing longer. The plugin is copied
/// alone into a temporary plugins directory so the app loads only this plugin.
async fn build_app_with_field_access() -> TestApp {
    // Two `AppState`s built concurrently race on plugin migrations; the shared
    // app other tests in this file use is built first.
    let shared = shared_app().await;

    let source = common::project_root()
        .join("plugins")
        .join(FIELD_ACCESS_PLUGIN);
    let wasm = format!("{FIELD_ACCESS_PLUGIN}.wasm");
    let info = format!("{FIELD_ACCESS_PLUGIN}.info.toml");
    assert!(
        source.join(&wasm).exists(),
        "fixture '{FIELD_ACCESS_PLUGIN}' is not built: cargo build -p {FIELD_ACCESS_PLUGIN} \
         --target wasm32-wasip1 --release && cp target/wasm32-wasip1/release/{wasm} \
         plugins/{FIELD_ACCESS_PLUGIN}/"
    );
    let plugins_dir =
        std::env::temp_dir().join(format!("trovato-gather-fa-{}", Uuid::now_v7().simple()));
    let target = plugins_dir.join(FIELD_ACCESS_PLUGIN);
    std::fs::create_dir_all(&target).expect("create the temporary plugins directory");
    for file in [&wasm, &info] {
        std::fs::copy(source.join(file), target.join(file)).expect("copy the fixture plugin");
    }

    let previous: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(FIELD_ACCESS_PLUGIN)
            .fetch_optional(&shared.db)
            .await
            .expect("read the plugin status");
    trovato_kernel::plugin::status::install_plugin(&shared.db, FIELD_ACCESS_PLUGIN, "1.0.0")
        .await
        .expect("enable the field-access fixture");

    let dir = plugins_dir.clone();
    let app = TestApp::with_config(move |config| config.plugins_dirs = vec![dir]).await;

    match previous {
        Some(status) => {
            sqlx::query("UPDATE plugin_status SET status = $1 WHERE name = $2")
                .bind(status)
                .bind(FIELD_ACCESS_PLUGIN)
                .execute(&shared.db)
                .await
                .expect("restore the plugin status");
        }
        None => {
            sqlx::query("DELETE FROM plugin_status WHERE name = $1")
                .bind(FIELD_ACCESS_PLUGIN)
                .execute(&shared.db)
                .await
                .expect("remove the plugin status");
        }
    }
    std::fs::remove_dir_all(&plugins_dir).ok();
    app
}

/// A published person carrying the governed `ssn` and the ungoverned `bio`,
/// deleted again when the test ends.
async fn make_person(app: &TestApp, marker: &str, ssn: &str, bio: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: PERSON_TYPE.to_string(),
                title: format!("{marker} {}", Uuid::now_v7().simple()),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "ssn": ssn, "bio": bio })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("whole fields projection test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create the person")
        .id;
    let items = std::sync::Arc::clone(app.state.items());
    common::defer_cleanup(move || async move {
        let _ = items.delete(id, &admin).await;
    });
    id
}

/// The `person` type, put back the way it was (or removed) when the test ends.
async fn ensure_person_type(app: &TestApp) {
    app.restore_content_type_on_exit(PERSON_TYPE).await;
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Person', 'Field-access fixture', true, 'Name', 'seam_test', '{\"fields\": []}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(PERSON_TYPE)
    .execute(&app.db)
    .await
    .expect("seed the person type");
    app.state
        .content_types()
        .replace(
            PERSON_TYPE,
            "Person",
            Some("Field-access fixture"),
            serde_json::json!({ "fields": [] }),
        )
        .await
        .ok();
}

/// People whose title carries `marker`, projecting `title` and the whole
/// `fields` column, under `label` when one is given.
fn whole_fields_definition(marker: &str, label: Option<&str>) -> QueryDefinition {
    QueryDefinition {
        base_table: "item".to_string(),
        item_type: Some(PERSON_TYPE.to_string()),
        filters: vec![QueryFilter {
            field: "title".to_string(),
            operator: FilterOperator::Contains,
            value: FilterValue::String(marker.to_string()),
            exposed: false,
            exposed_label: None,
            widget: Default::default(),
        }],
        fields: vec![
            QueryField {
                field_name: "title".to_string(),
                table_alias: None,
                label: None,
            },
            QueryField {
                field_name: "fields".to_string(),
                table_alias: None,
                label: label.map(str::to_string),
            },
        ],
        stage_aware: true,
        ..Default::default()
    }
}

/// A definition projecting the whole `fields` column drops the fields
/// `tap_field_access` denies, through the ad hoc endpoint and through a public
/// named gather run anonymously, exactly as a `SELECT item.*` gather does.
///
/// The ad hoc caller holds `administer site` through a role, not superuser: a
/// superuser bypasses field access on every read path, `item.*` included, so it
/// is asserted here only to see what `item.*` shows it.
#[test]
fn whole_fields_projection_drops_denied_fields() {
    run_test(async {
        let app = app_with_field_access();
        ensure_person_type(app).await;

        let marker = format!("GFIELDS-{}", Uuid::now_v7().simple());
        let ssn = format!("ssn-{marker}");
        let bio = format!("bio-{marker}");
        make_person(app, &marker, &ssn, &bio).await;

        // (a) The ad hoc endpoint, as a holder of `administer site` without
        // `view pii`.
        let (_id, cookies) = user_holding(app, "gfields-site", &["administer site"]).await;
        let (status, body) = post_adhoc(
            app,
            &whole_fields_definition(&marker, None),
            &cookies,
            &username("gfields-site"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(body.contains(&bio), "the allowed field is returned: {body}");
        assert!(!body.contains(&ssn), "the denied field leaked: {body}");

        // The same caller's `item.*` gather, for the parity claim.
        let mut star = whole_fields_definition(&marker, None);
        star.fields.clear();
        let (status, body) = post_adhoc(app, &star, &cookies, &username("gfields-star")).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(
            body.contains(&bio),
            "item.* returns the allowed field: {body}"
        );
        assert!(
            !body.contains(&ssn),
            "item.* drops the denied field: {body}"
        );

        // A superuser sees every field through both, by the admin bypass.
        let name = username("gfields-super");
        let super_cookies = app
            .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        for definition in [whole_fields_definition(&marker, None), star] {
            let (status, body) =
                post_adhoc(app, &definition, &super_cookies, &username("gfields-super")).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            assert!(
                body.contains(&bio) && body.contains(&ssn),
                "superuser: {body}"
            );
        }

        // (b) A public named gather projecting `fields` under a label, run
        // anonymously.
        let query_id = format!("gfields_named_{}", Uuid::now_v7().simple());
        let definition =
            serde_json::to_value(whole_fields_definition(&marker, Some("All"))).unwrap();
        let display = serde_json::to_value(display(25)).unwrap();
        sqlx::query(
            "INSERT INTO gather_query \
             (query_id, label, description, definition, display, plugin, created, changed) \
             VALUES ($1, 'whole fields', NULL, $2, $3, 'test', 0, 0)",
        )
        .bind(&query_id)
        .bind(&definition)
        .bind(&display)
        .execute(&app.db)
        .await
        .expect("seed the named gather");
        let gather = app.state.clone();
        let cleanup_id = query_id.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
                .bind(&cleanup_id)
                .execute(gather.db())
                .await;
            let _ = gather.gather().reload_from_db().await;
        });
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
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(body.contains(&bio), "the allowed field is returned: {body}");
        assert!(!body.contains(&ssn), "the denied field leaked: {body}");
    });
}
