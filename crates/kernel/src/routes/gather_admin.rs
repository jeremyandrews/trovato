//! Admin routes for gather query management.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::{generate_csrf_token, verify_csrf_token};
use crate::gather::{
    GatherQuery, GatherService, MAX_ITEMS_PER_PAGE, QueryDefinition, QueryDisplay,
};
use crate::models::User;
use crate::routes::auth::SESSION_USER_ID;
use crate::state::AppState;

// =============================================================================
// Auth helper
// =============================================================================

/// Check if user is authenticated AND is an admin, return user or redirect.
async fn require_auth(state: &AppState, session: &Session) -> Result<User, Response> {
    let user_id: Option<uuid::Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();

    if let Some(id) = user_id {
        if let Ok(Some(user)) = User::find_by_id(state.db(), id).await {
            if user.is_admin {
                return Ok(user);
            }
            // Logged in but not admin — return 403
            return Err((StatusCode::FORBIDDEN, Html("Access denied")).into_response());
        }
    }

    Err(Redirect::to("/user/login").into_response())
}

// =============================================================================
// Form data
// =============================================================================

/// Form data for clone/delete actions (CSRF token only).
#[derive(Debug, Deserialize)]
struct CsrfFormData {
    #[serde(rename = "_token")]
    token: String,
}

/// Form data for creating or editing a gather query.
#[derive(Debug, Deserialize)]
struct GatherFormData {
    #[serde(rename = "_token")]
    token: String,
    #[allow(dead_code)]
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    query_id: String,
    label: String,
    description: Option<String>,
    /// JSON-serialized QueryDefinition
    definition_json: String,
    /// JSON-serialized QueryDisplay
    display_json: String,
}

// =============================================================================
// Handlers
// =============================================================================

/// List all gather queries.
///
/// GET /admin/gather
async fn list_queries(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let queries = state.gather().list_queries();

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("queries", &queries);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/gather");
    super::helpers::inject_site_context(&state, &session, &mut context).await;

    render_admin_template(&state, "admin/gather-list.html", &context).await
}

/// Show create gather query form.
///
/// GET /admin/gather/create
async fn create_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let empty_definition = serde_json::to_string(&QueryDefinition::default()).unwrap_or_default();
    let empty_display = serde_json::to_string(&QueryDisplay::default()).unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/gather/create");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("definition_json", &empty_definition);
    context.insert("display_json", &empty_display);
    context.insert(
        "values",
        &serde_json::json!({
            "query_id": "",
            "label": "",
            "description": "",
        }),
    );
    context.insert("path", "/admin/gather/create");
    super::helpers::inject_site_context(&state, &session, &mut context).await;

    render_admin_template(&state, "admin/gather-form.html", &context).await
}

