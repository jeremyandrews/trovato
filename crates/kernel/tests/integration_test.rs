#![allow(clippy::unwrap_used, clippy::expect_used)]
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
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{Value, json};

mod common;
use common::{TestApp, extract_cookies};

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
    let username = format!("nonexistent_{}", &unique_id[..16]);

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
        "Expected 401 or 429, got {status}"
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
    let cookies = app
        .create_and_login_admin("admin_test_1", "password123", "admin1@test.com")
        .await;

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

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    // Should contain the "page" content type that's seeded
    assert!(
        body.contains("Basic Page") || body.contains("page"),
        "Response should list the 'page' content type"
    );
}

#[tokio::test]
async fn admin_add_content_type_form_returns_html() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("admin_test_2", "password123", "admin2@test.com")
        .await;

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

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(body.contains("form"), "Response should contain a form");
    assert!(
        body.contains("csrf") || body.contains("_token"),
        "Response should contain CSRF token"
    );
}

#[tokio::test]
async fn admin_manage_fields_returns_html() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("admin_test_3", "password123", "admin3@test.com")
        .await;

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

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Manage fields") || body.contains("field"),
        "Response should show field management UI"
    );
}

#[tokio::test]
async fn admin_nonexistent_content_type_returns_404() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("admin_test_4", "password123", "admin4@test.com")
        .await;

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
    let machine_name = format!("test_{}", &unique_id[..16]);

    // Login first
    let login_cookies = app
        .create_and_login_admin("admin_e2e_1", "password123", "e2e1@test.com")
        .await;

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
    let cookies = if form_cookies.is_empty() {
        login_cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for form, got {}. Body: {}",
        status,
        &form_html[..form_html.len().min(1000)]
    );

    // Extract CSRF token from the form
    let csrf_token = extract_csrf_token(&form_html).expect("Should find CSRF token in form");
    let form_build_id =
        extract_form_build_id(&form_html).unwrap_or_else(|| "test_build_id".to_string());

    // Submit the form to create a new content type
    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&label=Test+Blog&machine_name={machine_name}&description=A+test+blog+type"
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
        resp_status,
        &resp_body[..resp_body.len().min(1000)]
    );

    // Verify the content type exists in the database
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM item_type WHERE type = '{machine_name}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(
        exists,
        "Content type '{machine_name}' should exist in database"
    );
}

// =============================================================================
// Add Field E2E Test
// =============================================================================

