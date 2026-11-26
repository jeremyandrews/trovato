//! Admin CRUD routes for the tile (block) subsystem.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::models::tile::{
    CreateTile, GATHER_TILE_DEFAULT_ITEMS, Tile, UpdateTile, gather_tile_items,
    parse_gather_tile_items,
};
use crate::state::AppState;

use super::helpers::{
    is_valid_machine_name, render_admin_template, render_error, render_not_found,
    render_server_error, require_csrf, require_permission,
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
    /// A `gather_query` tile's row count, as typed; blank for the default.
    pub items: Option<String>,
    #[serde(default)]
    pub weight: i32,
    pub status: Option<String>,
    /// "Show to" from the form: "" (everyone), "anonymous" or "authenticated".
    pub audience: Option<String>,
    /// Comma-separated content-type machine names (B186): blank for "every
    /// page", else only a page showing an item of one of these types.
    pub item_types: Option<String>,
}

/// The choices the tile form offers, by context name: value and English
/// label, translated by the select macro.
const TILE_CHOICES: &[(&str, &[(&str, &str)])] = &[
    (
        "region_options",
        &[
            ("sidebar", "Sidebar"),
            ("header", "Header"),
            ("footer", "Footer"),
            ("content_top", "Content top"),
            ("content_bottom", "Content bottom"),
        ],
    ),
    (
        "tile_type_options",
        &[
            ("custom_html", "Custom HTML"),
            ("menu", "Menu"),
            ("gather_query", "Gather query"),
            ("chat", "Chat"),
            ("language_switcher", "Language switcher"),
        ],
    ),
    (
        "format_options",
        &[
            ("filtered_html", "Filtered HTML"),
            ("full_html", "Full HTML"),
            ("plain_text", "Plain text"),
        ],
    ),
    (
        "audience_options",
        &[
            ("", "Everyone"),
            ("anonymous", "Anonymous visitors only"),
            ("authenticated", "Signed-in users only"),
        ],
    ),
];

/// Put the tile form's choices in `context` (see [`TILE_CHOICES`]).
pub(crate) fn insert_choices(context: &mut tera::Context) {
    for (key, options) in TILE_CHOICES {
        context.insert(*key, options);
    }
}

/// The `audience` values the tile form's select may post, besides "" for
/// "Everyone".
const AUDIENCE_VALUES: [&str; 2] = ["anonymous", "authenticated"];

/// Why an `audience` value was refused, worded for whoever set it.
const AUDIENCE_ERROR: &str =
    "Show to must be Everyone, Anonymous visitors only, or Signed-in users only.";

/// Why an `item_types` value was refused, worded for whoever set it.
const ITEM_TYPES_ERROR: &str =
    "Content types must be a comma-separated list of machine names (lowercase letters, numbers, and underscores).";

/// The content-type machine names typed into the form's `item_types` field:
/// comma-separated, blank entries dropped. Blank input is the empty list —
/// "every page" (B186, [`crate::models::tile::Tile::is_visible`]'s
/// `item_types` rule absent).
///
/// # Errors
///
/// [`ITEM_TYPES_ERROR`] when an entry is not a valid machine name.
fn parse_item_types(typed: &str) -> Result<Vec<String>, &'static str> {
    typed
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            is_valid_machine_name(s)
                .then(|| s.to_string())
                .ok_or(ITEM_TYPES_ERROR)
        })
        .collect()
}

impl TileFormData {
    /// Build the type-specific config JSON from form data.
    ///
    /// Fails with a message for the form's user when a value is out of range.
    fn build_config(&self) -> Result<serde_json::Value, &'static str> {
        Ok(match self.tile_type.as_str() {
            "custom_html" => serde_json::json!({
                "body": self.body.as_deref().unwrap_or(""),
                "format": self.format.as_deref().unwrap_or("filtered_html"),
            }),
            "menu" => serde_json::json!({
                "menu_name": self.menu_name.as_deref().unwrap_or("main"),
            }),
            "gather_query" => serde_json::json!({
                "query_id": self.query_id.as_deref().unwrap_or(""),
                "items": parse_gather_tile_items(self.items.as_deref().unwrap_or(""))?,
            }),
            "chat" => serde_json::json!({}),
            "language_switcher" => serde_json::json!({}),
            _ => serde_json::json!({}),
        })
    }

    /// `existing` (a tile's current `visibility`, `{}` for a new tile) with
    /// `audience` and `item_types` set from the form, or removed when left
    /// blank ("Everyone" / every page).
    ///
    /// Any other keys (`paths`, `paths_exclude`, `roles`) a config import may
    /// have set are left alone, so saving this form can never silently drop a
    /// visibility rule the form has no control for.
    fn merge_visibility(
        &self,
        existing: &serde_json::Value,
    ) -> Result<serde_json::Value, &'static str> {
        let audience = self.audience.as_deref().unwrap_or("").trim();
        if !audience.is_empty() && !AUDIENCE_VALUES.contains(&audience) {
            return Err(AUDIENCE_ERROR);
        }
        let item_types = parse_item_types(self.item_types.as_deref().unwrap_or(""))?;

        let mut value = existing.as_object().cloned().unwrap_or_default();
        if audience.is_empty() {
            value.remove("audience");
        } else {
            value.insert(
                "audience".to_string(),
                serde_json::Value::String(audience.to_string()),
            );
        }
        if item_types.is_empty() {
            value.remove("item_types");
        } else {
            value.insert(
                "item_types".to_string(),
                serde_json::Value::Array(
                    item_types.into_iter().map(serde_json::Value::String).collect(),
                ),
            );
        }
        Ok(serde_json::Value::Object(value))
    }
}

