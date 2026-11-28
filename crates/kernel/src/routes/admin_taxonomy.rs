//! Admin routes for category and tag management.
//!
//! Every form here is built and checked by the form pipeline
//! (`routes::admin_form`): the category form, the tag form, and the
//! confirmations that delete a category or a tag.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form as PostedBody, Router};
use tower_sessions::Session;

use std::collections::HashMap;

use crate::content::submission::PostedForm;
use crate::form::{Form, FormElement, ValidationError};
use crate::models::{Category, CreateCategory, CreateTag, Tag, UpdateCategory, UpdateTag, User};
use crate::state::AppState;

use super::admin_form;
use super::admin_list::{ListQuery, page_of, sort_rows};
use super::helpers::{
    MACHINE_NAME_ERROR, SLUG_FORMAT_ERROR, is_valid_machine_name, is_valid_slug,
    render_admin_template, render_error, render_forbidden, render_not_found, render_server_error,
    require_any_permission, require_login, require_permission,
};

/// The category list, under which every category screen hangs.
const CATEGORIES: &str = "/admin/structure/categories";

/// The add and edit category form.
const CATEGORY_FORM_ID: &str = "category_form";

/// The form that confirms deleting a category.
const CATEGORY_DELETE_FORM_ID: &str = "category_delete_form";

/// The add and edit tag form, on the site screens and an owner's alike.
const TAG_FORM_ID: &str = "category_tag_form";

/// The form that confirms deleting a tag.
const TAG_DELETE_FORM_ID: &str = "category_tag_delete_form";

// =============================================================================
// Who may reach the terms
// =============================================================================

/// Administers every category, its terms included.
const ADMINISTER: &str = "administer categories";
/// The term permissions: Drupal's "create terms in X" and the like, held
/// without `administer categories` (B156).
const CREATE_TERMS: &str = "create category terms";
const EDIT_TERMS: &str = "edit category terms";
const DELETE_TERMS: &str = "delete category terms";

/// What opens the category list and a term list: any term permission.
const TERM_LIST: &[&str] = &[ADMINISTER, CREATE_TERMS, EDIT_TERMS, DELETE_TERMS];
/// The category whose tags are the workflow stages (`models::stage`).
const STAGES_CATEGORY: &str = "stages";
/// What opens the add-term form.
const TERM_CREATE: &[&str] = &[ADMINISTER, CREATE_TERMS];
/// What opens the edit-term form.
const TERM_EDIT: &[&str] = &[ADMINISTER, EDIT_TERMS];
/// What deletes a term.
const TERM_DELETE: &[&str] = &[ADMINISTER, DELETE_TERMS];

/// Whether `user`, past one of the gates above, reaches `category` here.
///
/// A term permission reaches the site's own vocabularies. A plugin's
/// category is managed under its owner's route, by whoever the owner lets
/// in (`routes::owned_vocabulary`); here it stays with `administer
/// categories`, so no term permission opens a category it did not before.
/// The workflow stages are tags too, in [`STAGES_CATEGORY`]; they are the
/// stage screens' to change, so no term permission reaches them either.
async fn reaches(state: &AppState, user: &crate::models::User, category: &Category) -> bool {
    (category.owner_type.is_none() && category.id != STAGES_CATEGORY)
        || state
            .permissions()
            .user_has_permission(user, ADMINISTER)
            .await
            .unwrap_or(false)
}

