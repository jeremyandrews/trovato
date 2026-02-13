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
