//! Admin routes for category and tag management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::i18n::UiMessage;
use crate::routes::admin_form::FieldError;
use crate::models::{CreateCategory, CreateTag, UpdateCategory, UpdateTag};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, MACHINE_NAME_ERROR, SLUG_FORMAT_ERROR, is_valid_machine_name, is_valid_slug,
    render_admin_template, render_error, render_not_found, render_server_error, require_csrf,
    require_permission,
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
pub(crate) struct TagFormData {
    #[serde(rename = "_token")]
    token: String,
    #[serde(rename = "_form_build_id")]
    form_build_id: String,
    label: String,
    description: Option<String>,
    slug: Option<String>,
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
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    let categories = match state.categories().list_categories().await {
        Ok(categories) => categories,
        Err(e) => {
            tracing::error!(error = %e, "failed to list categories");
            return render_server_error("Failed to load categories.");
        }
    };

    // Get tag counts for each category
    let mut tag_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for cat in &categories {
        let count = state.categories().count_tags(&cat.id).await.unwrap_or(0);
        tag_counts.insert(cat.id.clone(), count);
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("categories", &categories);
    context.insert("tag_counts", &tag_counts);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/categories");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/categories.html",
        context,
    )
    .await
}

/// Show add category form.
///
/// GET /admin/structure/categories/add
async fn add_category_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/categories/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("hierarchy_options", HIERARCHY_OPTIONS);
    context.insert("values", &serde_json::json!({"hierarchy": 0}));
    context.insert("path", "/admin/structure/categories/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/category-form.html",
        context,
    )
    .await
}

/// A category's hierarchy choices: value and English label, translated by
/// the select macro.
const HIERARCHY_OPTIONS: &[(&str, &str)] = &[
    ("0", "Flat (no hierarchy)"),
    ("1", "Single parent"),
    ("2", "Multiple parents"),
];

/// Handle add category form submission.
///
/// POST /admin/structure/categories/add
async fn add_category_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CategoryFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    if form.id.trim().is_empty() {
        errors.push(FieldError::on("id", "Machine name is required."));
    } else if !is_valid_machine_name(&form.id) {
        errors.push(FieldError::on("id", MACHINE_NAME_ERROR));
    }

    if form.label.trim().is_empty() {
        errors.push(FieldError::on("label", "Name is required."));
    }

    // Check if category already exists
    if state
        .categories()
        .category_exists(&form.id)
        .await
        .unwrap_or(false)
    {
        errors.push(FieldError::on(
            "id",
            UiMessage::new("A category with ID '{id}' already exists.").arg("id", &form.id),
        ));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/structure/categories/add");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("hierarchy_options", HIERARCHY_OPTIONS);
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

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/category-form.html",
            context,
        )
        .await;
    }

    let input = CreateCategory {
        id: form.id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match state.categories().create_category(input).await {
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
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    let Some(category) = state
        .categories()
        .get_category(&category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/categories/{category_id}/edit"),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("hierarchy_options", HIERARCHY_OPTIONS);
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

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/category-form.html",
        context,
    )
    .await
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
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    if form.label.trim().is_empty() {
        errors.push(FieldError::on("label", "Name is required."));
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert(
            "action",
            &format!("/admin/structure/categories/{category_id}/edit"),
        );
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("hierarchy_options", HIERARCHY_OPTIONS);
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

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/category-form.html",
            context,
        )
        .await;
    }

    let input = UpdateCategory {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        hierarchy: form.hierarchy.as_ref().and_then(|s| s.parse().ok()),
        weight: None,
    };

    match state
        .categories()
        .update_category(&category_id, input)
        .await
    {
        Ok(Some(_)) => {
            tracing::info!(id = %category_id, "category updated");
            Redirect::to("/admin/structure/categories").into_response()
        }
        Ok(None) => render_not_found(),
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
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.categories().delete_category(&category_id).await {
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
//
// The term list and term form are served in two places: here, for every
// vocabulary, behind `administer categories`; and under a plugin's owner route
// (`routes::owned_vocabulary`, B108) for that owner's vocabularies, behind the
// owner's access check. The handlers below check access and find the
// vocabulary; the `pub(crate)` functions after them do the rest for both,
// with the URLs a [`TermUrls`] gives.

/// Where the term screens of one vocabulary live.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct TermUrls {
    /// The page listing the vocabularies, the breadcrumb's first link.
    pub index: String,
    /// What that page is called.
    pub index_label: String,
    /// This vocabulary's term list.
    pub list: String,
    /// Its add-term form.
    pub add: String,
    /// The base a term's `/{id}/edit` and `/{id}/delete` hang off.
    pub term_base: String,
}

impl TermUrls {
    /// The site-wide screens under `/admin/structure`.
    pub(crate) fn site(category_id: &str) -> Self {
        Self {
            index: "/admin/structure/categories".to_string(),
            index_label: "Categories".to_string(),
            list: format!("/admin/structure/categories/{category_id}/tags"),
            add: format!("/admin/structure/categories/{category_id}/tags/add"),
            term_base: "/admin/structure/tags".to_string(),
        }
    }

    fn edit(&self, tag_id: uuid::Uuid) -> String {
        format!("{}/{tag_id}/edit", self.term_base)
    }
}

/// The posted term form.
pub(crate) type TermForm = TagFormData;

/// GET /admin/structure/categories/{id}/tags
async fn list_tags(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(category) = state
        .categories()
        .get_category(&category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };
    term_list(&state, &session, &category, &TermUrls::site(&category_id)).await
}

/// GET /admin/structure/categories/{id}/tags/add
async fn add_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(category) = state
        .categories()
        .get_category(&category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };
    term_add_form(&state, &session, &category, &TermUrls::site(&category_id)).await
}

/// POST /admin/structure/categories/{id}/tags/add
async fn add_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    Form(form): Form<TagFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(category) = state
        .categories()
        .get_category(&category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_not_found();
    };
    term_add_submit(
        &state,
        &session,
        &category,
        form,
        &TermUrls::site(&category_id),
    )
    .await
}

