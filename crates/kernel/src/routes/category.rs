//! Category API routes.
//!
//! REST endpoints for managing categories and tags, plus an HTML browse page
//! for the "topics" category that renders tags as a link grid.

use crate::error::AppError;
use crate::models::{CreateCategory, CreateTag, UpdateCategory, UpdateTag};
use crate::pagination::{Page, PageParams};
use crate::routes::helpers::{inject_site_context, require_csrf_header, require_permission_json};
use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, Json},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

/// Permission required to mutate the taxonomy (create/update/delete/reparent
/// categories and tags).
const ADMINISTER_CATEGORIES: &str = "administer categories";

/// Default page size of a category or tag list.
const LIST_DEFAULT: i64 = 100;

/// Largest page size of a category or tag list.
const LIST_MAX: i64 = 500;

/// Authorize a state-changing taxonomy request: the session user must hold
/// `administer categories` (admins implicitly do) **and** present a valid CSRF
/// token in the `X-CSRF-Token` header.
///
/// This closes AC-W1/CSRF-1 (the category & tag mutation API shipped fully
/// unauthenticated, no permission, no CSRF). All seven write handlers call it
/// before touching the service, mirroring the item-mutation pattern
/// (`item.rs` `create_item`).
async fn authorize_category_mutation(
    state: &AppState,
    session: &Session,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    require_permission_json(state, session, ADMINISTER_CATEGORIES).await?;
    require_csrf_header(session, headers).await?;
    Ok(())
}

/// Create the category router.
pub fn router() -> Router<AppState> {
    Router::new()
        // HTML browse page for topics
        .route("/topics", get(browse_topics))
}

/// The category and tag JSON API, under `/api/v1` ([`super::api`]), served
/// while `trovato_categories` is enabled. Writes take `administer categories`
/// and the `X-CSRF-Token` header.
pub fn api() -> super::api::ApiRoutes {
    use super::api::doc;
    let list = |summary: &str| doc(summary).paged(LIST_DEFAULT, LIST_MAX).alias();
    super::api::ApiRoutes::new("categories")
        .get("/categories", list_categories, list("List categories"))
        .post(
            "/category",
            create_category,
            doc("Create a category").alias(),
        )
        .get(
            "/category/{id}",
            get_category,
            doc("Get a category").alias(),
        )
        .put(
            "/category/{id}",
            update_category,
            doc("Update a category").alias(),
        )
        .delete(
            "/category/{id}",
            delete_category,
            doc("Delete a category").alias(),
        )
        .get(
            "/category/{id}/tags",
            list_tags,
            list("List a category's tags"),
        )
        .get(
            "/category/{id}/roots",
            get_root_tags,
            list("List a category's top-level tags"),
        )
        .post("/tag", create_tag, doc("Create a tag").alias())
        .get("/tag/{id}", get_tag, doc("Get a tag").alias())
        .put("/tag/{id}", update_tag, doc("Update a tag").alias())
        .delete("/tag/{id}", delete_tag, doc("Delete a tag").alias())
        .get(
            "/tag/{id}/parents",
            get_parents,
            list("List a tag's parents"),
        )
        .put(
            "/tag/{id}/parents",
            set_parents,
            doc("Set a tag's parents").alias(),
        )
        .get(
            "/tag/{id}/children",
            get_children,
            list("List a tag's children"),
        )
        .get(
            "/tag/{id}/ancestors",
            get_ancestors,
            list("List a tag's ancestors, with depth"),
        )
        .get(
            "/tag/{id}/descendants",
            get_descendants,
            list("List a tag's descendants, with depth"),
        )
        .get(
            "/tag/{id}/breadcrumb",
            get_breadcrumb,
            list("A tag's breadcrumb, root first"),
        )
        .plugin("trovato_categories")
}

// -------------------------------------------------------------------------
// Response types
// -------------------------------------------------------------------------

#[derive(Serialize)]
struct CategoryResponse {
    id: String,
    label: String,
    description: Option<String>,
    hierarchy: i16,
    weight: i16,
}

#[derive(Serialize)]
struct TagResponse {
    id: Uuid,
    category_id: String,
    label: String,
    description: Option<String>,
    weight: i16,
    created: i64,
    changed: i64,
}

#[derive(Serialize)]
struct TagWithDepthResponse {
    tag: TagResponse,
    depth: i32,
}

