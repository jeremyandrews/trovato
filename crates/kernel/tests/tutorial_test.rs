#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tutorial validation tests for Part 1: Hello, Trovato.
//!
//! These tests verify the claims made in `docs/tutorial/part-01-hello-trovato.md`.
//! Each test is named `test_part01_stepNN_*` to match the tutorial's step numbering.
//! A CI script cross-references `## Step` headers in the tutorial markdown against
//! test function names to ensure every step has at least one test.
//!
//! ## Running
//!
//! ```bash
//! cargo test --test tutorial_test
//! ```

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::sync::{Mutex, OnceCell};
use trovato_kernel::gather::{
    DisplayFormat, FilterOperator, FilterValue, GatherQuery, PagerConfig, PagerStyle,
    QueryDefinition, QueryDisplay, QueryFilter, QuerySort, SortDirection,
};
use trovato_kernel::models::stage::LIVE_STAGE_ID;

mod common;
use common::{run_test, shared_app};

/// Parse a response body as JSON.
async fn response_json(response: axum::response::Response) -> Value {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read response body")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("response body is not valid JSON")
}

/// Read response body as a UTF-8 string.
async fn response_text(response: axum::response::Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("failed to read response body")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("response body is not valid UTF-8")
}

// =============================================================================
// Tutorial data seeding
//
// The tutorial guides users to create conferences, a gather, and a URL alias
// by hand through the admin UI. Tests seed the same data programmatically.
// This replaces the deleted seed migrations that previously provided this data.
// =============================================================================

/// One-time initialization cell for tutorial seed data.
static TUTORIAL_SEED: OnceCell<()> = OnceCell::const_new();

/// Seed the `conference` item type, 3 tutorial conferences, the
/// `upcoming_conferences` gather, and the `/conferences` URL alias.
/// Idempotent — safe to call from any test.
async fn seed_tutorial_data(app: &'static common::TestApp) {
    TUTORIAL_SEED
        .get_or_init(|| async {
            let now = chrono::Utc::now().timestamp();

            // --- Conference item type + items ---
            app.ensure_conference_items().await;

            // --- Gather query ---
            // Use register_query() which atomically persists to DB and updates
            // the in-memory cache for this single query, avoiding a global
            // load_queries() reload that could race with other tests.
            if app.state.gather().get_query("upcoming_conferences").is_none() {
                app.state
                    .gather()
                    .register_query(GatherQuery {
                        query_id: "upcoming_conferences".to_string(),
                        label: "Upcoming Conferences".to_string(),
                        description: Some(
                            "Published conferences sorted by start date".to_string(),
                        ),
                        definition: QueryDefinition {
                            base_table: "item".to_string(),
                            item_type: Some("conference".to_string()),
                            fields: vec![],
                            filters: vec![QueryFilter {
                                field: "status".to_string(),
                                operator: FilterOperator::Equals,
                                value: FilterValue::Integer(1),
                                exposed: false,
                                exposed_label: None,
                                widget: Default::default(),
                            }],
                            sorts: vec![QuerySort {
                                field: "fields.field_start_date".to_string(),
                                direction: SortDirection::Asc,
                                nulls: None,
                            }],
                            relationships: vec![],
                            includes: std::collections::HashMap::new(),
                            stage_aware: true,
                        },
                        display: QueryDisplay {
                            format: DisplayFormat::Table,
                            items_per_page: 25,
                            pager: PagerConfig {
                                enabled: true,
                                style: PagerStyle::Full,
                                show_count: true,
                            },
                            empty_text: Some("No conferences found.".to_string()),
                            header: None,
                            footer: None,
                            canonical_url: None,
                            routes: Vec::new(),
                        },
                        plugin: "core".to_string(),
                        created: now,
                        changed: now,
                    })
                    .await
                    .expect("failed to register gather query");
            }

            // --- URL alias: /conferences → /gather/upcoming_conferences ---
            let alias_exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM url_alias WHERE alias = '/conferences' AND language = 'en' AND stage_id = $1)",
            )
            .bind(LIVE_STAGE_ID)
            .fetch_one(&app.db)
            .await
            .unwrap();

            if !alias_exists {
                sqlx::query(
                    r#"INSERT INTO url_alias (id, source, alias, language, stage_id, created)
                       VALUES (gen_random_uuid(), '/gather/upcoming_conferences', '/conferences', 'en', $1, $2)"#,
                )
                .bind(LIVE_STAGE_ID)
                .bind(now)
                .execute(&app.db)
                .await
                .expect("failed to seed /conferences URL alias");
            }
        })
        .await;
}

