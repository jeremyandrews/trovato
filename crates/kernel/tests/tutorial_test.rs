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
// - 17 fields with specific names, types, required status, and cardinality
// - title_label is "Conference Name"
// =============================================================================

#[test]
fn test_part01_step02_conference_type_in_api() {
    run_test(async {
        let app = shared_app().await;

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
fn test_part01_step02_conference_has_17_fields() {
    run_test(async {
        let app = shared_app().await;

        let row: (Value,) =
            sqlx::query_as("SELECT settings FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .expect("conference type must exist in item_type table");

        let fields = row.0["fields"]
            .as_array()
            .expect("settings.fields must be an array");

        // Tutorial Step 2 claims 17 fields
        assert_eq!(
            fields.len(),
            17,
            "conference type should have 17 fields, found {}",
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
            "field_topics",
            "field_logo",
            "field_venue_photos",
            "field_schedule_pdf",
            "field_speakers",
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

        // Text serializes as {"Text": {"max_length": N}}
        assert!(
            field_map["field_city"]["Text"]["max_length"].is_number(),
            "field_city should be Text with max_length"
        );

        // TextLong serializes as "TextLong"
        assert_eq!(field_map["field_description"], "TextLong");

        // RecordReference serializes as {"RecordReference": "target"}
        assert!(
            field_map["field_topics"]["RecordReference"].is_string(),
            "field_topics should be RecordReference"
        );

        // File serializes as "File"
        assert_eq!(field_map["field_logo"], "File");
    });
}

#[test]
fn test_part01_step02_required_fields() {
    run_test(async {
        let app = shared_app().await;

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
fn test_part01_step02_multivalue_fields() {
    run_test(async {
        let app = shared_app().await;

        let row: (Value,) =
            sqlx::query_as("SELECT settings FROM item_type WHERE type = 'conference'")
                .fetch_one(&app.db)
                .await
                .unwrap();

        let fields = row.0["fields"].as_array().unwrap();

        // Tutorial says topics, venue_photos, speakers have cardinality -1
        let multivalue: Vec<&str> = fields
            .iter()
            .filter(|f| f["cardinality"].as_i64() == Some(-1))
            .filter_map(|f| f["field_name"].as_str())
            .collect();

        for name in &["field_topics", "field_venue_photos", "field_speakers"] {
            assert!(
                multivalue.contains(name),
                "'{name}' should have cardinality -1 (multi-value); multi-value fields: {multivalue:?}"
            );
        }
    });
}

#[test]
fn test_part01_step02_title_label() {
    run_test(async {
        let app = shared_app().await;

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
// The tutorial claims:
// - 3 seeded conferences exist (RustConf, EuroRust, WasmCon Online)
// - Items viewable at /item/{uuid} (HTML) and /api/item/{uuid} (JSON)
// - Items have UUIDv7 IDs, Unix timestamps, and live stage_id
// =============================================================================

#[test]
fn test_part01_step03_seeded_conferences_exist() {
    run_test(async {
        let app = shared_app().await;

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT title FROM item WHERE type = 'conference' AND status = 1 ORDER BY title",
        )
        .fetch_all(&app.db)
        .await
        .unwrap();

        let titles: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();

        // Tutorial documents 3 seeded conferences
        assert!(
            titles.contains(&"RustConf 2026"),
            "RustConf 2026 must be seeded; found: {titles:?}"
        );
        assert!(
            titles.contains(&"EuroRust 2026"),
            "EuroRust 2026 must be seeded; found: {titles:?}"
        );
        assert!(
            titles.contains(&"WasmCon Online 2026"),
            "WasmCon Online 2026 must be seeded; found: {titles:?}"
        );
    });
}

#[test]
fn test_part01_step03_item_viewable_as_html() {
    run_test(async {
        let app = shared_app().await;

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

// =============================================================================
// Step 4: Build Your First Gather
// Validates: docs/tutorial/part-01-hello-trovato.md — Step 4
//
// The tutorial claims:
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

        let response = app
            .request(
                Request::get("/api/query/upcoming_conferences/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = response_json(response).await;

        // Should return at least the 3 seeded conferences
        let total = body["total"].as_u64().unwrap_or(0);
        assert!(
            total >= 3,
            "gather should return at least 3 seeded conferences, got {total}"
        );

        let items = body["items"].as_array().expect("items must be an array");
        assert!(!items.is_empty(), "gather items array should not be empty");
    });
}

#[test]
fn test_part01_step04_conferences_url_alias() {
    run_test(async {
        let app = shared_app().await;

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
        assert_eq!(source, "/gather/upcoming_conferences");
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
        assert!(
            body.contains("RustConf 2026") || body.contains("EuroRust 2026"),
            "gather page should contain seeded conference names"
        );
    });
}

#[test]
fn test_part01_step04_sorted_by_start_date() {
    run_test(async {
        let app = shared_app().await;

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