// -------------------------------------------------------------------------
// Request types
// -------------------------------------------------------------------------

#[derive(Deserialize)]
struct SetParentsRequest {
    parent_ids: Vec<Uuid>,
}

// -------------------------------------------------------------------------
// Category handlers
// -------------------------------------------------------------------------

async fn list_categories(
    State(state): State<AppState>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<CategoryResponse>>, AppError> {
    let categories = state
        .categories()
        .list_categories()
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        categories
            .into_iter()
            .map(|c| CategoryResponse {
                id: c.id,
                label: c.label,
                description: c.description,
                hierarchy: c.hierarchy,
                weight: c.weight,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<CategoryResponse>, AppError> {
    let category = state
        .categories()
        .get_category(&id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "category not found"))?;

    Ok(Json(CategoryResponse {
        id: category.id,
        label: category.label,
        description: category.description,
        hierarchy: category.hierarchy,
        weight: category.weight,
    }))
}

async fn create_category(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(input): Json<CreateCategory>,
) -> Result<(StatusCode, Json<CategoryResponse>), AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let category = state
        .categories()
        .create_category(input)
        .await
        .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(CategoryResponse {
            id: category.id,
            label: category.label,
            description: category.description,
            hierarchy: category.hierarchy,
            weight: category.weight,
        }),
    ))
}

async fn update_category(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<UpdateCategory>,
) -> Result<Json<CategoryResponse>, AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let category = state
        .categories()
        .update_category(&id, input)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "category not found"))?;

    Ok(Json(CategoryResponse {
        id: category.id,
        label: category.label,
        description: category.description,
        hierarchy: category.hierarchy,
        weight: category.weight,
    }))
}

async fn delete_category(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let deleted = state
        .categories()
        .delete_category(&id)
        .await
        .map_err(AppError::internal)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::with_status(
            StatusCode::NOT_FOUND,
            "category not found",
        ))
    }
}

// -------------------------------------------------------------------------
// Tag handlers
// -------------------------------------------------------------------------