/// GET /admin/structure/tags/{id}/edit
async fn edit_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(tag) = state.categories().get_tag(tag_id).await.ok().flatten() else {
        return render_not_found();
    };
    let Some(category) = state
        .categories()
        .get_category(&tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error("Category not found.");
    };
    let urls = TermUrls::site(&category.id);
    term_edit_form(&state, &session, &category, &tag, &urls).await
}

/// POST /admin/structure/tags/{id}/edit
async fn edit_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    Form(form): Form<TagFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(tag) = state.categories().get_tag(tag_id).await.ok().flatten() else {
        return render_not_found();
    };
    let Some(category) = state
        .categories()
        .get_category(&tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return render_error("Category not found.");
    };
    let urls = TermUrls::site(&category.id);
    term_edit_submit(&state, &session, &category, &tag, form, &urls).await
}

/// POST /admin/structure/tags/{id}/delete
async fn delete_tag(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer categories").await {
        return redirect;
    }
    let Some(tag) = state.categories().get_tag(tag_id).await.ok().flatten() else {
        return render_not_found();
    };
    let urls = TermUrls::site(&tag.category_id);
    term_delete(&state, &session, &tag, &form.token, &urls).await
}

// ---- Shared with the owner routes ------------------------------------------

/// The term list of `category`.
pub(crate) async fn term_list(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    urls: &TermUrls,
) -> Response {
    let tags = match state.categories().list_tags(&category.id).await {
        Ok(tags) => tags,
        Err(e) => {
            tracing::error!(error = %e, "failed to list tags");
            return render_server_error("Failed to load tags.");
        }
    };

    let csrf_token = generate_csrf_token(session).await;

    let mut context = tera::Context::new();
    context.insert("category", category);
    context.insert("tags", &tags);
    context.insert("csrf_token", &csrf_token);
    context.insert("urls", urls);
    context.insert("path", &urls.list);

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/tags.html",
        context,
    )
    .await
}

