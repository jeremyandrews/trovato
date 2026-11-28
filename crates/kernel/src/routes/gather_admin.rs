//! Admin routes for gather query management.
//!
//! Every form here (the query editor, the listing's clone buttons, delete)
//! is built and checked through `FormService` (`routes::admin_form`).

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::gather::field_index::UnindexedField;
use crate::gather::{
    GatherQuery, GatherService, MAX_ITEMS_PER_PAGE, QueryDefinition, QueryDisplay,
};
use crate::models::User;
use crate::state::AppState;

use super::admin_form;
use super::admin_list::{ListQuery, page_of, sort_rows};
use super::helpers::{
    render_admin_template, render_error, render_not_found, render_server_error, require_permission,
};

/// The listing, and where every form here returns.
const BASE: &str = "/admin/gather";

/// The query editor (create and edit). Its id is the one
/// `static/js/gather-builder.js` finds the form by, to write the builder's
/// state into the hidden fields before the form is sent.
const QUERY_FORM_ID: &str = "gather-form";

/// The listing's one form behind every row's Clone button: each button
/// belongs to it (`form=`) and posts to its own query's clone address
/// (`formaction`).
const CLONE_FORM_ID: &str = "gather_clone_form";

/// The delete confirmation form.
const DELETE_FORM_ID: &str = "gather_delete_form";

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

/// Insert the row-builder option lists into a gather form render's context:
/// every render of `admin/gather-form.html` needs them, success or
/// redisplayed-with-errors alike.
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
// The query editor
// =============================================================================

/// What the query editor shows or was sent, each as typed.
struct QueryFormValues {
    /// The query's machine name (fixed once the query exists).
    query_id: String,
    /// The query's label.
    label: String,
    /// The query's description.
    description: String,
    /// The definition as JSON, which the builder script reads and writes.
    definition_json: String,
    /// The display as JSON, which the builder script reads and writes.
    display_json: String,
}

impl QueryFormValues {
    /// What the query editor posted, the id and label trimmed.
    fn from_posted(posted: &PostedForm) -> Self {
        let value = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            query_id: value("query_id").trim().to_string(),
            label: value("label").trim().to_string(),
            description: value("description"),
            definition_json: value("definition_json"),
            display_json: value("display_json"),
        }
    }

    /// The description to store: `None` when left blank.
    fn stored_description(&self) -> Option<String> {
        Some(self.description.clone()).filter(|s| !s.trim().is_empty())
    }
}

/// One showing of the query editor, besides its field values.
struct EditorPage<'a> {
    /// The id of the query being edited; `None` when creating one.
    editing: Option<&'a str>,
    /// The fields' values.
    values: &'a QueryFormValues,
    /// The definition the builder's base table select starts from.
    definition: &'a QueryDefinition,
    /// Whether the query uses a shape the builder cannot show, which turns
    /// saving off ([`builder_cannot_show`]).
    cannot_show: bool,
    /// Fields the query filters or sorts on that no index serves (B207).
    unindexed: &'a [UnindexedField],
}

/// The builder's own controls (`admin/gather-builder.html`), in the page's
/// language: the part of the query editor its script drives. `None` (logged)
/// when the template fails to render.
fn builder_html(
    state: &AppState,
    text: &super::admin_nav::AdminText,
    page: &EditorPage<'_>,
) -> Option<String> {
    let mut context = tera::Context::new();
    context.insert("base_table_options", BASE_TABLE_OPTIONS);
    context.insert("display_format_options", DISPLAY_FORMAT_OPTIONS);
    context.insert("definition", page.definition);
    context.insert("unindexed_fields", page.unindexed);
    text.scope(|| {
        state
            .theme()
            .tera()
            .render("admin/gather-builder.html", &context)
    })
    .map_err(|e| tracing::error!(error = %e, "failed to render the gather builder"))
    .ok()
}