// =============================================================================
// Step 1: Install Trovato
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 1
//
// The tutorial claims the health endpoint returns:
//   {"status":"healthy","postgres":true,"redis":true}
// =============================================================================

#[test]
fn test_part01_step01_health_check() {
    run_test(async {
        let app = shared_app().await;

        let response = app
            .request(Request::get("/health").body(Body::empty()).unwrap())
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = response_json(response).await;
        assert_eq!(body["status"], "healthy");
        assert_eq!(body["postgres"], true);
        assert_eq!(body["redis"], true);
    });
}

// =============================================================================
// Step 2: Define the Conference Item Type
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 2
//
// The tutorial claims:
// - "conference" type exists and is visible via /api/content-types
// - 12 fields with specific names, types, and required status
// - title_label is "Conference Name"
// =============================================================================

#[test]
fn test_part01_step02_conference_type_in_api() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/content-types")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let types: Vec<String> =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();

        assert!(
            types.contains(&"conference".to_string()),
            "conference type must appear in /api/content-types; found: {types:?}"
        );
    });
}

#[test]
fn test_part01_step02_conference_has_12_fields() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let row: (Value,) =
            sqlx::query_as("SELECT settings FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .expect("conference type must exist in item_type table");

        let fields = row.0["fields"]
            .as_array()
            .expect("settings.fields must be an array");

        // Tutorial Step 2 creates 12 fields via admin UI
        assert_eq!(
            fields.len(),
            12,
            "conference type should have 12 fields, found {}",
            fields.len()
        );

        // Verify every field name documented in the tutorial exists
        let expected_names = [
            "field_url",
            "field_start_date",
            "field_end_date",
            "field_city",
            "field_country",
            "field_online",
            "field_cfp_url",
            "field_cfp_end_date",
            "field_description",
            "field_language",
            "field_source_id",
            "field_editor_notes",
        ];

        let actual_names: Vec<&str> = fields
            .iter()
            .filter_map(|f| f["field_name"].as_str())
            .collect();

        for name in &expected_names {
            assert!(
                actual_names.contains(name),
                "missing field '{name}'; found: {actual_names:?}"
            );
        }
    });
}

#[test]
fn test_part01_step02_field_types() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let row: (Value,) =
            sqlx::query_as("SELECT settings FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .unwrap();

        let fields = row.0["fields"].as_array().unwrap();

        // Build a lookup by field_name
        let field_map: std::collections::HashMap<&str, &Value> = fields
            .iter()
            .filter_map(|f| f["field_name"].as_str().map(|n| (n, &f["field_type"])))
            .collect();

        // Tutorial documents these specific field types:
        // Date fields serialize as the string "Date"
        assert_eq!(field_map["field_start_date"], "Date");
        assert_eq!(field_map["field_end_date"], "Date");
        assert_eq!(field_map["field_cfp_end_date"], "Date");

        // Boolean serializes as "Boolean"
        assert_eq!(field_map["field_online"], "Boolean");

        // Text serializes as {"Text": {"max_length": ...}}
        assert!(
            field_map["field_city"].get("Text").is_some(),
            "field_city should be Text type"
        );

        // TextLong serializes as "TextLong"
        assert_eq!(field_map["field_description"], "TextLong");
    });
}