async fn list_tags(
    State(state): State<AppState>,
    Path(category_id): Path<String>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagResponse>>, AppError> {
    let tags = state
        .categories()
        .list_tags(&category_id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        tags.into_iter()
            .map(|t| TagResponse {
                id: t.id,
                category_id: t.category_id,
                label: t.label,
                description: t.description,
                weight: t.weight,
                created: t.created,
                changed: t.changed,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_root_tags(
    State(state): State<AppState>,
    Path(category_id): Path<String>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagResponse>>, AppError> {
    let tags = state
        .categories()
        .get_root_tags(&category_id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        tags.into_iter()
            .map(|t| TagResponse {
                id: t.id,
                category_id: t.category_id,
                label: t.label,
                description: t.description,
                weight: t.weight,
                created: t.created,
                changed: t.changed,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_tag(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TagResponse>, AppError> {
    let tag = state
        .categories()
        .get_tag(id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "tag not found"))?;

    Ok(Json(TagResponse {
        id: tag.id,
        category_id: tag.category_id,
        label: tag.label,
        description: tag.description,
        weight: tag.weight,
        created: tag.created,
        changed: tag.changed,
    }))
}

async fn create_tag(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(input): Json<CreateTag>,
) -> Result<(StatusCode, Json<TagResponse>), AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let tag = state
        .categories()
        .create_tag(input)
        .await
        .map_err(|e| AppError::with_status(StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(TagResponse {
            id: tag.id,
            category_id: tag.category_id,
            label: tag.label,
            description: tag.description,
            weight: tag.weight,
            created: tag.created,
            changed: tag.changed,
        }),
    ))
}

async fn update_tag(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateTag>,
) -> Result<Json<TagResponse>, AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let tag = state
        .categories()
        .update_tag(id, input)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::with_status(StatusCode::NOT_FOUND, "tag not found"))?;

    Ok(Json(TagResponse {
        id: tag.id,
        category_id: tag.category_id,
        label: tag.label,
        description: tag.description,
        weight: tag.weight,
        created: tag.created,
        changed: tag.changed,
    }))
}

async fn delete_tag(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    let deleted = state
        .categories()
        .delete_tag(id)
        .await
        .map_err(AppError::internal)?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::with_status(
            StatusCode::NOT_FOUND,
            "tag not found",
        ))
    }
}

// -------------------------------------------------------------------------
// Hierarchy handlers
// -------------------------------------------------------------------------

async fn get_parents(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagResponse>>, AppError> {
    let parents = state
        .categories()
        .get_parents(id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        parents
            .into_iter()
            .map(|t| TagResponse {
                id: t.id,
                category_id: t.category_id,
                label: t.label,
                description: t.description,
                weight: t.weight,
                created: t.created,
                changed: t.changed,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn set_parents(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<SetParentsRequest>,
) -> Result<StatusCode, AppError> {
    authorize_category_mutation(&state, &session, &headers).await?;

    state
        .categories()
        .set_parents(id, &input.parent_ids)
        .await
        .map_err(AppError::internal)?;

    Ok(StatusCode::NO_CONTENT)
}

async fn get_children(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagResponse>>, AppError> {
    let children = state
        .categories()
        .get_children(id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        children
            .into_iter()
            .map(|t| TagResponse {
                id: t.id,
                category_id: t.category_id,
                label: t.label,
                description: t.description,
                weight: t.weight,
                created: t.created,
                changed: t.changed,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_ancestors(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagWithDepthResponse>>, AppError> {
    let ancestors = state
        .categories()
        .get_ancestors(id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        ancestors
            .into_iter()
            .map(|a| TagWithDepthResponse {
                tag: TagResponse {
                    id: a.tag.id,
                    category_id: a.tag.category_id,
                    label: a.tag.label,
                    description: a.tag.description,
                    weight: a.tag.weight,
                    created: a.tag.created,
                    changed: a.tag.changed,
                },
                depth: a.depth,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_descendants(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagWithDepthResponse>>, AppError> {
    let descendants = state
        .categories()
        .get_descendants(id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        descendants
            .into_iter()
            .map(|d| TagWithDepthResponse {
                tag: TagResponse {
                    id: d.tag.id,
                    category_id: d.tag.category_id,
                    label: d.tag.label,
                    description: d.tag.description,
                    weight: d.tag.weight,
                    created: d.tag.created,
                    changed: d.tag.changed,
                },
                depth: d.depth,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

async fn get_breadcrumb(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(paging): Query<PageParams>,
) -> Result<Json<Page<TagResponse>>, AppError> {
    let breadcrumb = state
        .categories()
        .get_breadcrumb(id)
        .await
        .map_err(AppError::internal)?;

    Ok(Json(Page::slice(
        breadcrumb
            .into_iter()
            .map(|t| TagResponse {
                id: t.id,
                category_id: t.category_id,
                label: t.label,
                description: t.description,
                weight: t.weight,
                created: t.created,
                changed: t.changed,
            })
            .collect::<Vec<_>>(),
        paging.window(LIST_DEFAULT, LIST_MAX),
    )))
}

// -------------------------------------------------------------------------
// HTML browse page
// -------------------------------------------------------------------------

/// Render a browsable topics index page.
///
/// Lists all tags in the "topics" category as a link grid pointing to
/// `/topics/{slug}`.
async fn browse_topics(
    State(state): State<AppState>,
    session: Session,
) -> axum::response::Response {
    let mut tags = state
        .categories()
        .list_tags("topics")
        .await
        .unwrap_or_default();
    // Re-sort case-insensitively for the browse page. list_tags returns
    // weight-based order; alphabetical is more intuitive for a directory.
    tags.sort_by_key(|a| a.label.to_lowercase());

    let mut content = String::from("<div class=\"topics-grid\">");
    for tag in &tags {
        let slug = tag.slug.as_deref().unwrap_or_default();
        if slug.is_empty() {
            continue;
        }
        let label = crate::routes::helpers::html_escape(&tag.label);
        // Infallible: writing to String never fails
        use std::fmt::Write;
        // Infallible: write! to String never fails
        #[allow(clippy::unwrap_used)]
        write!(
            content,
            "<a href=\"/topics/{slug}\" class=\"topic-chip\">{label}</a>"
        )
        .unwrap();
    }
    content.push_str("</div>");

    let mut context = tera::Context::new();
    inject_site_context(&state, &session, &mut context, "/topics").await;

    match state
        .theme()
        .render_page("/topics", "Topics", &content, &mut context)
    {
        Ok(html) => axum::response::IntoResponse::into_response(Html(html)),
        Err(e) => {
            tracing::error!(error = %e, "failed to render the topics page");
            crate::routes::helpers::render_server_error("The topics page could not be displayed.")
        }
    }
}
