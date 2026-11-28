//! Admin routes for content item management.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::FormElement;
use crate::models::CreateItem;
use crate::state::AppState;

use super::admin_form;
use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    admin_user_context, build_local_tasks, render_admin_template, render_error, render_forbidden,
    render_not_found, render_server_error, require_any_permission, require_login,
    require_permission, single_form_field,
};

/// Drupal's permission for the content list, without the right to edit
/// everything on it: what an author holds to find their own work.
pub const ACCESS_CONTENT_OVERVIEW: &str = "access content overview";

/// What lets a viewer see and edit every item: the content list shows such a
/// viewer every row, and anyone else only the rows they may open.
pub const EDIT_ANY_CONTENT: &str = "edit any content";

/// Session key for flash messages on the content list page.
const CONTENT_FLASH_KEY: &str = "content_admin_flash";

/// The content list's bulk form: the action select, the target stage and
/// Apply. The row checkboxes join it through their `form` attribute.
const BULK_FORM_ID: &str = "content_bulk_form";

/// The form on the page that confirms deleting one item.
const DELETE_FORM_ID: &str = "content_delete_form";

/// The bulk form, offering `actions` and, when there are any, the stages a
/// move can target. Both option lists are `(value, label)`; the action labels
/// are English sources the form templates translate, the stage labels data.
fn bulk_form(actions: &[(&str, &str)], stages: &[(String, &str)]) -> crate::form::Form {
    let mut form = crate::form::Form::new(BULK_FORM_ID)
        .action("/admin/content/bulk?confirm=delete")
        .element(
            "action",
            FormElement::select(
                actions
                    .iter()
                    .map(|(value, label)| ((*value).to_string(), (*label).to_string()))
                    .collect(),
            )
            .title("Bulk action")
            .weight(0),
        );
    if !stages.is_empty() {
        // The target of "Move selected to stage"; the other actions ignore
        // it. Each item's move is decided by the editorial workflow.
        form = form.element(
            "stage",
            FormElement::select(
                stages
                    .iter()
                    .map(|(value, label)| (value.clone(), (*label).to_string()))
                    .collect(),
            )
            .title("Target stage")
            .weight(1),
        );
    }
    form.element(
        "op",
        FormElement::submit("Apply")
            .attribute("class", "button--secondary button--small")
            .weight(10),
    )
}

/// The form that deletes item `id`, with a Cancel link back to the list.
fn delete_form_for(id: uuid::Uuid, cancel: &str) -> crate::form::Form {
    crate::form::Form::new(DELETE_FORM_ID)
        .action(format!("/admin/content/{id}/delete"))
        .element(
            "op",
            FormElement::submit("Delete")
                .attribute("class", "button--danger")
                .suffix(format!(
                    r#"<a href="/admin/content" class="button button--secondary">{cancel}</a>"#
                ))
                .weight(10),
        )
}

/// List all content.
///
/// GET /admin/content
/// The content listing's sortable columns: key and SQL.
const CONTENT_SORTS: &[(&str, &str)] = &[
    ("title", "lower(title)"),
    ("type", "type"),
    ("status", "status"),
    ("changed", "changed"),
];

async fn list_content(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let user = match require_any_permission(
        &state,
        &session,
        &[ACCESS_CONTENT_OVERVIEW, EDIT_ANY_CONTENT],
    )
    .await
    {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };

    let type_filter = params
        .get("type")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty());
    let status_filter = params.get("status").and_then(|s| s.parse::<i16>().ok());
    let query = ListQuery::from_params(&params);
    let sort = query.sort(CONTENT_SORTS, ("changed", true));
    let like = query.like();

    let (items, total) = match state
        .items()
        .admin_page(
            type_filter,
            status_filter,
            like.as_deref(),
            &sort.order_by(),
            PER_PAGE,
            query.offset(),
        )
        .await
    {
        Ok(page) => page,
        Err(e) => {
            tracing::error!(error = %e, "failed to list content");
            return render_server_error("Failed to load content.");
        }
    };

    // What this viewer may do with each row. `edit any content` sees every
    // item, as the list always showed it; whoever holds only the overview
    // sees the items they may view, Drupal's content overview under item
    // access, and is offered Edit and Delete only where the item pages would
    // let them. The answers are `check_access`'s, so the list never shows or
    // offers more than the items themselves allow.
    let viewer = admin_user_context(&state, &user).await;
    let editor = viewer.can(EDIT_ANY_CONTENT);
    let deleter = viewer.can("delete any content");
    let mut shown = Vec::with_capacity(items.len());
    let mut operations: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    for item in items {
        if !editor && !allowed(&state, &item, "view", &viewer).await {
            continue;
        }
        // Which edit form the row links to: the admin one, the item page's, or none.
        let edit = if editor {
            Some("admin")
        } else if allowed(&state, &item, "edit", &viewer).await {
            Some("item")
        } else {
            None
        };
        let delete = deleter || allowed(&state, &item, "delete", &viewer).await;
        operations.insert(
            item.id.to_string(),
            serde_json::json!({ "edit": edit, "delete": delete }),
        );
        shown.push(item);
    }
    let items = shown;

    // Get authors for display
    let mut authors: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &items {
        if !authors.contains_key(&item.author_id.to_string())
            && let Ok(Some(user)) = state.users().find_by_id(item.author_id).await
        {
            authors.insert(item.author_id.to_string(), user.name);
        }
    }

    let content_types = state.content_types().list_all().await;

    // The targets the bulk move offers. Every stage: whether a user may move an
    // item there is decided per item when the action runs.
    let stages = match state.items().stage_view().await {
        Ok(view) => view.stages().to_vec(),
        Err(e) => {
            tracing::warn!(error = %e, "stage list for the bulk move failed");
            Vec::new()
        }
    };

    // Read and clear flash message
    let flash: Option<serde_json::Value> = session.get(CONTENT_FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(CONTENT_FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear flash message");
    }

    let mut context = tera::Context::new();
    context.insert("items", &items);
    context.insert("operations", &operations);
    context.insert("authors", &authors);
    context.insert("content_types", &content_types);
    context.insert("type_filter", &type_filter.unwrap_or(""));
    // The filter and bulk selects' options, as [value, label] pairs for the
    // form macros. Type and stage labels are data; the others are English
    // sources the template translates.
    let type_options: Vec<(&str, &str)> = content_types
        .iter()
        .map(|ct| (ct.machine_name.as_str(), ct.label.as_str()))
        .collect();
    context.insert("type_options", &type_options);
    context.insert(
        "status_options",
        &[("1", "Published"), ("0", "Unpublished")],
    );
    let mut bulk_options = vec![
        ("publish", "Publish selected"),
        ("unpublish", "Unpublish selected"),
        ("promote", "Promote selected to front page"),
        ("demote", "Remove selected from front page"),
        ("sticky", "Make selected sticky"),
        ("unsticky", "Make selected not sticky"),
        ("delete", "Delete selected"),
    ];
    if !stages.is_empty() {
        bulk_options.push(("move_to_stage", "Move selected to stage"));
    }
    // Only the actions the viewer may take are offered; the bulk handler asks
    // the same question again.
    bulk_options.retain(|(action, _)| bulk_action_offered(&viewer, action));
    let stage_options: Vec<(String, &str)> = stages
        .iter()
        .map(|s| (s.id.to_string(), s.label.as_str()))
        .collect();
    // The bulk form, when the list has rows and the viewer may take an action.
    let bulk_form_html = if items.is_empty() || bulk_options.is_empty() {
        None
    } else {
        match admin_form::render(
            &state,
            &session,
            &user,
            bulk_form(&bulk_options, &stage_options),
            &[],
        )
        .await
        {
            Ok(html) => Some(html),
            Err(response) => return response,
        }
    };
    context.insert("bulk_form", &bulk_form_html);
    context.insert("bulk_form_id", BULK_FORM_ID);
    context.insert(
        "status_filter",
        &status_filter.map(|s| s.to_string()).unwrap_or_default(),
    );
    context.insert("flash", &flash);
    context.insert("list", &query.view(total, items.len(), Some(&sort)));
    context.insert("path", "/admin/content");

    render_admin_template(&state, &viewer, "admin/content-list.html", context).await
}