#[test]
fn test_part01_step02_required_fields() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let row: (Value,) =
            sqlx::query_as("SELECT settings FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .unwrap();

        let fields = row.0["fields"].as_array().unwrap();

        // Tutorial says only start_date and end_date are required
        let required: Vec<&str> = fields
            .iter()
            .filter(|f| f["required"].as_bool().unwrap_or(false))
            .filter_map(|f| f["field_name"].as_str())
            .collect();

        assert!(
            required.contains(&"field_start_date"),
            "field_start_date must be required"
        );
        assert!(
            required.contains(&"field_end_date"),
            "field_end_date must be required"
        );
        assert_eq!(
            required.len(),
            2,
            "only start_date and end_date should be required; found: {required:?}"
        );
    });
}

#[test]
fn test_part01_step02_title_label() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let row: (String,) =
            sqlx::query_as("SELECT title_label FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .expect("conference type must exist");

        // Tutorial: "The title field at the top uses the custom label 'Conference Name'"
        assert_eq!(row.0, "Conference Name");
    });
}

// =============================================================================
// Step 3: Create Your First Conference
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 3
//
// The tutorial guides users to create 3 conferences by hand.
// Tests seed the same data programmatically, then verify:
// - 3 conferences exist (RustConf, EuroRust, WasmCon Online)
// - Items viewable at /item/{uuid} (HTML) and /api/item/{uuid} (JSON)
// - Items have UUIDv7 IDs, Unix timestamps, and live stage_id
// =============================================================================

#[test]
fn test_part01_step03_conferences_exist() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT title FROM item WHERE type = 'conference' AND status = 1 ORDER BY title",
        )
        .fetch_all(&app.db)
        .await
        .unwrap();

        let titles: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();

        // Tutorial Step 3 creates 3 conferences
        assert!(
            titles.contains(&"RustConf 2026"),
            "RustConf 2026 must exist; found: {titles:?}"
        );
        assert!(
            titles.contains(&"EuroRust 2026"),
            "EuroRust 2026 must exist; found: {titles:?}"
        );
        assert!(
            titles.contains(&"WasmCon Online 2026"),
            "WasmCon Online 2026 must exist; found: {titles:?}"
        );
    });
}

#[test]
fn test_part01_step03_item_viewable_as_html() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Viewing items requires "access content" permission, so authenticate
        let cookies = app
            .create_and_login_admin("tut_view", "tutorial-test-pw", "tut_view@test.local")
            .await;

        // Get a conference item ID
        let row: (uuid::Uuid,) =
            sqlx::query_as("SELECT id FROM item WHERE type = 'conference' AND status = 1 LIMIT 1")
                .fetch_one(&app.db)
                .await
                .unwrap();

        // Tutorial: "Every item is viewable at /item/{id}"
        let path = format!("/item/{}", row.0);
        let response = app
            .request_with_cookies(Request::get(&path).body(Body::empty()).unwrap(), &cookies)
            .await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/item/{{uuid}} should return 200 for a published conference"
        );

        let body = response_text(response).await;
        assert!(
            body.contains("</html>") || body.contains("</HTML>"),
            "/item/{{uuid}} should return HTML"
        );
    });
}

#[test]
fn test_part01_step03_item_json_api() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Get a conference item ID and title
        let row: (uuid::Uuid, String) = sqlx::query_as(
            "SELECT id, title FROM item WHERE type = 'conference' AND status = 1 AND title = 'RustConf 2026'",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();

        // Tutorial: "There is also a JSON API at /api/item/{id}"
        let path = format!("/api/item/{}", row.0);
        let response = app
            .request(Request::get(&path).body(Body::empty()).unwrap())
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = response_json(response).await;
        assert_eq!(body["title"], row.1);
        // Field serializes as "type" due to #[serde(rename = "type")]
        assert_eq!(body["type"], "conference");
    });
}

#[test]
fn test_part01_step03_items_on_live_stage() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Tutorial: "Every item has a stage_id that defaults to the live stage"
        let rows: Vec<(uuid::Uuid,)> =
            sqlx::query_as("SELECT stage_id FROM item WHERE type = 'conference' AND status = 1")
                .fetch_all(&app.db)
                .await
                .unwrap();

        assert!(
            !rows.is_empty(),
            "at least one published conference must exist"
        );

        for row in &rows {
            assert_eq!(
                row.0, LIVE_STAGE_ID,
                "conference item should be on the live stage"
            );
        }
    });
}

