//! Category API routes.
//!
//! REST endpoints for managing categories and tags.

use crate::models::{CreateCategory, CreateTag, UpdateCategory, UpdateTag};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Create the category router.
pub fn router() -> Router<AppState> {
    Router::new()
        // Category routes
        .route("/api/categories", get(list_categories))
        .route("/api/category", post(create_category))
        .route("/api/category/{id}", get(get_category))
        .route("/api/category/{id}", put(update_category))
        .route("/api/category/{id}", delete(delete_category))
        .route("/api/category/{id}/tags", get(list_tags))
        .route("/api/category/{id}/roots", get(get_root_tags))
        // Tag routes
        .route("/api/tag", post(create_tag))
        .route("/api/tag/{id}", get(get_tag))
        .route("/api/tag/{id}", put(update_tag))
        .route("/api/tag/{id}", delete(delete_tag))
        .route("/api/tag/{id}/parents", get(get_parents))
        .route("/api/tag/{id}/parents", put(set_parents))
        .route("/api/tag/{id}/children", get(get_children))
        .route("/api/tag/{id}/ancestors", get(get_ancestors))
        .route("/api/tag/{id}/descendants", get(get_descendants))
        .route("/api/tag/{id}/breadcrumb", get(get_breadcrumb))
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

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
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
) -> Result<Json<Vec<CategoryResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let categories = state
        .categories()
        .list_categories()
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(Json(
        categories
            .into_iter()
            .map(|c| CategoryResponse {
                id: c.id,
                label: c.label,
                description: c.description,
                hierarchy: c.hierarchy,
                weight: c.weight,
            })
            .collect(),
    ))
}

async fn get_category(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<CategoryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let category = state
        .categories()
        .get_category(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "category not found".to_string(),
                }),
            )
        })?;

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
    Json(input): Json<CreateCategory>,
) -> Result<(StatusCode, Json<CategoryResponse>), (StatusCode, Json<ErrorResponse>)> {
    let category = state
        .categories()
        .create_category(input)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

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
    Path(id): Path<String>,
    Json(input): Json<UpdateCategory>,
) -> Result<Json<CategoryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let category = state
        .categories()
        .update_category(&id, input)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "category not found".to_string(),
                }),
            )
        })?;

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
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let deleted = state
        .categories()
        .delete_category(&id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "category not found".to_string(),
            }),
        ))
    }
}

// -------------------------------------------------------------------------
// Tag handlers
// -------------------------------------------------------------------------

async fn list_tags(
    State(state): State<AppState>,
    Path(category_id): Path<String>,
) -> Result<Json<Vec<TagResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let tags = state.categories().list_tags(&category_id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    Ok(Json(
        tags
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
            .collect(),
    ))
}

async fn get_root_tags(
    State(state): State<AppState>,
    Path(category_id): Path<String>,
) -> Result<Json<Vec<TagResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let tags = state
        .categories()
        .get_root_tags(&category_id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(Json(
        tags
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
            .collect(),
    ))
}

async fn get_tag(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TagResponse>, (StatusCode, Json<ErrorResponse>)> {
    let tag = state
        .categories()
        .get_tag(id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "tag not found".to_string(),
                }),
            )
        })?;

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
    Json(input): Json<CreateTag>,
) -> Result<(StatusCode, Json<TagResponse>), (StatusCode, Json<ErrorResponse>)> {
    let tag = state.categories().create_tag(input).await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

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
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateTag>,
) -> Result<Json<TagResponse>, (StatusCode, Json<ErrorResponse>)> {
    let tag = state
        .categories()
        .update_tag(id, input)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "tag not found".to_string(),
                }),
            )
        })?;

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
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let deleted = state.categories().delete_tag(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "tag not found".to_string(),
            }),
        ))
    }
}

// -------------------------------------------------------------------------
// Hierarchy handlers
// -------------------------------------------------------------------------

async fn get_parents(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TagResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let parents = state.categories().get_parents(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    Ok(Json(
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
            .collect(),
    ))
}

async fn set_parents(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(input): Json<SetParentsRequest>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    state
        .categories()
        .set_parents(id, &input.parent_ids)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn get_children(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TagResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let children = state.categories().get_children(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    Ok(Json(
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
            .collect(),
    ))
}

async fn get_ancestors(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TagWithDepthResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let ancestors = state.categories().get_ancestors(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    Ok(Json(
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
            .collect(),
    ))
}

async fn get_descendants(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TagWithDepthResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let descendants = state
        .categories()
        .get_descendants(id)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(Json(
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
            .collect(),
    ))
}

async fn get_breadcrumb(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<TagResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let breadcrumb = state.categories().get_breadcrumb(id).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: e.to_string(),
            }),
        )
    })?;

    Ok(Json(
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
            .collect(),
    ))
}