/// Whether `user` holds one of `permissions`.
async fn holds_any(state: &AppState, user: &crate::models::User, permissions: &[&str]) -> bool {
    for permission in permissions {
        if state
            .permissions()
            .user_has_permission(user, permission)
            .await
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The site screens of `category_id`, offering `user` what they may do.
async fn site_urls(state: &AppState, user: &crate::models::User, category_id: &str) -> TermUrls {
    TermUrls {
        offer: TermOffer {
            create: holds_any(state, user, TERM_CREATE).await,
            edit: holds_any(state, user, TERM_EDIT).await,
            delete: holds_any(state, user, TERM_DELETE).await,
        },
        ..TermUrls::site(category_id)
    }
}

/// The refusal for a category this user does not reach.
fn not_yours() -> Response {
    render_forbidden("You do not have permission to manage the terms of this category.")
}

/// A Cancel link to `href`, in `user`'s admin language on `path`.
async fn cancel_link(state: &AppState, user: &User, path: &str, href: &str) -> String {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, path).await;
    format!(
        r#"<a href="{}" class="button button--secondary">{}</a>"#,
        tera::escape_html(href),
        text.html("Cancel", &[])
    )
}

/// A confirmation form with one Delete button, posting to `action`, and
/// `cancel` beside it.
fn delete_form(form_id: &str, action: String, cancel: String) -> Form {
    Form::new(form_id).action(action).element(
        "op",
        FormElement::submit("Delete")
            .attribute("class", "button--danger")
            .suffix(cancel)
            .weight(10),
    )
}

/// A text field for a machine name, a slug or a code: no spell checking, no
/// capitals.
fn plain_text() -> FormElement {
    FormElement::textfield()
        .attribute("spellcheck", "false")
        .attribute("autocapitalize", "none")
}

// =============================================================================
// Category handlers
// =============================================================================

/// The category listing's sorts. The list is sorted in memory, so the second
/// value is only the key's name. `weight` is the stored order and has no
/// column of its own.
const CATEGORY_SORTS: &[(&str, &str)] = &[
    ("weight", "weight"),
    ("name", "name"),
    ("id", "id"),
    ("tags", "tags"),
];

/// List all categories.
///
/// GET /admin/structure/categories
async fn list_categories(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_LIST).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    let query = ListQuery::from_params(&params);
    let sort = query.sort(CATEGORY_SORTS, ("weight", false));

    let mut categories: Vec<Category> = match state.categories().list_categories().await {
        Ok(categories) => {
            let mut shown = Vec::with_capacity(categories.len());
            for category in categories {
                if query.matches(&[&category.label, &category.id])
                    && reaches(&state, &user, &category).await
                {
                    shown.push(category);
                }
            }
            shown
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list categories");
            return render_server_error("Failed to load categories.");
        }
    };

    // Get tag counts for each category
    let mut tag_counts: HashMap<String, i64> = HashMap::new();
    for cat in &categories {
        let count = state.categories().count_tags(&cat.id).await.unwrap_or(0);
        tag_counts.insert(cat.id.clone(), count);
    }

    // A short list held in memory: sorted and paged here. The default is the
    // stored order, weight then name.
    if sort.key == "tags" {
        categories.sort_by_key(|c| tag_counts.get(&c.id).copied().unwrap_or(0));
        if sort.desc {
            categories.reverse();
        }
    } else {
        sort_rows(
            &mut categories,
            &sort,
            &[
                ("weight", |c: &Category| {
                    format!("{:06}\u{0}{}", i32::from(c.weight) + 40_000, c.label)
                }),
                ("name", |c: &Category| c.label.clone()),
                ("id", |c: &Category| c.id.clone()),
            ],
        );
    }
    let total = i64::try_from(categories.len()).unwrap_or(i64::MAX);
    let categories = page_of(&categories, &query);

    let list = query.view(total, categories.len(), Some(&sort));
    let forms = match super::admin_list::render_forms(
        &state,
        &session,
        &user,
        &list,
        CATEGORIES,
        "Filter categories",
        "Name or machine name contains",
        Vec::new(),
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    forms.insert_into(&mut context);
    context.insert("list", &list);
    context.insert("categories", &categories);
    context.insert("tag_counts", &tag_counts);
    // Adding, editing and deleting a category are offered to whoever
    // administers categories; a holder of a term permission sees the list.
    context.insert(
        "administers",
        &holds_any(&state, &user, &[ADMINISTER]).await,
    );
    context.insert("path", CATEGORIES);

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/categories.html",
        context,
    )
    .await
}

/// A category's hierarchy choices: value and English label, translated by
/// the select template.
const HIERARCHY_OPTIONS: &[(&str, &str)] = &[
    ("0", "Flat (no hierarchy)"),
    ("1", "Single parent"),
    ("2", "Multiple parents"),
];

/// What the category form's fields hold.
#[derive(Debug)]
struct CategoryValues {
    id: String,
    label: String,
    description: String,
    hierarchy: String,
}