/// The query editor form for `page`.
fn query_form(
    text: &super::admin_nav::AdminText,
    page: &EditorPage<'_>,
    builder: String,
) -> crate::form::Form {
    let action = match page.editing {
        Some(id) => format!("{BASE}/{id}/save"),
        None => format!("{BASE}/create"),
    };
    let values = page.values;
    let mut query_id = FormElement::textfield()
        .title("Query ID")
        .description(
            "Machine name (e.g., \"blog.recent_posts\"). Lowercase letters, numbers, dots, \
             colons, hyphens and underscores. Cannot be changed after creation.",
        )
        .default_value(values.query_id.clone())
        .required()
        .attribute("pattern", "[a-z0-9_.:-]+")
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
        .weight(3);
    if page.editing.is_some() {
        query_id = query_id.attribute("readonly", "readonly");
    }
    let mut submit = FormElement::submit(if page.editing.is_some() {
        "Save changes"
    } else {
        "Create query"
    })
    .suffix(format!(
        r#"<a href="{BASE}" class="button button--secondary">{}</a>"#,
        text.html("Cancel", &[])
    ))
    .weight(10);
    if page.cannot_show {
        submit = submit.disabled();
    }
    let mut form = crate::form::Form::new(QUERY_FORM_ID)
        .action(action)
        .element(
            "definition_json",
            FormElement::hidden()
                .default_value(values.definition_json.clone())
                .weight(0),
        )
        .element(
            "display_json",
            FormElement::hidden()
                .default_value(values.display_json.clone())
                .weight(1),
        )
        .element("query_id", query_id)
        .element(
            "label",
            FormElement::textfield()
                .title("Label")
                .default_value(values.label.clone())
                .required()
                .weight(4),
        )
        .element(
            "description",
            FormElement::textarea(2)
                .title("Description")
                .default_value(values.description.clone())
                .weight(5),
        )
        .element("builder", FormElement::markup(builder).weight(6))
        .element("op", submit);
    if page.cannot_show {
        form = form.element(
            "cannot_show",
            FormElement::markup(format!(
                r#"<div class="notice notice--warning" role="status" id="gather-builder-cannot-show"><strong class="notice__label">{}</strong> {}</div>"#,
                text.html("Warning:", &[]),
                text.html(
                    "This gather uses filter groups or a summary (group_by, aggregates, having, distinct), which this builder cannot show. Saving is off here, because saving would lose them. Change it where it is defined: its config file or the plugin that installs it.",
                    &[]
                )
            ))
            .weight(2),
        );
    }
    form
}

/// Show the query editor for `page` with `errors`.
async fn render_editor(
    state: &AppState,
    session: &Session,
    user: &User,
    page: &EditorPage<'_>,
    errors: &[ValidationError],
) -> Response {
    let path = match page.editing {
        Some(id) => format!("{BASE}/{id}/edit"),
        None => format!("{BASE}/create"),
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, &path).await;
    let Some(builder) = builder_html(state, &text, page) else {
        return render_server_error("The form could not be displayed.");
    };
    let form = query_form(&text, page, builder);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("editing", &page.editing.is_some());
    context.insert("label", &page.values.label);
    insert_builder_words(&mut context);
    context.insert("path", &path);
    super::helpers::inject_site_context(state, session, &mut context, BASE).await;
    render_admin_template(state, &viewer, "admin/gather-form.html", context).await
}