/// The moderated content list: every item of a moderated type whose latest
/// revision is a Draft or Archived, newest change first.
///
/// GET /admin/content/moderated, `?state=draft` or `?state=archived` to narrow
/// it. For a holder of `view latest version` or `edit any content`.
async fn list_moderated(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    use crate::content::moderation;

    let viewer = crate::routes::helpers::get_user_context(&session, &state).await;
    if !(viewer.can(moderation::VIEW_LATEST_VERSION) || viewer.can("edit any content")) {
        return match require_permission(&state, &session, moderation::VIEW_LATEST_VERSION).await {
            Err(refusal) => refusal,
            Ok(_) => render_forbidden("You may not see the moderated content list."),
        };
    }
    let state_filter = params
        .get("state")
        .and_then(|s| moderation::state(s))
        .filter(|s| *s != moderation::PUBLISHED);

    let rows = match crate::models::Item::list_moderated(state.db(), state_filter, 200).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "failed to list moderated content");
            return render_server_error("Failed to load moderated content.");
        }
    };
    let mut ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    ids.dedup();
    // An item this viewer may not see is not listed.
    let items = crate::models::Item::find_by_ids(state.db(), &ids)
        .await
        .unwrap_or_default();
    let mut visible = std::collections::HashSet::new();
    for item in &items {
        if state
            .items()
            .check_access(item, "view", &viewer)
            .await
            .unwrap_or(false)
        {
            visible.insert(item.id);
        }
    }
    let mut author_ids: Vec<uuid::Uuid> = rows.iter().map(|r| r.author_id).collect();
    author_ids.sort_unstable();
    author_ids.dedup();
    let authors = state
        .users()
        .names_by_ids(&author_ids)
        .await
        .unwrap_or_default();
    let labels: std::collections::HashMap<String, String> = state
        .content_types()
        .list_all()
        .await
        .into_iter()
        .map(|ct| (ct.machine_name, ct.label))
        .collect();
    let listed: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| visible.contains(&r.id))
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "title": r.title,
                "type": labels.get(&r.item_type).unwrap_or(&r.item_type),
                "author": authors.get(&r.author_id).cloned().unwrap_or_else(|| {
                    crate::models::user::ANONYMOUS_DISPLAY_NAME.to_string()
                }),
                "state": moderation::label(&r.state),
                "ahead": r.ahead,
                "changed": chrono::DateTime::from_timestamp(r.changed, 0)
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
            })
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("items", &listed);
    context.insert("state_filter", &state_filter.unwrap_or(""));
    context.insert(
        "state_options",
        &[("draft", "Draft"), ("archived", "Archived")],
    );
    context.insert("path", "/admin/content/moderated");
    render_admin_template(&state, &viewer, "admin/content-moderated.html", context).await
}

/// Whether `viewer` may do `op` to `item`; a failed check is a no.
async fn allowed(
    state: &AppState,
    item: &crate::models::Item,
    op: &str,
    viewer: &crate::tap::UserContext,
) -> bool {
    matches!(state.items().check_access(item, op, viewer).await, Ok(true))
}

/// Select content type before adding.
///
/// GET /admin/content/add
async fn select_content_type(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = admin_user_context(&state, &user).await;

    // Drupal's chooser opens to whoever may create some type and lists those
    // types: each link leads to `/admin/content/add/{type}`, which asks for
    // `create {type} content`. `create content` still lists every type, as it
    // did; nobody who may create nothing is let in.
    let content_types: Vec<_> = state
        .content_types()
        .list_all()
        .await
        .into_iter()
        .filter(|t| {
            viewer.can("create content")
                || viewer.can(&format!("create {} content", t.machine_name))
        })
        .collect();
    if content_types.is_empty() && !viewer.can("create content") {
        return render_forbidden("You do not have permission to view this page.");
    }

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("path", "/admin/content/add");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/content-add-select.html",
        context,
    )
    .await
}