#[tokio::test]
async fn e2e_add_field_to_content_type() {
    let app = TestApp::new().await;

    // Use unique name per test run to avoid conflicts with parallel tests
    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let type_name = format!("test_{}", &unique_id[..16]);
    let field_name = format!("field_{}", &unique_id[8..16]);

    // Login first
    let login_cookies = app
        .create_and_login_admin("admin_e2e_2", "password123", "e2e2@test.com")
        .await;

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
    let cookies = if form_cookies.is_empty() {
        login_cookies.clone()
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;
    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    // Create the content type
    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&label=Field+Test&machine_name={type_name}&description=For+testing"
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
            Request::get(format!("/admin/structure/types/{type_name}/fields"))
                .body(Body::empty())
                .unwrap(),
            &login_cookies,
        )
        .await;

    let status = fields_response.status();
    // Extract cookies for session continuity
    let field_cookies = extract_cookies(&fields_response);
    let cookies = if field_cookies.is_empty() {
        login_cookies
    } else {
        field_cookies
    };
    let fields_html = response_text(fields_response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for fields page, got {}. Body: {}",
        status,
        &fields_html[..fields_html.len().min(1000)]
    );

    let csrf_token = extract_csrf_token(&fields_html).expect("Should find CSRF token");
    let form_build_id =
        extract_form_build_id(&fields_html).unwrap_or_else(|| "test_build_id".to_string());

    // Add a field
    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&label=Test+Field&name={field_name}&field_type=text"
    );

    // Use request_with_cookies to maintain session
    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/types/{type_name}/fields/add"))
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
        resp_status,
        &resp_body[..resp_body.len().min(1000)]
    );

    // Verify the field was added (check settings JSON)
    let settings: serde_json::Value = sqlx::query_scalar(&format!(
        "SELECT settings FROM item_type WHERE type = '{type_name}'"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    let fields = settings.get("fields").and_then(|f| f.as_array());
    assert!(
        fields
            .map(|f| f.iter().any(|field| {
                field.get("field_name").and_then(|n| n.as_str()) == Some(field_name.as_str())
            }))
            .unwrap_or(false),
        "Field '{field_name}' should exist in settings. Got: {settings:?}"
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
    let search_term = format!("findme_{}", &unique_id[..16]);

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
    .bind(format!("Test Page {search_term}"))
    .bind(uuid::Uuid::nil()) // System user
    .execute(&app.db)
    .await
    .expect("Failed to insert test item");

    // Search for the item via API
    let response = app
        .request(
            Request::get(format!("/api/search?q={search_term}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["query"], search_term);
    assert!(
        body["total"].as_i64().unwrap_or(0) >= 1,
        "Should find at least one result"
    );

    // Verify the result contains our item
    let results = body["results"].as_array().expect("results should be array");
    let found = results
        .iter()
        .any(|r| r["id"].as_str() == Some(&item_id.to_string()));
    assert!(
        found,
        "Search should find our test item. Results: {results:?}"
    );
}

#[tokio::test]
async fn e2e_search_html_page_works() {
    let app = TestApp::new().await;

    // Login first (search requires session for user context)
    let cookies = app
        .create_and_login_admin("search_test", "password123", "search@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/search?q=test").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Search") || body.contains("search"),
        "Should render search page"
    );
}

#[tokio::test]
async fn e2e_search_empty_query_returns_no_results() {
    let app = TestApp::new().await;

    let response = app
        .request(Request::get("/api/search?q=").body(Body::empty()).unwrap())
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
        "Cron status should be completed or skipped, got: {body:?}"
    );
}

#[tokio::test]
async fn e2e_cron_status_requires_admin() {
    let app = TestApp::new().await;

    // Try without login
    let response = app
        .request(Request::get("/cron/status").body(Body::empty()).unwrap())
        .await;

    // Should redirect to login or return forbidden
    assert!(
        response.status() == StatusCode::SEE_OTHER || response.status() == StatusCode::FORBIDDEN,
        "Cron status should require auth, got: {}",
        response.status()
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
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await;

    // Should require authentication
    assert!(
        response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::SEE_OTHER,
        "File upload should require auth, got: {}",
        response.status()
    );
}

#[tokio::test]
async fn e2e_file_upload_with_auth() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("upload_test", "password123", "upload@test.com")
        .await;

    // Fetch CSRF token
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin").await;

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
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_json(response).await;

    // File upload should succeed - response is {success: true, file: {...}}
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {status}. Body: {body:?}"
    );

    assert_eq!(body["success"], true, "Upload should succeed");
    assert!(
        body["file"].get("id").is_some(),
        "Response should include file ID"
    );
    assert!(
        body["file"].get("url").is_some(),
        "Response should include file URL"
    );
}

#[tokio::test]
async fn e2e_file_get_info() {
    let app = TestApp::new().await;

    // Login and upload a file first
    let cookies = app
        .create_and_login_admin("file_info_test", "password123", "fileinfo@test.com")
        .await;

    // Fetch CSRF token
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin").await;

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
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    if upload_response.status() != StatusCode::OK {
        let body = response_text(upload_response).await;
        panic!("Upload failed: {body}");
    }

    let upload_body = response_json(upload_response).await;
    assert_eq!(
        upload_body["success"], true,
        "Upload should succeed: {upload_body:?}"
    );
    let file_id = upload_body["file"]["id"]
        .as_str()
        .expect("Should have file ID in file.id");

    // Now retrieve file info
    let info_response = app
        .request_with_cookies(
            Request::get(format!("/file/{file_id}"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = info_response.status();
    let info_body = response_json(info_response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for file info, got {status}. Body: {info_body:?}"
    );

    assert_eq!(info_body["id"], file_id);
    assert!(info_body["filename"].as_str().is_some());
    assert!(info_body["url"].as_str().is_some());
}

#[tokio::test]
async fn e2e_file_invalid_mime_type_rejected() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("mime_test", "password123", "mime@test.com")
        .await;

    // Fetch CSRF token
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin").await;

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
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    // Should reject invalid MIME type (415 Unsupported Media Type or 400 Bad Request)
    assert!(
        status == StatusCode::UNSUPPORTED_MEDIA_TYPE || status == StatusCode::BAD_REQUEST,
        "Should reject executable MIME type, got: {status}"
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
    assert!(
        body.contains("http_requests_total"),
        "Should contain http_requests metric"
    );
    assert!(
        body.contains("cache_hits_total"),
        "Should contain cache_hits metric"
    );
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
    assert!(
        body.contains("http_requests_total"),
        "Metrics should include http_requests_total"
    );
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
    assert!(
        body.contains("cache_hits_total"),
        "Should have cache_hits_total metric"
    );
    assert!(
        body.contains("cache_misses_total"),
        "Should have cache_misses_total metric"
    );
}

// =============================================================================
// User Management Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_users() {
    let app = TestApp::new().await;

    // Login first
    let cookies = app
        .create_and_login_admin("admin_users_1", "password123", "users1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/people").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Users") || body.contains("users"),
        "Response should show users list"
    );
    assert!(
        body.contains("admin_users_1"),
        "Response should list the logged in user"
    );
}

#[tokio::test]
async fn e2e_admin_add_user_form() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_users_2", "password123", "users2@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/people/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(body.contains("form"), "Response should contain a form");
    assert!(
        body.contains("name") && body.contains("mail") && body.contains("password"),
        "Response should contain user fields"
    );
}

#[tokio::test]
async fn e2e_admin_create_user() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let new_username = format!("newuser_{}", &unique_id[..16]);
    let new_email = format!("{new_username}@test.com");

    let cookies = app
        .create_and_login_admin("admin_users_3", "password123", "users3@test.com")
        .await;

    // Get the form to extract CSRF token
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/people/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    // Submit the form
    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&name={new_username}&mail={new_email}&password=testpass123&status=1"
    );

    let response = app
        .request_with_cookies(
            Request::post("/admin/people/add")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();

    // Should redirect on success
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify user was created
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM users WHERE name = '{new_username}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "User '{new_username}' should exist in database");
}

#[tokio::test]
async fn e2e_admin_edit_user() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let username = format!("edituser_{}", &unique_id[..16]);
    let email = format!("{username}@test.com");

    // Create user to edit
    app.create_test_user(&username, "testpass123", &email).await;

    // Get the user ID
    let user_id: uuid::Uuid =
        sqlx::query_scalar(&format!("SELECT id FROM users WHERE name = '{username}'"))
            .fetch_one(&app.db)
            .await
            .unwrap();

    let cookies = app
        .create_and_login_admin("admin_users_4", "password123", "users4@test.com")
        .await;

    // Get the edit form
    let form_response = app
        .request_with_cookies(
            Request::get(format!("/admin/people/{user_id}/edit"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = form_response.status();
    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    assert_eq!(status, StatusCode::OK, "Edit form should return 200");

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    // Update the user
    let new_email = format!("updated_{}@test.com", &unique_id[..16]);
    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&name={username}&mail={new_email}&status=1"
    );

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/{user_id}/edit"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify email was updated
    let updated_email: String =
        sqlx::query_scalar(&format!("SELECT mail FROM users WHERE id = '{user_id}'"))
            .fetch_one(&app.db)
            .await
            .unwrap();

    assert_eq!(updated_email, new_email, "Email should be updated");
}

#[tokio::test]
async fn e2e_admin_delete_user() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let username = format!("deluser_{}", &unique_id[..16]);

    // Create user to delete
    app.create_test_user(&username, "testpass123", &format!("{username}@test.com"))
        .await;

    let user_id: uuid::Uuid =
        sqlx::query_scalar(&format!("SELECT id FROM users WHERE name = '{username}'"))
            .fetch_one(&app.db)
            .await
            .unwrap();

    let cookies = app
        .create_and_login_admin("admin_users_5", "password123", "users5@test.com")
        .await;

    // Fetch CSRF token from user list page
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/{user_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify user was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = '{user_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "User should be deleted");
}

#[tokio::test]
async fn e2e_admin_cannot_delete_self() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let username = format!("selfuser_{}", &unique_id[..16]);

    // Must be admin to reach delete handler (require_admin check)
    app.create_test_admin(&username, "testpass123", &format!("{username}@test.com"))
        .await;

    let user_id: uuid::Uuid =
        sqlx::query_scalar(&format!("SELECT id FROM users WHERE name = '{username}'"))
            .fetch_one(&app.db)
            .await
            .unwrap();

    // Login as the same admin user we're trying to delete
    let cookies = app.login(&username, "testpass123").await;

    // Fetch CSRF token
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/{user_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    // Should fail - can't delete yourself
    assert_eq!(
        resp_status,
        StatusCode::BAD_REQUEST,
        "Should not be able to delete yourself"
    );
}

// =============================================================================
// Role Management Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_roles() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_roles_1", "password123", "roles1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/people/roles")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Roles") || body.contains("roles"),
        "Response should show roles list"
    );
}

#[tokio::test]
async fn e2e_admin_create_role() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let role_name = format!("TestRole_{}", &unique_id[..16]);

    let cookies = app
        .create_and_login_admin("admin_roles_2", "password123", "roles2@test.com")
        .await;

    // Get form
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/people/roles/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!("_token={csrf_token}&_form_build_id={form_build_id}&name={role_name}");

    let response = app
        .request_with_cookies(
            Request::post("/admin/people/roles/add")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify role was created
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM roles WHERE name = '{role_name}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Role '{role_name}' should exist");
}

#[tokio::test]
async fn e2e_admin_delete_role() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let role_name = format!("DelRole_{}", &unique_id[..16]);

    // Create role to delete
    let role_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO roles (id, name) VALUES ($1, $2)")
        .bind(role_id)
        .bind(&role_name)
        .execute(&app.db)
        .await
        .expect("Failed to create test role");

    let cookies = app
        .create_and_login_admin("admin_roles_3", "password123", "roles3@test.com")
        .await;

    // Fetch CSRF token from roles list page
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people/roles").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/roles/{role_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify role was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM roles WHERE id = '{role_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "Role should be deleted");
}

