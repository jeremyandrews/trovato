//! Admin routes for category and tag management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::{Category, CreateCategory, CreateTag, Tag, UpdateCategory, UpdateTag};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, MACHINE_NAME_ERROR, is_valid_machine_name, render_admin_template, render_error,
    render_not_found, render_server_error, require_admin, require_csrf,
};

// =============================================================================
// Form data
// =============================================================================

/// Category form data.
#[derive(Debug, Deserialize)]
struct CategoryFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    id: String,
    label: String,
    description: Option<String>,
    hierarchy: Option<String>,
}

/// Tag form data.
#[derive(Debug, Deserialize)]
struct TagFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    label: String,
    description: Option<String>,
    weight: Option<String>,
    parent_id: Option<String>,
}

// =============================================================================
// Category handlers
// =============================================================================

/// List all categories.
///
/// GET /admin/structure/categories
async fn list_categories(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let categories = match Category::list(state.db()).await {
        Ok(categories) => categories,
        Err(e) => {
            tracing::error!(error = %e, "failed to list categories");
            return render_server_error("Failed to load categories.");
        }
    };

    // Get tag counts for each category
    let mut tag_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for cat in &categories {
        let count = Tag::count_by_category(state.db(), &cat.id)
            .await
            .unwrap_or(0);
        tag_counts.insert(cat.id.clone(), count);
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("categories", &categories);
    context.insert("tag_counts", &tag_counts);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/categories");

    render_admin_template(&state, "admin/categories.html", context).await
}

/// Show add category form.
///
/// GET /admin/structure/categories/add
async fn add_category_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/categories/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({"hierarchy": 0}));
    context.insert("path", "/admin/structure/categories/add");

    render_admin_template(&state, "admin/category-form.html", context).await
}

/// Handle add category form submission.
///
/// POST /admin/structure/categories/add
async fn add_category_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CategoryFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    if form.id.trim().is_empty() {
        errors.push("Machine name is required.".to_string());
    } else if !is_valid_machine_name(&form.id) {
        errors.push(MACHINE_NAME_ERROR.to_string());
    }

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    // Check if category already exists
    if Category::exists(state.db(), &form.id)
        .await
        .unwrap_or(false)
    {
        errors.push(format!("A category with ID '{}' already exists.", form.id));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/structure/categories/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "id": form.id,
                "label": form.label,
                "description": form.description,
                "hierarchy": form.hierarchy,
            }),
        );
        context.insert("path", "/admin/structure/categories/add");

        return render_admin_template(&state, "admin/category-form.html", context).await;
    }

    let input = CreateCategory {
        id: form.id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match Category::create(state.db(), input).await {
        Ok(_) => {
            tracing::info!(id = %form.id, "category created");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create category");
            render_server_error("Failed to create category.")
        }
    }
}

/// Show edit category form.
///
/// GET /admin/structure/categories/{id}/edit
async fn edit_category_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/categories/{category_id}/edit"),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("category_id", &category_id);
    context.insert(
        "values",
        &serde_json::json!({
            "id": category.id,
            "label": category.label,
            "description": category.description,
            "hierarchy": category.hierarchy,
        }),
    );
    context.insert(
        "path",
        &format!("/admin/structure/categories/{category_id}/edit"),
    );

    render_admin_template(&state, "admin/category-form.html", context).await
}

/// Handle edit category form submission.
///
/// POST /admin/structure/categories/{id}/edit
async fn edit_category_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<CategoryFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    if !Category::exists(state.db(), &category_id)
        .await
        .unwrap_or(false)
    {
        return render_not_found();
    }

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/categories/{category_id}/edit"),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("category_id", &category_id);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "id": form.id,
                "label": form.label,
                "description": form.description,
                "hierarchy": form.hierarchy,
            }),
        );
        context.insert(
            "path",
            &format!("/admin/structure/categories/{category_id}/edit"),
        );

        return render_admin_template(&state, "admin/category-form.html", context).await;
    }

    let input = UpdateCategory {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match Category::update(state.db(), &category_id, input).await {
        Ok(_) => {
            tracing::info!(id = %category_id, "category updated");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update category");
            render_server_error("Failed to update category.")
        }
    }
}