/// Show add content form.
///
/// GET /admin/content/add/{type}
async fn add_content_form(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    // The per-type permission `/item/add/{type}` already checks, so the two
    // ways into the same form agree. Gating this on `is_admin` was what made a
    // role holding a plugin's own content permissions unable to use the admin
    // UI to create that plugin's content.
    let user =
        match require_permission(&state, &session, &format!("create {type_name} content")).await {
            Ok(user) => user,
            Err(redirect) => return redirect,
        };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };

    let user_ctx = admin_user_context(&state, &user).await;
    render_content_form(&state, &session, &user_ctx, &content_type, None, None, &[]).await
}

/// Render the admin content form, add or edit, with whatever posted values
/// and errors the caller has.
///
/// One body for every caller — the GETs, a validation failure, and a write
/// the field or publish gate refused — so they cannot disagree about what the
/// page shows. The form itself comes from `item_form::build`, the pipeline
/// `/item/add` and `/item/{id}/edit` use too; this adds the admin chrome.
async fn render_content_form(
    state: &AppState,
    session: &Session,
    user_ctx: &crate::tap::UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    posted: Option<&PostedForm>,
    errors: &[crate::form::ValidationError],
) -> Response {
    render_content_form_page(
        state,
        session,
        user_ctx,
        content_type,
        item,
        posted,
        errors,
        None,
    )
    .await
}

/// The preview of a submission, above the admin content form shown again
/// with what was posted. Nothing is saved.
async fn render_content_preview(
    state: &AppState,
    session: &Session,
    user_ctx: &crate::tap::UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    posted: &PostedForm,
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Response {
    let title = posted.get("title").unwrap_or_default().trim().to_string();
    let preview = super::item_form::render_preview(
        state,
        user_ctx,
        content_type,
        &title,
        fields,
        "/admin/content",
    )
    .await;
    render_content_form_page(
        state,
        session,
        user_ctx,
        content_type,
        item,
        Some(posted),
        &[],
        Some(preview),
    )
    .await
}

/// [`render_content_form`], under `preview_html` when the page answers a
/// preview.
#[allow(clippy::too_many_arguments)]
async fn render_content_form_page(
    state: &AppState,
    session: &Session,
    user_ctx: &crate::tap::UserContext,
    content_type: &trovato_sdk::types::ContentTypeDefinition,
    item: Option<&crate::models::Item>,
    posted: Option<&PostedForm>,
    errors: &[crate::form::ValidationError],
    preview_html: Option<String>,
) -> Response {
    let type_name = &content_type.machine_name;
    let path = match item {
        Some(item) => format!("/admin/content/{}/edit", item.id),
        None => format!("/admin/content/add/{type_name}"),
    };
    let text = super::admin_nav::AdminText::for_page(state, user_ctx, &path).await;
    let mut extra = vec![(
        "cancel".to_string(),
        crate::form::FormElement::markup(format!(
            r#"<a href="/admin/content" class="button button--secondary">{}</a>"#,
            text.html("Cancel", &[])
        ))
        .weight(crate::content::SUBMIT_WEIGHT + 1),
    )];
    if let Some(item) = item {
        extra.extend(super::item_form::stage_element(state, user_ctx, item).await);
    }
    let mut form = match super::item_form::build(
        state,
        session,
        user_ctx,
        content_type,
        item,
        &path,
        extra,
        preview_html.is_some(),
    )
    .await
    {
        Ok(form) => form,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the content form");
            return render_server_error("Failed to build the form.");
        }
    };
    if let Some(posted) = posted {
        super::item_form::redisplay(&mut form, posted);
    }
    // In the viewer's admin language: the form templates' own words go
    // through `t`.
    let form_html = match text.scope(|| state.theme().render_form_with_errors(&form, errors)) {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to render the content form");
            return render_server_error("Failed to render the form.");
        }
    };

    let mut context = tera::Context::new();
    context.insert("editing", &item.is_some());
    context.insert("content_type", content_type);
    context.insert("form_html", &form_html);
    if let Some(preview_html) = &preview_html {
        context.insert("preview_html", preview_html);
    }
    context.insert("path", &path);
    if let Some(item) = item
        && let Some(lock) = super::item::take_edit_lock(state, user_ctx, item.id).await
    {
        context.insert("content_lock", &lock);
        // The token the lock script's first request sends (`<meta
        // name="csrf-token">`); each answer carries the next.
        context.insert(
            "csrf_token",
            &crate::form::csrf::generate_csrf_token(session).await,
        );
    }
    if let Some(item) = item {
        let item_id = item.id;
        // Local task tabs for item edit pages (hardcoded + plugin-registered)
        context.insert(
            "local_tasks",
            &build_local_tasks(
                state,
                "/admin/content/:id",
                &path,
                Some(&item_id.to_string()),
                vec![
                    serde_json::json!({"title": "View", "path": format!("/item/{item_id}"), "active": false}),
                    serde_json::json!({"title": "Edit", "path": &path, "active": true}),
                    serde_json::json!({"title": "Revisions", "path": format!("/item/{item_id}/revisions"), "active": false}),
                ],
            ),
        );
    }

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/content-form.html",
        context,
    )
    .await
}

