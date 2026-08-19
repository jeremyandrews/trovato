//! Admin routes for menu link management.
//!
//! A site's navigation lives in `menu_link` rows, which the render layer reads
//! per request (`routes/helpers::inject_site_context`). Until these routes
//! existed the only way to write one was `trovato config import`, which meant
//! editing YAML and running a CLI to change a link in a navigation bar. That
//! fails the standard 1.0 is held to: a site should be configurable through the
//! interface.
//!
//! ## What this screen owns, and what it does not
//!
//! It edits `menu_link` rows and nothing else. Plugin-registered navigation
//! (`tap_menu`) is not rows at all — it lives in the in-memory
//! [`MenuRegistry`](crate::menu::MenuRegistry), rebuilt from the plugins at
//! startup — so it is listed read-only, attributed to its plugin, with no edit
//! or delete affordance. A `menu_link` row that some plugin inserted and stamped
//! with its own name is treated the same way: shown, labelled, not editable
//! here. `core` is what this form writes and what it will edit.
//!
//! ## No cache to invalidate
//!
//! An edit is visible on the next request without a restart, and that needs no
//! plumbing: `inject_site_context` queries `menu_link` for every render rather
//! than reading a registry built at startup. The startup-built registry holds
//! the plugin-registered half, which this screen does not touch.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::models::stage::LIVE_STAGE_ID;
use crate::models::{CreateMenuLink, MenuLink, UpdateMenuLink};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_not_found, render_server_error, require_admin,
    require_csrf,
};

/// The plugin name a link this form owns carries.
///
/// `menu_link.plugin` defaults to `'core'`, and a row wearing anything else was
/// put there by a plugin, so this form shows it and leaves it alone.
const CORE_PLUGIN: &str = "core";

/// Menus the theme renders, so they are offered even when empty.
///
/// `templates/page.html` reads exactly these two context variables (`main_menu`,
/// `footer_menu`). A site can hold links under any other name and this screen
/// will list that menu, but it cannot invent a place for the theme to render it,
/// so these two are the ones it always offers.
const THEME_MENUS: [&str; 2] = ["main", "footer"];

/// How deep the indented tree renders before it stops descending.
///
/// A guard rather than a design limit: cycles are rejected on save, and this
/// keeps a row that predates that rejection (or arrived through config import
/// against an older kernel) from rendering forever.
const MAX_TREE_DEPTH: usize = 16;

// =============================================================================
// Form data
// =============================================================================

/// The add/edit form's fields.
///
/// `hidden` is an HTML checkbox, which submits nothing at all when unchecked, so
/// it is an `Option` and absence means false.
#[derive(Debug, Deserialize)]
struct MenuLinkFormData {
    #[serde(rename = "_token")]
    token: String,
    title: String,
    path: String,
    /// The parent link's UUID, or empty for a top-level link.
    parent_id: Option<String>,
    weight: Option<i32>,
    hidden: Option<String>,
}

// =============================================================================
// Display structs
// =============================================================================

/// One menu on the index.
#[derive(Debug, Serialize)]
struct MenuSummary {
    name: String,
    links: usize,
    /// Links this form can edit, i.e. those owned by `core`.
    editable: usize,
    /// Whether the theme renders this menu.
    rendered: bool,
}

/// One row of the indented tree.
#[derive(Debug, Serialize)]
struct TreeRow {
    id: Uuid,
    title: String,
    path: String,
    weight: i32,
    hidden: bool,
    plugin: String,
    /// Nesting depth, 0 for a top-level link. Drives the indent.
    depth: usize,
    /// Whether this form may edit and delete the row.
    editable: bool,
    /// How many children the row has, so delete can say what happens to them.
    children: usize,
}

/// One option in the parent select.
#[derive(Debug, Serialize)]
struct ParentOption {
    id: Uuid,
    /// Title prefixed with depth markers, so the select reads as a tree.
    label: String,
}