/// Delete a category.
///
/// POST /admin/structure/categories/{id}/delete
async fn delete_category(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match Category::delete(state.db(), &category_id).await {
        Ok(true) => {
            tracing::info!(id = %category_id, "category deleted");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete category");
            render_server_error("Failed to delete category.")
        }
    }
}

// =============================================================================
// Tag handlers
// =============================================================================

/// List tags in a category.
///
/// GET /admin/structure/categories/{id}/tags
async fn list_tags(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    let tags = match Tag::list_by_category(state.db(), &category_id).await {
        Ok(tags) => tags,
        Err(e) => {
            tracing::error!(error = %e, "failed to list tags");
            return render_server_error("Failed to load tags.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("category", &category);
    context.insert("tags", &tags);
    context.insert("csrf_token", &csrf_token);
    context.insert(
        "path",
        &format!("/admin/structure/categories/{category_id}/tags"),
    );

    render_admin_template(&state, "admin/tags.html", context).await
}

/// Show add tag form.
///
/// GET /admin/structure/categories/{id}/tags/add
async fn add_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    // Get existing tags for parent selector
    let tags = Tag::list_by_category(state.db(), &category_id)
        .await
        .unwrap_or_default();

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/categories/{category_id}/tags/add"),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("category", &category);
    context.insert("existing_tags", &tags);
    context.insert("values", &serde_json::json!({}));
    context.insert(
        "path",
        &format!("/admin/structure/categories/{category_id}/tags/add"),
    );

    render_admin_template(&state, "admin/tag-form.html", context).await
}

/// Handle add tag form submission.
///
/// POST /admin/structure/categories/{id}/tags/add
async fn add_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<TagFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(category) = Category::find_by_id(state.db(), &category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let tags = Tag::list_by_category(state.db(), &category_id)
            .await
            .unwrap_or_default();
        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/categories/{category_id}/tags/add"),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("category", &category);
        context.insert("existing_tags", &tags);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "description": form.description,
                "weight": form.weight,
                "parent_id": form.parent_id,
            }),
        );
        context.insert(
            "path",
            &format!("/admin/structure/categories/{category_id}/tags/add"),
        );

        return render_admin_template(&state, "admin/tag-form.html", context).await;
    }

    let parent_ids = match &form.parent_id {
        Some(id) if !id.is_empty() => match uuid::Uuid::parse_str(id) {
            Ok(uuid) => Some(vec![uuid]),
            Err(_) => None,
        },
        _ => None,
    };

    let input = CreateTag {
        category_id: category_id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
        parent_ids,
    };

    match Tag::create(state.db(), input).await {
        Ok(_) => {
            tracing::info!(category = %category_id, label = %form.label, "tag created");
            Redirect::to(&format!("/admin/structure/categories/{category_id}/tags")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create tag");
            render_server_error("Failed to create tag.")
        }
    }
}

/// Show edit tag form.
///
/// GET /admin/structure/tags/{id}/edit
async fn edit_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(tag) = Tag::find_by_id(state.db(), tag_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(category) = Category::find_by_id(state.db(), &tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error("Category not found.");
    };

    // Get existing tags for parent selector (excluding self)
    let tags: Vec<_> = Tag::list_by_category(state.db(), &tag.category_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|t| t.id != tag_id)
        .collect();

    // Get current parents
    let parents = Tag::get_parents(state.db(), tag_id)
        .await
        .unwrap_or_default();
    let current_parent_id = parents.first().map(|p| p.id.to_string());

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/structure/tags/{tag_id}/edit"));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("tag_id", &tag_id.to_string());
    context.insert("category", &category);
    context.insert("existing_tags", &tags);
    context.insert(
        "values",
        &serde_json::json!({
            "label": tag.label,
            "description": tag.description,
            "weight": tag.weight,
            "parent_id": current_parent_id,
        }),
    );
    context.insert("path", &format!("/admin/structure/tags/{tag_id}/edit"));

    render_admin_template(&state, "admin/tag-form.html", context).await
}