impl Default for CategoryValues {
    fn default() -> Self {
        Self {
            id: String::new(),
            label: String::new(),
            description: String::new(),
            hierarchy: "0".to_string(),
        }
    }
}

impl CategoryValues {
    /// What the category form posted.
    fn posted(posted: &PostedForm) -> Self {
        Self {
            id: posted.get("id").unwrap_or_default().to_string(),
            label: posted.get("label").unwrap_or_default().to_string(),
            description: posted.get("description").unwrap_or_default().to_string(),
            hierarchy: posted.get("hierarchy").unwrap_or_default().to_string(),
        }
    }
}

/// The add and edit category form: `action` is where it posts, `values`
/// what its fields hold, `editing` whether it saves an existing category
/// (whose machine name is then read only), and `cancel` the link beside its
/// button.
fn category_form(action: &str, values: &CategoryValues, editing: bool, cancel: String) -> Form {
    let hierarchy_options = HIERARCHY_OPTIONS
        .iter()
        .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
        .collect();
    let mut machine_name = plain_text()
        .title("Machine name")
        .description(
            "A unique machine-readable name. Can only contain lowercase letters, numbers, \
             and underscores.",
        )
        .default_value(values.id.clone())
        .attribute("pattern", "[a-z][a-z0-9_]*")
        .required()
        .weight(1);
    if editing {
        machine_name = machine_name.attribute("readonly", "readonly");
    }
    Form::new(CATEGORY_FORM_ID)
        .action(action)
        .element(
            "label",
            FormElement::textfield()
                .title("Name")
                .description("The human-readable name of this category.")
                .default_value(values.label.clone())
                .attribute("data-machine-name-source", "id")
                .required()
                .weight(0),
        )
        .element("id", machine_name)
        .element(
            "description",
            FormElement::textarea(3)
                .title("Description")
                .default_value(values.description.clone())
                .weight(2),
        )
        .element(
            "hierarchy",
            FormElement::select(hierarchy_options)
                .title("Hierarchy type")
                .description("Whether tags can have parent tags.")
                .default_value(if values.hierarchy.is_empty() {
                    "0".to_string()
                } else {
                    values.hierarchy.clone()
                })
                .required()
                .weight(3),
        )
        .element(
            "op",
            FormElement::submit(if editing {
                "Save changes"
            } else {
                "Create category"
            })
            .suffix(cancel)
            .weight(10),
        )
}

/// Render the add category form, or the edit form of the category
/// `editing`.
async fn render_category_form(
    state: &AppState,
    session: &Session,
    user: &User,
    editing: Option<&str>,
    values: &CategoryValues,
    errors: &[ValidationError],
) -> Response {
    let action = match editing {
        Some(id) => format!("{CATEGORIES}/{id}/edit"),
        None => format!("{CATEGORIES}/add"),
    };
    let cancel = cancel_link(state, user, &action, CATEGORIES).await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        category_form(&action, values, editing.is_some(), cancel),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("editing", &editing.is_some());
    context.insert("label", &values.label);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/category-form.html", context).await
}

/// Show add category form.
///
/// GET /admin/structure/categories/add
async fn add_category_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_category_form(
        &state,
        &session,
        &user,
        None,
        &CategoryValues::default(),
        &[],
    )
    .await
}

/// Handle add category form submission.
///
/// POST /admin/structure/categories/add
async fn add_category_submit(
    State(state): State<AppState>,
    session: Session,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let values = CategoryValues::posted(&posted);

    // Validate
    let mut errors = Vec::new();

    if values.id.trim().is_empty() {
        errors.push(ValidationError::field("id", "Machine name is required."));
    } else if !is_valid_machine_name(&values.id) {
        errors.push(ValidationError::field("id", MACHINE_NAME_ERROR));
    }

    if values.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }

    // Check if category already exists
    if state
        .categories()
        .category_exists(&values.id)
        .await
        .unwrap_or(false)
    {
        errors.push(ValidationError::field(
            "id",
            format!("A category with ID '{}' already exists.", values.id),
        ));
    }

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, CATEGORY_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_category_form(&state, &session, &user, None, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    let input = CreateCategory {
        id: values.id.clone(),
        label: values.label.clone(),
        description: posted.get("description").map(str::to_string),
        hierarchy: values.hierarchy.parse().ok(),
        weight: None,
    };

    match state.categories().create_category(input).await {
        Ok(_) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(id = %values.id, "category created");
            Redirect::to(CATEGORIES).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create category");
            render_server_error("Failed to create category.")
        }
    }
}