#[tokio::test]
async fn e2e_admin_cannot_delete_builtin_roles() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_roles_4", "password123", "roles4@test.com")
        .await;

    // Fetch CSRF token from the add-role page (the list page only has tokens
    // on non-built-in role delete buttons, which may not exist in a clean DB).
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people/roles/add").await;

    // Try to delete anonymous role (UUID 1)
    let anonymous_role_id = uuid::Uuid::from_u128(1);

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/people/roles/{anonymous_role_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert_eq!(
        resp_status,
        StatusCode::BAD_REQUEST,
        "Should not be able to delete built-in role"
    );
}

#[tokio::test]
async fn e2e_admin_permissions_matrix() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_roles_5", "password123", "roles5@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/people/permissions")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Permissions") || body.contains("permissions"),
        "Response should show permissions matrix"
    );
    assert!(
        body.contains("administer site") || body.contains("access content"),
        "Response should list available permissions"
    );
}

// =============================================================================
// Content Management Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_content() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_content_1", "password123", "content1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/content").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Content") || body.contains("content"),
        "Response should show content list"
    );
}

#[tokio::test]
async fn e2e_admin_select_content_type() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_content_2", "password123", "content2@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/content/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("page") || body.contains("Page"),
        "Response should show available content types"
    );
}

#[tokio::test]
async fn e2e_admin_create_content() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let title = format!("Test Content {}", &unique_id[..16]);

    let cookies = app
        .create_and_login_admin("admin_content_3", "password123", "content3@test.com")
        .await;

    // Get form for page content type
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/content/add/page")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_status = form_response.status();
    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    assert_eq!(
        form_status,
        StatusCode::OK,
        "Expected 200 OK for content form, got {}. Body: {}",
        form_status,
        &form_html[..form_html.len().min(2000)]
    );

    let csrf_token = extract_csrf_token(&form_html).unwrap_or_else(|| {
        panic!(
            "CSRF token not found. HTML: {}",
            &form_html[..form_html.len().min(2000)]
        )
    });
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!(
        "_token={}&_form_build_id={}&title={}&status=1",
        csrf_token,
        form_build_id,
        urlencoding::encode(&title)
    );

    let response = app
        .request_with_cookies(
            Request::post("/admin/content/add/page")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify content was created
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM item WHERE title = '{title}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Content '{title}' should exist");
}

#[tokio::test]
async fn e2e_admin_edit_content() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let title = format!("Edit Content {}", &unique_id[..16]);
    let new_title = format!("Updated Content {}", &unique_id[..16]);

    // Create content to edit
    let item_id = uuid::Uuid::now_v7();
    let author_id = uuid::Uuid::nil();
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, fields, created, changed) VALUES ($1, 'page', $2, $3, 1, '{}', $4, $5)"
    )
    .bind(item_id)
    .bind(&title)
    .bind(author_id)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create test content");

    let cookies = app
        .create_and_login_admin("admin_content_4", "password123", "content4@test.com")
        .await;

    // Get edit form
    let form_response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/{item_id}/edit"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!(
        "_token={}&_form_build_id={}&title={}&status=1",
        csrf_token,
        form_build_id,
        urlencoding::encode(&new_title)
    );

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/{item_id}/edit"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify title was updated
    let updated_title: String =
        sqlx::query_scalar(&format!("SELECT title FROM item WHERE id = '{item_id}'"))
            .fetch_one(&app.db)
            .await
            .unwrap();

    assert_eq!(updated_title, new_title, "Title should be updated");
}

#[tokio::test]
async fn e2e_admin_delete_content() {
    let app = TestApp::new().await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let title = format!("Delete Content {}", &unique_id[..16]);

    // Create content to delete
    let item_id = uuid::Uuid::now_v7();
    let author_id = uuid::Uuid::nil();
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, author_id, status, fields, created, changed) VALUES ($1, 'page', $2, $3, 1, '{}', $4, $5)"
    )
    .bind(item_id)
    .bind(&title)
    .bind(author_id)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create test content");

    let cookies = app
        .create_and_login_admin("admin_content_5", "password123", "content5@test.com")
        .await;

    // Fetch CSRF token from content list page
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/content").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/{item_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify content was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM item WHERE id = '{item_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "Content should be deleted");
}

#[tokio::test]
async fn e2e_admin_content_filter_by_type() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_content_6", "password123", "content6@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/content?type=page")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );
}

// =============================================================================
// Category Management Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_categories() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let cookies = app
        .create_and_login_admin("admin_cat_1", "password123", "cat1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/categories")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Categories") || body.contains("categories"),
        "Response should show categories list"
    );
}

#[tokio::test]
async fn e2e_admin_create_category() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let cat_id = format!("cat_{}", &unique_id[..16]);
    let cat_label = format!("Test Category {}", &unique_id[..16]);

    let cookies = app
        .create_and_login_admin("admin_cat_2", "password123", "cat2@test.com")
        .await;

    // Get form
    let form_response = app
        .request_with_cookies(
            Request::get("/admin/structure/categories/add")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!(
        "_token={}&_form_build_id={}&id={}&label={}&hierarchy=0",
        csrf_token,
        form_build_id,
        cat_id,
        urlencoding::encode(&cat_label)
    );

    let response = app
        .request_with_cookies(
            Request::post("/admin/structure/categories/add")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify category was created
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM category WHERE id = '{cat_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Category '{cat_id}' should exist");
}

#[tokio::test]
async fn e2e_admin_delete_category() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let cat_id = format!("delcat_{}", &unique_id[..16]);

    // Create category to delete
    sqlx::query(
        "INSERT INTO category (id, label, hierarchy, weight) VALUES ($1, 'Delete Me', 0, 0)",
    )
    .bind(&cat_id)
    .execute(&app.db)
    .await
    .expect("Failed to create test category");

    let cookies = app
        .create_and_login_admin("admin_cat_3", "password123", "cat3@test.com")
        .await;

    // Fetch CSRF token from categories list page
    let (cookies, csrf_token) =
        fetch_csrf_token(&app, &cookies, "/admin/structure/categories").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/categories/{cat_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify category was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM category WHERE id = '{cat_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "Category should be deleted");
}

#[tokio::test]
async fn e2e_admin_list_tags() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let cat_id = format!("tagcat_{}", &unique_id[..16]);

    // Create category for tags
    sqlx::query(
        "INSERT INTO category (id, label, hierarchy, weight) VALUES ($1, 'Tag Category', 0, 0)",
    )
    .bind(&cat_id)
    .execute(&app.db)
    .await
    .expect("Failed to create test category");

    let cookies = app
        .create_and_login_admin("admin_cat_4", "password123", "cat4@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/structure/categories/{cat_id}/tags"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Tags") || body.contains("tags") || body.contains("Tag Category"),
        "Response should show tags list"
    );
}