/// Render the term form. `tag_id` is the term being edited, if any;
/// `values` what the fields hold.
#[allow(clippy::too_many_arguments)]
async fn render_term_form(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    tag_id: Option<uuid::Uuid>,
    values: serde_json::Value,
    errors: &[FieldError],
    urls: &TermUrls,
) -> Response {
    // The parent selector: every term of the vocabulary but this one.
    let tags: Vec<_> = state
        .categories()
        .list_tags(&category.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|t| Some(t.id) != tag_id)
        .collect();

    let csrf_token = generate_csrf_token(session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();
    let action = match tag_id {
        Some(id) => urls.edit(id),
        None => urls.add.clone(),
    };

    let mut context = tera::Context::new();
    context.insert("action", &action);
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &tag_id.is_some());
    if let Some(id) = tag_id {
        context.insert("tag_id", &id.to_string());
    }
    context.insert("category", category);
    // The parent choices, as [id, name] pairs for the select macro.
    let parent_options: Vec<(String, String)> = tags
        .iter()
        .map(|t| (t.id.to_string(), t.label.clone()))
        .collect();
    context.insert("parent_options", &parent_options);
    if !errors.is_empty() {
        context.insert("errors", errors);
    }
    context.insert("values", &values);
    context.insert("urls", urls);
    context.insert("path", &action);

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/tag-form.html",
        context,
    )
    .await
}

/// What the posted form held, to show it again.
fn posted_values(form: &TagFormData) -> serde_json::Value {
    serde_json::json!({
        "label": form.label,
        "description": form.description,
        "slug": form.slug,
        "weight": form.weight,
        "parent_id": form.parent_id,
    })
}

/// The empty add-term form for `category`.
pub(crate) async fn term_add_form(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    urls: &TermUrls,
) -> Response {
    render_term_form(
        state,
        session,
        category,
        None,
        serde_json::json!({}),
        &[],
        urls,
    )
    .await
}

/// Add a term to `category`.
pub(crate) async fn term_add_submit(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    form: TermForm,
    urls: &TermUrls,
) -> Response {
    if let Err(resp) = require_csrf(session, &form.token).await {
        return resp;
    }

    // Normalize slug: empty string → None, otherwise validate.
    let slug = form
        .slug
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(String::from);

    let mut errors = Vec::new();
    if form.label.trim().is_empty() {
        errors.push(FieldError::on("label", "Name is required."));
    }
    if let Some(ref s) = slug
        && !is_valid_slug(s)
    {
        errors.push(FieldError::on("slug", SLUG_FORMAT_ERROR));
    }
    // Check for duplicate slug within this category.
    if errors.is_empty()
        && let Some(ref s) = slug
        && let Ok(Some(_)) = crate::models::Tag::find_by_slug(state.db(), &category.id, s).await
    {
        errors.push(FieldError::on(
            "slug",
            "A tag with this slug already exists in this category.",
        ));
    }
    let parent_ids = match parent_of(state, category, form.parent_id.as_deref()).await {
        Ok(parents) => parents,
        Err(e) => {
            errors.push(FieldError::on("parent_id", e));
            None
        }
    };
    if !errors.is_empty() {
        return render_term_form(
            state,
            session,
            category,
            None,
            posted_values(&form),
            &errors,
            urls,
        )
        .await;
    }

    let input = CreateTag {
        category_id: category.id.clone(),
        label: form.label.clone(),
        description: form.description.clone(),
        slug,
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
        parent_ids,
    };

    match state.categories().create_tag(input).await {
        Ok(_) => {
            tracing::info!(category = %category.id, label = %form.label, "tag created");
            Redirect::to(&urls.list).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create tag");
            render_server_error("Failed to create tag.")
        }
    }
}

/// The parent a posted `parent_id` names: none for empty, else a term of the
/// same vocabulary. A term of another vocabulary is refused, so an owner's
/// manager cannot hang a term off another owner's tree.
async fn parent_of(
    state: &AppState,
    category: &crate::models::Category,
    parent_id: Option<&str>,
) -> Result<Option<Vec<uuid::Uuid>>, String> {
    let Some(id) = parent_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let Ok(id) = uuid::Uuid::parse_str(id) else {
        return Ok(None);
    };
    match state.categories().get_tag(id).await {
        Ok(Some(parent)) if parent.category_id == category.id => Ok(Some(vec![id])),
        _ => Err("The parent tag must be a tag of this category.".to_string()),
    }
}

