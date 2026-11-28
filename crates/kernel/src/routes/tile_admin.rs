//! Admin CRUD routes for the tile (block) subsystem.
//!
//! Every form here (add, edit, delete) is built and checked through
//! `FormService` (`routes::admin_form`).

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::models::User;
use crate::models::tile::{
    CreateTile, GATHER_TILE_DEFAULT_ITEMS, Tile, UpdateTile, gather_tile_items,
    parse_gather_tile_items,
};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{
    is_valid_machine_name, render_admin_template, render_error, render_not_found,
    render_server_error, require_permission,
};

/// The listing, and where every form here returns.
const BASE: &str = "/admin/structure/tiles";

/// The add and edit form.
const TILE_FORM_ID: &str = "tile_form";

/// The delete confirmation form.
const DELETE_FORM_ID: &str = "tile_delete_form";

// -------------------------------------------------------------------------
// Form data
// -------------------------------------------------------------------------

/// What the tile form shows or was sent: each field as typed, so a refused
/// submission is shown again exactly as it came.
#[derive(Debug, Clone, Default)]
pub struct TileFormData {
    /// The tile's machine name (fixed once the tile exists).
    pub machine_name: String,
    /// The tile's label.
    pub label: String,
    /// The region the tile is shown in.
    pub region: String,
    /// The kind of tile, which decides what its config holds.
    pub tile_type: String,
    /// A `custom_html` tile's body.
    pub body: String,
    /// A `custom_html` tile's text format.
    pub format: String,
    /// A `menu` tile's menu machine name.
    pub menu_name: String,
    /// A `gather_query` tile's query id.
    pub query_id: String,
    /// A `gather_query` tile's row count, as typed; blank for the default.
    pub items: String,
    /// The tile's weight in its region, as typed.
    pub weight: String,
    /// Whether the tile is active.
    pub status: bool,
    /// "Show to" from the form: "" (everyone), "anonymous" or "authenticated".
    pub audience: String,
    /// Comma-separated content-type machine names (B186): blank for "every
    /// page", else only a page showing an item of one of these types.
    pub item_types: String,
}

/// The choices the tile form offers, by name: value and English label,
/// translated by the select template.
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
            ("user_login", "User login"),
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