/// One plugin-registered navigation entry, shown read-only.
#[derive(Debug, Serialize)]
struct PluginEntry {
    path: String,
    title: String,
    plugin: String,
    parent: Option<String>,
    weight: i32,
}

// =============================================================================
// Path validation
// =============================================================================

/// Reject anything that is not a local absolute path.
///
/// A menu link's path is emitted into an `href`, so an off-site or scheme-bearing
/// value turns a site's own navigation into an outbound link or worse. The rule
/// is deliberately narrow: one leading slash, no scheme, no protocol-relative
/// `//host`, no traversal, no control characters or whitespace.
///
/// What this does **not** do is check that the path resolves. The kernel's router
/// is an axum `Router`, which cannot be enumerated, and half the paths a site
/// wants in a menu are content aliases created after the link is. A path that
/// resolves to nothing is a 404 when someone clicks it, which is a thing a form
/// cannot know at save time and should not pretend to.
fn validate_path(path: &str) -> Result<String, String> {
    let path = path.trim();

    if path.is_empty() {
        return Err("Path is required.".to_string());
    }
    // Scheme and protocol-relative first, so `https://example.com/` is reported as
    // the off-site URL it is rather than as a missing leading slash. Both fail
    // either way; only the message differs, and the message is the whole point of
    // rejecting at the form instead of at the database.
    if path.contains("://") {
        return Err("Path must be local, not a full URL.".to_string());
    }
    if path.starts_with("//") {
        return Err(
            "Path must not begin with '//': that is a protocol-relative URL pointing off site."
                .to_string(),
        );
    }
    if !path.starts_with('/') {
        return Err(
            "Path must be a local absolute path beginning with '/', for example /about."
                .to_string(),
        );
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err("Path must not contain '..'.".to_string());
    }
    if path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("Path must not contain whitespace or control characters.".to_string());
    }
    if path.len() > 512 {
        return Err("Path must be 512 characters or fewer.".to_string());
    }

    Ok(path.to_string())
}

/// Parse the submitted parent field: absent or empty means top level.
fn parse_parent(raw: Option<&str>) -> Result<Option<Uuid>, String> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => value
            .parse::<Uuid>()
            .map(Some)
            .map_err(|_| "Parent is not a valid menu link.".to_string()),
    }
}

// =============================================================================
// Tree helpers
// =============================================================================

/// Order links depth-first by (weight, title) so the listing reads as a tree.
///
/// Rows whose parent is not in this menu are treated as top level: a parent in a
/// different menu cannot be rendered as an ancestor here, and dropping the row
/// entirely would hide it from the only screen that can fix it.
fn build_tree(links: &[MenuLink]) -> Vec<TreeRow> {
    let ids: std::collections::HashSet<Uuid> = links.iter().map(|l| l.id).collect();
    let mut child_count: std::collections::HashMap<Uuid, usize> = std::collections::HashMap::new();
    for link in links {
        if let Some(parent) = link.parent_id
            && ids.contains(&parent)
        {
            *child_count.entry(parent).or_insert(0) += 1;
        }
    }

    let mut rows = Vec::with_capacity(links.len());
    let mut emitted: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

    fn children_of<'a>(
        links: &'a [MenuLink],
        ids: &std::collections::HashSet<Uuid>,
        parent: Option<Uuid>,
    ) -> Vec<&'a MenuLink> {
        let mut kids: Vec<&MenuLink> = links
            .iter()
            .filter(|l| match l.parent_id {
                Some(p) if ids.contains(&p) => parent == Some(p),
                // Parent missing from this menu, or absent: a root.
                _ => parent.is_none(),
            })
            .collect();
        kids.sort_by(|a, b| a.weight.cmp(&b.weight).then_with(|| a.title.cmp(&b.title)));
        kids
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        links: &[MenuLink],
        ids: &std::collections::HashSet<Uuid>,
        child_count: &std::collections::HashMap<Uuid, usize>,
        parent: Option<Uuid>,
        depth: usize,
        emitted: &mut std::collections::HashSet<Uuid>,
        rows: &mut Vec<TreeRow>,
    ) {
        if depth > MAX_TREE_DEPTH {
            return;
        }
        for link in children_of(links, ids, parent) {
            if !emitted.insert(link.id) {
                continue;
            }
            rows.push(TreeRow {
                id: link.id,
                title: link.title.clone(),
                path: link.path.clone(),
                weight: link.weight,
                hidden: link.hidden,
                plugin: link.plugin.clone(),
                depth,
                editable: link.plugin == CORE_PLUGIN,
                children: child_count.get(&link.id).copied().unwrap_or(0),
            });
            walk(
                links,
                ids,
                child_count,
                Some(link.id),
                depth + 1,
                emitted,
                rows,
            );
        }
    }

    walk(links, &ids, &child_count, None, 0, &mut emitted, &mut rows);

    // Anything the walk could not reach (a pre-existing cycle) is still listed,
    // flat, rather than silently dropped from the only screen that can fix it.
    for link in links {
        if emitted.contains(&link.id) {
            continue;
        }
        rows.push(TreeRow {
            id: link.id,
            title: link.title.clone(),
            path: link.path.clone(),
            weight: link.weight,
            hidden: link.hidden,
            plugin: link.plugin.clone(),
            depth: 0,
            editable: link.plugin == CORE_PLUGIN,
            children: child_count.get(&link.id).copied().unwrap_or(0),
        });
    }

    rows
}