/// Handle add content form submission.
///
/// POST /admin/content/add/{type}
async fn add_content_submit(
    State(state): State<AppState>,
    session: Session,
    Extension(resolved_lang): Extension<crate::middleware::language::ResolvedLanguage>,
    Path(type_name): Path<String>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    // Same per-type permission the GET above checks, and the same one
    // `/item/add/{type}` checks.
    let user =
        match require_permission(&state, &session, &format!("create {type_name} content")).await {
            Ok(user) => user,
            Err(redirect) => return redirect,
        };

    let Some(content_type) = state.content_types().get(&type_name) else {
        return render_not_found();
    };
    let user_ctx = admin_user_context(&state, &user).await;

    let (form_state, fields_json) =
        match super::item_form::process(&state, &session, &user_ctx, &content_type, None, &posted)
            .await
        {
            Ok((_, fields)) if super::item_form::wants_preview(&posted) => {
                return render_content_preview(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    &posted,
                    &fields,
                )
                .await;
            }
            Ok(processed) => processed,
            Err(crate::form::ProcessError::Csrf) => {
                return render_error("Invalid or expired form token. Please try again.");
            }
            Err(crate::form::ProcessError::Invalid(errors)) => {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    Some(&posted),
                    &errors,
                )
                .await;
            }
        };

    let publishing = match super::item_form::requested_publishing(&state, &posted).await {
        Ok(publishing) => publishing,
        Err(errors) => {
            return render_content_form(
                &state,
                &session,
                &user_ctx,
                &content_type,
                None,
                Some(&posted),
                &errors,
            )
            .await;
        }
    };

    let input = CreateItem {
        item_type: type_name.clone(),
        title: posted.get("title").unwrap_or_default().trim().to_string(),
        author_id: publishing.author_id.unwrap_or(user.id),
        status: requested_status(&posted),
        promote: publishing.promote,
        sticky: publishing.sticky,
        fields: Some(serde_json::Value::Object(fields_json)),
        stage_id: None,
        language: Some(resolved_lang.0),
        log: Some("Created via admin UI".to_string()),
    };

    match state
        .items()
        .create_moderated(
            input,
            publishing.created,
            requested_moderation_state(&posted),
            &user_ctx,
        )
        .await
    {
        Ok(item) => {
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item.id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;

            tracing::info!(item_id = %item.id, "content created");

            // Flash success message with link to edit the created item
            let msg = crate::i18n::UiMessage::new("{type} {title} has been created.")
                .arg("type", content_type.label.clone())
                .arg("title", item.title.clone())
                .link(format!("/admin/content/{}/edit", item.id), "Edit {title}");
            if let Err(e) = session.insert(CONTENT_FLASH_KEY, &msg).await {
                tracing::warn!(error = %e, "failed to set flash message");
            }

            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            // A write the field or publish gate or the field value check
            // refused is the user's to fix, so the form comes back with the
            // reason on it rather than a 500.
            if let Some(errors) = write_refusal(&e) {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    Some(&posted),
                    &errors,
                )
                .await;
            }
            let owned_terms = e
                .downcast_ref::<crate::content::owned_vocabulary::TermsNotOwned>()
                .map(|refused| {
                    refused
                        .0
                        .iter()
                        .map(|(field, message)| {
                            crate::form::ValidationError::field(field.clone(), message.clone())
                        })
                        .collect::<Vec<_>>()
                });
            let page_builder =
                e.downcast_ref::<crate::content::PageBuilderInvalid>()
                    .map(|invalid| {
                        invalid
                            .0
                            .iter()
                            .map(|(field, error)| {
                                crate::form::ValidationError::field(
                                    field.clone(),
                                    error.to_string(),
                                )
                            })
                            .collect::<Vec<_>>()
                    });
            if let Some(errors) = page_builder.or(owned_terms) {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    None,
                    Some(&posted),
                    &errors,
                )
                .await;
            }
            tracing::error!(error = %e, "failed to create content");
            render_server_error("Failed to create content.")
        }
    }
}

/// The form errors for a write the service refused for a reason the person at
/// the form can fix: a write gate ([`crate::content::WriteDenied`]) or the
/// field value check ([`crate::content::InvalidFieldValues`]), each value's
/// error beside its field. `None` for any other failure.
fn write_refusal(e: &anyhow::Error) -> Option<Vec<crate::form::ValidationError>> {
    if let Some(denied) = e.downcast_ref::<crate::content::WriteDenied>() {
        return Some(vec![crate::form::ValidationError::form(denied.to_string())]);
    }
    e.downcast_ref::<crate::content::InvalidFieldValues>()
        .map(|invalid| {
            invalid
                .0
                .iter()
                .map(|v| crate::form::ValidationError::field(&v.field, v.message.clone()))
                .collect()
        })
}

/// Show edit content form.
///
/// GET /admin/content/{id}/edit
async fn edit_content_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
) -> Response {
    let user = match require_permission(&state, &session, "edit any content").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };

    let user_ctx = admin_user_context(&state, &user).await;

    // `edit any content` admits a user to the screen; whether they may edit
    // this item is the access seam's decision, a plugin's Deny included. Asked
    // before anything of the item is rendered, and answered as the submit is.
    match state.items().check_access(&item, "edit", &user_ctx).await {
        Ok(true) => {}
        Ok(false) => return render_forbidden("You may not edit this item."),
        Err(e) => {
            tracing::error!(error = %e, item_id = %item_id, "edit access check failed");
            return render_server_error("Could not check access to this item.");
        }
    }

    render_content_form(
        &state,
        &session,
        &user_ctx,
        &content_type,
        Some(&item),
        None,
        &[],
    )
    .await
}

/// The status a submission asks for, or `None` when it said nothing.
///
/// An unchecked checkbox posts nothing, so the form stamps `_status_rendered`
/// beside the published control it renders for a user who may publish.
/// Without the marker an absent `status` and a form that never offered one
/// are the same request, and `None` goes to the service as "leave it".
fn requested_status(posted: &PostedForm) -> Option<i16> {
    posted
        .contains("_status_rendered")
        .then(|| i16::from(posted.contains("status")))
}

/// The content moderation state a submission asks for, on a moderated type;
/// `None` when the form offered no choice.
fn requested_moderation_state(posted: &PostedForm) -> Option<&str> {
    posted.get("moderation_state").filter(|s| !s.is_empty())
}