/// The category `id`, or 404.
async fn load_category(state: &AppState, id: &str) -> Result<Category, Response> {
    state
        .categories()
        .get_category(id)
        .await
        .ok()
        .flatten()
        .ok_or_else(render_not_found)
}

/// Show edit category form.
///
/// GET /admin/structure/categories/{id}/edit
async fn edit_category_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let category = match load_category(&state, &category_id).await {
        Ok(category) => category,
        Err(response) => return response,
    };
    let values = CategoryValues {
        id: category.id,
        label: category.label,
        description: category.description.unwrap_or_default(),
        hierarchy: category.hierarchy.to_string(),
    };
    render_category_form(&state, &session, &user, Some(&category_id), &values, &[]).await
}

/// Handle edit category form submission.
///
/// POST /admin/structure/categories/{id}/edit
async fn edit_category_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let values = CategoryValues::posted(&posted);

    // Validate
    let mut errors = Vec::new();

    if values.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, CATEGORY_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_category_form(
                    &state,
                    &session,
                    &user,
                    Some(&category_id),
                    &values,
                    &errors,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };

    let input = UpdateCategory {
        label: Some(values.label.clone()),
        description: posted.get("description").map(str::to_string),
        hierarchy: values.hierarchy.parse().ok(),
        weight: None,
    };

    match state
        .categories()
        .update_category(&category_id, input)
        .await
    {
        Ok(Some(_)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(id = %category_id, "category updated");
            Redirect::to(CATEGORIES).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update category");
            render_server_error("Failed to update category.")
        }
    }
}

/// Confirm deleting a category.
///
/// GET /admin/structure/categories/{id}/delete
async fn delete_category_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let category = match load_category(&state, &category_id).await {
        Ok(category) => category,
        Err(response) => return response,
    };
    let action = format!("{CATEGORIES}/{category_id}/delete");
    let cancel = cancel_link(&state, &user, &action, CATEGORIES).await;
    let form = delete_form(CATEGORY_DELETE_FORM_ID, action.clone(), cancel);
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let mut context = tera::Context::new();
    context.insert("label", &category.label);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(&state, &viewer, "admin/category-delete.html", context).await
}

/// Delete a category.
///
/// POST /admin/structure/categories/{id}/delete
async fn delete_category(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        CATEGORY_DELETE_FORM_ID,
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

    match state.categories().delete_category(&category_id).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(id = %category_id, "category deleted");
            Redirect::to(CATEGORIES).into_response()
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
// category, behind `administer categories`; and under a plugin's owner route
// (`routes::owned_vocabulary`, B108) for that owner's categories, behind the
// owner's access check. The handlers below check access and find the
// category; the `pub(crate)` functions after them do the rest for both,
// with the URLs a [`TermUrls`] gives.

/// Where the term screens of one category live.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct TermUrls {
    /// The page listing the categories, the breadcrumb's first link.
    pub index: String,
    /// What that page is called.
    pub index_label: String,
    /// This category's term list.
    pub list: String,
    /// Its add-term form.
    pub add: String,
    /// The base a term's `/{id}/edit` and `/{id}/delete` hang off.
    pub term_base: String,
    /// What the term list offers: Add, Edit and Delete only to whoever may
    /// use them. Everything, unless a handler says less.
    pub offer: TermOffer,
}

/// The term actions a list offers its viewer.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct TermOffer {
    pub create: bool,
    pub edit: bool,
    pub delete: bool,
}

impl TermOffer {
    /// Every action, for a viewer the route already admitted to all of them.
    pub(crate) const ALL: Self = Self {
        create: true,
        edit: true,
        delete: true,
    };
}