/// Parent options for the select, excluding `exclude` and its descendants.
///
/// Excluding the subtree is what makes a cycle unreachable from the form rather
/// than merely rejected by it: a link cannot be offered its own child as parent.
fn parent_options(links: &[MenuLink], exclude: Option<Uuid>) -> Vec<ParentOption> {
    let excluded = match exclude {
        Some(id) => descendants_of(links, id),
        None => std::collections::HashSet::new(),
    };

    build_tree(links)
        .into_iter()
        .filter(|row| !excluded.contains(&row.id))
        .map(|row| ParentOption {
            id: row.id,
            label: format!("{}{}", "— ".repeat(row.depth), row.title),
        })
        .collect()
}

/// `id` and everything under it.
fn descendants_of(links: &[MenuLink], id: Uuid) -> std::collections::HashSet<Uuid> {
    let mut found = std::collections::HashSet::new();
    found.insert(id);
    // Repeat until nothing new is added: the set grows by at most one row per
    // pass, so this terminates even on a cyclic table.
    loop {
        let before = found.len();
        for link in links {
            if let Some(parent) = link.parent_id
                && found.contains(&parent)
            {
                found.insert(link.id);
            }
        }
        if found.len() == before {
            break;
        }
    }
    found
}

/// Whether making `candidate` the parent of `link_id` would create a cycle.
///
/// Walks up from the candidate. Reaching `link_id` means the candidate is a
/// descendant, so the link would become its own ancestor.
async fn would_cycle(
    state: &AppState,
    link_id: Uuid,
    candidate: Uuid,
) -> Result<bool, anyhow::Error> {
    if candidate == link_id {
        return Ok(true);
    }
    let mut seen = std::collections::HashSet::new();
    seen.insert(link_id);
    let mut cursor = Some(candidate);
    while let Some(current) = cursor {
        if !seen.insert(current) {
            // A cycle that already existed. Refusing the edit is right either
            // way, and it is not this request's to repair.
            return Ok(true);
        }
        let Some(row) = MenuLink::find_by_id(state.db(), current).await? else {
            return Ok(false);
        };
        if row.parent_id == Some(link_id) {
            return Ok(true);
        }
        cursor = row.parent_id;
    }
    Ok(false)
}

/// Load a menu's links on the Live stage, ordered by weight then title.
async fn menu_links(state: &AppState, menu_name: &str) -> Result<Vec<MenuLink>, anyhow::Error> {
    MenuLink::find_by_menu_and_stage(state.db(), menu_name, LIVE_STAGE_ID).await
}