/// The options of the choice list `key` in [`TILE_CHOICES`], for a select.
fn choices(key: &str) -> Vec<(String, String)> {
    TILE_CHOICES
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, options)| {
            options
                .iter()
                .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// The `audience` values the tile form's select may post, besides "" for
/// "Everyone".
pub(crate) const AUDIENCE_VALUES: [&str; 2] = ["anonymous", "authenticated"];

/// Why an `audience` value was refused, worded for whoever set it.
const AUDIENCE_ERROR: &str =
    "Show to must be Everyone, Anonymous visitors only, or Signed-in users only.";

/// Why an `item_types` value was refused, worded for whoever set it.
const ITEM_TYPES_ERROR: &str = "Content types must be a comma-separated list of machine names (lowercase letters, numbers, and underscores).";

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
    /// The form's values for a new tile: an active custom HTML tile in the
    /// sidebar, the gather default row count.
    fn for_new_tile() -> Self {
        Self {
            region: "sidebar".to_string(),
            tile_type: "custom_html".to_string(),
            format: "filtered_html".to_string(),
            menu_name: "main".to_string(),
            items: GATHER_TILE_DEFAULT_ITEMS.to_string(),
            weight: "0".to_string(),
            status: true,
            ..Self::default()
        }
    }

    /// The form's values for `tile` as stored.
    fn from_tile(tile: &Tile) -> Self {
        let config = |key: &str, default: &str| {
            tile.config
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or(default)
                .to_string()
        };
        Self {
            machine_name: tile.machine_name.clone(),
            label: tile.label.clone(),
            region: tile.region.clone(),
            tile_type: tile.tile_type.clone(),
            body: config("body", ""),
            format: config("format", "filtered_html"),
            menu_name: config("menu_name", "main"),
            query_id: config("query_id", ""),
            items: gather_tile_items(&tile.config)
                .unwrap_or(GATHER_TILE_DEFAULT_ITEMS)
                .to_string(),
            weight: tile.weight.to_string(),
            status: tile.status == 1,
            audience: tile
                .visibility
                .get("audience")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            item_types: tile
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
        }
    }

    /// What the tile form posted.
    fn from_posted(posted: &PostedForm) -> Self {
        let value = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            machine_name: value("machine_name"),
            label: value("label"),
            region: value("region"),
            tile_type: value("tile_type"),
            body: value("body"),
            format: value("format"),
            menu_name: value("menu_name"),
            query_id: value("query_id"),
            items: value("items"),
            weight: value("weight"),
            status: posted.get("status").is_some(),
            audience: value("audience"),
            item_types: value("item_types"),
        }
    }

    /// The weight as typed: blank is 0.
    ///
    /// # Errors
    ///
    /// A message for the form's user when it is not a whole number.
    fn parsed_weight(&self) -> Result<i32, &'static str> {
        let typed = self.weight.trim();
        if typed.is_empty() {
            return Ok(0);
        }
        typed.parse().map_err(|_| "Weight must be a whole number.")
    }

    /// Build the type-specific config JSON from form data.
    ///
    /// Fails with a message for the form's user when a value is out of range.
    fn build_config(&self) -> Result<serde_json::Value, &'static str> {
        let or = |value: &str, default: &str| {
            if value.is_empty() {
                default.to_string()
            } else {
                value.to_string()
            }
        };
        Ok(match self.tile_type.as_str() {
            "custom_html" => serde_json::json!({
                "body": self.body,
                "format": or(&self.format, "filtered_html"),
            }),
            "menu" => serde_json::json!({
                "menu_name": or(&self.menu_name, "main"),
            }),
            "gather_query" => serde_json::json!({
                "query_id": self.query_id,
                "items": parse_gather_tile_items(&self.items)?,
            }),
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
    ) -> Result<serde_json::Value, ValidationError> {
        let audience = self.audience.trim();
        if !audience.is_empty() && !AUDIENCE_VALUES.contains(&audience) {
            return Err(ValidationError::field("audience", AUDIENCE_ERROR));
        }
        let item_types = parse_item_types(&self.item_types)
            .map_err(|message| ValidationError::field("item_types", message))?;

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
                    item_types
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }
        Ok(serde_json::Value::Object(value))
    }
}

// -------------------------------------------------------------------------
// Forms
// -------------------------------------------------------------------------