/// The stage a submission asks to move the item to: `Ok(None)` when it asked
/// for none, `Err` when the value is not a stage id. Whether the move is
/// allowed is `ItemService::update`'s decision.
fn requested_stage(posted: &PostedForm) -> Result<Option<uuid::Uuid>, ()> {
    match posted.get("_stage").map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => uuid::Uuid::parse_str(value).map(Some).map_err(|_| ()),
    }
}

/// Handle edit content form submission.
///
/// POST /admin/content/{id}/edit
async fn edit_content_submit(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "edit any content").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return render_not_found();
    };

    let Some(content_type) = state.content_types().get(&item.item_type) else {
        return render_server_error("Content type not found.");
    };
    let user_ctx = admin_user_context(&state, &user).await;

    // Built over the stored fields from the list the form rendered, so a key
    // it did not show is kept rather than erased.
    let (form_state, fields_json) = match super::item_form::process(
        &state,
        &session,
        &user_ctx,
        &content_type,
        Some(&item),
        &posted,
    )
    .await
    {
        Ok((_, fields)) if super::item_form::wants_preview(&posted) => {
            return render_content_preview(
                &state,
                &session,
                &user_ctx,
                &content_type,
                Some(&item),
                &posted,
                &fields,
            )
            .await;
        }
        Ok(processed) => processed,
        Err(crate::form::ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        Err(crate::form::ProcessError::Invalid(errors)) => {
            return render_content_form(
                &state,
                &session,
                &user_ctx,
                &content_type,
                Some(&item),
                Some(&posted),
                &errors,
            )
            .await;
        }
    };

    let Ok(stage_id) = requested_stage(&posted) else {
        return render_error("The form could not be read.");
    };
    let publishing = match super::item_form::requested_publishing(&state, &posted).await {
        Ok(publishing) => publishing,
        Err(errors) => {
            return render_content_form(
                &state,
                &session,
                &user_ctx,
                &content_type,
                Some(&item),
                Some(&posted),
                &errors,
            )
            .await;
        }
    };

    let input = crate::models::UpdateItem {
        author_id: publishing.author_id,
        created: publishing.created,
        stage_id,
        title: posted.get("title").map(|t| t.trim().to_string()),
        status: requested_status(&posted),
        promote: publishing.promote,
        sticky: publishing.sticky,
        fields: Some(serde_json::Value::Object(fields_json)),
        log: Some("Updated via admin UI".to_string()),
    };

    // Another editor's lock refuses the save, on the form.
    if super::item::locked_by_other(&state, &user_ctx, item_id).await {
        let mut response = render_content_form(
            &state,
            &session,
            &user_ctx,
            &content_type,
            Some(&item),
            Some(&posted),
            &[crate::form::ValidationError::form(
                super::item::LOCKED_MESSAGE.to_string(),
            )],
        )
        .await;
        *response.status_mut() = axum::http::StatusCode::CONFLICT;
        return response;
    }

    match state
        .items()
        .update_moderated(
            item_id,
            input,
            requested_moderation_state(&posted),
            &user_ctx,
        )
        .await
    {
        Ok(updated) => {
            super::item::release_edit_lock(&state, &user_ctx, item_id).await;
            state
                .forms()
                .submitted(
                    &form_state,
                    &posted,
                    Some(item_id),
                    &super::item_form::tap_state(&state, &user_ctx),
                )
                .await;

            tracing::info!(item_id = %item_id, "content updated");

            if let Some(ref updated_item) = updated {
                let msg = crate::i18n::UiMessage::new("{type} {title} has been updated.")
                    .arg("type", content_type.label.clone())
                    .arg("title", updated_item.title.clone())
                    .link(format!("/admin/content/{item_id}/edit"), "Edit {title}");
                if let Err(e) = session.insert(CONTENT_FLASH_KEY, &msg).await {
                    tracing::warn!(error = %e, "failed to set flash message");
                }
            }

            Redirect::to("/admin/content").into_response()
        }
        Err(e) => {
            // As on the add path: a refusal belongs on the form, not in a 500.
            if let Some(errors) = write_refusal(&e) {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    Some(&item),
                    Some(&posted),
                    &errors,
                )
                .await;
            }
            let owned_terms = e
                .downcast_ref::<crate::content::owned_vocabulary::TermsNotOwned>()
                .map(|refused| {
                    refused
                        .0
                        .iter()
                        .map(|(field, message)| {
                            crate::form::ValidationError::field(field.clone(), message.clone())
                        })
                        .collect::<Vec<_>>()
                });
            let page_builder =
                e.downcast_ref::<crate::content::PageBuilderInvalid>()
                    .map(|invalid| {
                        invalid
                            .0
                            .iter()
                            .map(|(field, error)| {
                                crate::form::ValidationError::field(
                                    field.clone(),
                                    error.to_string(),
                                )
                            })
                            .collect::<Vec<_>>()
                    });
            if let Some(errors) = page_builder.or(owned_terms) {
                return render_content_form(
                    &state,
                    &session,
                    &user_ctx,
                    &content_type,
                    Some(&item),
                    Some(&posted),
                    &errors,
                )
                .await;
            }
            // The item-level refusal from `ItemService::update`: a 403, as the
            // form answers, rather than a 500.
            if e.to_string().contains("access denied") {
                return render_forbidden("You may not edit this item.");
            }
            tracing::error!(error = %e, "failed to update content");
            render_server_error("Failed to update content.")
        }
    }
}

/// Who may delete item `id`, and the item: `delete own content` is enough to
/// come in, as on Drupal; whether this item may go is the item's own access
/// check, asked before the delete so a refusal is a 403 and not a failed
/// write. The confirm page and the delete itself both start here.
async fn delete_gate(
    state: &AppState,
    session: &Session,
    item_id: uuid::Uuid,
) -> Result<
    (
        crate::models::User,
        crate::tap::UserContext,
        crate::models::Item,
    ),
    Response,
