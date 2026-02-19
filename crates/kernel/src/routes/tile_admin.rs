//! Admin CRUD routes for the tile (block) subsystem.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::models::tile::{CreateTile, Tile, UpdateTile};
use crate::state::AppState;

use super::helpers::{
    is_valid_machine_name, render_admin_template, render_error, render_not_found,
    render_server_error, require_admin, require_csrf,
};

// -------------------------------------------------------------------------
// Form data
// -------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TileFormData {
    #[serde(rename = "_token")]
    pub token: String,
    pub machine_name: String,
    pub label: String,
    pub region: String,
    pub tile_type: String,
    pub body: Option<String>,
    pub format: Option<String>,
    pub menu_name: Option<String>,
    pub query_id: Option<String>,
    #[serde(default)]
    pub weight: i32,
    pub status: Option<String>,
}

impl TileFormData {
    /// Build the type-specific config JSON from form data.
    fn build_config(&self) -> serde_json::Value {
        match self.tile_type.as_str() {
            "custom_html" => serde_json::json!({
                "body": self.body.as_deref().unwrap_or(""),
                "format": self.format.as_deref().unwrap_or("filtered_html"),
            }),
            "menu" => serde_json::json!({
                "menu_name": self.menu_name.as_deref().unwrap_or("main"),
            }),
            "gather_query" => serde_json::json!({
                "query_id": self.query_id.as_deref().unwrap_or(""),
            }),
            _ => serde_json::json!({}),
        }
    }
}

// -------------------------------------------------------------------------
// Handlers
// -------------------------------------------------------------------------

/// List all tiles.
///
/// GET /admin/structure/tiles
async fn list_tiles(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let tiles = Tile::list_all(state.db()).await.unwrap_or_default();
    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    // Group tiles by region
    let mut by_region: std::collections::BTreeMap<String, Vec<&Tile>> =
        std::collections::BTreeMap::new();
    for tile in &tiles {
        by_region.entry(tile.region.clone()).or_default().push(tile);
    }

    let mut context = tera::Context::new();
    context.insert("tiles", &tiles);
    context.insert("by_region", &by_region);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/tiles");

    render_admin_template(&state, "admin/tiles.html", context).await
}

/// Show add tile form.
///
/// GET /admin/structure/tiles/add
async fn add_tile_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/tiles/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("editing", &false);
    context.insert("values", &serde_json::json!({}));
    context.insert("path", "/admin/structure/tiles/add");

    render_admin_template(&state, "admin/tile-form.html", context).await
}

/// Handle add tile form submission.
///
/// POST /admin/structure/tiles/add
async fn add_tile_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<TileFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    if !is_valid_machine_name(&form.machine_name) {
        return render_error(
            "Invalid machine name. Use only lowercase letters, numbers, and underscores.",
        );
    }

    let config = form.build_config();
    let input = CreateTile {
        machine_name: form.machine_name.clone(),
        label: form.label.clone(),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: None,
        weight: Some(form.weight),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        plugin: None,
        stage_id: None,
    };

    match Tile::create(state.db(), input).await {
        Ok(_) => Redirect::to("/admin/structure/tiles").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to create tile");
            render_server_error("Failed to create tile.")
        }
    }
}

/// Show edit tile form.
///
/// GET /admin/structure/tiles/{id}/edit
async fn edit_tile_form(
    State(state): State<AppState>,
    session: Session,
    Path(tile_id): Path<Uuid>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await.unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert(
        "action",
        &format!("/admin/structure/tiles/{}/edit", tile_id),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("editing", &true);
    context.insert("tile_id", &tile_id.to_string());
    context.insert(
        "values",
        &serde_json::json!({
            "machine_name": tile.machine_name,
            "label": tile.label,
            "region": tile.region,
            "tile_type": tile.tile_type,
            "body": tile.config.get("body").and_then(|v| v.as_str()).unwrap_or(""),
            "format": tile.config.get("format").and_then(|v| v.as_str()).unwrap_or("filtered_html"),
            "menu_name": tile.config.get("menu_name").and_then(|v| v.as_str()).unwrap_or("main"),
            "query_id": tile.config.get("query_id").and_then(|v| v.as_str()).unwrap_or(""),
            "weight": tile.weight,
            "status": tile.status == 1,
        }),
    );
    context.insert("path", &format!("/admin/structure/tiles/{}/edit", tile_id));

    render_admin_template(&state, "admin/tile-form.html", context).await
}

/// Handle edit tile form submission.
///
/// POST /admin/structure/tiles/{id}/edit
async fn edit_tile_submit(
    State(state): State<AppState>,
    session: Session,
    Path(tile_id): Path<Uuid>,
    Form(form): Form<TileFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let config = form.build_config();
    let input = UpdateTile {
        label: Some(form.label.clone()),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: None,
        weight: Some(form.weight),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        plugin: None,
        stage_id: None,
    };

    match Tile::update(state.db(), tile_id, input).await {
        Ok(_) => Redirect::to("/admin/structure/tiles").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update tile");
            render_server_error("Failed to update tile.")
        }
    }
}

/// Form data for tile deletion (CSRF only).
#[derive(Debug, Deserialize)]
pub struct TileDeleteData {
    #[serde(rename = "_token")]
    pub token: String,
}

/// Delete a tile.
///
/// POST /admin/structure/tiles/{id}/delete
async fn delete_tile(
    State(state): State<AppState>,
    session: Session,
    Path(tile_id): Path<Uuid>,
    Form(form): Form<TileDeleteData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match Tile::delete(state.db(), tile_id).await {
        Ok(true) => Redirect::to("/admin/structure/tiles").into_response(),
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete tile");
            render_server_error("Failed to delete tile.")
        }
    }
}

/// Create the tile admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/tiles", get(list_tiles))
        .route(
            "/admin/structure/tiles/add",
            get(add_tile_form).post(add_tile_submit),
        )
        .route(
            "/admin/structure/tiles/{id}/edit",
            get(edit_tile_form).post(edit_tile_submit),
        )
        .route("/admin/structure/tiles/{id}/delete", post(delete_tile))
}