/// The opening of the group of fields one tile type has: admin-helpers.js
/// shows only the chosen type's group (`fields-<type>`); without JavaScript
/// every group is shown.
fn group_start(tile_type: &str) -> String {
    format!(r#"<div id="fields-{tile_type}" class="tile-type-fields">"#)
}

/// The close of a [`group_start`] group.
const GROUP_END: &str = "</div>";

/// A tile type whose only setting is a line of explanation: its group, the
/// words translated for this page.
fn description_group(tile_type: &str, html: &str) -> FormElement {
    FormElement::markup(format!(
        r#"{}<p class="description">{html}</p>{GROUP_END}"#,
        group_start(tile_type)
    ))
}

/// The add and edit form, showing `values`. `editing` is the tile being
/// edited, `None` for a new one.
async fn tile_form(
    state: &AppState,
    user: &User,
    editing: Option<Uuid>,
    values: &TileFormData,
) -> crate::form::Form {
    let action = match editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/add"),
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, &action).await;
    let mut machine_name = FormElement::textfield()
        .title("Machine name")
        .description("Lowercase letters, numbers, and underscores only")
        .default_value(values.machine_name.clone())
        .required()
        .attribute("pattern", "[a-z0-9_]+")
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
        .weight(0);
    if editing.is_some() {
        machine_name = machine_name.attribute("readonly", "readonly");
    }
    crate::form::Form::new(TILE_FORM_ID)
        .action(action)
        .element("machine_name", machine_name)
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .default_value(values.label.clone())
                .required()
                .weight(1),
        )
        .element(
            "region",
            FormElement::select(choices("region_options"))
                .title("Region")
                .default_value(values.region.clone())
                .required()
                .weight(2),
        )
        .element(
            "tile_type",
            FormElement::select(choices("tile_type_options"))
                .title("Type")
                .default_value(values.tile_type.clone())
                .required()
                .weight(3),
        )
        .element(
            "body",
            FormElement::textarea(8)
                .title("Body")
                .default_value(values.body.clone())
                .prefix(group_start("custom_html"))
                .weight(4),
        )
        .element(
            "format",
            FormElement::select(choices("format_options"))
                .title("Text format")
                .default_value(values.format.clone())
                .required()
                .suffix(GROUP_END)
                .weight(5),
        )
        .element(
            "menu_name",
            FormElement::textfield()
                .title("Menu")
                .description("The machine name of the menu to show, such as main.")
                .default_value(values.menu_name.clone())
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "none")
                .prefix(group_start("menu"))
                .suffix(GROUP_END)
                .weight(6),
        )
        .element(
            "query_id",
            FormElement::textfield()
                .title("Query ID")
                .description("The ID of the gather query to list, such as blog_listing.")
                .default_value(values.query_id.clone())
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "none")
                .prefix(group_start("gather_query"))
                .weight(7),
        )
        .element(
            "items",
            FormElement::input("number")
                .title("Items")
                .description("How many of the gather's rows the tile lists, from 1 to 50.")
                .default_value(values.items.clone())
                .attribute("min", "1")
                .attribute("max", "50")
                .attribute("step", "1")
                .suffix(GROUP_END)
                .weight(8),
        )
        .element(
            "chat_note",
            description_group(
                "chat",
                &format!(
                    r#"{} <a href="/admin/system/ai-chat">{}</a>"#,
                    text.html(
                        "The chat widget will be rendered in this region. Its settings are on the AI chat page.",
                        &[]
                    ),
                    text.html("Configure AI chat", &[])
                ),
            )
            .weight(9),
        )
        .element(
            "language_switcher_note",
            description_group(
                "language_switcher",
                &text.html(
                    "One link per enabled language, to this same page in that language. Hidden automatically on a site with only one language.",
                    &[],
                ),
            )
            .weight(10),
        )
        .element(
            "user_login_note",
            description_group(
                "user_login",
                &text.html(
                    "The login form, for visitors who are not signed in. It signs in through the login page and returns to the page the visitor was on. Not shown to signed-in users or on the login page.",
                    &[],
                ),
            )
            .weight(11),
        )
        .element(
            "audience",
            FormElement::select(choices("audience_options"))
                .title("Show to")
                .description("Who may see this tile, regardless of its other visibility rules.")
                .default_value(values.audience.clone())
                .required()
                .weight(12),
        )
        .element(
            "item_types",
            FormElement::textfield()
                .title("Content types")
                .description(
                    "Show only on a page showing an item of one of these types, such as \
                     article, recipe. Blank for every page.",
                )
                .default_value(values.item_types.clone())
                .attribute("spellcheck", "false")
                .attribute("autocapitalize", "none")
                .weight(13),
        )
        .element(
            "weight",
            FormElement::input("number")
                .title("Weight")
                .default_value(values.weight.clone())
                .weight(14),
        )
        .element(
            "status",
            FormElement::checkbox()
                .title("Active")
                .default_value(values.status)
                .weight(15),
        )
        .element(
            "op",
            FormElement::submit("Save")
                .suffix(format!(
                    r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
                    text.html("Cancel", &[])
                ))
                .weight(20),
        )
}

/// Show the tile form with `values` and `errors`.
async fn render_tile_form(
    state: &AppState,
    session: &Session,
    user: &User,
    editing: Option<Uuid>,
    values: &TileFormData,
    errors: &[ValidationError],
) -> Response {
    let form = tile_form(state, user, editing, values).await;
    let path = form.action.clone();
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("editing", &editing.is_some());
    context.insert("label", &values.label);
    context.insert("path", &path);
    render_admin_template(state, &viewer, "admin/tile-form.html", context).await
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
    context.insert("path", BASE);

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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    render_tile_form(
        &state,
        &session,
        &user,
        None,
        &TileFormData::for_new_tile(),
        &[],
    )
    .await
}