/// Handle create gather query form submission.
///
/// POST /admin/gather/create
async fn create_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<GatherFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error("Invalid or expired form token. Please try again.");
    }

    // Validate basic fields
    let mut errors = Vec::new();

    let query_id = form.query_id.trim().to_string();
    let label = form.label.trim().to_string();

    if query_id.is_empty() {
        errors.push("Query ID is required.".to_string());
    } else if !is_valid_query_id(&query_id) {
        errors.push(
            "Query ID must start with a letter and contain only lowercase letters, numbers, hyphens, underscores, dots, and colons."
                .to_string(),
        );
    }

    if label.is_empty() {
        errors.push("Label is required.".to_string());
    }

    // Check if query ID already exists
    if state.gather().get_query(&query_id).is_some() {
        errors.push(format!(
            "A gather query with ID '{}' already exists.",
            query_id
        ));
    }

    // Parse definition JSON
    let definition: QueryDefinition = match serde_json::from_str(&form.definition_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(format!("Invalid definition JSON: {}", e));
            QueryDefinition::default()
        }
    };

    // Parse display JSON
    let display: QueryDisplay = match serde_json::from_str(&form.display_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(format!("Invalid display JSON: {}", e));
            QueryDisplay::default()
        }
    };

    // Validate definition
    let definition_errors = GatherService::validate_definition(&definition);
    errors.extend(definition_errors);

    // Cap items_per_page
    let mut display = display;
    if display.items_per_page > MAX_ITEMS_PER_PAGE {
        display.items_per_page = MAX_ITEMS_PER_PAGE;
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/gather/create");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert("definition_json", &form.definition_json);
        context.insert("display_json", &form.display_json);
        context.insert(
            "values",
            &serde_json::json!({
                "query_id": query_id,
                "label": label,
                "description": form.description,
            }),
        );
        context.insert("path", "/admin/gather/create");
        super::helpers::inject_site_context(&state, &session, &mut context).await;

        return render_admin_template(&state, "admin/gather-form.html", &context).await;
    }

    // Build the query
    let now = chrono::Utc::now().timestamp();
    let query = GatherQuery {
        query_id: query_id.clone(),
        label,
        description: form.description.filter(|s| !s.trim().is_empty()),
        definition,
        display,
        plugin: "admin".to_string(),
        created: now,
        changed: now,
    };

    match state.gather().register_query(query).await {
        Ok(()) => {
            tracing::info!(query_id = %query_id, "gather query created");
            Redirect::to("/admin/gather").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create gather query");
            render_error("Failed to create gather query.")
        }
    }
}

/// Show edit gather query form.
///
/// GET /admin/gather/{id}/edit
async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    let Some(query) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let definition_json = serde_json::to_string(&query.definition).unwrap_or_default();
    let display_json = serde_json::to_string(&query.display).unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/gather/{}/save", id));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("definition_json", &definition_json);
    context.insert("display_json", &display_json);
    context.insert(
        "values",
        &serde_json::json!({
            "query_id": query.query_id,
            "label": query.label,
            "description": query.description.unwrap_or_default(),
        }),
    );
    context.insert("path", &format!("/admin/gather/{}/edit", id));
    super::helpers::inject_site_context(&state, &session, &mut context).await;

    render_admin_template(&state, "admin/gather-form.html", &context).await
}

/// Handle edit gather query form submission.
///
/// POST /admin/gather/{id}/save
async fn save_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<GatherFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify the query exists
    let Some(existing) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);

    if !token_valid {
        return render_error("Invalid or expired form token. Please try again.");
    }

    // Validate
    let mut errors = Vec::new();

    let label = form.label.trim().to_string();
    if label.is_empty() {
        errors.push("Label is required.".to_string());
    }

    // Parse definition JSON
    let definition: QueryDefinition = match serde_json::from_str(&form.definition_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(format!("Invalid definition JSON: {}", e));
            QueryDefinition::default()
        }
    };

    // Parse display JSON
    let display: QueryDisplay = match serde_json::from_str(&form.display_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(format!("Invalid display JSON: {}", e));
            QueryDisplay::default()
        }
    };

    // Validate definition
    let definition_errors = GatherService::validate_definition(&definition);
    errors.extend(definition_errors);

    // Cap items_per_page
    let mut display = display;
    if display.items_per_page > MAX_ITEMS_PER_PAGE {
        display.items_per_page = MAX_ITEMS_PER_PAGE;
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/gather/{}/save", id));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("errors", &errors);
        context.insert("definition_json", &form.definition_json);
        context.insert("display_json", &form.display_json);
        context.insert(
            "values",
            &serde_json::json!({
                "query_id": id,
                "label": label,
                "description": form.description,
            }),
        );
        context.insert("path", &format!("/admin/gather/{}/edit", id));
        super::helpers::inject_site_context(&state, &session, &mut context).await;

        return render_admin_template(&state, "admin/gather-form.html", &context).await;
    }

    // Update the query (preserve original created timestamp and plugin)
    let now = chrono::Utc::now().timestamp();
    let query = GatherQuery {
        query_id: id.clone(),
        label,
        description: form.description.filter(|s| !s.trim().is_empty()),
        definition,
        display,
        plugin: existing.plugin,
        created: existing.created,
        changed: now,
    };

    match state.gather().register_query(query).await {
        Ok(()) => {
            tracing::info!(query_id = %id, "gather query updated");
            Redirect::to("/admin/gather").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update gather query");
            render_error("Failed to update gather query.")
        }
    }
}