/// Handle edit tag form submission.
///
/// POST /admin/structure/tags/{id}/edit
async fn edit_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    Form(form): Form<TagFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(tag) = Tag::find_by_id(state.db(), tag_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(category) = Category::find_by_id(state.db(), &tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error("Category not found.");
    };

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push("Name is required.".to_string());
    }

    if !errors.is_empty() {
        let tags: Vec<_> = Tag::list_by_category(state.db(), &tag.category_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.id != tag_id)
            .collect();

        let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/structure/tags/{tag_id}/edit"));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("tag_id", &tag_id.to_string());
        context.insert("category", &category);
        context.insert("existing_tags", &tags);
        context.insert("errors", &errors);
        context.insert(
            "values",
            &serde_json::json!({
                "label": form.label,
                "description": form.description,
                "weight": form.weight,
                "parent_id": form.parent_id,
            }),
        );
        context.insert("path", &format!("/admin/structure/tags/{tag_id}/edit"));

        return render_admin_template(&state, "admin/tag-form.html", context).await;
    }

    // Update tag
    let input = UpdateTag {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
    };

    if let Err(e) = Tag::update(state.db(), tag_id, input).await {
        tracing::error!(error = %e, "failed to update tag");
        return render_server_error("Failed to update tag.");
    }

    // Update parent if hierarchy is enabled
    if category.hierarchy > 0 {
        let parent_ids: Vec<uuid::Uuid> = match &form.parent_id {
            Some(id) if !id.is_empty() => match uuid::Uuid::parse_str(id) {
                Ok(uuid) => vec![uuid],
                Err(_) => vec![],
            },
            _ => vec![],
        };

        if let Err(e) = Tag::set_parents(state.db(), tag_id, &parent_ids).await {
            tracing::error!(error = %e, "failed to update tag parents");
        }
    }

    tracing::info!(tag_id = %tag_id, "tag updated");
    Redirect::to(&format!(
        "/admin/structure/categories/{}/tags",
        tag.category_id
    ))
    .into_response()
}

/// Delete a tag.
///
/// POST /admin/structure/tags/{id}/delete
async fn delete_tag(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Get category ID for redirect
    let category_id = Tag::find_by_id(state.db(), tag_id)
        .await
        .ok()
        .flatten()
        .map(|t| t.category_id);

    match Tag::delete(state.db(), tag_id).await {
        Ok(true) => {
            tracing::info!(tag_id = %tag_id, "tag deleted");
            let redirect_url = category_id
                .map(|id| format!("/admin/structure/categories/{id}/tags"))
                .unwrap_or_else(|| "/admin/structure/categories".to_string());
            Redirect::to(&redirect_url).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete tag");
            render_server_error("Failed to delete tag.")
        }
    }
}

// =============================================================================
// Router
// =============================================================================

/// Category and tag admin routes (registered when "categories" plugin is enabled).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/categories", get(list_categories))
        .route("/admin/structure/categories/add", get(add_category_form))
        .route("/admin/structure/categories/add", post(add_category_submit))
        .route(
            "/admin/structure/categories/{id}/edit",
            get(edit_category_form),
        )
        .route(
            "/admin/structure/categories/{id}/edit",
            post(edit_category_submit),
        )
        .route(
            "/admin/structure/categories/{id}/delete",
            post(delete_category),
        )
        .route("/admin/structure/categories/{id}/tags", get(list_tags))
        .route(
            "/admin/structure/categories/{id}/tags/add",
            get(add_tag_form),
        )
        .route(
            "/admin/structure/categories/{id}/tags/add",
            post(add_tag_submit),
        )
        .route("/admin/structure/tags/{id}/edit", get(edit_tag_form))
        .route("/admin/structure/tags/{id}/edit", post(edit_tag_submit))
        .route("/admin/structure/tags/{id}/delete", post(delete_tag))
}
