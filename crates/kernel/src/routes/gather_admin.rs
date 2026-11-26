//! Admin routes for gather query management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::gather::{
    GatherQuery, GatherService, MAX_ITEMS_PER_PAGE, QueryDefinition, QueryDisplay,
};
use crate::i18n::UiMessage;
use crate::routes::admin_form::FieldError;
use crate::state::AppState;

use super::admin_list::{ListQuery, page_of, sort_rows};
use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_csrf,
    require_permission,
};

// =============================================================================
// What the builder can show
// =============================================================================

/// The base tables the builder offers: table name and English label,
/// translated by the select macro.
const BASE_TABLE_OPTIONS: &[(&str, &str)] = &[
    ("item", "Content"),
    ("users", "Users"),
    ("url_alias", "URL aliases"),
    ("role", "Roles"),
    ("gather_query", "Gather queries"),
];

/// The display formats the builder offers. "List of items", not "List": the
/// word alone is also a verb, and a translator could not tell which.
const DISPLAY_FORMAT_OPTIONS: &[(&str, &str)] = &[
    ("table", "Table"),
    ("unformatted", "Unformatted"),
    ("list", "List of items"),
    ("grid", "Grid"),
];

/// Why the builder will not save a definition it cannot show.
const BUILDER_CANNOT_SHOW: &str = "This gather uses filter groups or a summary \
    (group_by, aggregates, having, distinct), which this builder cannot show. Saving \
    it here would lose them, so it is not saved. Change it where it is defined: its \
    config file or the plugin that installs it.";

// -------------------------------------------------------------------------
// Row-builder words (B253)
// -------------------------------------------------------------------------
//
// `static/js/gather-builder.js` builds the filter, sort, relationship and
// include rows at runtime: an option list, a placeholder, an aria-label, a
// button, a preview message. None of those words live in the script as a
// literal; each is English here, named the same as the script reads it from
// the `gather-i18n` data block `templates/admin/gather-form.html` renders,
// and `t()` translates when the template builds that block (the mechanism
// `templates/admin/ai-provider-form.html`'s `ai-operations` block already
// uses for `static/js/ai-admin.js`).

/// The filter operators the builder offers, value and English label.
const BUILDER_OPERATORS: &[(&str, &str)] = &[
    ("equals", "Equals"),
    ("not_equals", "Not equals"),
    ("contains", "Contains"),
    ("not_contains", "Not contains"),
    ("greater_than", "Greater than"),
    ("less_than", "Less than"),
    ("in", "In"),
    ("not_in", "Not in"),
    ("is_null", "Is null"),
    ("is_not_null", "Is not null"),
    ("full_text_search", "Full-text search"),
];

/// The relationship join types the builder offers.
const BUILDER_JOIN_TYPES: &[(&str, &str)] = &[
    ("inner", "Inner join"),
    ("left", "Left join"),
    ("right", "Right join"),
];

/// The sort directions the builder offers.
const BUILDER_SORT_DIRECTIONS: &[(&str, &str)] = &[("asc", "Ascending"), ("desc", "Descending")];

/// Every other word the builder's script puts on the page. `{name}`
/// placeholders in a value are filled by the script itself at the point it
/// knows the number (a status code, a count), the same convention `t()`
/// placeholders use everywhere else.
const BUILDER_STRINGS: &[(&str, &str)] = &[
    ("field_name", "Field name"),
    ("filter_field_name", "Filter field name"),
    ("filter_operator", "Filter operator"),
    ("value", "Value"),
    ("filter_value", "Filter value"),
    ("remove", "Remove"),
    ("remove_filter", "Remove filter"),
    ("sort_field_name", "Sort field name"),
    ("sort_direction", "Sort direction"),
    ("remove_sort", "Remove sort"),
    ("join_type", "Join type"),
    ("target_table", "Target table"),
    ("local_field", "Local field"),
    ("foreign_field", "Foreign field"),
    ("name_alias", "Name (alias)"),
    ("relationship_name", "Relationship name"),
    ("remove_relationship", "Remove relationship"),
    (
        "max_relationships_reached",
        "Maximum of {max} relationships reached",
    ),
    ("include_name", "Include name"),
    ("parent_field_example", "Parent field (e.g., id)"),
    ("parent_field", "Parent field"),
    ("child_field_example", "Child field (e.g., fields.story_id)"),
    ("child_field", "Child field"),
    ("child_base_table", "Child base table"),
    ("child_item_type_optional", "Child item type (optional)"),
    ("child_item_type", "Child item type"),
    ("singular_label", "Singular (single object, not array)"),
    ("limit", "Limit (default: 1000)"),
    ("include_limit", "Include limit"),
    ("remove_include", "Remove include"),
    ("loading", "Loading..."),
    ("loading_preview", "Loading preview..."),
    ("load_preview", "Load preview"),
    ("invalid_response", "Invalid response from server."),
    ("preview_failed", "Preview failed ({status})"),
    (
        "network_error",
        "Network error. Please check your connection.",
    ),
    ("no_results_found", "No results found."),
    ("showing_results", "Showing {shown} of {total} results."),
];