/// The errors in what the tile form posted, with the config, visibility and
/// weight to save when there are none. `existing` is the tile's stored
/// visibility (`{}` for a new tile); `new_tile` is whether the machine name
/// is being set.
fn read_tile(
    form: &TileFormData,
    existing: &serde_json::Value,
    new_tile: bool,
) -> Result<(serde_json::Value, serde_json::Value, i32), Vec<ValidationError>> {
    let mut errors = Vec::new();
    if new_tile && !is_valid_machine_name(&form.machine_name) {
        errors.push(ValidationError::field(
            "machine_name",
            "Invalid machine name. Use only lowercase letters, numbers, and underscores.",
        ));
    }
    let config = form
        .build_config()
        .map_err(|message| errors.push(ValidationError::field("items", message)))
        .ok();
    let visibility = form
        .merge_visibility(existing)
        .map_err(|error| errors.push(error))
        .ok();
    let weight = form
        .parsed_weight()
        .map_err(|message| errors.push(ValidationError::field("weight", message)))
        .ok();
    match (config, visibility, weight) {
        (Some(config), Some(visibility), Some(weight)) if errors.is_empty() => {
            Ok((config, visibility, weight))
        }
        _ => Err(errors),
    }
}

/// Handle add tile form submission.
///
/// POST /admin/structure/tiles/add
async fn add_tile_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form = TileFormData::from_posted(&posted);
    let read = read_tile(&form, &serde_json::json!({}), true);
    let errors = read.as_ref().err().cloned().unwrap_or_default();
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, TILE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_tile_form(&state, &session, &user, None, &form, &errors).await;
            }
            Err(Err(response)) => return response,
        };
    let Ok((config, visibility, weight)) = read else {
        return render_not_found();
    };
    let input = CreateTile {
        machine_name: form.machine_name.clone(),
        label: form.label.clone(),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: Some(visibility),
        weight: Some(weight),
        status: Some(i32::from(form.status)),
        plugin: None,
        stage_id: None,
    };

    match state.tiles().create(input).await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };
    render_tile_form(
        &state,
        &session,
        &user,
        Some(tile_id),
        &TileFormData::from_tile(&tile),
        &[],
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };
    let mut form = TileFormData::from_posted(&posted);
    // The machine name is fixed once the tile exists: show the stored one.
    form.machine_name = tile.machine_name.clone();
    let read = read_tile(&form, &tile.visibility, false);
    let errors = read.as_ref().err().cloned().unwrap_or_default();
    let form_state = match admin_form::check(&state, &session, &user, &posted, TILE_FORM_ID, errors)
        .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_tile_form(&state, &session, &user, Some(tile_id), &form, &errors).await;
        }
        Err(Err(response)) => return response,
    };
    let Ok((config, visibility, weight)) = read else {
        return render_not_found();
    };
    let input = UpdateTile {
        label: Some(form.label.clone()),
        region: Some(form.region.clone()),
        tile_type: Some(form.tile_type.clone()),
        config: Some(config),
        visibility: Some(visibility),
        weight: Some(weight),
        status: Some(i32::from(form.status)),
        plugin: None,
        stage_id: None,
    };

    match state.tiles().update(tile_id, input).await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to update tile");
            render_server_error("Failed to update tile.")
        }
    }
}

/// The delete confirmation form for the tile `tile_id`.
fn delete_form_for(tile_id: Uuid) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{tile_id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .weight(10),
        )
}

/// Ask before deleting a tile.
///
/// GET /admin/structure/tiles/{id}/delete
async fn delete_tile_form(
    State(state): State<AppState>,
    session: Session,
    Path(tile_id): Path<Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(tile) = Tile::find_by_id(state.db(), tile_id).await.ok().flatten() else {
        return render_not_found();
    };
    let form_html =
        match admin_form::render(&state, &session, &user, delete_form_for(tile_id), &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("label", &tile.label);
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{tile_id}/delete"));
    render_admin_template(&state, &viewer, "admin/tile-delete.html", context).await
}

/// Delete a tile.
///
/// POST /admin/structure/tiles/{id}/delete
async fn delete_tile(
    State(state): State<AppState>,
    session: Session,
    Path(tile_id): Path<Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };

    match state.tiles().delete(tile_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            Redirect::to(BASE).into_response()
        }
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
        .route(BASE, get(list_tiles))
        .route(
            "/admin/structure/tiles/add",
            get(add_tile_form).post(add_tile_submit),
        )
        .route(
            "/admin/structure/tiles/{id}/edit",
            get(edit_tile_form).post(edit_tile_submit),
        )
        .route(
            "/admin/structure/tiles/{id}/delete",
            get(delete_tile_form).post(delete_tile),
        )
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}