/// Plugin-registered navigation, sorted for a deterministic listing.
///
/// One row per path (`by_path`) rather than one per registration (`all`): a path
/// a plugin serves for both `GET` and `POST` is one page in a listing that has no
/// method column, and showing it twice would read as a duplicate rather than as
/// a form.
fn plugin_entries(state: &AppState) -> Vec<PluginEntry> {
    let mut entries: Vec<PluginEntry> = state
        .menu_registry()
        .by_path()
        .map(|menu| PluginEntry {
            path: menu.path.clone(),
            title: menu.title.clone(),
            plugin: menu.plugin.clone(),
            parent: menu.parent.clone(),
            weight: menu.weight,
        })
        .collect();
    entries.sort_by(|a, b| a.plugin.cmp(&b.plugin).then_with(|| a.path.cmp(&b.path)));
    entries
}

// =============================================================================
// Handlers
// =============================================================================

/// List the site's menus.
///
/// GET /admin/structure/menus
async fn list_menus(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let rows: Vec<(String, i64, i64)> = match sqlx::query_as(
        "SELECT menu_name, COUNT(*), COUNT(*) FILTER (WHERE plugin = $1) \
         FROM menu_link WHERE stage_id = $2 GROUP BY menu_name ORDER BY menu_name",
    )
    .bind(CORE_PLUGIN)
    .bind(LIVE_STAGE_ID)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "failed to list menus");
            return render_server_error("Failed to load menus.");
        }
    };

    let mut menus: Vec<MenuSummary> = rows
        .into_iter()
        .map(|(name, total, editable)| MenuSummary {
            rendered: THEME_MENUS.contains(&name.as_str()),
            name,
            links: usize::try_from(total).unwrap_or(0),
            editable: usize::try_from(editable).unwrap_or(0),
        })
        .collect();

    // The two menus the theme renders are always offered, empty or not.
    for name in THEME_MENUS {
        if !menus.iter().any(|m| m.name == name) {
            menus.push(MenuSummary {
                name: name.to_string(),
                links: 0,
                editable: 0,
                rendered: true,
            });
        }
    }
    menus.sort_by(|a, b| a.name.cmp(&b.name));

    let mut context = tera::Context::new();
    context.insert("menus", &menus);
    context.insert("plugin_entries", &plugin_entries(&state));
    context.insert("path", "/admin/structure/menus");

    render_admin_template(&state, "admin/menus.html", context).await
}