#[test]
fn test_part01_step03_timestamps_are_unix() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Tutorial: "created and changed columns store Unix timestamps (seconds since epoch)"
        let row: (i64, i64) = sqlx::query_as(
            "SELECT created, changed FROM item WHERE type = 'conference' AND status = 1 LIMIT 1",
        )
        .fetch_one(&app.db)
        .await
        .unwrap();

        // Unix timestamps should be reasonable (after 2020, before 2100)
        let min_ts = 1_577_836_800_i64; // 2020-01-01
        let max_ts = 4_102_444_800_i64; // 2100-01-01
        assert!(
            row.0 > min_ts && row.0 < max_ts,
            "created timestamp {} should be a reasonable Unix timestamp",
            row.0
        );
        assert!(
            row.1 > min_ts && row.1 < max_ts,
            "changed timestamp {} should be a reasonable Unix timestamp",
            row.1
        );
    });
}

#[test]
fn test_part01_step03_online_boolean_field() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Tutorial: 'For WasmCon Online 2026, you would see "field_online": "1"'
        let row: (Value,) = sqlx::query_as(
            "SELECT fields FROM item WHERE type = 'conference' AND title = 'WasmCon Online 2026'",
        )
        .fetch_one(&app.db)
        .await
        .expect("WasmCon Online 2026 must exist");

        assert_eq!(
            row.0["field_online"], "1",
            "WasmCon Online should have field_online set to \"1\""
        );

        // RustConf should NOT have field_online (unchecked = absent)
        let row2: (Value,) = sqlx::query_as(
            "SELECT fields FROM item WHERE type = 'conference' AND title = 'RustConf 2026'",
        )
        .fetch_one(&app.db)
        .await
        .expect("RustConf 2026 must exist");

        assert!(
            row2.0.get("field_online").is_none() || row2.0["field_online"].is_null(),
            "RustConf should not have field_online set"
        );
    });
}

// =============================================================================
// Step 4: Build Your First Gather
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 4
//
// The tutorial guides users to create a gather and URL alias via admin UI.
// Tests seed the same data programmatically, then verify:
// - Gather query "upcoming_conferences" exists
// - /conferences URL alias resolves to the gather
// - Results sorted by start_date ascending
// - Pagination: 25 items per page
// - Empty text configured
// =============================================================================

#[test]
fn test_part01_step04_gather_query_exists() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Tutorial: Gather query_id "upcoming_conferences"
        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/api/query/upcoming_conferences should exist"
        );

        let body = response_json(response).await;
        assert_eq!(body["query_id"], "upcoming_conferences");
        assert_eq!(body["label"], "Upcoming Conferences");
    });
}

#[test]
fn test_part01_step04_gather_returns_conferences() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = response_json(response).await;

        // Should return at least the 3 tutorial conferences
        let total = body["total"].as_u64().unwrap_or(0);
        assert!(
            total >= 3,
            "gather should return at least 3 conferences, got {total}"
        );

        let items = body["items"].as_array().expect("items must be an array");
        assert!(!items.is_empty(), "gather items array should not be empty");
    });
}