/// Clone a gather query.
///
/// POST /admin/gather/{id}/clone
async fn clone_query(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<CsrfFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);
    if !token_valid {
        return render_error("Invalid or expired form token. Please try again.");
    }

    if state.gather().get_query(&id).is_none() {
        return render_not_found();
    }

    let timestamp = chrono::Utc::now().timestamp();
    let new_id = format!("{}-copy-{}", id, timestamp);

    match state.gather().clone_query(&id, &new_id).await {
        Ok(_) => {
            tracing::info!(source = %id, new_id = %new_id, "gather query cloned");
            Redirect::to("/admin/gather").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to clone gather query");
            render_error("Failed to clone gather query.")
        }
    }
}

/// Delete a gather query.
///
/// POST /admin/gather/{id}/delete
async fn delete_query(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<CsrfFormData>,
) -> Response {
    if let Err(redirect) = require_auth(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    let token_valid = verify_csrf_token(&session, &form.token)
        .await
        .unwrap_or(false);
    if !token_valid {
        return render_error("Invalid or expired form token. Please try again.");
    }

    match state.gather().delete_query(&id).await {
        Ok(true) => {
            tracing::info!(query_id = %id, "gather query deleted");
            Redirect::to("/admin/gather").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete gather query");
            render_error("Failed to delete gather query.")
        }
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Render an admin template, falling back to an error page on failure.
async fn render_admin_template(
    state: &AppState,
    template: &str,
    context: &tera::Context,
) -> Response {
    match state.theme().tera().render(template, context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!(error = %e, template = %template, "failed to render template");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html(format!(
                    r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body><h1>Template Error</h1><pre>{}</pre></body></html>"#,
                    html_escape(&e.to_string())
                )),
            )
                .into_response()
        }
    }
}

/// Render an error page.
fn render_error(message: &str) -> Response {
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><title>Error</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Error</h1>
<p>{}</p>
<p><a href="javascript:history.back()">Go back</a></p>
</div>
</body></html>"#,
        html_escape(message)
    );

    (StatusCode::BAD_REQUEST, Html(html)).into_response()
}

/// Render a 404 page.
fn render_not_found() -> Response {
    let html = r#"<!DOCTYPE html>
<html><head><title>Not Found</title></head>
<body>
<div style="max-width: 600px; margin: 100px auto; text-align: center;">
<h1>Not Found</h1>
<p>The requested gather query could not be found.</p>
<p><a href="/admin/gather">Return to gather queries</a></p>
</div>
</body></html>"#;

    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

/// Validate a query ID format.
///
/// Must start with a lowercase letter and contain only lowercase letters,
/// numbers, hyphens, underscores, dots, and colons. Matches the HTML
/// pattern `[a-z0-9_.:-]+` used in gather-form.html.
fn is_valid_query_id(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }

    let mut chars = id.chars();

    // First character must be lowercase letter
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }

    // Remaining characters: lowercase letters, digits, hyphens, underscores, dots, colons
    chars.all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.' || c == ':'
    })
}

/// Escape HTML characters.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

// =============================================================================
// Router
// =============================================================================

/// Create the gather admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/gather", get(list_queries))
        .route("/admin/gather/create", get(create_form).post(create_submit))
        .route("/admin/gather/{id}/edit", get(edit_form))
        .route("/admin/gather/{id}/save", post(save_submit))
        .route("/admin/gather/{id}/clone", post(clone_query))
        .route("/admin/gather/{id}/delete", post(delete_query))
}