/// The definition and display the query editor posted, and every problem
/// with them. `stored` is the query being edited, whose display the posted
/// one is laid over ([`display_over_stored`]) and whose definition counts
/// for [`builder_cannot_show`].
fn read_query(
    values: &QueryFormValues,
    stored: Option<&GatherQuery>,
) -> (QueryDefinition, QueryDisplay, Vec<ValidationError>) {
    let mut errors = Vec::new();
    let definition: QueryDefinition = match serde_json::from_str(&values.definition_json) {
        Ok(d) => d,
        Err(e) => {
            errors.push(ValidationError::form(format!(
                "Invalid definition JSON: {e}"
            )));
            QueryDefinition::default()
        }
    };
    // Over the stored display when editing: a key the form did not send keeps
    // its stored value (B160). The builder edits only some of a display's
    // keys, so saving a gather there dropped its feed, its routes, its header
    // and footer.
    let display = match stored {
        Some(stored) => display_over_stored(&stored.display, &values.display_json),
        None => serde_json::from_str(&values.display_json),
    };
    let mut display: QueryDisplay = match display {
        Ok(d) => d,
        Err(e) => {
            errors.push(ValidationError::form(format!("Invalid display JSON: {e}")));
            QueryDisplay::default()
        }
    };
    errors.extend(
        GatherService::validate_definition(&definition)
            .into_iter()
            .map(ValidationError::form),
    );
    // The stored definition counts too: the builder dropped what it could not
    // show before posting, so the submitted one alone would look fine.
    if stored.is_some_and(|q| builder_cannot_show(&q.definition))
        || builder_cannot_show(&definition)
    {
        errors.push(ValidationError::form(BUILDER_CANNOT_SHOW));
    }
    if display
        .items_per_page
        .is_some_and(|n| n > MAX_ITEMS_PER_PAGE)
    {
        display.items_per_page = Some(MAX_ITEMS_PER_PAGE);
    }
    (definition, display, errors)
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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

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

    // The rows' Clone buttons belong to this one form; a page with no rows
    // has none to build.
    let clone_form = if queries.is_empty() {
        String::new()
    } else {
        let form = crate::form::Form::new(CLONE_FORM_ID).action(BASE);
        match admin_form::render(&state, &session, &user, form, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        }
    };

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, queries.len(), Some(&sort)));
    context.insert("queries", &queries);
    context.insert("clone_form", &clone_form);
    context.insert("clone_form_id", CLONE_FORM_ID);
    context.insert("path", BASE);
    super::helpers::inject_site_context(&state, &session, &mut context, BASE).await;

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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let definition = QueryDefinition::default();
    let values = QueryFormValues {
        query_id: String::new(),
        label: String::new(),
        description: String::new(),
        definition_json: serde_json::to_string(&definition).unwrap_or_default(),
        display_json: serde_json::to_string(&QueryDisplay::default()).unwrap_or_default(),
    };
    let page = EditorPage {
        editing: None,
        values: &values,
        definition: &definition,
        cannot_show: false,
        unindexed: &[],
    };
    render_editor(&state, &session, &user, &page, &[]).await
}