/// The edit form for `tag`.
pub(crate) async fn term_edit_form(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    tag: &crate::models::Tag,
    urls: &TermUrls,
) -> Response {
    let parents = state
        .categories()
        .get_parents(tag.id)
        .await
        .unwrap_or_default();
    let current_parent_id = parents.first().map(|p| p.id.to_string());
    let values = serde_json::json!({
        "label": tag.label,
        "description": tag.description,
        "slug": tag.slug,
        "weight": tag.weight,
        "parent_id": current_parent_id,
    });
    render_term_form(state, session, category, Some(tag.id), values, &[], urls).await
}

/// Save `tag`.
pub(crate) async fn term_edit_submit(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    tag: &crate::models::Tag,
    form: TermForm,
    urls: &TermUrls,
) -> Response {
    if let Err(resp) = require_csrf(session, &form.token).await {
        return resp;
    }
    let tag_id = tag.id;

    // For update: empty string signals "clear the slug" to Tag::update,
    // None means "keep current value". Non-empty values are validated.
    let slug_value = form.slug.as_deref().unwrap_or("");

    let mut errors = Vec::new();
    if form.label.trim().is_empty() {
        errors.push(FieldError::on("label", "Name is required."));
    }
    if !slug_value.is_empty() && !is_valid_slug(slug_value) {
        errors.push(FieldError::on("slug", SLUG_FORMAT_ERROR));
    }
    // Check for duplicate slug within the same category (excluding this tag).
    if errors.is_empty()
        && !slug_value.is_empty()
        && let Ok(Some(existing)) =
            crate::models::Tag::find_by_slug(state.db(), &tag.category_id, slug_value).await
        && existing.id != tag_id
    {
        errors.push(FieldError::on(
            "slug",
            "A tag with this slug already exists in this category.",
        ));
    }
    let parent_ids = if category.hierarchy > 0 {
        match parent_of(state, category, form.parent_id.as_deref()).await {
            Ok(parents) => parents.unwrap_or_default(),
            Err(e) => {
                errors.push(FieldError::on("parent_id", e));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if parent_ids.contains(&tag_id) {
        errors.push(FieldError::on("parent_id", "A tag cannot be its own parent."));
    }
    if !errors.is_empty() {
        return render_term_form(
            state,
            session,
            category,
            Some(tag_id),
            posted_values(&form),
            &errors,
            urls,
        )
        .await;
    }

    // Update tag — None keeps current slug, Some("") clears it, Some(value) sets it.
    let input = UpdateTag {
        label: Some(form.label.clone()),
        description: form.description.clone(),
        slug: form.slug.clone(),
        weight: form.weight.as_ref().and_then(|s| s.parse().ok()),
    };

    if let Err(e) = state.categories().update_tag(tag_id, input).await {
        tracing::error!(error = %e, "failed to update tag");
        return render_server_error("Failed to update tag.");
    }

    // Update parent if hierarchy is enabled
    if category.hierarchy > 0
        && let Err(e) = state.categories().set_parents(tag_id, &parent_ids).await
    {
        tracing::error!(error = %e, "failed to update tag parents");
    }

    tracing::info!(tag_id = %tag_id, "tag updated");
    Redirect::to(&urls.list).into_response()
}

/// Delete `tag`.
pub(crate) async fn term_delete(
    state: &AppState,
    session: &Session,
    tag: &crate::models::Tag,
    token: &str,
    urls: &TermUrls,
) -> Response {
    if let Err(resp) = require_csrf(session, token).await {
        return resp;
    }
    match state.categories().delete_tag(tag.id).await {
        Ok(true) => {
            tracing::info!(tag_id = %tag.id, "tag deleted");
            Redirect::to(&urls.list).into_response()
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