#[tokio::test]
async fn e2e_admin_create_tag() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let cat_id = format!("newtagcat_{}", &unique_id[..16]);
    let tag_label = format!("Test Tag {}", &unique_id[..16]);

    // Create category for tag
    sqlx::query(
        "INSERT INTO category (id, label, hierarchy, weight) VALUES ($1, 'New Tag Category', 0, 0)",
    )
    .bind(&cat_id)
    .execute(&app.db)
    .await
    .expect("Failed to create test category");

    let cookies = app
        .create_and_login_admin("admin_cat_5", "password123", "cat5@test.com")
        .await;

    // Get form
    let form_response = app
        .request_with_cookies(
            Request::get(format!("/admin/structure/categories/{cat_id}/tags/add"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    let csrf_token = extract_csrf_token(&form_html).expect("CSRF token");
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!(
        "_token={}&_form_build_id={}&label={}&weight=0",
        csrf_token,
        form_build_id,
        urlencoding::encode(&tag_label)
    );

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/categories/{cat_id}/tags/add"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify tag was created
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM category_tag WHERE label = '{tag_label}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Tag '{tag_label}' should exist");
}

#[tokio::test]
async fn e2e_admin_delete_tag() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("categories").await;

    let unique_id = uuid::Uuid::now_v7().simple().to_string();
    let cat_id = format!("deltagcat_{}", &unique_id[..16]);

    // Create category
    sqlx::query(
        "INSERT INTO category (id, label, hierarchy, weight) VALUES ($1, 'Del Tag Category', 0, 0)",
    )
    .bind(&cat_id)
    .execute(&app.db)
    .await
    .expect("Failed to create test category");

    // Create tag to delete
    let tag_id = uuid::Uuid::now_v7();
    let now = Utc::now().timestamp();
    sqlx::query("INSERT INTO category_tag (id, category_id, label, weight, created, changed) VALUES ($1, $2, 'Delete Me', 0, $3, $4)")
        .bind(tag_id)
        .bind(&cat_id)
        .bind(now)
        .bind(now)
        .execute(&app.db)
        .await
        .expect("Failed to create test tag");

    // Also create hierarchy entry
    sqlx::query("INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL)")
        .bind(tag_id)
        .execute(&app.db)
        .await
        .expect("Failed to create tag hierarchy");

    let cookies = app
        .create_and_login_admin("admin_cat_6", "password123", "cat6@test.com")
        .await;

    // Fetch CSRF token (use /admin/people which always has CSRF tokens)
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/tags/{tag_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify tag was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM category_tag WHERE id = '{tag_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "Tag should be deleted");
}

// =============================================================================
// File Management Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_files() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_files_1", "password123", "files1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/content/files")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Files") || body.contains("files"),
        "Response should show files list"
    );
}

#[tokio::test]
async fn e2e_admin_file_details() {
    let app = TestApp::new().await;

    // Create a test file record with unique URI
    let file_id = uuid::Uuid::now_v7();
    let unique_id = file_id.simple().to_string();
    let owner_id = uuid::Uuid::nil();
    let now = Utc::now().timestamp();
    let filename = format!("test_{}.txt", &unique_id[..16]);
    let uri = format!("local://{filename}");
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) VALUES ($1, $2, $3, $4, 'text/plain', 100, 0, $5, $6)"
    )
    .bind(file_id)
    .bind(owner_id)
    .bind(&filename)
    .bind(&uri)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create test file");

    let cookies = app
        .create_and_login_admin("admin_files_2", "password123", "files2@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/files/{file_id}"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains(&filename),
        "Response should show file details for {filename}"
    );
}

#[tokio::test]
async fn e2e_admin_delete_file() {
    let app = TestApp::new().await;

    // Create a test file record with unique URI
    let file_id = uuid::Uuid::now_v7();
    let owner_id = uuid::Uuid::nil();
    let now = Utc::now().timestamp();
    let unique_uri = format!("local://delete_me_{}.txt", file_id.simple());
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) VALUES ($1, $2, 'delete_me.txt', $5, 'text/plain', 100, 0, $3, $4)"
    )
    .bind(file_id)
    .bind(owner_id)
    .bind(now)
    .bind(now)
    .bind(&unique_uri)
    .execute(&app.db)
    .await
    .expect("Failed to create test file");

    let cookies = app
        .create_and_login_admin("admin_files_3", "password123", "files3@test.com")
        .await;

    // Fetch CSRF token from files list page
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/content/files").await;

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/files/{file_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify file was deleted
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM file_managed WHERE id = '{file_id}')"
    ))
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "File should be deleted");
}

#[tokio::test]
async fn e2e_admin_files_filter_by_status() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_files_4", "password123", "files4@test.com")
        .await;

    // Filter for temporary files
    let response = app
        .request_with_cookies(
            Request::get("/admin/content/files?status=0")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for filtered files, got {status}"
    );

    // Filter for permanent files
    let response = app
        .request_with_cookies(
            Request::get("/admin/content/files?status=1")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for filtered files, got {status}"
    );
}

// =============================================================================
// Search Configuration Tests (Admin UI)
// =============================================================================

#[tokio::test]
async fn e2e_admin_search_config_page() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_search_1", "password123", "search1@test.com")
        .await;

    // Access search config for the 'page' content type
    let response = app
        .request_with_cookies(
            Request::get("/admin/structure/types/page/search")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let status = response.status();
    let body = response_text(response).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK, got {}. Body: {}",
        status,
        &body[..body.len().min(1000)]
    );

    assert!(
        body.contains("Search configuration"),
        "Response should show search configuration page"
    );
    assert!(
        body.contains("Title"),
        "Response should show title field (always indexed)"
    );
}