/// Insert the row-builder option lists into a gather form render's context,
/// alongside `base_table_options` and `display_format_options`: every render
/// of `admin/gather-form.html` needs them, success or redisplayed-with-errors
/// alike.
fn insert_builder_words(context: &mut tera::Context) {
    context.insert("builder_operators", BUILDER_OPERATORS);
    context.insert("builder_join_types", BUILDER_JOIN_TYPES);
    context.insert("builder_sort_directions", BUILDER_SORT_DIRECTIONS);
    context.insert("builder_strings", BUILDER_STRINGS);
}

/// Whether a definition uses a shape the builder has no control for. The
/// builder rebuilds the definition from its controls on save, so it would
/// drop such a shape; it refuses instead.
fn builder_cannot_show(definition: &QueryDefinition) -> bool {
    !definition.filter_groups.is_empty() || definition.aggregation.is_set()
}

// =============================================================================
// Form data
// =============================================================================

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

/// The gather listing's sortable columns. The list is sorted in memory, so
/// the second value is only the key's name.
const GATHER_SORTS: &[(&str, &str)] = &[
    ("label", "label"),
    ("id", "id"),
    ("table", "table"),
    ("plugin", "plugin"),
];

/// List all gather queries.
///
/// GET /admin/gather
async fn list_queries(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // A small list held in memory: filtered, sorted and paged here.
    let query = ListQuery::from_params(&params);
    let sort = query.sort(GATHER_SORTS, ("label", false));
    let mut queries: Vec<GatherQuery> = state
        .gather()
        .list_queries()
        .into_iter()
        .filter(|g| query.matches(&[&g.label, &g.query_id, &g.definition.base_table, &g.plugin]))
        .collect();
    // Ties (and the unsorted map's order) settle on the id.
    queries.sort_by(|a, b| a.query_id.cmp(&b.query_id));
    sort_rows(
        &mut queries,
        &sort,
        &[
            ("label", |g: &GatherQuery| g.label.clone()),
            ("id", |g: &GatherQuery| g.query_id.clone()),
            ("table", |g: &GatherQuery| g.definition.base_table.clone()),
            ("plugin", |g: &GatherQuery| g.plugin.clone()),
        ],
    );
    let total = i64::try_from(queries.len()).unwrap_or(i64::MAX);
    let queries = page_of(&queries, &query);

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, queries.len(), Some(&sort)));
    context.insert("queries", &queries);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/gather");
    super::helpers::inject_site_context(&state, &session, &mut context, "/admin/gather").await;

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/gather-list.html",
        context,
    )
    .await
}

/// Show create gather query form.
///
/// GET /admin/gather/create
async fn create_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let default_definition = QueryDefinition::default();
    let default_display = QueryDisplay::default();
    let empty_definition = serde_json::to_string(&default_definition).unwrap_or_default();
    let empty_display = serde_json::to_string(&default_display).unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("action", "/admin/gather/create");
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &false);
    context.insert("base_table_options", BASE_TABLE_OPTIONS);
    context.insert("display_format_options", DISPLAY_FORMAT_OPTIONS);
    insert_builder_words(&mut context);
    context.insert("definition", &default_definition);
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
    super::helpers::inject_site_context(&state, &session, &mut context, "/admin/gather").await;

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/gather-form.html",
        context,
    )
    .await
}

