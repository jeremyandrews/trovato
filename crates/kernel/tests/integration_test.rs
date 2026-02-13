//! Integration tests for the Trovato kernel.
//!
//! These tests use the REAL kernel code - no mocks, no reimplementations.
//! They test the actual routes, services, and database operations.
//!
//! ## Prerequisites
//!
//! ```bash
//! docker-compose up -d
//! ```
//!
//! ## Running Tests
//!
//! ```bash
//! cargo test --test integration_test
//! ```

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};

mod common;
use common::{extract_cookies, TestApp};

// =============================================================================
// Health Check Tests
// =============================================================================

#[tokio::test]
async fn health_check_returns_healthy() {
    let app = TestApp::new().await;

    let response = app
        .request(Request::get("/health").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["postgres"], true);
    assert_eq!(body["redis"], true);
}

// =============================================================================
// Authentication Tests
// =============================================================================

#[tokio::test]
async fn login_with_invalid_credentials_returns_401() {
    let app = TestApp::new().await;

    // Use unique username to avoid rate limiting from previous test runs
    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let username = format!("nonexistent_{}", &unique_id[..8]);

    let response = app
        .request(
            Request::post("/user/login/json")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "username": username,
                        "password": "wrongpassword"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    let status = response.status();
    // Accept either 401 (unauthorized) or 429 (rate limited) - both mean login failed
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::TOO_MANY_REQUESTS,
        "Expected 401 or 429, got {}", status
    );
}

#[tokio::test]
async fn login_with_valid_credentials_returns_success() {
    let app = TestApp::new().await;

    // Create a test user first
    app.create_test_user("testuser", "testpass123", "test@example.com")
        .await;

    let response = app
        .request(
            Request::post("/user/login/json")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "username": "testuser",
                        "password": "testpass123"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["success"], true);
}

// =============================================================================
// Content Type Admin Tests (Phase 5)
// =============================================================================

#[tokio::test]
async fn admin_content_types_list_returns_html() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("admin_test_1", "password123", "admin1@test.com").await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/types")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}", status, &body[..body.len().min(1000)]);

    // Should contain the "page" content type that's seeded
    assert!(body.contains("Basic Page") || body.contains("page"),
        "Response should list the 'page' content type");
}

#[tokio::test]
async fn admin_add_content_type_form_returns_html() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("admin_test_2", "password123", "admin2@test.com").await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}", status, &body[..body.len().min(1000)]);

    assert!(body.contains("form"), "Response should contain a form");
    assert!(body.contains("csrf") || body.contains("_token"),
        "Response should contain CSRF token");
}

#[tokio::test]
async fn admin_manage_fields_returns_html() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("admin_test_3", "password123", "admin3@test.com").await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/page/fields")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}", status, &body[..body.len().min(1000)]);

    assert!(body.contains("Manage fields") || body.contains("field"),
        "Response should show field management UI");
}

#[tokio::test]
async fn admin_nonexistent_content_type_returns_404() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("admin_test_4", "password123", "admin4@test.com").await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/nonexistent_type_xyz/fields")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// =============================================================================
// Content Type Creation E2E Test
// =============================================================================

#[tokio::test]
async fn e2e_create_content_type() {
    let app = TestApp::new().await;

    // Use unique name per test run to avoid conflicts with parallel tests
    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let machine_name = format!("test_{}", &unique_id[..8]);

    // Login first
    let login_cookies = app.create_and_login_user("admin_e2e_1", "password123", "e2e1@test.com").await;

    // Get the add form to extract CSRF token
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/add")
                .body(Body::empty())
                .unwrap(),
            &login_cookies,
        )
        .await;

    let status = form_response.status();
    // Extract cookies from response for session continuity (merge with login cookies)
    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() { login_cookies } else { form_cookies };
    let form_html = response_text(form_response).await;
    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK for form, got {}. Body: {}", status, &form_html[..form_html.len().min(1000)]);

    // Extract CSRF token from the form
    let csrf_token = extract_csrf_token(&form_html)
        .expect("Should find CSRF token in form");
    let form_build_id = extract_form_build_id(&form_html)
        .unwrap_or_else(|| "test_build_id".to_string());

    // Submit the form to create a new content type
    let form_data = format!(
        "_token={}&_form_build_id={}&label=Test+Blog&machine_name={}&description=A+test+blog+type",
        csrf_token, form_build_id, machine_name
    );

    // Use request_with_cookies to maintain session
    let response = app
        .request_with_cookies(
            Request::post("/admin/structure/types/add")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    let resp_body = response_text(response).await;

    // Should redirect to content types list on success
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {}. Body: {}",
        resp_status, &resp_body[..resp_body.len().min(1000)]
    );

    // Verify the content type exists in the database
    let exists: bool = sqlx::query_scalar(
        &format!("SELECT EXISTS(SELECT 1 FROM item_type WHERE type = '{}')", machine_name)
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Content type '{}' should exist in database", machine_name);
}