> {
    let user = require_any_permission(
        state,
        session,
        &["delete any content", "delete own content"],
    )
    .await?;
    let user_ctx = admin_user_context(state, &user).await;
    let Some(item) = state.items().load(item_id).await.ok().flatten() else {
        return Err(render_not_found());
    };
    if !allowed(state, &item, "delete", &user_ctx).await {
        return Err(render_forbidden(
            "You do not have permission to delete this content.",
        ));
    }
    Ok((user, user_ctx, item))
}

/// Confirm deleting one item: what the content list's Delete link opens, so
/// the delete is never one click away.
///
/// GET /admin/content/{id}/delete
async fn delete_content_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
) -> Response {
    let (user, viewer, item) = match delete_gate(&state, &session, item_id).await {
        Ok(gated) => gated,
        Err(refusal) => return refusal,
    };
    let path = format!("/admin/content/{item_id}/delete");
    let text = super::admin_nav::AdminText::for_page(&state, &viewer, &path).await;
    let form = delete_form_for(item_id, &text.html("Cancel", &[]));
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("form", &form_html);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/content-delete.html", context).await
}

/// Delete content.
///
/// POST /admin/content/{id}/delete, from the confirm page's form.
async fn delete_content(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<uuid::Uuid>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let (user, user_ctx, _item) = match delete_gate(&state, &session, item_id).await {
        Ok(gated) => gated,
        Err(refusal) => return refusal,
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
    match state.items().delete(item_id, &user_ctx).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(item_id = %item_id, "content deleted");
            if let Err(e) = session
                .insert(CONTENT_FLASH_KEY, "Content has been deleted.")
                .await
            {
                tracing::warn!(error = %e, "failed to set flash message");
            }
            Redirect::to("/admin/content").into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete content");
            render_server_error("Failed to delete content.")
        }
    }
}

/// Bulk action form data, read from the body the content list screen posts.
///
/// The screen posts one `ids[]` pair per checked box. That shape is read by
/// [`BulkActionForm::parse`] rather than by `axum::Form`: `Form` deserializes
/// with `serde_urlencoded`, which does not gather repeated keys into a
/// sequence, so a `Vec` field fed by `ids[]` refused every request that
/// selected anything, with a 422, before the handler ran.
#[derive(Debug, PartialEq)]
struct BulkActionForm {
    action: String,
    ids: Vec<uuid::Uuid>,
    /// The target of `move_to_stage`, as sent; read only by that action.
    stage: Option<String>,
}

/// Why a bulk action body was not turned into a [`BulkActionForm`].
#[derive(Debug, PartialEq)]
enum BulkFormError {
    /// `_token` or `action` is missing or appears more than once. Refused with
    /// a 400 before any permission lookup: there is no single action to gate.
    Malformed(&'static str),
    /// An `ids[]` value is not a UUID. The action is carried so the handler
    /// can still run its permission and form checks before refusing the
    /// whole selection.
    InvalidId { action: String, value: String },
}

impl BulkActionForm {
    /// Parse an `application/x-www-form-urlencoded` bulk action body.
    ///
    /// `_token` and `action` must each appear exactly once (the same rule
    /// [`single_form_field`] applies to the CSRF token everywhere else); the
    /// token itself is checked by the form pipeline with the rest of the
    /// submission. Every
    /// `ids[]` value must be a UUID; a repeated id is kept once, at the
    /// position it was first seen. No `ids[]` at all is an empty selection.
    fn parse(body: &str) -> Result<Self, BulkFormError> {
        single_form_field(body, "_token")
            .ok_or(BulkFormError::Malformed("missing or repeated _token"))?;
        let action = single_form_field(body, "action")
            .ok_or(BulkFormError::Malformed("missing or repeated action"))?;

        let mut ids: Vec<uuid::Uuid> = Vec::new();
        for (key, value) in url::form_urlencoded::parse(body.as_bytes()) {
            if key != "ids[]" {
                continue;
            }
            let Ok(id) = uuid::Uuid::parse_str(&value) else {
                return Err(BulkFormError::InvalidId {
                    action,
                    value: value.into_owned(),
                });
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let stage = single_form_field(body, "stage");
        Ok(Self { action, ids, stage })
    }
}

/// The bulk actions that set an item's promote or sticky flag.
const PROMOTION_ACTIONS: &[&str] = &["promote", "demote", "sticky", "unsticky"];

/// Bulk operations on content items.
///
/// POST /admin/content/bulk
async fn bulk_content_action(
    State(state): State<AppState>,
    session: Session,
    body: axum::body::Bytes,
) -> Response {
    let Ok(body) = std::str::from_utf8(&body) else {
        return render_error("The form could not be read.");
    };
    // An unreadable id does not refuse the request here: the permission and
    // form checks still run first, and the selection is refused below them,
    // before any write.
    let (form, invalid_id) = match BulkActionForm::parse(body) {
        Ok(form) => (form, None),
        Err(BulkFormError::InvalidId { action, value }) => (
            BulkActionForm {
                action,
                ids: Vec::new(),
                stage: None,
            },
            Some(value),
        ),
        Err(BulkFormError::Malformed(reason)) => {
            tracing::debug!(reason, "bulk action body refused");
            return render_error("The form could not be read.");
        }
    };

    // A bulk action is gated on the permissions for the action it performs, not
    // on one permission for the whole screen: bulk delete is a delete, and a
    // role that may publish must not get a delete for free by routing it
    // through this endpoint. Bulk publish and unpublish each change an item's
    // published state, so they need `publish content` on top of the edit
    // permission — the same answer `ItemService::gate_publish` would give one
    // item at a time, asked once here so the screen refuses rather than
    // reporting every item as failed. A move to another stage is an edit, and
    // each item's move is then decided by the workflow in
    // `ItemService::update`. An unrecognized action asks for the edit
    // permission and is then rejected as unknown below.
    let required = bulk_permissions(&form.action);
    let mut holder = None;
    for permissions in required {
        match require_any_permission(&state, &session, permissions).await {
            Ok(user) => holder = Some(user),
            Err(redirect) => return redirect,
        }
    }
    let Some(user) = holder else {
        // Unreachable: every arm above names at least one permission.
        return render_server_error("No permission to check for this action.");
    };
    // Promote and sticky are set by a holder of `publish content` or
    // `administer content`, the answer `ItemService::gate_promotion` gives one
    // item at a time, asked once here for the same reason as publish above.
    if PROMOTION_ACTIONS.contains(&form.action.as_str()) {
        let ctx = admin_user_context(&state, &user).await;
        if !(ctx.can(crate::content::PUBLISH_CONTENT)
            || ctx.can(crate::content::ADMINISTER_CONTENT))
        {
            return render_forbidden("You may not promote items or make them sticky.");
        }
    }
    // The form token and the saved state of the list's bulk form. The
    // confirmation page rebuilds a bulk delete under this form's id, so a
    // confirmed delete passes here too.
    let posted = PostedForm(
        url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect(),
    );
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, BULK_FORM_ID, Vec::new()).await {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return render_error("The form has expired. Reload the page and try again.");
            }
            Err(Err(response)) => return response,
        };