impl TermUrls {
    /// The site-wide screens under `/admin/structure`.
    pub(crate) fn site(category_id: &str) -> Self {
        Self {
            index: CATEGORIES.to_string(),
            index_label: "Categories".to_string(),
            list: format!("{CATEGORIES}/{category_id}/tags"),
            add: format!("{CATEGORIES}/{category_id}/tags/add"),
            term_base: "/admin/structure/tags".to_string(),
            offer: TermOffer::ALL,
        }
    }

    fn edit(&self, tag_id: uuid::Uuid) -> String {
        format!("{}/{tag_id}/edit", self.term_base)
    }

    fn delete(&self, tag_id: uuid::Uuid) -> String {
        format!("{}/{tag_id}/delete", self.term_base)
    }
}

/// The posted term form: every field as it came, in order, for the form
/// pipeline to check.
pub(crate) type TermForm = Vec<(String, String)>;

/// The tag `tag_id` and its category, for `user` past one of the term
/// gates: 404 for no such tag, and the refusal for a category `user` does
/// not reach.
async fn site_term(
    state: &AppState,
    user: &User,
    tag_id: uuid::Uuid,
) -> Result<(Tag, Category), Response> {
    let Some(tag) = state.categories().get_tag(tag_id).await.ok().flatten() else {
        return Err(render_not_found());
    };
    let Some(category) = state
        .categories()
        .get_category(&tag.category_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(render_error("Category not found."));
    };
    if !reaches(state, user, &category).await {
        return Err(not_yours());
    }
    Ok((tag, category))
}

/// The category `category_id`, for `user` past one of the term gates.
async fn site_category(
    state: &AppState,
    user: &User,
    category_id: &str,
) -> Result<Category, Response> {
    let category = load_category(state, category_id).await?;
    if !reaches(state, user, &category).await {
        return Err(not_yours());
    }
    Ok(category)
}

/// GET /admin/structure/categories/{id}/tags
async fn list_tags(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_LIST).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let category = match site_category(&state, &user, &category_id).await {
        Ok(category) => category,
        Err(response) => return response,
    };
    term_list(
        &state,
        &session,
        &user,
        &category,
        &site_urls(&state, &user, &category_id).await,
        &params,
    )
    .await
}

/// GET /admin/structure/categories/{id}/tags/add
async fn add_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_CREATE).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let category = match site_category(&state, &user, &category_id).await {
        Ok(category) => category,
        Err(response) => return response,
    };
    term_add_form(&state, &session, &category, &TermUrls::site(&category_id)).await
}

/// POST /admin/structure/categories/{id}/tags/add
async fn add_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<String>,
    PostedBody(form): PostedBody<TermForm>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_CREATE).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let category = match site_category(&state, &user, &category_id).await {
        Ok(category) => category,
        Err(response) => return response,
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
    let user = match require_any_permission(&state, &session, TERM_EDIT).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (tag, category) = match site_term(&state, &user, tag_id).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let urls = site_urls(&state, &user, &category.id).await;
    term_edit_form(&state, &session, &category, &tag, &urls).await
}

/// POST /admin/structure/tags/{id}/edit
async fn edit_tag_submit(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    PostedBody(form): PostedBody<TermForm>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_EDIT).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (tag, category) = match site_term(&state, &user, tag_id).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let urls = site_urls(&state, &user, &category.id).await;
    term_edit_submit(&state, &session, &category, &tag, form, &urls).await
}

/// GET /admin/structure/tags/{id}/delete: confirm.
async fn delete_tag_form(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_any_permission(&state, &session, TERM_DELETE).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (tag, category) = match site_term(&state, &user, tag_id).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    term_delete_page(
        &state,
        &session,
        &user,
        &category,
        &tag,
        &TermUrls::site(&category.id),
    )
    .await
}

/// POST /admin/structure/tags/{id}/delete
async fn delete_tag(
    State(state): State<AppState>,
    session: Session,
    Path(tag_id): Path<uuid::Uuid>,
    PostedBody(pairs): PostedBody<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_any_permission(&state, &session, TERM_DELETE).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let (tag, _) = match site_term(&state, &user, tag_id).await {
        Ok(found) => found,
        Err(response) => return response,
    };
    let urls = TermUrls::site(&tag.category_id);
    term_delete(&state, &session, &user, &tag, &posted, &urls).await
}