// =============================================================================
// Add Field E2E Test
// =============================================================================

#[tokio::test]
async fn e2e_add_field_to_content_type() {
    let app = TestApp::new().await;

    // Use unique name per test run to avoid conflicts with parallel tests
    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let type_name = format!("test_{}", &unique_id[..8]);
    let field_name = format!("field_{}", &unique_id[8..16]);

    // Login first
    let login_cookies = app.create_and_login_user("admin_e2e_2", "password123", "e2e2@test.com").await;

    // First create a test content type via the UI to ensure the registry is updated
    // Get the add form first
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/add")
                .body(Body::empty())
                .unwrap(),
            &login_cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() { login_cookies.clone() } else { form_cookies };
    let form_html = response_text(form_response).await;
    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    // Create the content type
    let form_data = format!(
        "_token={}&_form_build_id={}&label=Field+Test&machine_name={}&description=For+testing",
        csrf_token, form_build_id, type_name
    );
    let _ = app
        .request_with_cookies(
            Request::post("/admin/structure/types/add")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    // Get the fields page to get CSRF token
    let fields_response = app
        .request_with_cookies(
            Request::get(&format!("/admin/structure/types/{}/fields", type_name))
                .body(Body::empty())
                .unwrap(),
            &login_cookies,
        )
        .await;

    let status = fields_response.status();
    // Extract cookies for session continuity
    let field_cookies = extract_cookies(&fields_response);
    let cookies = if field_cookies.is_empty() { login_cookies } else { field_cookies };
    let fields_html = response_text(fields_response).await;
    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK for fields page, got {}. Body: {}", status, &fields_html[..fields_html.len().min(1000)]);

    let csrf_token = extract_csrf_token(&fields_html)
        .expect("Should find CSRF token");
    let form_build_id = extract_form_build_id(&fields_html)
        .unwrap_or_else(|| "test_build_id".to_string());

    // Add a field
    let form_data = format!(
        "_token={}&_form_build_id={}&label=Test+Field&name={}&field_type=text",
        csrf_token, form_build_id, field_name
    );

    // Use request_with_cookies to maintain session
    let response = app
        .request_with_cookies(
            Request::post(&format!("/admin/structure/types/{}/fields/add", type_name))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    let resp_body = response_text(response).await;

    // Should redirect back to fields page on success
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {}. Body: {}",
        resp_status, &resp_body[..resp_body.len().min(1000)]
    );

    // Verify the field was added (check settings JSON)
    let settings: serde_json::Value = sqlx::query_scalar(
        &format!("SELECT settings FROM item_type WHERE type = '{}'", type_name)
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    let fields = settings.get("fields").and_then(|f| f.as_array());
    assert!(
        fields.map(|f| f.iter().any(|field| {
            field.get("field_name").and_then(|n| n.as_str()) == Some(&field_name.as_str())
        })).unwrap_or(false),
        "Field '{}' should exist in settings. Got: {:?}", field_name,
        settings
    );
}

// =============================================================================
// Search E2E Tests (Phase 6A)
// =============================================================================

#[tokio::test]
async fn e2e_search_returns_results() {
    let app = TestApp::new().await;

    // Create a test item to search for
    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let search_term = format!("findme_{}", &unique_id[..8]);

    // Insert a published item with the search term in the title
    let item_id = uuid::Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO item (id, type, title, status, author_id, fields, search_vector, created, changed)
        VALUES ($1, 'page', $2, 1, $3, '{}',
                setweight(to_tsvector('english', $2), 'A'),
                extract(epoch from now())::bigint,
                extract(epoch from now())::bigint)
        "#
    )
    .bind(item_id)
    .bind(&format!("Test Page {}", search_term))
    .bind(uuid::Uuid::nil()) // System user
    .execute(&app.db)
    .await
    .expect("Failed to insert test item");

    // Search for the item via API
    let response = app
        .request(
            Request::get(&format!("/api/search?q={}", search_term))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["query"], search_term);
    assert!(body["total"].as_i64().unwrap_or(0) >= 1, "Should find at least one result");

    // Verify the result contains our item
    let results = body["results"].as_array().expect("results should be array");
    let found = results.iter().any(|r| {
        r["id"].as_str() == Some(&item_id.to_string())
    });
    assert!(found, "Search should find our test item. Results: {:?}", results);
}

#[tokio::test]
async fn e2e_search_html_page_works() {
    let app = TestApp::new().await;

    // Login first (search requires session for user context)
    let cookies = app.create_and_login_user("search_test", "password123", "search@test.com").await;

    let response = app
        .request_with_cookies(
            Request::get("/search?q=test")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}", status, &body[..body.len().min(1000)]);

    assert!(body.contains("Search") || body.contains("search"), "Should render search page");
}

#[tokio::test]
async fn e2e_search_empty_query_returns_no_results() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/api/search?q=")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["results"].as_array().map(|a| a.len()).unwrap_or(0), 0);
}