    // Validate action before processing
    if !matches!(
        form.action.as_str(),
        "publish" | "unpublish" | "delete" | "move_to_stage"
    ) && !PROMOTION_ACTIONS.contains(&form.action.as_str())
    {
        if let Err(e) = session
            .insert(
                CONTENT_FLASH_KEY,
                crate::i18n::UiMessage::new("Unknown action: {action}")
                    .arg("action", form.action.chars().take(64).collect::<String>())
                    .error(),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to set flash message");
        }
        return Redirect::to("/admin/content").into_response();
    }

    // Never act on part of a selection the browser did not send cleanly. The
    // value is echoed escaped and cut short: it is whatever the client sent.
    if let Some(value) = invalid_id {
        let shown: String = value.chars().take(64).collect();
        if let Err(e) = session
            .insert(
                CONTENT_FLASH_KEY,
                crate::i18n::UiMessage::new(
                    "Nothing was changed: \"{value}\" is not a valid item id.",
                )
                .arg("value", shown)
                .error(),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to set flash message");
        }
        return Redirect::to("/admin/content").into_response();
    }

    if form.ids.is_empty() {
        if let Err(e) = session
            .insert(
                CONTENT_FLASH_KEY,
                crate::i18n::UiMessage::new("No items selected.").error(),
            )
            .await
        {
            tracing::warn!(error = %e, "failed to set flash message");
        }
        return Redirect::to("/admin/content").into_response();
    }

    // A move needs a target, and the target must be a stage id. Refused whole,
    // before any write; whether the stage exists, and whether this user may
    // move each item there, is the service's decision per item.
    let target_stage = if form.action == "move_to_stage" {
        match form
            .stage
            .as_deref()
            .and_then(|v| uuid::Uuid::parse_str(v.trim()).ok())
        {
            Some(stage) => Some(stage),
            None => {
                if let Err(e) = session
                    .insert(
                        CONTENT_FLASH_KEY,
                        crate::i18n::UiMessage::new("Nothing was moved: choose a stage.").error(),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "failed to set flash message");
                }
                return Redirect::to("/admin/content").into_response();
            }
        }
    } else {
        None
    };

    let user_ctx = admin_user_context(&state, &user).await;

    let mut success_count = 0u32;
    let mut fail_count = 0u32;
    match form.action.as_str() {
        "publish" | "unpublish" => {
            let new_status: i16 = if form.action == "publish" { 1 } else { 0 };
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: None,
                    title: None,
                    status: Some(new_status),
                    fields: None,
                    promote: None,
                    sticky: None,
                    log: Some(format!("Bulk {}", form.action)),
                };
                // `Ok(None)` is an id that names no item: a failure, as the
                // delete arm's `Ok(false)` is, not a success.
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(Some(_)) => success_count += 1,
                    Ok(None) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk action failed");
                        fail_count += 1;
                    }
                }
            }
        }
        "promote" | "demote" | "sticky" | "unsticky" => {
            let (promote, sticky) = match form.action.as_str() {
                "promote" => (Some(1), None),
                "demote" => (Some(0), None),
                "sticky" => (None, Some(1)),
                _ => (None, Some(0)),
            };
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: None,
                    title: None,
                    status: None,
                    fields: None,
                    promote,
                    sticky,
                    log: Some(format!("Bulk {}", form.action)),
                };
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(Some(_)) => success_count += 1,
                    Ok(None) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk action failed");
                        fail_count += 1;
                    }
                }
            }
        }
        "move_to_stage" => {
            for id in &form.ids {
                let update = crate::models::UpdateItem {
                    author_id: None,
                    created: None,
                    stage_id: target_stage,
                    title: None,
                    status: None,
                    fields: None,
                    promote: None,
                    sticky: None,
                    log: Some("Bulk move to stage".to_string()),
                };
                match state.items().update(*id, update, &user_ctx).await {
                    Ok(Some(_)) => success_count += 1,
                    Ok(None) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk move failed");
                        fail_count += 1;
                    }
                }
            }
        }
        "delete" => {
            for id in &form.ids {
                match state.items().delete(*id, &user_ctx).await {
                    Ok(true) => success_count += 1,
                    Ok(false) => fail_count += 1,
                    Err(e) => {
                        tracing::warn!(item_id = %id, error = %e, "bulk delete failed");
                        fail_count += 1;
                    }
                }
            }
        }
        _ => unreachable!(), // Validated above
    }

    // One whole sentence per action, singular and plural, so each can be
    // translated; the failures are a second sentence of their own.
    let (one, many) = match form.action.as_str() {
        "publish" => ("{count} item published.", "{count} items published."),
        "unpublish" => ("{count} item unpublished.", "{count} items unpublished."),
        "delete" => ("{count} item deleted.", "{count} items deleted."),
        "move_to_stage" => ("{count} item moved.", "{count} items moved."),
        "promote" => (
            "{count} item promoted to the front page.",
            "{count} items promoted to the front page.",
        ),
        "demote" => (
            "{count} item removed from the front page.",
            "{count} items removed from the front page.",
        ),
        "sticky" => ("{count} item made sticky.", "{count} items made sticky."),
        "unsticky" => (
            "{count} item made not sticky.",
            "{count} items made not sticky.",
        ),
        _ => unreachable!(),
    };
    let msg = if fail_count > 0 {
        // The failures follow as a number: "2 items published. Failed: 1."
        crate::i18n::UiMessage::new(format!("{one} Failed: {{failed}}."))
            .plural(
                format!("{many} Failed: {{failed}}."),
                i64::from(success_count),
            )
            .arg("failed", fail_count.to_string())
            .error()
    } else {
        crate::i18n::UiMessage::new(one).plural(many, i64::from(success_count))
    };
    if let Err(e) = session.insert(CONTENT_FLASH_KEY, msg).await {
        tracing::warn!(error = %e, "failed to set flash message");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;

    Redirect::to("/admin/content").into_response()
}