// ---- Shared with the owner routes ------------------------------------------

/// The term list of `category`, filtered by the listing query in `params`.
/// Terms keep their weight order, so the list has no sort. A row's Delete is
/// a link to the confirmation page at the term's delete address.
pub(crate) async fn term_list(
    state: &AppState,
    session: &Session,
    user: &User,
    category: &crate::models::Category,
    urls: &TermUrls,
    params: &HashMap<String, String>,
) -> Response {
    let query = ListQuery::from_params(params);
    let tags: Vec<Tag> = match state.categories().list_tags(&category.id).await {
        Ok(tags) => tags
            .into_iter()
            .filter(|t| query.matches(&[&t.label, t.slug.as_deref().unwrap_or("")]))
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list tags");
            return render_server_error("Failed to load tags.");
        }
    };
    let total = i64::try_from(tags.len()).unwrap_or(i64::MAX);
    let tags = page_of(&tags, &query);

    let list = query.view(total, tags.len(), None);
    let forms = match super::admin_list::render_forms(
        state,
        session,
        user,
        &list,
        &urls.list,
        "Filter tags",
        "Name or slug contains",
        Vec::new(),
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    forms.insert_into(&mut context);
    context.insert("list", &list);
    context.insert("category", category);
    context.insert("tags", &tags);
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

/// What the term form's fields hold.
#[derive(Debug, Default)]
struct TermValues {
    label: String,
    description: String,
    slug: String,
    weight: String,
    parent_id: String,
}

impl TermValues {
    /// What the term form posted.
    fn posted(posted: &PostedForm) -> Self {
        Self {
            label: posted.get("label").unwrap_or_default().to_string(),
            description: posted.get("description").unwrap_or_default().to_string(),
            slug: posted.get("slug").unwrap_or_default().to_string(),
            weight: posted.get("weight").unwrap_or_default().to_string(),
            parent_id: posted.get("parent_id").unwrap_or_default().to_string(),
        }
    }
}

/// The add and edit term form: `action` is where it posts, `values` what
/// its fields hold, `parents` the choices of parent (offered only when the
/// category has a hierarchy), `editing` whether it saves an existing term,
/// and `cancel` the link beside its button.
fn term_form(
    action: &str,
    values: &TermValues,
    parents: Option<Vec<(String, String)>>,
    editing: bool,
    cancel: String,
) -> Form {
    let mut form = Form::new(TAG_FORM_ID)
        .action(action)
        .element(
            "label",
            FormElement::textfield()
                .title("Name")
                .default_value(values.label.clone())
                .required()
                .weight(0),
        )
        .element(
            "description",
            FormElement::textarea(3)
                .title("Description")
                .default_value(values.description.clone())
                .weight(1),
        )
        .element(
            "slug",
            plain_text()
                .title("URL slug")
                .description(
                    "Lowercase letters, digits, and hyphens (e.g. \"rust\", \"ai-data\"). Used \
                     in URL aliases. Leave empty if not needed.",
                )
                .default_value(values.slug.clone())
                .attribute("pattern", "[a-z0-9][a-z0-9\\-]*")
                .max_length(128)
                .weight(2),
        )
        .element(
            "weight",
            FormElement::input("number")
                .title("Weight")
                .description("Lower weights appear first in listings.")
                .default_value(if values.weight.is_empty() {
                    "0".to_string()
                } else {
                    values.weight.clone()
                })
                .weight(4),
        )
        .element(
            "op",
            FormElement::submit(if editing {
                "Save changes"
            } else {
                "Create tag"
            })
            .suffix(cancel)
            .weight(10),
        );
    if let Some(parents) = parents {
        form = form.element(
            "parent_id",
            FormElement::select(parents)
                .title("Parent tag")
                .description("Select a parent tag to create a hierarchy.")
                .default_value(values.parent_id.clone())
                .weight(3),
        );
    }
    form
}

/// Render the term form. `tag_id` is the term being edited, if any;
/// `values` what the fields hold.
#[allow(clippy::too_many_arguments)]
async fn render_term_form(
    state: &AppState,
    session: &Session,
    user: &User,
    category: &crate::models::Category,
    tag_id: Option<uuid::Uuid>,
    values: &TermValues,
    errors: &[ValidationError],
    urls: &TermUrls,
) -> Response {
    // The parent selector: every term of the category but this one.
    let parents = if category.hierarchy > 0 {
        let tags = state
            .categories()
            .list_tags(&category.id)
            .await
            .unwrap_or_default();
        Some(
            tags.into_iter()
                .filter(|t| Some(t.id) != tag_id)
                .map(|t| (t.id.to_string(), t.label))
                .collect(),
        )
    } else {
        None
    };

    let action = match tag_id {
        Some(id) => urls.edit(id),
        None => urls.add.clone(),
    };
    let cancel = cancel_link(state, user, &action, &urls.list).await;
    let form_html = match admin_form::render(
        state,
        session,
        user,
        term_form(&action, values, parents, tag_id.is_some(), cancel),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("editing", &tag_id.is_some());
    context.insert("label", &values.label);
    context.insert("category", category);
    context.insert("form", &form_html);
    context.insert("urls", urls);
    context.insert("path", &action);

    render_admin_template(
        state,
        &super::helpers::user_context_for(state, user).await,
        "admin/tag-form.html",
        context,
    )
    .await
}

/// The empty add-term form for `category`.
pub(crate) async fn term_add_form(
    state: &AppState,
    session: &Session,
    category: &crate::models::Category,
    urls: &TermUrls,
) -> Response {
    let user = match require_login(state, session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_term_form(
        state,
        session,
        &user,
        category,
        None,
        &TermValues::default(),
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
    let posted = PostedForm(form);
    let user = match require_login(state, session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let values = TermValues::posted(&posted);

    // Normalize slug: empty string → None, otherwise validate.
    let slug = Some(values.slug.clone()).filter(|s| !s.is_empty());

    let mut errors = Vec::new();
    if values.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }
    if let Some(ref s) = slug
        && !is_valid_slug(s)
    {
        errors.push(ValidationError::field("slug", SLUG_FORMAT_ERROR));
    }
    // Check for duplicate slug within this category.
    if errors.is_empty()
        && let Some(ref s) = slug
        && let Ok(Some(_)) = crate::models::Tag::find_by_slug(state.db(), &category.id, s).await
    {
        errors.push(ValidationError::field(
            "slug",
            "A tag with this slug already exists in this category.",
        ));
    }
    let parent_ids = match parent_of(state, category, posted.get("parent_id")).await {
        Ok(parents) => parents,
        Err(e) => {
            errors.push(ValidationError::field("parent_id", e));
            None
        }
    };
    let form_state =
        match admin_form::check(state, session, &user, &posted, TAG_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_term_form(
                    state, session, &user, category, None, &values, &errors, urls,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };

    let input = CreateTag {
        category_id: category.id.clone(),
        label: values.label.clone(),
        description: posted.get("description").map(str::to_string),
        slug,
        weight: values.weight.parse().ok(),
        parent_ids,
    };

    match state.categories().create_tag(input).await {
        Ok(_) => {
            admin_form::submitted(state, &user, &form_state, &posted).await;
            tracing::info!(category = %category.id, label = %values.label, "tag created");
            Redirect::to(&urls.list).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create tag");
            render_server_error("Failed to create tag.")
        }
    }
}

/// The parent a posted `parent_id` names: none for empty, else a term of the
/// same category. A term of another category is refused, so an owner's
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
    let user = match require_login(state, session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let parents = state
        .categories()
        .get_parents(tag.id)
        .await
        .unwrap_or_default();
    let values = TermValues {
        label: tag.label.clone(),
        description: tag.description.clone().unwrap_or_default(),
        slug: tag.slug.clone().unwrap_or_default(),
        weight: tag.weight.to_string(),
        parent_id: parents
            .first()
            .map(|p| p.id.to_string())
            .unwrap_or_default(),
    };
    render_term_form(
        state,
        session,
        &user,
        category,
        Some(tag.id),
        &values,
        &[],
        urls,
    )
    .await
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
    let posted = PostedForm(form);
    let user = match require_login(state, session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let values = TermValues::posted(&posted);
    let tag_id = tag.id;

    // For update: empty string signals "clear the slug" to Tag::update,
    // None means "keep current value". Non-empty values are validated.
    let slug_value = values.slug.as_str();

    let mut errors = Vec::new();
    if values.label.trim().is_empty() {
        errors.push(ValidationError::field("label", "Name is required."));
    }
    if !slug_value.is_empty() && !is_valid_slug(slug_value) {
        errors.push(ValidationError::field("slug", SLUG_FORMAT_ERROR));
    }
    // Check for duplicate slug within the same category (excluding this tag).
    if errors.is_empty()
        && !slug_value.is_empty()
        && let Ok(Some(existing)) =
            crate::models::Tag::find_by_slug(state.db(), &tag.category_id, slug_value).await
        && existing.id != tag_id
    {
        errors.push(ValidationError::field(
            "slug",
            "A tag with this slug already exists in this category.",
        ));
    }
    let parent_ids = if category.hierarchy > 0 {
        match parent_of(state, category, posted.get("parent_id")).await {
            Ok(parents) => parents.unwrap_or_default(),
            Err(e) => {
                errors.push(ValidationError::field("parent_id", e));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    if parent_ids.contains(&tag_id) {
        errors.push(ValidationError::field(
            "parent_id",
            "A tag cannot be its own parent.",
        ));
    }
    let form_state =
        match admin_form::check(state, session, &user, &posted, TAG_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_term_form(
                    state,
                    session,
                    &user,
                    category,
                    Some(tag_id),
                    &values,
                    &errors,
                    urls,
                )
                .await;
            }
            Err(Err(response)) => return response,
        };

    // Update tag — None keeps current slug, Some("") clears it, Some(value) sets it.
    let input = UpdateTag {
        label: Some(values.label.clone()),
        description: posted.get("description").map(str::to_string),
        slug: posted.get("slug").map(str::to_string),
        weight: values.weight.parse().ok(),
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

    admin_form::submitted(state, &user, &form_state, &posted).await;
    tracing::info!(tag_id = %tag_id, "tag updated");
    Redirect::to(&urls.list).into_response()
}

/// Delete `tag` once its confirmation form, posted in `posted`, checks out,
/// and go back to its list: for the site-wide screens and the owner routes.
pub(crate) async fn term_delete(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    tag: &crate::models::Tag,
    posted: &PostedForm,
    urls: &TermUrls,
) -> Response {
    let form_state =
        match admin_form::check(state, session, user, posted, TAG_DELETE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };
    let response = delete_term_now(state, tag, urls).await;
    if response.status().is_redirection() {
        admin_form::submitted(state, user, &form_state, posted).await;
    }
    response
}

/// The page asking whether to delete `tag` of `category`, its form posting to
/// the delete under `urls`.
pub(crate) async fn term_delete_page(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    category: &crate::models::Category,
    tag: &crate::models::Tag,
    urls: &TermUrls,
) -> Response {
    let action = urls.delete(tag.id);
    let cancel = cancel_link(state, user, &action, &urls.list).await;
    let form = delete_form(TAG_DELETE_FORM_ID, action.clone(), cancel);
    let form_html = match admin_form::render(state, session, user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("label", &tag.label);
    context.insert("category", category);
    context.insert("urls", urls);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/tag-delete.html", context).await
}

/// Delete `tag`, the submission already checked, and go back to its list.
async fn delete_term_now(state: &AppState, tag: &crate::models::Tag, urls: &TermUrls) -> Response {
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
        .route(
            "/admin/structure/categories/add",
            get(add_category_form).post(add_category_submit),
        )
        .route(
            "/admin/structure/categories/{id}/edit",
            get(edit_category_form).post(edit_category_submit),
        )
        .route(
            "/admin/structure/categories/{id}/delete",
            get(delete_category_form).post(delete_category),
        )
        .route("/admin/structure/categories/{id}/tags", get(list_tags))
        .route(
            "/admin/structure/categories/{id}/tags/add",
            get(add_tag_form).post(add_tag_submit),
        )
        .route(
            "/admin/structure/tags/{id}/edit",
            get(edit_tag_form).post(edit_tag_submit),
        )
        .route(
            "/admin/structure/tags/{id}/delete",
            get(delete_tag_form).post(delete_tag),
        )
}