/// List one menu's links as an indented tree.
///
/// GET /admin/structure/menus/{menu}
async fn show_menu(
    State(state): State<AppState>,
    session: Session,
    Path(menu_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let links = match menu_links(&state, &menu_name).await {
        Ok(links) => links,
        Err(e) => {
            tracing::error!(error = %e, menu = %menu_name, "failed to load menu links");
            return render_server_error("Failed to load menu links.");
        }
    };

    let csrf_token = generate_csrf_token(&session).await;

    let mut context = tera::Context::new();
    context.insert("menu_name", &menu_name);
    context.insert("rows", &build_tree(&links));
    context.insert("csrf_token", &csrf_token);
    context.insert("path", &format!("/admin/structure/menus/{menu_name}"));

    render_admin_template(&state, "admin/menu-links.html", context).await
}

/// Render the add/edit form.
async fn render_form(
    state: &AppState,
    session: &Session,
    menu_name: &str,
    link_id: Option<Uuid>,
    values: serde_json::Value,
    errors: Vec<String>,
) -> Response {
    let links = match menu_links(state, menu_name).await {
        Ok(links) => links,
        Err(e) => {
            tracing::error!(error = %e, menu = %menu_name, "failed to load menu links");
            return render_server_error("Failed to load menu links.");
        }
    };

    let action = match link_id {
        Some(id) => format!("/admin/structure/menus/{menu_name}/{id}/edit"),
        None => format!("/admin/structure/menus/{menu_name}/add"),
    };

    let csrf_token = generate_csrf_token(session).await;

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("menu_name", &menu_name);
    context.insert("action", &action);
    context.insert("editing", &link_id.is_some());
    context.insert("values", &values);
    context.insert("parents", &parent_options(&links, link_id));
    context.insert("errors", &errors);
    context.insert("path", &action);

    render_admin_template(state, "admin/menu-link-form.html", context).await
}

/// Add-link form.
///
/// GET /admin/structure/menus/{menu}/add
async fn add_link_form(
    State(state): State<AppState>,
    session: Session,
    Path(menu_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    // Every field the template reads is present, empty rather than absent: Tera
    // errors on an undefined variable in a bare `{% if %}`, so a partial context
    // is a 500 on the add form and nothing else.
    let values = serde_json::json!({
        "title": "",
        "path": "",
        "parent_id": "",
        "weight": 0,
        "hidden": false,
    });
    render_form(&state, &session, &menu_name, None, values, Vec::new()).await
}

/// Add-link submit.
///
/// POST /admin/structure/menus/{menu}/add
async fn add_link_submit(
    State(state): State<AppState>,
    session: Session,
    Path(menu_name): Path<String>,
    Form(form): Form<MenuLinkFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let mut errors = Vec::new();
    let title = form.title.trim().to_string();
    if title.is_empty() {
        errors.push("Title is required.".to_string());
    }
    let path = match validate_path(&form.path) {
        Ok(path) => path,
        Err(e) => {
            errors.push(e);
            String::new()
        }
    };
    let parent_id = match parse_parent(form.parent_id.as_deref()) {
        Ok(parent) => parent,
        Err(e) => {
            errors.push(e);
            None
        }
    };

    // A parent has to be a link in this menu; anything else would render as a
    // root anyway and would be a silent lie about the tree.
    if let Some(parent) = parent_id {
        match MenuLink::find_by_id(state.db(), parent).await {
            Ok(Some(row)) if row.menu_name == menu_name => {}
            Ok(_) => errors.push("Parent must be a link in this menu.".to_string()),
            Err(e) => {
                tracing::error!(error = %e, "failed to verify menu link parent");
                errors.push("Failed to verify the parent link.".to_string());
            }
        }
    }

    let values = serde_json::json!({
        "title": title,
        "path": form.path,
        "parent_id": parent_id.map(|p| p.to_string()).unwrap_or_default(),
        "weight": form.weight.unwrap_or(0),
        "hidden": form.hidden.is_some(),
    });

    if !errors.is_empty() {
        return render_form(&state, &session, &menu_name, None, values, errors).await;
    }

    let input = CreateMenuLink {
        menu_name: Some(menu_name.clone()),
        path,
        title,
        parent_id,
        weight: Some(form.weight.unwrap_or(0)),
        hidden: Some(form.hidden.is_some()),
        plugin: Some(CORE_PLUGIN.to_string()),
        stage_id: Some(LIVE_STAGE_ID),
    };

    match MenuLink::create(state.db(), input).await {
        Ok(link) => {
            tracing::info!(link_id = %link.id, menu = %menu_name, "menu link created");
            Redirect::to(&format!("/admin/structure/menus/{menu_name}")).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to create menu link");
            render_form(
                &state,
                &session,
                &menu_name,
                None,
                values,
                vec![
                    "Failed to create the link. A link with this path may already exist in this \
                     menu."
                        .to_string(),
                ],
            )
            .await
        }
    }
}

/// Edit-link form.
///
/// GET /admin/structure/menus/{menu}/{id}/edit
async fn edit_link_form(
    State(state): State<AppState>,
    session: Session,
    Path((menu_name, link_id)): Path<(String, Uuid)>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let link = match MenuLink::find_by_id(state.db(), link_id).await {
        Ok(Some(link)) => link,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load menu link");
            return render_server_error("Failed to load the menu link.");
        }
    };
    if link.plugin != CORE_PLUGIN {
        return plugin_owned_response(&link.plugin);
    }

    let values = serde_json::json!({
        "title": link.title,
        "path": link.path,
        "parent_id": link.parent_id.map(|p| p.to_string()).unwrap_or_default(),
        "weight": link.weight,
        "hidden": link.hidden,
    });

    render_form(
        &state,
        &session,
        &menu_name,
        Some(link_id),
        values,
        Vec::new(),
    )
    .await
}

/// Edit-link submit.
///
/// POST /admin/structure/menus/{menu}/{id}/edit
async fn edit_link_submit(
    State(state): State<AppState>,
    session: Session,
    Path((menu_name, link_id)): Path<(String, Uuid)>,
    Form(form): Form<MenuLinkFormData>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let existing = match MenuLink::find_by_id(state.db(), link_id).await {
        Ok(Some(link)) => link,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load menu link");
            return render_server_error("Failed to load the menu link.");
        }
    };
    if existing.plugin != CORE_PLUGIN {
        return plugin_owned_response(&existing.plugin);
    }

    let mut errors = Vec::new();
    let title = form.title.trim().to_string();
    if title.is_empty() {
        errors.push("Title is required.".to_string());
    }
    let path = match validate_path(&form.path) {
        Ok(path) => path,
        Err(e) => {
            errors.push(e);
            String::new()
        }
    };
    let parent_id = match parse_parent(form.parent_id.as_deref()) {
        Ok(parent) => parent,
        Err(e) => {
            errors.push(e);
            None
        }
    };

    if let Some(parent) = parent_id {
        match MenuLink::find_by_id(state.db(), parent).await {
            Ok(Some(row)) if row.menu_name == menu_name => {}
            Ok(_) => errors.push("Parent must be a link in this menu.".to_string()),
            Err(e) => {
                tracing::error!(error = %e, "failed to verify menu link parent");
                errors.push("Failed to verify the parent link.".to_string());
            }
        }

        match would_cycle(&state, link_id, parent).await {
            Ok(true) => errors.push(
                "A link cannot be its own ancestor. Choose a parent that is not below this link."
                    .to_string(),
            ),
            Ok(false) => {}
            Err(e) => {
                tracing::error!(error = %e, "failed to check the menu link parent chain");
                errors.push("Failed to check the parent chain.".to_string());
            }
        }
    }

    let values = serde_json::json!({
        "title": title,
        "path": form.path,
        "parent_id": parent_id.map(|p| p.to_string()).unwrap_or_default(),
        "weight": form.weight.unwrap_or(existing.weight),
        "hidden": form.hidden.is_some(),
    });

    if !errors.is_empty() {
        return render_form(&state, &session, &menu_name, Some(link_id), values, errors).await;
    }

    let input = UpdateMenuLink {
        menu_name: None,
        path: Some(path),
        title: Some(title),
        // The outer Some means "change it"; the inner value may be None, which
        // is how a link is moved back to the top level.
        parent_id: Some(parent_id),
        weight: Some(form.weight.unwrap_or(existing.weight)),
        hidden: Some(form.hidden.is_some()),
        plugin: None,
        stage_id: None,
    };

    match MenuLink::update(state.db(), link_id, input).await {
        Ok(Some(_)) => {
            tracing::info!(link_id = %link_id, menu = %menu_name, "menu link updated");
            Redirect::to(&format!("/admin/structure/menus/{menu_name}")).into_response()
        }
        Ok(None) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to update menu link");
            render_form(
                &state,
                &session,
                &menu_name,
                Some(link_id),
                values,
                vec![
                    "Failed to save the link. A link with this path may already exist in this \
                     menu."
                        .to_string(),
                ],
            )
            .await
        }
    }
}