// -------------------------------------------------------------------------
// Handlers
// -------------------------------------------------------------------------

/// List all tiles.
///
/// GET /admin/structure/tiles
async fn list_tiles(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Tiles keep their render order (region, then weight), so the listing
    // is filtered and paged but has no sort.
    let query = super::admin_list::ListQuery::from_params(&params);
    let tiles: Vec<Tile> = Tile::list_all(state.db())
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|t| query.matches(&[&t.label, &t.machine_name, &t.tile_type, &t.region]))
        .collect();
    let total = i64::try_from(tiles.len()).unwrap_or(i64::MAX);
    let tiles = super::admin_list::page_of(&tiles, &query);
    let csrf_token = generate_csrf_token(&session).await;

    // Group tiles by region
    let mut by_region: std::collections::BTreeMap<String, Vec<&Tile>> =
        std::collections::BTreeMap::new();
    for tile in &tiles {
        by_region.entry(tile.region.clone()).or_default().push(tile);
    }

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, tiles.len(), None));
    context.insert("tiles", &tiles);
    context.insert("by_region", &by_region);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/structure/tiles");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/tiles.html",
        context,
    )
    .await
}

/// Show add tile form.
///
/// GET /admin/structure/tiles/add
async fn add_tile_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("action", "/admin/structure/tiles/add");
    context.insert("csrf_token", &csrf_token);
    context.insert("editing", &false);
    insert_choices(&mut context);
    context.insert(
        "values",
        &serde_json::json!({ "items": GATHER_TILE_DEFAULT_ITEMS, "audience": "", "item_types": "" }),
    );
    context.insert("path", "/admin/structure/tiles/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/tile-form.html",
        context,
    )
    .await
}

/// Handle add tile form submission.
///
/// POST /admin/structure/tiles/add
async fn add_tile_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<TileFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
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

    let config = match form.build_config() {
        Ok(config) => config,
        Err(message) => return render_error(message),
    };
    let visibility = match form.merge_visibility(&serde_json::json!({})) {
        Ok(visibility) => visibility,
        Err(message) => return render_error(message),
    };
    let input = CreateTile {
        machine_name: form.machine_name.clone(),
        label: form.label.clone(),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: Some(visibility),
        weight: Some(form.weight),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        plugin: None,
        stage_id: None,
    };

    match state.tiles().create(input).await {
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/structure/tiles/{tile_id}/edit"));
    context.insert("csrf_token", &csrf_token);
    context.insert("editing", &true);
    insert_choices(&mut context);
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
            "items": gather_tile_items(&tile.config).unwrap_or(GATHER_TILE_DEFAULT_ITEMS),
            "weight": tile.weight,
            "status": tile.status == 1,
            "audience": tile.visibility.get("audience").and_then(|v| v.as_str()).unwrap_or(""),
            "item_types": tile
                .visibility
                .get("item_types")
                .and_then(|v| v.as_array())
                .map(|types| {
                    types
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
        }),
    );
    context.insert("path", &format!("/admin/structure/tiles/{tile_id}/edit"));

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/tile-form.html",
        context,
    )
    .await
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };

    let config = match form.build_config() {
        Ok(config) => config,
        Err(message) => return render_error(message),
    };
    let visibility = match form.merge_visibility(&tile.visibility) {
        Ok(visibility) => visibility,
        Err(message) => return render_error(message),
    };
    let input = UpdateTile {
        label: Some(form.label.clone()),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: Some(visibility),
        weight: Some(form.weight),
        status: Some(if form.status.is_some() { 1 } else { 0 }),
        plugin: None,
        stage_id: None,
    };

    match state.tiles().update(tile_id, input).await {
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.tiles().delete(tile_id).await {
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
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}