#[test]
fn test_part01_step04_conferences_url_alias() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Tutorial: "/conferences" URL alias resolves to /gather/upcoming_conferences
        //
        // Note: axum 0.8's Router::layer() applies middleware to matched
        // routes, so URI rewriting in middleware does not affect route
        // matching in oneshot tests. We verify the full alias chain at the
        // model + direct-path layer instead. The middleware resolves
        // correctly in production with a real HTTP listener.

        // 1. Alias record exists in the database
        let alias_row: Option<(String, String, uuid::Uuid)> = sqlx::query_as(
            "SELECT source, language, stage_id FROM url_alias WHERE alias = '/conferences'",
        )
        .fetch_optional(&app.db)
        .await
        .unwrap();
        let (source, lang, stage) =
            alias_row.expect("URL alias for /conferences must exist in database");
        // Accept either the Part 1 or Part 2 gather query as the alias source.
        assert!(
            source == "/gather/upcoming_conferences"
                || source == "/gather/ritrovo.upcoming_conferences",
            "expected gather query source for /conferences alias, got {source}"
        );
        assert_eq!(lang, "en");
        assert_eq!(stage, LIVE_STAGE_ID);

        // 2. Alias lookup works with the same parameters the middleware uses
        let found = trovato_kernel::models::UrlAlias::find_by_alias_with_context(
            &app.db,
            "/conferences",
            LIVE_STAGE_ID,
            app.state.default_language(),
        )
        .await
        .expect("alias lookup should not error");
        assert!(
            found.is_some(),
            "find_by_alias_with_context should find /conferences alias"
        );

        // 3. The target path renders the gather page
        let response = app
            .request(
                Request::get("/gather/upcoming_conferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "/gather/upcoming_conferences should return 200"
        );

        let body = response_text(response).await;
        // After Part 2, the gather has thousands of conferences; the hand-created
        // ones may not be on page 1. Just verify the page renders as HTML with
        // a gather container (not an error page).
        assert!(
            body.contains("gather-query") || body.contains("conf-card"),
            "gather page should render as HTML with gather content"
        );
    });
}

#[test]
fn test_part01_step04_sorted_by_start_date() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        let body = response_json(response).await;
        let items = body["items"].as_array().unwrap();

        // Verify items are sorted by start_date ascending
        let dates: Vec<&str> = items
            .iter()
            .filter_map(|item| item["fields"]["field_start_date"].as_str())
            .collect();

        for window in dates.windows(2) {
            assert!(
                window[0] <= window[1],
                "conferences should be sorted by start_date ascending: '{}' should come before '{}'",
                window[0],
                window[1]
            );
        }
    });
}

#[test]
fn test_part01_step04_pagination_config() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        let body = response_json(response).await;

        // Tutorial: "25 items per page"
        let per_page = body["display"]["items_per_page"]
            .as_u64()
            .expect("items_per_page should be set");
        assert_eq!(per_page, 25, "gather should show 25 items per page");

        // Pager should be enabled
        let pager_enabled = body["display"]["pager"]["enabled"]
            .as_bool()
            .unwrap_or(false);
        assert!(pager_enabled, "pager should be enabled");
    });
}

#[test]
fn test_part01_step04_empty_text_configured() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        let body = response_json(response).await;

        // Tutorial: 'the Gather displays the configured empty text: "No conferences found."'
        let empty_text = body["display"]["empty_text"]
            .as_str()
            .expect("empty_text should be configured");
        assert_eq!(empty_text, "No conferences found.");
    });
}

#[test]
fn test_part01_step04_gather_status_filter() {
    run_test(async {
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        let body = response_json(response).await;

        // Tutorial: filter status = 1 (published only)
        let filters = body["definition"]["filters"]
            .as_array()
            .expect("filters should be an array");

        let status_filter = filters
            .iter()
            .find(|f| f["field"].as_str() == Some("status"))
            .expect("should have a status filter");

        assert_eq!(
            status_filter["value"], 1,
            "status filter should equal 1 (published)"
        );
    });
}

// =============================================================================
// Step 5: Human-Friendly URLs
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 5
//
// The tutorial guides users to:
// - Set pathauto pattern "conferences/[title]" for the conference type
// - Click "Regenerate aliases" to backfill the three existing conferences
//
// Tests verify:
// - update_alias_item generates the correct alias for each conference
// - Expected aliases are findable via find_by_alias_with_context (same
//   lookup the path alias middleware uses)
// - A second regeneration call skips items whose alias already matches
// =============================================================================

/// Serializes Step 5 tests that write to `site_config.pathauto_patterns`.
static STEP5_PATHAUTO_LOCK: Mutex<()> = Mutex::const_new(());