/// Handle create gather query form submission.
///
/// POST /admin/gather/create
async fn create_submit(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<GatherFormData>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate basic fields
    let mut errors = Vec::new();

    let query_id = form.query_id.trim().to_string();
    let label = form.label.trim().to_string();

    if query_id.is_empty() {
        errors.push(FieldError::on("query_id", "Query ID is required."));
    } else if !is_valid_query_id(&query_id) {
        errors.push(FieldError::on(
            "query_id",
            "Query ID must start with a letter and contain only lowercase letters, numbers, hyphens, underscores, dots, and colons.",
        ));
    }

    if label.is_empty() {
        errors.push(FieldError::on("label", "Label is required."));
    }

    // Check if query ID already exists
    if state.gather().get_query(&query_id).is_some() {
        errors.push(FieldError::on(
            "query_id",
            UiMessage::new("A gather query with ID '{id}' already exists.").arg("id", &query_id),
        ));
    }

    // Parse definition JSON
    let definition: QueryDefinition = match serde_json::from_str(&form.definition_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(FieldError::form(
                UiMessage::new("Invalid definition JSON: {error}").arg("error", e.to_string()),
            ));
            QueryDefinition::default()
        }
    };

    // Parse display JSON
    let display: QueryDisplay = match serde_json::from_str(&form.display_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(FieldError::form(
                UiMessage::new("Invalid display JSON: {error}").arg("error", e.to_string()),
            ));
            QueryDisplay::default()
        }
    };

    // Validate definition
    let definition_errors = GatherService::validate_definition(&definition);
    errors.extend(definition_errors.into_iter().map(FieldError::form));
    if builder_cannot_show(&definition) {
        errors.push(FieldError::form(BUILDER_CANNOT_SHOW));
    }

    // Cap items_per_page
    let mut display = display;
    if display
        .items_per_page
        .is_some_and(|n| n > MAX_ITEMS_PER_PAGE)
    {
        display.items_per_page = Some(MAX_ITEMS_PER_PAGE);
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", "/admin/gather/create");
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &false);
        context.insert("base_table_options", BASE_TABLE_OPTIONS);
        context.insert("display_format_options", DISPLAY_FORMAT_OPTIONS);
        insert_builder_words(&mut context);
        context.insert("errors", &errors);
        context.insert("definition", &definition);
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
        super::helpers::inject_site_context(&state, &session, &mut context, "/admin/gather").await;

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/gather-form.html",
            context,
        )
        .await;
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
            render_server_error("Failed to create gather query.")
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let Some(query) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    let csrf_token = generate_csrf_token(&session).await;
    let form_build_id = uuid::Uuid::new_v4().to_string();

    let definition_json = serde_json::to_string(&query.definition).unwrap_or_default();
    let display_json = serde_json::to_string(&query.display).unwrap_or_default();

    let mut context = tera::Context::new();
    context.insert("action", &format!("/admin/gather/{id}/save"));
    context.insert("csrf_token", &csrf_token);
    context.insert("form_build_id", &form_build_id);
    context.insert("editing", &true);
    context.insert("base_table_options", BASE_TABLE_OPTIONS);
    context.insert("display_format_options", DISPLAY_FORMAT_OPTIONS);
    insert_builder_words(&mut context);
    context.insert(
        "builder_cannot_show",
        &builder_cannot_show(&query.definition),
    );
    context.insert("definition", &query.definition);
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
    context.insert("path", &format!("/admin/gather/{id}/edit"));
    super::helpers::inject_site_context(&state, &session, &mut context, "/admin/gather").await;

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/gather-form.html",
        context,
    )
    .await
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
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify the query exists
    let Some(existing) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate
    let mut errors = Vec::new();

    let label = form.label.trim().to_string();
    if label.is_empty() {
        errors.push(FieldError::on("label", "Label is required."));
    }

    // Parse definition JSON
    let definition: QueryDefinition = match serde_json::from_str(&form.definition_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(FieldError::form(
                UiMessage::new("Invalid definition JSON: {error}").arg("error", e.to_string()),
            ));
            QueryDefinition::default()
        }
    };

    // Parse display JSON, over the stored display: a key the form did not send
    // keeps its stored value (B160). The builder edits only some of a
    // display's keys, so saving a gather there dropped its feed, its routes,
    // its header and footer.
    let display: QueryDisplay = match display_over_stored(&existing.display, &form.display_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(FieldError::form(
                UiMessage::new("Invalid display JSON: {error}").arg("error", e.to_string()),
            ));
            QueryDisplay::default()
        }
    };

    // Validate definition
    let definition_errors = GatherService::validate_definition(&definition);
    errors.extend(definition_errors.into_iter().map(FieldError::form));
    // The stored definition counts too: the builder dropped what it could not
    // show before posting, so the submitted one alone would look fine.
    if builder_cannot_show(&existing.definition) || builder_cannot_show(&definition) {
        errors.push(FieldError::form(BUILDER_CANNOT_SHOW));
    }

    // Cap items_per_page
    let mut display = display;
    if display
        .items_per_page
        .is_some_and(|n| n > MAX_ITEMS_PER_PAGE)
    {
        display.items_per_page = Some(MAX_ITEMS_PER_PAGE);
    }

    if !errors.is_empty() {
        let csrf_token = generate_csrf_token(&session).await;
        let form_build_id = uuid::Uuid::new_v4().to_string();

        let mut context = tera::Context::new();
        context.insert("action", &format!("/admin/gather/{id}/save"));
        context.insert("csrf_token", &csrf_token);
        context.insert("form_build_id", &form_build_id);
        context.insert("editing", &true);
        context.insert("base_table_options", BASE_TABLE_OPTIONS);
        context.insert("display_format_options", DISPLAY_FORMAT_OPTIONS);
        insert_builder_words(&mut context);
        context.insert(
            "builder_cannot_show",
            &(builder_cannot_show(&existing.definition) || builder_cannot_show(&definition)),
        );
        context.insert("errors", &errors);
        context.insert("definition", &definition);
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
        context.insert("path", &format!("/admin/gather/{id}/edit"));
        super::helpers::inject_site_context(&state, &session, &mut context, "/admin/gather").await;

        return render_admin_template(
            &state,
            &crate::routes::helpers::get_user_context(&session, &state).await,
            "admin/gather-form.html",
            context,
        )
        .await;
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
            render_server_error("Failed to update gather query.")
        }
    }
}