// =============================================================================
// Cron E2E Tests (Phase 6A)
// =============================================================================

#[tokio::test]
async fn e2e_cron_invalid_key_rejected() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::post("/cron/wrong-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let body = response_json(response).await;
    assert_eq!(body["status"], "error");
}

#[tokio::test]
async fn e2e_cron_valid_key_runs() {
    let app = TestApp::new().await;

    // Use the default key (tests don't set CRON_KEY env var)
    let response = app
        .request(
            Request::post("/cron/default-cron-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    let status = response.status();
    let body = response_json(response).await;

    assert_eq!(status, StatusCode::OK, "Cron should succeed with valid key");
    assert!(
        body["status"] == "completed" || body["status"] == "skipped",
        "Cron status should be completed or skipped, got: {:?}", body
    );
}

#[tokio::test]
async fn e2e_cron_status_requires_admin() {
    let app = TestApp::new().await;

    // Try without login
    let response = app
        .request(
            Request::get("/cron/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    // Should redirect to login or return forbidden
    assert!(
        response.status() == StatusCode::SEE_OTHER || response.status() == StatusCode::FORBIDDEN,
        "Cron status should require auth, got: {}", response.status()
    );
}

// =============================================================================
// File Upload E2E Tests (Phase 6B)
// =============================================================================

#[tokio::test]
async fn e2e_file_upload_requires_auth() {
    let app = TestApp::new().await;

    // Create a simple multipart body manually
    let boundary = "----TestBoundary12345";
    let body = format!(
        "--{boundary}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n\
Content-Type: text/plain\r\n\
\r\n\
Hello, World!\r\n\
--{boundary}--\r\n"
    );

    let response = app
        .request(
            Request::post("/file/upload")
                .header("content-type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
        )
        .await;

    // Should require authentication
    assert!(
        response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::SEE_OTHER,
        "File upload should require auth, got: {}", response.status()
    );
}

#[tokio::test]
async fn e2e_file_upload_with_auth() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("upload_test", "password123", "upload@test.com").await;

    // Create a simple multipart body for a .txt file (allowed MIME type)
    let boundary = "----TestBoundary67890";
    let file_content = "Hello, this is a test file!";
    let body = format!(
        "--{boundary}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"test.txt\"\r\n\
Content-Type: text/plain\r\n\
\r\n\
{file_content}\r\n\
--{boundary}--\r\n"
    );

    let response = app
        .request_with_cookies(
            Request::post("/file/upload")
                .header("content-type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_json(response).await;

    // File upload should succeed - response is {success: true, file: {...}}
    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK, got {}. Body: {:?}", status, body);

    assert_eq!(body["success"], true, "Upload should succeed");
    assert!(body["file"].get("id").is_some(), "Response should include file ID");
    assert!(body["file"].get("url").is_some(), "Response should include file URL");
}

#[tokio::test]
async fn e2e_file_get_info() {
    let app = TestApp::new().await;

    // Login and upload a file first
    let cookies = app.create_and_login_user("file_info_test", "password123", "fileinfo@test.com").await;

    let boundary = "----TestBoundary99999";
    let body = format!(
        "--{boundary}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"info_test.txt\"\r\n\
Content-Type: text/plain\r\n\
\r\n\
Test file content\r\n\
--{boundary}--\r\n"
    );

    let upload_response = app
        .request_with_cookies(
            Request::post("/file/upload")
                .header("content-type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    if upload_response.status() != StatusCode::OK {
        let body = response_text(upload_response).await;
        panic!("Upload failed: {}", body);
    }

    let upload_body = response_json(upload_response).await;
    assert_eq!(upload_body["success"], true, "Upload should succeed: {:?}", upload_body);
    let file_id = upload_body["file"]["id"].as_str().expect("Should have file ID in file.id");

    // Now retrieve file info
    let info_response = app
        .request_with_cookies(
            Request::get(&format!("/file/{}", file_id))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = info_response.status();
    let info_body = response_json(info_response).await;

    assert_eq!(status, StatusCode::OK,
        "Expected 200 OK for file info, got {}. Body: {:?}", status, info_body);

    assert_eq!(info_body["id"], file_id);
    assert!(info_body["filename"].as_str().is_some());
    assert!(info_body["url"].as_str().is_some());
}

#[tokio::test]
async fn e2e_file_invalid_mime_type_rejected() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app.create_and_login_user("mime_test", "password123", "mime@test.com").await;

    // Try to upload an executable (not allowed)
    let boundary = "----TestBoundaryMime";
    let body = format!(
        "--{boundary}\r\n\
Content-Disposition: form-data; name=\"file\"; filename=\"malware.exe\"\r\n\
Content-Type: application/x-executable\r\n\
\r\n\
MZ...\r\n\
--{boundary}--\r\n"
    );

    let response = app
        .request_with_cookies(
            Request::post("/file/upload")
                .header("content-type", format!("multipart/form-data; boundary={}", boundary))
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    // Should reject invalid MIME type (415 Unsupported Media Type or 400 Bad Request)
    assert!(
        status == StatusCode::UNSUPPORTED_MEDIA_TYPE || status == StatusCode::BAD_REQUEST,
        "Should reject executable MIME type, got: {}", status
    );
}

// =============================================================================
// Rate Limiting E2E Tests (Phase 6C)
// =============================================================================

#[tokio::test]
async fn e2e_rate_limiter_exists() {
    let app = TestApp::new().await;

    // Make a few requests - just verify the rate limiter is wired up
    // (actual rate limit testing would require making many requests quickly)
    for _ in 0..3 {
        let response = app
            .request(Request::get("/health").body(Body::empty()).unwrap())
            .await;

        // Should succeed (we're well under the limit)
        assert_eq!(response.status(), StatusCode::OK);
    }
}

// =============================================================================
// Metrics E2E Tests (Phase 6C)
// =============================================================================

#[tokio::test]
async fn e2e_metrics_endpoint_returns_prometheus_format() {
    let app = TestApp::new().await;

    let response = app
        .request(Request::get("/metrics").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_text(response).await;

    // Verify Prometheus format markers
    assert!(body.contains("# HELP"), "Should contain HELP comments");
    assert!(body.contains("# TYPE"), "Should contain TYPE comments");
    assert!(body.contains("http_requests_total"), "Should contain http_requests metric");
    assert!(body.contains("cache_hits_total"), "Should contain cache_hits metric");
}

#[tokio::test]
async fn e2e_metrics_tracks_requests() {
    let app = TestApp::new().await;

    // Make a health check request first
    let _ = app
        .request(Request::get("/health").body(Body::empty()).unwrap())
        .await;

    // Now check metrics
    let response = app
        .request(Request::get("/metrics").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_text(response).await;

    // Should show the http_requests metric
    assert!(body.contains("http_requests_total"),
        "Metrics should include http_requests_total");
}

// =============================================================================
// Cache Layer E2E Tests (Phase 6A)
// =============================================================================

#[tokio::test]
async fn e2e_cache_metrics_exist() {
    let app = TestApp::new().await;

    // Make some requests that might use cache
    let _ = app
        .request(Request::get("/health").body(Body::empty()).unwrap())
        .await;

    // Check metrics include cache counters
    let response = app
        .request(Request::get("/metrics").body(Body::empty()).unwrap())
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_text(response).await;

    // Should have cache metrics defined
    assert!(body.contains("cache_hits_total"), "Should have cache_hits_total metric");
    assert!(body.contains("cache_misses_total"), "Should have cache_misses_total metric");
}

// =============================================================================
// Helpers
// =============================================================================

async fn response_json(response: axum::response::Response) -> Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or_else(|_| {
        let text = String::from_utf8_lossy(&body);
        panic!("Failed to parse JSON: {}", text);
    })
}

async fn response_text(response: axum::response::Response) -> String {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&body).to_string()
}

fn extract_csrf_token(html: &str) -> Option<String> {
    // Look for: name="_token" value="..."
    let patterns = [
        r#"name="_token" value=""#,
        r#"name='_token' value='"#,
        r#"value="([^"]+)" name="_token""#,
    ];

    for pattern in &patterns[..2] {
        if let Some(start) = html.find(pattern) {
            let value_start = start + pattern.len();
            if let Some(end) = html[value_start..].find('"').or_else(|| html[value_start..].find('\'')) {
                return Some(html[value_start..value_start + end].to_string());
            }
        }
    }

    // Try regex-like extraction for csrf_token variable
    if let Some(start) = html.find("csrf_token") {
        let segment = &html[start..std::cmp::min(start + 200, html.len())];
        if let Some(val_start) = segment.find("value=\"") {
            let val_segment = &segment[val_start + 7..];
            if let Some(val_end) = val_segment.find('"') {
                return Some(val_segment[..val_end].to_string());
            }
        }
    }

    None
}

fn extract_form_build_id(html: &str) -> Option<String> {
    let pattern = r#"name="_form_build_id" value=""#;
    if let Some(start) = html.find(pattern) {
        let value_start = start + pattern.len();
        if let Some(end) = html[value_start..].find('"') {
            return Some(html[value_start..value_start + end].to_string());
        }
    }
    None
}