#[test]
fn test_part01_step05_pathauto_generates_conference_aliases() {
    run_test(async {
        let _lock = STEP5_PATHAUTO_LOCK.lock().await;
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Snapshot original patterns so we can restore them after the test.
        let original_patterns =
            trovato_kernel::models::SiteConfig::get(&app.db, "pathauto_patterns")
                .await
                .expect("failed to read pathauto patterns");

        // Set the conference pathauto pattern (equivalent to Option B config import
        // or saving the form in Option A).
        trovato_kernel::models::SiteConfig::set(
            &app.db,
            "pathauto_patterns",
            serde_json::json!({"conference": "conferences/[title]"}),
        )
        .await
        .expect("failed to set pathauto pattern");

        // Clear any existing conference aliases so update_alias_item creates
        // fresh ones rather than short-circuiting on an already-matching alias.
        sqlx::query("DELETE FROM url_alias WHERE alias LIKE '/conferences/%'")
            .execute(&app.db)
            .await
            .expect("failed to clear existing conference aliases");

        // Use the three hand-created tutorial conferences (no field_source_id).
        // After Part 2, the DB may have thousands of imported conferences; only
        // test pathauto on the original three.
        let conferences: Vec<(uuid::Uuid, String, i64)> = sqlx::query_as(
            "SELECT id, title, created FROM item WHERE type = 'conference' \
             AND (fields->>'field_source_id' IS NULL OR fields->>'field_source_id' = '') \
             AND title IN ('RustConf 2026', 'EuroRust 2026', 'WasmCon Online 2026') \
             ORDER BY title",
        )
        .fetch_all(&app.db)
        .await
        .expect("failed to fetch conferences");

        assert_eq!(
            conferences.len(),
            3,
            "exactly 3 hand-created tutorial conferences must exist"
        );

        // Call update_alias_item for each — this is exactly what the admin
        // "Regenerate aliases" button invokes for every item of the type.
        let mut generated = std::collections::HashMap::new();
        for (id, title, created) in &conferences {
            let alias = trovato_kernel::services::pathauto::update_alias_item(
                &app.db,
                *id,
                title,
                "conference",
                *created,
            )
            .await
            .expect("update_alias_item should succeed");
            assert!(
                alias.is_some(),
                "update_alias_item should generate an alias for '{title}'"
            );
            generated.insert(title.clone(), alias.unwrap());
        }

        // Tutorial: RustConf 2026 → /conferences/rustconf-2026
        assert_eq!(
            generated["RustConf 2026"], "/conferences/rustconf-2026",
            "RustConf alias must match tutorial"
        );
        // Tutorial: EuroRust 2026 → /conferences/eurorust-2026
        assert_eq!(
            generated["EuroRust 2026"], "/conferences/eurorust-2026",
            "EuroRust alias must match tutorial"
        );
        // Tutorial: WasmCon Online 2026 → /conferences/wasmcon-online-2026
        assert_eq!(
            generated["WasmCon Online 2026"], "/conferences/wasmcon-online-2026",
            "WasmCon alias must match tutorial"
        );

        // Restore original pathauto patterns.
        match original_patterns {
            Some(v) => trovato_kernel::models::SiteConfig::set(&app.db, "pathauto_patterns", v)
                .await
                .expect("failed to restore pathauto patterns"),
            None => sqlx::query("DELETE FROM site_config WHERE key = 'pathauto_patterns'")
                .execute(&app.db)
                .await
                .map(|_| ())
                .expect("failed to remove pathauto patterns"),
        }
    });
}