/// Delete a link, promoting its children to its own parent.
///
/// POST /admin/structure/menus/{menu}/{id}/delete
async fn delete_link(
    State(state): State<AppState>,
    session: Session,
    Path((menu_name, link_id)): Path<(String, Uuid)>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }
    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    let link = match MenuLink::find_by_id(state.db(), link_id).await {
        Ok(Some(link)) => link,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load menu link");
            return render_server_error("Failed to load the menu link.");
        }
    };
    if link.plugin != CORE_PLUGIN {
        return plugin_owned_response(&link.plugin);
    }

    // Promote children to the deleted link's own parent, rather than letting the
    // foreign key's ON DELETE SET NULL turn a nested branch into a row of
    // top-level links. The listing warns which of the two happens, and this is
    // the one it promises.
    if let Err(e) =
        sqlx::query("UPDATE menu_link SET parent_id = $1, changed = $2 WHERE parent_id = $3")
            .bind(link.parent_id)
            .bind(chrono::Utc::now().timestamp())
            .bind(link_id)
            .execute(state.db())
            .await
    {
        tracing::error!(error = %e, "failed to promote menu link children");
        return render_server_error("Failed to reparent the link's children.");
    }

    match MenuLink::delete(state.db(), link_id).await {
        Ok(true) => {
            tracing::info!(link_id = %link_id, menu = %menu_name, "menu link deleted");
            Redirect::to(&format!("/admin/structure/menus/{menu_name}")).into_response()
        }
        Ok(false) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to delete menu link");
            render_server_error("Failed to delete the menu link.")
        }
    }
}