/// Handle create gather query form submission.
///
/// POST /admin/gather/create
async fn create_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let values = QueryFormValues::from_posted(&posted);

    let mut errors = Vec::new();
    if values.query_id.is_empty() {
        errors.push(ValidationError::field("query_id", "Query ID is required."));
    } else if !is_valid_query_id(&values.query_id) {
        errors.push(ValidationError::field(
            "query_id",
            "Query ID must start with a letter and contain only lowercase letters, numbers, hyphens, underscores, dots, and colons.",
        ));
    }
    if values.label.is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }
    if state.gather().get_query(&values.query_id).is_some() {
        errors.push(ValidationError::field(
            "query_id",
            format!(
                "A gather query with ID '{}' already exists.",
                values.query_id
            ),
        ));
    }
    let (definition, display, query_errors) = read_query(&values, None);
    errors.extend(query_errors);

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, QUERY_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let page = EditorPage {
                    editing: None,
                    values: &values,
                    definition: &definition,
                    cannot_show: false,
                    unindexed: &[],
                };
                return render_editor(&state, &session, &user, &page, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    let now = chrono::Utc::now().timestamp();
    let query = GatherQuery {
        query_id: values.query_id.clone(),
        label: values.label.clone(),
        description: values.stored_description(),
        definition,
        display,
        plugin: "admin".to_string(),
        created: now,
        changed: now,
    };

    match state.gather().register_query(query).await {
        Ok(()) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(query_id = %values.query_id, "gather query created");
            // A field this gather filters or sorts on is now marked unless it
            // says otherwise (B207): build its index.
            state.reconcile_field_indexes();
            Redirect::to(BASE).into_response()
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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(query) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    // Fields this gather filters or sorts on that no index serves (B207).
    let unindexed = crate::gather::field_index::unindexed_fields(
        state.db(),
        state.content_types(),
        state.gather(),
        &query.definition,
    )
    .await;
    let values = QueryFormValues {
        query_id: query.query_id.clone(),
        label: query.label.clone(),
        description: query.description.clone().unwrap_or_default(),
        definition_json: serde_json::to_string(&query.definition).unwrap_or_default(),
        display_json: serde_json::to_string(&query.display).unwrap_or_default(),
    };
    let page = EditorPage {
        editing: Some(&id),
        values: &values,
        definition: &query.definition,
        cannot_show: builder_cannot_show(&query.definition),
        unindexed: &unindexed,
    };
    render_editor(&state, &session, &user, &page, &[]).await
}

/// Handle edit gather query form submission.
///
/// POST /admin/gather/{id}/save
async fn save_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(existing) = state.gather().get_query(&id) else {
        return render_not_found();
    };

    let mut values = QueryFormValues::from_posted(&posted);
    // The id is fixed once the query exists: show the stored one.
    values.query_id = id.clone();
    let mut errors = Vec::new();
    if values.label.is_empty() {
        errors.push(ValidationError::field("label", "Label is required."));
    }
    let (definition, display, query_errors) = read_query(&values, Some(&existing));
    errors.extend(query_errors);

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, QUERY_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let page = EditorPage {
                    editing: Some(&id),
                    values: &values,
                    definition: &definition,
                    cannot_show: builder_cannot_show(&existing.definition)
                        || builder_cannot_show(&definition),
                    unindexed: &[],
                };
                return render_editor(&state, &session, &user, &page, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    // Update the query (preserve original created timestamp and plugin)
    let now = chrono::Utc::now().timestamp();
    let query = GatherQuery {
        query_id: id.clone(),
        label: values.label.clone(),
        description: values.stored_description(),
        definition,
        display,
        plugin: existing.plugin,
        created: existing.created,
        changed: now,
    };

    match state.gather().register_query(query).await {
        Ok(()) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(query_id = %id, "gather query updated");
            state.reconcile_field_indexes();
            Redirect::to(BASE).into_response()
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        CLONE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    if state.gather().get_query(&id).is_none() {
        return render_not_found();
    }

    let timestamp = chrono::Utc::now().timestamp();
    let new_id = format!("{id}-copy-{timestamp}");

    match state.gather().clone_query(&id, &new_id).await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(source = %id, new_id = %new_id, "gather query cloned");
            Redirect::to(BASE).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to clone gather query");
            render_server_error("Failed to clone gather query.")
        }
    }
}

/// The delete confirmation form for the query `id`.
fn delete_form_for(id: &str) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("{BASE}/{id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .weight(10),
        )
}

/// Ask before deleting a gather query.
///
/// GET /admin/gather/{id}/delete
async fn delete_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let Some(query) = state.gather().get_query(&id) else {
        return render_not_found();
    };
    let form_html =
        match admin_form::render(&state, &session, &user, delete_form_for(&id), &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("label", &query.label);
    context.insert("form", &form_html);
    context.insert("path", &format!("{BASE}/{id}/delete"));
    render_admin_template(&state, &viewer, "admin/gather-delete.html", context).await
}

/// Delete a gather query.
///
/// POST /admin/gather/{id}/delete
async fn delete_query(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
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

    match state.gather().delete_query(&id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(query_id = %id, "gather query deleted");
            Redirect::to(BASE).into_response()
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
/// pattern `[a-z0-9_.:-]+` the query editor's `query_id` field carries.
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
        .route(BASE, get(list_queries))
        .route("/admin/gather/create", get(create_form).post(create_submit))
        .route("/admin/gather/{id}/edit", get(edit_form))
        .route("/admin/gather/{id}/save", post(save_submit))
        .route("/admin/gather/{id}/clone", post(clone_query))
        .route(
            "/admin/gather/{id}/delete",
            get(delete_form).post(delete_query),
        )
        .layer(axum::middleware::from_fn(super::admin_confirm::ask_first))
}