/// The display a save submits, laid over the stored one: each top-level key the
/// submitted JSON object carries replaces the stored key, and every key it
/// leaves out keeps its stored value.
///
/// So a form that edits some of a display's keys (the gather builder edits
/// format, page size, empty text, canonical URL, pager and feed settings) cannot
/// erase the others (routes, a feed it never showed, header and footer) by not
/// knowing them. A key sent as `null` is cleared.
fn display_over_stored(
    stored: &QueryDisplay,
    submitted_json: &str,
) -> Result<QueryDisplay, serde_json::Error> {
    let submitted: serde_json::Value = serde_json::from_str(submitted_json)?;
    let mut merged = serde_json::to_value(stored)?;
    if let (Some(merged), Some(submitted)) = (merged.as_object_mut(), submitted.as_object()) {
        for (key, value) in submitted {
            merged.insert(key.clone(), value.clone());
        }
    } else {
        // Not an object: let deserialization say what is wrong with it.
        return serde_json::from_value(submitted);
    }
    serde_json::from_value(merged)
}

/// Clone a gather query.
///
/// POST /admin/gather/{id}/clone
async fn clone_query(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    if state.gather().get_query(&id).is_none() {
        return render_not_found();
    }

    let timestamp = chrono::Utc::now().timestamp();
    let new_id = format!("{id}-copy-{timestamp}");

    match state.gather().clone_query(&id, &new_id).await {
        Ok(_) => {
            tracing::info!(source = %id, new_id = %new_id, "gather query cloned");
            Redirect::to("/admin/gather").into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to clone gather query");
            render_server_error("Failed to clone gather query.")
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
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    // Verify CSRF token
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    match state.gather().delete_query(&id).await {
        Ok(true) => {
            tracing::info!(query_id = %id, "gather query deleted");
            Redirect::to("/admin/gather").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete gather query");
            render_server_error("Failed to delete gather query.")
        }
    }
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
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}