#[tokio::test]
async fn e2e_admin_add_search_config() {
    let app = TestApp::new().await;

    // Use the existing 'page' content type
    let type_name = "page";
    let field_name = "search_test_field";

    let cookies = app
        .create_and_login_admin("admin_search_2", "password123", "search2@test.com")
        .await;

    // STEP 1: First add a field to the content type via the admin UI
    // This updates both the database AND the in-memory cache
    let fields_response = app
        .request_with_cookies(
            Request::get(format!("/admin/structure/types/{type_name}/fields"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let fields_cookies = extract_cookies(&fields_response);
    let cookies = if fields_cookies.is_empty() {
        cookies
    } else {
        fields_cookies
    };
    let fields_html = response_text(fields_response).await;

    let csrf_token = extract_csrf_token(&fields_html).expect("CSRF token for field form");
    let form_build_id = extract_form_build_id(&fields_html).unwrap_or_default();

    // Add a field
    let add_field_data = format!(
        "_token={}&_form_build_id={}&label={}&name={}&field_type=text_long",
        csrf_token, form_build_id, "Search Test Field", field_name
    );

    let add_field_response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/types/{type_name}/fields/add"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(add_field_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let add_cookies = extract_cookies(&add_field_response);
    let cookies = if add_cookies.is_empty() {
        cookies
    } else {
        add_cookies
    };

    // STEP 2: Now get the search config form (field should be available)
    let form_response = app
        .request_with_cookies(
            Request::get(format!("/admin/structure/types/{type_name}/search"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    let form_status = form_response.status();
    let form_cookies = extract_cookies(&form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let form_html = response_text(form_response).await;

    // Check status first
    assert_eq!(
        form_status,
        StatusCode::OK,
        "Expected 200 OK for search config form, got {}. Body: {}",
        form_status,
        &form_html[..form_html.len().min(3000)]
    );

    // The form should now show our field
    assert!(
        form_html.contains(field_name),
        "Search config page should show the new field"
    );

    let csrf_token = extract_csrf_token(&form_html).unwrap_or_else(|| {
        panic!(
            "CSRF token not found. HTML: {}",
            &form_html[..form_html.len().min(2000)]
        )
    });
    let form_build_id = extract_form_build_id(&form_html).unwrap_or_default();

    let form_data = format!(
        "_token={csrf_token}&_form_build_id={form_build_id}&field_name={field_name}&weight=B"
    );

    let response = app
        .request_with_cookies(
            Request::post(format!("/admin/structure/types/{type_name}/search/add"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_data))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify search config was created
    let exists: bool = sqlx::query_scalar(
        &format!("SELECT EXISTS(SELECT 1 FROM search_field_config WHERE bundle = '{type_name}' AND field_name = '{field_name}')")
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(exists, "Search config should exist for field {field_name}");

    // Clean up
    sqlx::query("DELETE FROM search_field_config WHERE bundle = $1 AND field_name = $2")
        .bind(type_name)
        .bind(field_name)
        .execute(&app.db)
        .await
        .ok();
}

#[tokio::test]
async fn e2e_admin_remove_search_config() {
    let app = TestApp::new().await;

    // Use the existing 'page' content type which has a 'body' field
    let type_name = "page";
    let field_name = "body";

    // Create a search config to delete
    sqlx::query(
        "INSERT INTO search_field_config (id, bundle, field_name, weight) VALUES ($1, $2, $3, 'C') ON CONFLICT (bundle, field_name) DO NOTHING"
    )
    .bind(uuid::Uuid::now_v7())
    .bind(type_name)
    .bind(field_name)
    .execute(&app.db)
    .await
    .expect("Failed to create search config");

    let cookies = app
        .create_and_login_admin("admin_search_3", "password123", "search3@test.com")
        .await;

    // Fetch CSRF token (use /admin/people which always has CSRF tokens)
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people").await;

    let response = app
        .request_with_cookies(
            Request::post(format!(
                "/admin/structure/types/{type_name}/search/{field_name}/delete"
            ))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(csrf_form_body(&csrf_token))
            .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );

    // Verify search config was deleted
    let exists: bool = sqlx::query_scalar(
        &format!("SELECT EXISTS(SELECT 1 FROM search_field_config WHERE bundle = '{type_name}' AND field_name = '{field_name}')")
    )
    .fetch_one(&app.db)
    .await
    .unwrap();

    assert!(!exists, "Search config should be deleted");
}

#[tokio::test]
async fn e2e_admin_reindex_content_type() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("admin_search_4", "password123", "search4@test.com")
        .await;

    // Fetch CSRF token (use /admin/people which always has CSRF tokens)
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/people").await;

    // Test reindex for 'page' content type (always exists)
    let response = app
        .request_with_cookies(
            Request::post("/admin/structure/types/page/search/reindex")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;

    let resp_status = response.status();
    assert!(
        resp_status == StatusCode::SEE_OTHER || resp_status == StatusCode::OK,
        "Expected redirect or success, got {resp_status}"
    );
}

// =============================================================================
// Admin Auth Guard Tests
// =============================================================================

#[tokio::test]
async fn e2e_admin_pages_require_login() {
    let app = TestApp::new().await;

    // All these core admin routes should redirect to login when not authenticated.
    // Only non-gated routes are listed here; plugin-gated routes (e.g. categories,
    // comments) may return 404 if the plugin is disabled, which is correct but
    // would make this test fragile.
    let admin_routes = vec![
        "/admin/people",
        "/admin/people/roles",
        "/admin/people/permissions",
        "/admin/content",
        "/admin/structure/types",
        "/admin/content/files",
    ];

    for route in admin_routes {
        let response = app
            .request(Request::get(route).body(Body::empty()).unwrap())
            .await;

        assert_eq!(
            response.status(),
            StatusCode::SEE_OTHER,
            "Route {route} should redirect when not logged in"
        );
    }
}

// =============================================================================
// Static File Tests
// =============================================================================

#[tokio::test]
async fn e2e_static_file_serves_js() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/static/js/file-upload.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    let status = response.status();
    assert_eq!(
        status,
        StatusCode::OK,
        "Expected 200 OK for static JS file, got {status}"
    );

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        content_type.contains("javascript"),
        "Content-Type should be JavaScript, got {content_type}"
    );
}

#[tokio::test]
async fn e2e_static_file_returns_404_for_missing() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/static/nonexistent.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Missing file should return 404"
    );
}

// =============================================================================
// Batch API Tests (Phase 6D)
// =============================================================================

#[tokio::test]
async fn e2e_batch_create_operation() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::post("/api/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operation_type": "test_operation",
                        "params": {"key": "value"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "Should create batch operation"
    );

    let body = response_json(response).await;
    assert!(
        body["id"].is_string(),
        "Response should contain operation ID"
    );
    assert_eq!(
        body["status"], "pending",
        "Initial status should be pending"
    );
}

#[tokio::test]
async fn e2e_batch_get_operation() {
    let app = TestApp::new().await;

    // Create a batch operation first
    let create_response = app
        .request(
            Request::post("/api/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operation_type": "test_get",
                        "params": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    let create_body = response_json(create_response).await;
    let batch_id = create_body["id"].as_str().unwrap();

    // Get the operation status
    let response = app
        .request(
            Request::get(format!("/api/batch/{batch_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["id"], batch_id);
    assert_eq!(body["operation_type"], "test_get");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["progress"]["total"], 0);
    assert_eq!(body["progress"]["processed"], 0);
    assert_eq!(body["progress"]["percentage"], 0);
}

#[tokio::test]
async fn e2e_batch_get_nonexistent_returns_404() {
    let app = TestApp::new().await;

    let fake_id = uuid::Uuid::now_v7();
    let response = app
        .request(
            Request::get(format!("/api/batch/{fake_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn e2e_batch_cancel_operation() {
    let app = TestApp::new().await;

    // Create a batch operation first
    let create_response = app
        .request(
            Request::post("/api/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operation_type": "test_cancel",
                        "params": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    let create_body = response_json(create_response).await;
    let batch_id = create_body["id"].as_str().unwrap();

    // Cancel the operation
    let response = app
        .request(
            Request::post(format!("/api/batch/{batch_id}/cancel"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert_eq!(body["status"], "cancelled");
}

#[tokio::test]
async fn e2e_batch_delete_operation() {
    let app = TestApp::new().await;

    // Create a batch operation first
    let create_response = app
        .request(
            Request::post("/api/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "operation_type": "test_delete",
                        "params": {}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;

    let create_body = response_json(create_response).await;
    let batch_id = create_body["id"].as_str().unwrap();

    // Delete the operation
    let response = app
        .request(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/batch/{batch_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // Verify it's gone
    let get_response = app
        .request(
            Request::get(format!("/api/batch/{batch_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(get_response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn e2e_batch_delete_nonexistent_returns_404() {
    let app = TestApp::new().await;

    let fake_id = uuid::Uuid::now_v7();
    let response = app
        .request(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/batch/{fake_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// =============================================================================
// Helpers
// =============================================================================

async fn response_json(response: axum::response::Response) -> Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap_or_else(|_| {
        let text = String::from_utf8_lossy(&body);
        panic!("Failed to parse JSON: {text}");
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
            if let Some(end) = html[value_start..]
                .find('"')
                .or_else(|| html[value_start..].find('\''))
            {
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

/// Fetch a CSRF token from an admin page. Returns (updated cookies, csrf token).
///
/// GETs the given page, extracts session cookies and the CSRF token from the HTML.
async fn fetch_csrf_token(app: &TestApp, cookies: &str, page: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get(page).body(Body::empty()).unwrap(), cookies)
        .await;
    let new_cookies = extract_cookies(&response);
    let cookies = if new_cookies.is_empty() {
        cookies.to_string()
    } else {
        new_cookies
    };
    let html = response_text(response).await;
    let csrf_token = extract_csrf_token(&html).expect("CSRF token should be present on admin page");
    (cookies, csrf_token)
}

/// Percent-encode a value for use in form-urlencoded bodies.
fn url_encode(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    result
}

/// Build a form-urlencoded POST body with the given CSRF token.
fn csrf_form_body(csrf_token: &str) -> Body {
    Body::from(format!("_token={}", url_encode(csrf_token)))
}

/// Build a form-urlencoded POST body with CSRF token and extra fields.
fn csrf_form_body_with(csrf_token: &str, extra_fields: &str) -> Body {
    Body::from(format!(
        "_token={}&{}",
        url_encode(csrf_token),
        extra_fields
    ))
}

// =============================================================================
// JSON API Tests
// =============================================================================

#[tokio::test]
async fn e2e_api_list_items_returns_paginated() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/api/items?per_page=10")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert!(body["items"].is_array());
    assert!(body["pagination"].is_object());
    assert!(body["pagination"]["total"].is_number());
    assert!(body["pagination"]["page"].is_number());
    assert!(body["pagination"]["per_page"].is_number());
    assert!(body["pagination"]["total_pages"].is_number());
}

#[tokio::test]
async fn e2e_api_list_items_filters_by_type() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/api/items?type=article")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert!(body["items"].is_array());
    // All items should be of type article (or empty if none exist)
    for item in body["items"].as_array().unwrap() {
        assert_eq!(item["type"], "article");
    }
}

#[tokio::test]
async fn e2e_api_list_items_filters_by_status() {
    let app = TestApp::new().await;

    let response = app
        .request(
            Request::get("/api/items?status=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_json(response).await;
    assert!(body["items"].is_array());
    // All items should have status 1
    for item in body["items"].as_array().unwrap() {
        assert_eq!(item["status"], 1);
    }
}

#[tokio::test]
async fn e2e_api_get_item_not_found() {
    let app = TestApp::new().await;

    let fake_id = uuid::Uuid::now_v7();
    let response = app
        .request(
            Request::get(format!("/api/item/{fake_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// =============================================================================
// Comment API Tests
// =============================================================================

#[tokio::test]
async fn e2e_api_list_comments_for_nonexistent_item() {
    let app = TestApp::new().await;

    let fake_id = uuid::Uuid::now_v7();
    let response = app
        .request(
            Request::get(format!("/api/item/{fake_id}/comments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn e2e_api_create_comment_requires_auth() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("comments").await;

    let fake_id = uuid::Uuid::now_v7();
    let response = app
        .request(
            Request::post(format!("/api/item/{fake_id}/comments"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "Test comment"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn e2e_api_comment_crud() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("comments").await;

    // Create a user and login
    let cookies = app
        .create_and_login_user("comment_user", "password123", "comment@test.com")
        .await;

    // Get user ID for author
    let user_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE name = 'comment_user' LIMIT 1")
            .fetch_one(&app.db)
            .await
            .expect("User should exist");

    // Ensure content type exists
    let type_name = format!("commenttest_{}", &uuid::Uuid::now_v7().to_string()[..8]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, plugin, settings)
         VALUES ($1, 'Comment Test', 'For testing', 'test', '{}'::jsonb)
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&type_name)
    .execute(&app.db)
    .await
    .expect("Failed to create content type");

    // Create item directly in DB for testing
    let item_id = uuid::Uuid::now_v7();
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, promote, sticky, fields)
         VALUES ($1, $2, 'Comment Test Item', 1, $3, $4, $5, 0, 0, '{}'::jsonb)"
    )
    .bind(item_id)
    .bind(&type_name)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create item");

    // Test 1: List comments (should be empty)
    let list_response = app
        .request(
            Request::get(format!("/api/item/{item_id}/comments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["total"], 0);
    assert!(body["comments"].as_array().unwrap().is_empty());

    // Fetch CSRF token for API requests
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/").await;

    // Test 2: Create a comment
    let create_response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "This is a test comment"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let created = response_json(create_response).await;
    let comment_id = created["id"].as_str().expect("Comment should have id");
    assert_eq!(created["body"], "This is a test comment");
    assert_eq!(created["status"], 1);
    assert_eq!(created["depth"], 0);

    // Test 3: List comments (should have one)
    let list_response = app
        .request(
            Request::get(format!("/api/item/{item_id}/comments"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_json(list_response).await;
    assert_eq!(body["total"], 1);

    // Test 4: Get single comment
    let get_response = app
        .request(
            Request::get(format!("/api/comment/{comment_id}?include=author"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(get_response.status(), StatusCode::OK);
    let comment = response_json(get_response).await;
    assert_eq!(comment["body"], "This is a test comment");
    assert!(comment["author"].is_object());

    // Fetch fresh CSRF token for update
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/").await;

    // Test 5: Update comment
    let update_response = app
        .request_with_cookies(
            Request::put(format!("/api/comment/{comment_id}"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "Updated comment text"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(update_response.status(), StatusCode::OK);
    let updated = response_json(update_response).await;
    assert_eq!(updated["body"], "Updated comment text");

    // Fetch fresh CSRF token for reply
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/").await;

    // Test 6: Create a reply
    let reply_response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "This is a reply",
                        "parent_id": comment_id
                    }))
                    .unwrap(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(reply_response.status(), StatusCode::OK);
    let reply = response_json(reply_response).await;
    assert_eq!(reply["depth"], 1);
    let reply_id = reply["id"].as_str().expect("Reply should have id");

    // Fetch fresh CSRF token for delete
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/").await;

    // Test 7: Delete comment
    let delete_response = app
        .request_with_cookies(
            Request::delete(format!("/api/comment/{reply_id}"))
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(delete_response.status(), StatusCode::OK);

    // Cleanup
    sqlx::query("DELETE FROM comment WHERE item_id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

#[tokio::test]
async fn e2e_api_comment_validation() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("comments").await;

    let cookies = app
        .create_and_login_user("comment_val_user", "password123", "commentval@test.com")
        .await;

    // Get user ID for author
    let user_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE name = 'comment_val_user' LIMIT 1")
            .fetch_one(&app.db)
            .await
            .expect("User should exist");

    // Ensure content type exists
    let type_name = format!("commentval_{}", &uuid::Uuid::now_v7().to_string()[..8]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, plugin, settings)
         VALUES ($1, 'Comment Val', 'For testing', 'test', '{}'::jsonb)
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&type_name)
    .execute(&app.db)
    .await
    .expect("Failed to create content type");

    // Create item directly in DB
    let item_id = uuid::Uuid::now_v7();
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, promote, sticky, fields)
         VALUES ($1, $2, 'Val Test Item', 1, $3, $4, $5, 0, 0, '{}'::jsonb)"
    )
    .bind(item_id)
    .bind(&type_name)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create item");

    // Fetch CSRF token
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/").await;

    // Test: Empty body should fail
    let empty_response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "   "
                    }))
                    .unwrap(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(empty_response.status(), StatusCode::BAD_REQUEST);

    // Cleanup
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

// =============================================================================
// Comment Admin Moderation Tests
// =============================================================================

#[tokio::test]
async fn e2e_admin_list_comments() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("comments").await;

    let cookies = app
        .create_and_login_admin("comment_admin", "password123", "commentadmin@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/content/comments")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;
    assert!(body.contains("Comments"));
}

#[tokio::test]
async fn e2e_admin_comment_moderation() {
    let app = TestApp::new().await;
    app.ensure_plugin_enabled("comments").await;

    let cookies = app
        .create_and_login_admin("comment_mod", "password123", "commentmod@test.com")
        .await;

    // Get user ID
    let user_id: uuid::Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE name = 'comment_mod' LIMIT 1")
            .fetch_one(&app.db)
            .await
            .expect("User should exist");

    // Create content type and item
    let type_name = format!("commentmod_{}", &uuid::Uuid::now_v7().to_string()[..8]);
    sqlx::query(
        "INSERT INTO item_type (type, label, description, plugin, settings)
         VALUES ($1, 'Comment Mod', 'For testing', 'test', '{}'::jsonb)
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(&type_name)
    .execute(&app.db)
    .await
    .expect("Failed to create content type");

    let item_id = uuid::Uuid::now_v7();
    let now = Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO item (id, type, title, status, author_id, created, changed, promote, sticky, fields)
         VALUES ($1, $2, 'Mod Test Item', 1, $3, $4, $5, 0, 0, '{}'::jsonb)"
    )
    .bind(item_id)
    .bind(&type_name)
    .bind(user_id)
    .bind(now)
    .bind(now)
    .execute(&app.db)
    .await
    .expect("Failed to create item");

    // Fetch CSRF token for API request
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin").await;

    // Create a comment via API
    let create_response = app
        .request_with_cookies(
            Request::post(format!("/api/item/{item_id}/comments"))
                .header("content-type", "application/json")
                .header("X-CSRF-Token", &csrf_token)
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "body": "Comment for moderation test"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(create_response.status(), StatusCode::OK);
    let created = response_json(create_response).await;
    let comment_id = created["id"].as_str().expect("Comment should have id");

    // Test: View comments list
    let list_response = app
        .request_with_cookies(
            Request::get("/admin/content/comments")
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(list_response.status(), StatusCode::OK);
    let body = response_text(list_response).await;
    assert!(body.contains("Comment for moderation"));

    // Test: Edit comment form (also extracts CSRF token for subsequent POSTs)
    let edit_form_response = app
        .request_with_cookies(
            Request::get(format!("/admin/content/comments/{comment_id}/edit"))
                .body(Body::empty())
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(edit_form_response.status(), StatusCode::OK);
    let form_cookies = extract_cookies(&edit_form_response);
    let cookies = if form_cookies.is_empty() {
        cookies
    } else {
        form_cookies
    };
    let body = response_text(edit_form_response).await;
    assert!(body.contains("Edit Comment"));
    let csrf_token = extract_csrf_token(&body).expect("CSRF token");

    // Test: Edit comment submit (includes CSRF token)
    let edit_response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/comments/{comment_id}/edit"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body_with(
                    &csrf_token,
                    "body=Updated+comment+body&status=1",
                ))
                .unwrap(),
            &cookies,
        )
        .await;
    assert!(edit_response.status().is_redirection());

    // Fetch fresh CSRF token for action buttons
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/content/comments").await;

    // Test: Unpublish comment
    let unpublish_response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/comments/{comment_id}/unpublish"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;
    assert!(unpublish_response.status().is_redirection());

    // Verify it's unpublished
    let comment_status: i16 = sqlx::query_scalar("SELECT status FROM comment WHERE id = $1")
        .bind(uuid::Uuid::parse_str(comment_id).unwrap())
        .fetch_one(&app.db)
        .await
        .expect("Comment should exist");
    assert_eq!(comment_status, 0);

    // Fetch fresh CSRF token for approve
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/content/comments").await;

    // Test: Approve comment
    let approve_response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/comments/{comment_id}/approve"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;
    assert!(approve_response.status().is_redirection());

    // Verify it's published
    let comment_status: i16 = sqlx::query_scalar("SELECT status FROM comment WHERE id = $1")
        .bind(uuid::Uuid::parse_str(comment_id).unwrap())
        .fetch_one(&app.db)
        .await
        .expect("Comment should exist");
    assert_eq!(comment_status, 1);

    // Fetch fresh CSRF token for delete
    let (cookies, csrf_token) = fetch_csrf_token(&app, &cookies, "/admin/content/comments").await;

    // Test: Delete comment
    let delete_response = app
        .request_with_cookies(
            Request::post(format!("/admin/content/comments/{comment_id}/delete"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(csrf_form_body(&csrf_token))
                .unwrap(),
            &cookies,
        )
        .await;
    assert!(delete_response.status().is_redirection());

    // Verify it's deleted
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM comment WHERE id = $1)")
        .bind(uuid::Uuid::parse_str(comment_id).unwrap())
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert!(!exists);

    // Cleanup
    sqlx::query("DELETE FROM item WHERE id = $1")
        .bind(item_id)
        .execute(&app.db)
        .await
        .ok();
}

// =============================================================================
// Installer Tests
// =============================================================================

#[tokio::test]
async fn e2e_installer_redirects_when_installed() {
    let app = TestApp::new().await;

    // Ensure site is marked as installed
    sqlx::query("INSERT INTO site_config (key, value) VALUES ('installed', 'true'::jsonb) ON CONFLICT (key) DO UPDATE SET value = 'true'::jsonb")
        .execute(&app.db)
        .await
        .ok();

    // Access /install - should redirect somewhere (either / or to an install step)
    let response = app
        .request(Request::get("/install").body(Body::empty()).unwrap())
        .await;

    // In a shared test database, other tests may change the installed flag
    // Accept any redirect - the important thing is that /install doesn't error
    assert!(response.status().is_redirection());
    let location = response
        .headers()
        .get("location")
        .expect("should have location header")
        .to_str()
        .unwrap();
    // Valid destinations: "/" (installed), "/install/admin" (no admin), "/install/site" (has admin)
    assert!(
        location == "/" || location == "/install/admin" || location == "/install/site",
        "Unexpected redirect location: {location}"
    );
}

#[tokio::test]
async fn e2e_installer_shows_welcome_page() {
    let app = TestApp::new().await;

    // Access /install/welcome directly
    let response = app
        .request(
            Request::get("/install/welcome")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    // Even when installed, welcome page is accessible (will redirect to /)
    let status = response.status();
    assert!(status.is_success() || status.is_redirection());
}

#[tokio::test]
async fn e2e_installer_admin_form_accessible() {
    let app = TestApp::new().await;

    // Mark as NOT installed
    sqlx::query("INSERT INTO site_config (key, value) VALUES ('installed', 'false'::jsonb) ON CONFLICT (key) DO UPDATE SET value = 'false'::jsonb")
        .execute(&app.db)
        .await
        .ok();

    // Access /install/admin
    let response = app
        .request(Request::get("/install/admin").body(Body::empty()).unwrap())
        .await;

    let status = response.status();

    // In a shared test database, admin users may exist from other tests
    // If so, we get redirected to /install/site (303 redirect)
    // If no admin users exist, we get the form (200 OK)
    if status == StatusCode::OK {
        let body = response_text(response).await;
        assert!(body.contains("Create Admin Account"));
        assert!(body.contains("Username"));
        assert!(body.contains("Password"));
    } else {
        // Accept redirect to site config if admin exists
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get("location")
            .expect("should have location header")
            .to_str()
            .unwrap();
        assert!(location == "/install/site" || location == "/");
    }

    // Restore installed state for other tests
    sqlx::query("UPDATE site_config SET value = 'true'::jsonb WHERE key = 'installed'")
        .execute(&app.db)
        .await
        .ok();
}

#[tokio::test]
async fn e2e_installer_complete_page() {
    let app = TestApp::new().await;

    // Access /install/complete directly - always accessible
    let response = app
        .request(
            Request::get("/install/complete")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_text(response).await;
    assert!(body.contains("Congratulations"));
    assert!(body.contains("Installation Complete"));
}

// =============================================================================
// Plugin Admin Tests
// =============================================================================

#[tokio::test]
async fn e2e_admin_plugin_list_requires_admin() {
    let app = TestApp::new().await;

    // Non-admin user should get 403
    let cookies = app
        .create_and_login_user("plugin_user_1", "password123", "pluguser1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "Non-admin users should be denied access to plugin admin"
    );
}

#[tokio::test]
async fn e2e_admin_plugin_list_shows_plugins() {
    let app = TestApp::new().await;

    let cookies = app
        .create_and_login_admin("plugin_admin_1", "password123", "plugadmin1@test.com")
        .await;

    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = response_text(response).await;
    assert!(body.contains("Plugins"), "Page should have Plugins heading");
    assert!(body.contains("blog"), "Should list the blog plugin");
}

#[tokio::test]
async fn e2e_admin_plugin_toggle() {
    let app = TestApp::new().await;
    // Ensure the redirects plugin is installed so the toggle form appears
    app.ensure_plugin_enabled("redirects").await;

    let cookies = app
        .create_and_login_admin("plugin_admin_2", "password123", "plugadmin2@test.com")
        .await;

    // Load the plugin list page to get a CSRF token
    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await;

    // Extract CSRF token from the form
    let csrf_token = body
        .split("name=\"_token\" value=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("CSRF token");

    // Disable the redirects plugin (safe to toggle — it has no gated routes)
    let form_body = format!("_token={csrf_token}&plugin_name=redirects&action=disable");

    let response = app
        .request_with_cookies(
            Request::post("/admin/plugins/toggle")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
            &cookies,
        )
        .await;

    // Should redirect back to plugin list
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "Toggle should redirect"
    );

    // Follow the redirect to see the flash message
    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    let body = response_text(response).await;
    assert!(
        body.contains("disabled"),
        "Flash message should confirm plugin was disabled"
    );

    // Re-enable so we leave clean state
    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;

    let body = response_text(response).await;
    let csrf_token = body
        .split("name=\"_token\" value=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("CSRF token for re-enable");

    let form_body = format!("_token={csrf_token}&plugin_name=redirects&action=enable");

    let response = app
        .request_with_cookies(
            Request::post("/admin/plugins/toggle")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
            &cookies,
        )
        .await;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
}

// =============================================================================
// Runtime Plugin Gate Tests
// =============================================================================

/// Verify that POST /admin/plugins/toggle on a gated plugin immediately
/// affects route availability (no restart needed).
#[tokio::test]
async fn e2e_toggle_gated_plugin_affects_routes() {
    let app = TestApp::new().await;
    // Ensure categories is installed in DB and enabled in-memory
    app.ensure_plugin_enabled("categories").await;

    let cookies = app
        .create_and_login_admin("gate_toggle_admin", "password123", "gatetoggle@test.com")
        .await;

    // Route should be reachable
    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Route should be reachable when categories is enabled"
    );

    // Get CSRF token from plugin list page
    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    let body = response_text(response).await;
    let csrf_token = body
        .split("name=\"_token\" value=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("CSRF token");

    // Disable categories via the admin UI toggle
    let form_body = format!("_token={csrf_token}&plugin_name=categories&action=disable");
    let response = app
        .request_with_cookies(
            Request::post("/admin/plugins/toggle")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    // Gated route should now return 404
    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Gated route should return 404 after disabling via admin toggle"
    );

    // Re-enable via toggle
    let response = app
        .request_with_cookies(
            Request::get("/admin/plugins").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    let body = response_text(response).await;
    let csrf_token = body
        .split("name=\"_token\" value=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("CSRF token for re-enable");

    let form_body = format!("_token={csrf_token}&plugin_name=categories&action=enable");
    let response = app
        .request_with_cookies(
            Request::post("/admin/plugins/toggle")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form_body))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);

    // Route should be reachable again
    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Gated route should be reachable again after re-enabling via admin toggle"
    );
}

/// Verify that disabling a gated plugin at runtime makes its routes return 404,
/// and re-enabling restores them — without a server restart.
#[tokio::test]
async fn e2e_runtime_plugin_gate_returns_404_when_disabled() {
    let app = TestApp::new().await;

    // Ensure the categories plugin is enabled in memory for this test.
    app.state.set_plugin_enabled("categories", true);

    // With the plugin enabled, the gated API route should NOT be 404.
    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Gated route should be reachable when plugin is enabled"
    );

    // Disable the plugin at runtime (in-memory only — no DB write needed).
    app.state.set_plugin_enabled("categories", false);

    // The same route should now return 404 from the gate middleware.
    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Gated route should return 404 when plugin is disabled"
    );

    // Re-enable and confirm the route is reachable again.
    app.state.set_plugin_enabled("categories", true);

    let response = app
        .request(Request::get("/api/categories").body(Body::empty()).unwrap())
        .await;
    assert_ne!(
        response.status(),
        StatusCode::NOT_FOUND,
        "Gated route should be reachable again after re-enabling"
    );
}