/// Build admin content routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/content", get(list_content))
        .route("/admin/content/moderated", get(list_moderated))
        .route("/admin/content/add", get(select_content_type))
        .route(
            "/admin/content/add/{type}",
            get(add_content_form).post(add_content_submit),
        )
        .route(
            "/admin/content/{id}/edit",
            get(edit_content_form).post(edit_content_submit),
        )
        .route(
            "/admin/content/{id}/delete",
            get(delete_content_form).post(delete_content),
        )
        .route("/admin/content/bulk", post(bulk_content_action))
}

/// The permission sets a bulk action needs: one of each set must be held. The
/// "own" permissions admit an author to act on their own items, as on Drupal;
/// every item is then checked by the service, so an author's selection of
/// someone else's item fails for that item and changes nothing.
fn bulk_permissions(action: &str) -> &'static [&'static [&'static str]] {
    const EDIT: &[&str] = &["edit any content", "edit own content"];
    const DELETE: &[&str] = &["delete any content", "delete own content"];
    const PUBLISH: &[&str] = &[crate::content::PUBLISH_CONTENT];
    match action {
        "delete" => &[DELETE],
        "publish" | "unpublish" => &[EDIT, PUBLISH],
        _ => &[EDIT],
    }
}

/// Whether the content list offers a bulk action to this viewer: the
/// permissions `bulk_permissions` names, and for promote and sticky the
/// publish or administer permission the bulk handler also asks for.
fn bulk_action_offered(viewer: &crate::tap::UserContext, action: &str) -> bool {
    let holds = bulk_permissions(action)
        .iter()
        .all(|set| set.iter().any(|p| viewer.can(p)));
    if PROMOTION_ACTIONS.contains(&action) {
        return holds
            && (viewer.can(crate::content::PUBLISH_CONTENT)
                || viewer.can(crate::content::ADMINISTER_CONTENT));
    }
    holds
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const A: &str = "0190a0b0-0000-7000-8000-00000000000a";
    const B: &str = "0190a0b0-0000-7000-8000-00000000000b";
    const C: &str = "0190a0b0-0000-7000-8000-00000000000c";

    fn uuid(s: &str) -> uuid::Uuid {
        uuid::Uuid::parse_str(s).unwrap()
    }

    #[test]
    fn several_ids_are_read_in_the_order_they_were_sent() {
        let body = format!("_token=t&action=publish&ids%5B%5D={B}&ids%5B%5D={A}&ids[]={C}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.ids, vec![uuid(B), uuid(A), uuid(C)]);
        assert_eq!(form.action, "publish");
    }

    #[test]
    fn a_token_with_reserved_characters_is_read_once() {
        // `+`, `/` and `=` percent encoded, and a literal `+` for a space, as a
        // browser encodes the form.
        let body = format!("_token=a%2Bb%2Fc%3D%3D+d&action=delete&ids%5B%5D={A}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.action, "delete");
        assert_eq!(form.ids, vec![uuid(A)]);
    }

    #[test]
    fn an_unreadable_id_is_an_error_that_keeps_the_token_and_action() {
        let body = format!("_token=t&action=publish&ids%5B%5D={A}&ids%5B%5D=not-a-uuid");
        assert_eq!(
            BulkActionForm::parse(&body),
            Err(BulkFormError::InvalidId {
                action: "publish".to_string(),
                value: "not-a-uuid".to_string(),
            })
        );
    }

    #[test]
    fn a_repeated_id_is_kept_once_at_its_first_position() {
        let body = format!("_token=t&action=publish&ids[]={A}&ids[]={B}&ids[]={A}");
        let form = BulkActionForm::parse(&body).unwrap();
        assert_eq!(form.ids, vec![uuid(A), uuid(B)]);
    }

    #[test]
    fn a_repeated_token_or_action_is_malformed() {
        assert!(matches!(
            BulkActionForm::parse("_token=t&_token=t&action=publish"),
            Err(BulkFormError::Malformed(_))
        ));
        assert!(matches!(
            BulkActionForm::parse("_token=t&action=publish&action=delete"),
            Err(BulkFormError::Malformed(_))
        ));
    }

    #[test]
    fn a_missing_token_or_action_is_malformed() {
        assert!(matches!(
            BulkActionForm::parse("action=publish"),
            Err(BulkFormError::Malformed(_))
        ));
        assert!(matches!(
            BulkActionForm::parse("_token=t"),
            Err(BulkFormError::Malformed(_))
        ));
    }

    #[test]
    fn no_ids_is_an_empty_selection() {
        let form = BulkActionForm::parse("_token=t&action=unpublish").unwrap();
        assert!(form.ids.is_empty());
    }
}