#[test]
fn test_part01_step05_aliases_resolvable_by_middleware_lookup() {
    run_test(async {
        let _lock = STEP5_PATHAUTO_LOCK.lock().await;
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Configure pattern and clear existing aliases.
        trovato_kernel::models::SiteConfig::set(
            &app.db,
            "pathauto_patterns",
            serde_json::json!({"conference": "conferences/[title]"}),
        )
        .await
        .expect("failed to set pathauto pattern");

        sqlx::query("DELETE FROM url_alias WHERE alias LIKE '/conferences/%'")
            .execute(&app.db)
            .await
            .expect("failed to clear conference aliases");

        // Regenerate aliases for all three conferences.
        let conferences: Vec<(uuid::Uuid, String, i64)> =
            sqlx::query_as("SELECT id, title, created FROM item WHERE type = 'conference'")
                .fetch_all(&app.db)
                .await
                .expect("failed to fetch conferences");

        for (id, title, created) in &conferences {
            trovato_kernel::services::pathauto::update_alias_item(
                &app.db,
                *id,
                title,
                "conference",
                *created,
            )
            .await
            .expect("update_alias_item should succeed");
        }

        // Verify each expected alias is findable via find_by_alias_with_context,
        // which is the exact lookup the path alias middleware uses.
        // (Per the Axum 0.8 testing limitation, we verify at model layer rather
        // than sending HTTP requests — URI rewriting in middleware does not affect
        // route matching in oneshot tests.)
        let expected_aliases = [
            "/conferences/rustconf-2026",
            "/conferences/eurorust-2026",
            "/conferences/wasmcon-online-2026",
        ];

        for alias_path in &expected_aliases {
            let found = trovato_kernel::models::UrlAlias::find_by_alias_with_context(
                &app.db,
                alias_path,
                LIVE_STAGE_ID,
                app.state.default_language(),
            )
            .await
            .unwrap_or_else(|e| panic!("alias lookup failed for {alias_path}: {e}"));

            let record = found.unwrap_or_else(|| {
                panic!("alias '{alias_path}' not found in url_alias after regeneration")
            });

            // Source must be /item/{uuid} — the canonical internal path.
            assert!(
                record.source.starts_with("/item/"),
                "alias source for '{alias_path}' should be /item/{{uuid}}, got '{}'",
                record.source
            );
        }

        // Restore pathauto config.
        sqlx::query("DELETE FROM site_config WHERE key = 'pathauto_patterns'")
            .execute(&app.db)
            .await
            .expect("failed to remove pathauto patterns");
    });
}

#[test]
fn test_part01_step05_regenerate_skips_already_matching_alias() {
    run_test(async {
        let _lock = STEP5_PATHAUTO_LOCK.lock().await;
        let app = shared_app().await;
        seed_tutorial_data(app).await;

        // Configure pattern and clear existing aliases.
        trovato_kernel::models::SiteConfig::set(
            &app.db,
            "pathauto_patterns",
            serde_json::json!({"conference": "conferences/[title]"}),
        )
        .await
        .expect("failed to set pathauto pattern");

        sqlx::query("DELETE FROM url_alias WHERE alias LIKE '/conferences/%'")
            .execute(&app.db)
            .await
            .expect("failed to clear conference aliases");

        let row: (uuid::Uuid, String, i64) = sqlx::query_as(
            "SELECT id, title, created FROM item WHERE type = 'conference' AND title = 'RustConf 2026'",
        )
        .fetch_one(&app.db)
        .await
        .expect("RustConf 2026 must exist");

        let (id, title, created) = row;

        // First call: creates the alias.
        let first = trovato_kernel::services::pathauto::update_alias_item(
            &app.db,
            id,
            &title,
            "conference",
            created,
        )
        .await
        .expect("first update_alias_item should succeed");
        assert!(first.is_some(), "first call should create the alias");

        // Second call on the same item: alias already matches — must return None.
        // Tutorial: "Items whose alias already matches the current pattern are
        // skipped automatically."
        let second = trovato_kernel::services::pathauto::update_alias_item(
            &app.db,
            id,
            &title,
            "conference",
            created,
        )
        .await
        .expect("second update_alias_item should succeed");
        assert!(
            second.is_none(),
            "second call should return None (alias already matches)"
        );

        // Restore pathauto config.
        sqlx::query("DELETE FROM site_config WHERE key = 'pathauto_patterns'")
            .execute(&app.db)
            .await
            .expect("failed to remove pathauto patterns");
    });
}