/// The refusal a plugin-owned link gets.
fn plugin_owned_response(plugin: &str) -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::response::Html(format!(
            "<h1>Not editable here</h1><p>This link is owned by the <code>{}</code> plugin. \
             Change it in the plugin, not in this form.</p>",
            super::helpers::html_escape(plugin)
        )),
    )
        .into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Menu administration routes.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/menus", get(list_menus))
        .route("/admin/structure/menus/{menu}", get(show_menu))
        .route("/admin/structure/menus/{menu}/add", get(add_link_form))
        .route("/admin/structure/menus/{menu}/add", post(add_link_submit))
        .route(
            "/admin/structure/menus/{menu}/{id}/edit",
            get(edit_link_form),
        )
        .route(
            "/admin/structure/menus/{menu}/{id}/edit",
            post(edit_link_submit),
        )
        .route(
            "/admin/structure/menus/{menu}/{id}/delete",
            post(delete_link),
        )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn link(id: u128, title: &str, parent: Option<u128>, weight: i32) -> MenuLink {
        MenuLink {
            id: Uuid::from_u128(id),
            menu_name: "main".to_string(),
            path: format!("/{title}"),
            title: title.to_string(),
            parent_id: parent.map(Uuid::from_u128),
            weight,
            hidden: false,
            plugin: CORE_PLUGIN.to_string(),
            stage_id: LIVE_STAGE_ID,
            created: 0,
            changed: 0,
        }
    }

    #[test]
    fn a_local_absolute_path_is_accepted_and_trimmed() {
        assert_eq!(validate_path("  /about  ").unwrap(), "/about");
        assert_eq!(validate_path("/").unwrap(), "/");
    }

    #[test]
    fn an_off_site_or_relative_path_is_rejected_with_the_reason_that_fits() {
        // The message matters, not just the refusal: it is the only thing the
        // person filling in the form has to go on.
        for (bad, expected) in [
            ("https://example.com/", "not a full URL"),
            ("//example.com/", "protocol-relative"),
            ("about", "absolute path"),
            ("javascript:alert(1)", "absolute path"),
            ("/a/../../etc/passwd", "must not contain"),
            ("/a b", "whitespace"),
            ("", "required"),
            ("   ", "required"),
        ] {
            let Err(error) = validate_path(bad) else {
                panic!("{bad:?} must not be accepted as a menu path");
            };
            assert!(
                error.contains(expected),
                "{bad:?} should be refused for {expected:?}, got: {error}"
            );
        }
    }

    #[test]
    fn an_empty_parent_field_means_top_level() {
        assert_eq!(parse_parent(None).unwrap(), None);
        assert_eq!(parse_parent(Some("")).unwrap(), None);
        assert_eq!(parse_parent(Some("  ")).unwrap(), None);
        assert!(parse_parent(Some("not-a-uuid")).is_err());
    }

    #[test]
    fn the_tree_is_depth_first_by_weight_then_title() {
        // Deliberately inserted out of order.
        let links = vec![
            link(3, "install", Some(2), 0),
            link(1, "docs", None, 0),
            link(2, "guide", Some(1), 5),
            link(4, "reference", Some(1), 1),
            link(5, "about", None, 10),
        ];
        let rows = build_tree(&links);
        let shape: Vec<(&str, usize)> = rows.iter().map(|r| (r.title.as_str(), r.depth)).collect();
        assert_eq!(
            shape,
            vec![
                ("docs", 0),
                ("reference", 1),
                ("guide", 1),
                ("install", 2),
                ("about", 0),
            ]
        );
    }

    #[test]
    fn child_counts_are_reported_so_delete_can_say_what_happens() {
        let links = vec![
            link(1, "docs", None, 0),
            link(2, "guide", Some(1), 0),
            link(3, "install", Some(2), 0),
        ];
        let rows = build_tree(&links);
        let docs = rows.iter().find(|r| r.title == "docs").unwrap();
        let install = rows.iter().find(|r| r.title == "install").unwrap();
        assert_eq!(docs.children, 1);
        assert_eq!(install.children, 0);
    }

    #[test]
    fn a_link_whose_parent_is_in_another_menu_is_listed_as_a_root() {
        let mut orphan = link(2, "orphan", Some(999), 0);
        orphan.parent_id = Some(Uuid::from_u128(999));
        let links = vec![link(1, "docs", None, 0), orphan];
        let rows = build_tree(&links);
        assert_eq!(rows.len(), 2, "the orphan must still be listed");
        assert!(rows.iter().all(|r| r.depth == 0));
    }

    #[test]
    fn a_pre_existing_cycle_is_still_listed_rather_than_dropped() {
        // Two links naming each other: no root, so the depth-first walk emits
        // nothing and the flat fallback has to.
        let links = vec![link(1, "a", Some(2), 0), link(2, "b", Some(1), 0)];
        let rows = build_tree(&links);
        assert_eq!(
            rows.len(),
            2,
            "a cyclic pair must still be listed: {rows:?}"
        );
    }

    #[test]
    fn the_parent_select_excludes_the_link_and_its_descendants() {
        let links = vec![
            link(1, "docs", None, 0),
            link(2, "guide", Some(1), 0),
            link(3, "install", Some(2), 0),
            link(4, "about", None, 0),
        ];
        let options = parent_options(&links, Some(Uuid::from_u128(1)));
        let ids: Vec<Uuid> = options.iter().map(|o| o.id).collect();
        assert_eq!(
            ids,
            vec![Uuid::from_u128(4)],
            "only a link outside the subtree may be offered as parent"
        );
    }

    #[test]
    fn the_parent_select_reads_as_a_tree() {
        let links = vec![
            link(1, "docs", None, 0),
            link(2, "guide", Some(1), 0),
            link(3, "install", Some(2), 0),
        ];
        let labels: Vec<String> = parent_options(&links, None)
            .into_iter()
            .map(|o| o.label)
            .collect();
        assert_eq!(labels, vec!["docs", "— guide", "— — install"]);
    }

    #[test]
    fn a_plugin_owned_row_is_not_editable() {
        let mut owned = link(1, "argus", None, 0);
        owned.plugin = "argus".to_string();
        let rows = build_tree(&[owned]);
        assert!(!rows[0].editable);
        assert_eq!(rows[0].plugin, "argus");
    }
}
