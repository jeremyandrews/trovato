//! Admin routes for pathauto (automatic path alias) configuration.
//!
//! Provides settings and bulk-regeneration endpoints for the pathauto
//! pattern system. Patterns are stored as a JSON object in `site_config`
//! under the key `pathauto_patterns`.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Deserialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::models::{Item, SiteConfig};
use crate::services::pathauto::update_alias_item;
use crate::state::AppState;

use super::helpers::{
    is_valid_machine_name, render_admin_template, render_error, render_server_error, require_admin,
    require_csrf,
};

/// Session key for flash messages on the pathauto settings page.
const FLASH_KEY: &str = "pathauto_flash";

/// Maximum number of items to regenerate aliases for in a single request.
///
/// Regeneration is synchronous and runs one DB round-trip per item, so an
/// unbounded operation would block an Axum worker thread for an arbitrary
/// amount of time and exhaust memory on large datasets. Administrators with
/// sites that exceed this limit should run regeneration via the CLI.
const MAX_REGENERATE_ITEMS: usize = 10_000;

/// Path prefixes that the alias middleware explicitly skips.
///
/// Patterns that expand to one of these prefixes generate aliases that are
/// never resolved by the middleware, and may shadow system routes. They are
/// rejected at save time to prevent administrator confusion.
const RESERVED_PATTERN_PREFIXES: &[&str] = &[
    "admin/", "api/", "user/", "item/", "static/", "install/", "system/", "health",
];

// =============================================================================
// Form data
// =============================================================================

/// Regenerate aliases form.
///
/// Posted to `POST /admin/config/pathauto/regenerate`.
#[derive(Debug, Deserialize)]
struct RegenerateForm {
    #[serde(rename = "_token")]
    token: String,
    item_type: String,
}

// =============================================================================
// Handlers
// =============================================================================

/// Pathauto configuration page.
///
/// Renders a form listing all registered content types with a text input
/// for each type's URL pattern. Patterns are pre-populated from `site_config`.
///
/// GET /admin/config/pathauto
async fn pathauto_config_page(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let content_types = state.content_types().list_all().await;

    // Load existing patterns map from site_config.
    let patterns_json = match SiteConfig::get(state.db(), "pathauto_patterns").await {
        Ok(v) => v.unwrap_or(serde_json::json!({})),
        Err(e) => {
            tracing::error!(error = %e, "failed to load pathauto patterns");
            return render_server_error("Failed to load pathauto configuration.");
        }
    };

    // Build a flat map of machine_name -> pattern string for the template.
    let patterns: HashMap<String, String> = content_types
        .iter()
        .map(|ct| {
            let pat = patterns_json
                .get(&ct.machine_name)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            (ct.machine_name.clone(), pat)
        })
        .collect();

    let csrf_token = generate_csrf_token(&session).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("patterns", &patterns);
    context.insert("csrf_token", &csrf_token);
    context.insert("path", "/admin/config/pathauto");
    if let Some(msg) = flash {
        context.insert("flash", &msg);
    }

    render_admin_template(&state, "admin/pathauto.html", context).await
}

/// Save pathauto patterns.
///
/// Accepts form fields named `pattern.{machine_name}` (one per content type).
/// Empty patterns are dropped (removing a pattern disables alias generation
/// for that type going forward; existing aliases are not deleted).
///
/// Keys that are not valid machine names are silently ignored — they cannot
/// originate from the admin UI and indicate a crafted request. Patterns whose
/// leading path segment collides with a reserved system prefix (e.g. `admin/`,
/// `api/`) are rejected with a 400 error, since the alias middleware skips
/// those paths and such patterns would generate unreachable aliases.
///
/// POST /admin/config/pathauto
async fn save_pathauto_config(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    // Extract and verify CSRF token from the flat form map.
    let token = form.get("_token").map(String::as_str).unwrap_or("");
    if let Err(resp) = require_csrf(&session, token).await {
        return resp;
    }

    // Collect non-empty patterns from fields prefixed with "pattern.".
    let mut patterns = serde_json::Map::new();
    for (key, value) in &form {
        let Some(type_name) = key.strip_prefix("pattern.") else {
            continue;
        };

        // Guard: a field literally named "pattern." yields an empty suffix.
        if type_name.is_empty() {
            continue;
        }

        // Only accept keys that are valid content type machine names. This
        // prevents crafted requests from polluting site_config with arbitrary
        // JSON keys. Our own form only ever generates valid machine names.
        if !is_valid_machine_name(type_name) {
            continue;
        }

        let trimmed = value.trim();
        if trimmed.is_empty() {
            // Empty pattern = remove alias generation for this type.
            continue;
        }

        // Reject patterns whose leading segment collides with a system prefix.
        // The alias middleware never resolves paths starting with these prefixes,
        // so such patterns would create unreachable aliases and could shadow
        // system routes.
        if RESERVED_PATTERN_PREFIXES
            .iter()
            .any(|prefix| trimmed.starts_with(prefix))
        {
            return render_error(&format!(
                "Pattern for '{type_name}' begins with a reserved path prefix (e.g. admin/, api/, user/). \
                 The alias middleware skips these paths, so the alias would never resolve. \
                 Choose a different prefix."
            ));
        }

        patterns.insert(
            type_name.to_string(),
            serde_json::Value::String(trimmed.to_string()),
        );
    }

    if let Err(e) = SiteConfig::set(
        state.db(),
        "pathauto_patterns",
        serde_json::Value::Object(patterns),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save pathauto patterns");
        return render_server_error("Failed to save pathauto configuration.");
    }

    let _ = session.insert(FLASH_KEY, "Pathauto patterns saved.").await;
    Redirect::to("/admin/config/pathauto").into_response()
}

/// Regenerate path aliases for all items of a given content type.
///
/// Validates that the requested content type is registered, then iterates
/// every item of that type (up to [`MAX_REGENERATE_ITEMS`]), calls
/// [`update_alias_item`] for each, and counts how many aliases were created
/// or updated. Items whose alias already matches the current pattern are
/// skipped automatically.
///
/// Returns a 400 error if the content type is unrecognized or the item
/// count exceeds the per-request limit.
///
/// POST /admin/config/pathauto/regenerate
async fn regenerate_aliases(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<RegenerateForm>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    if let Err(resp) = require_csrf(&session, &form.token).await {
        return resp;
    }

    // Validate that the requested type is actually registered. This prevents
    // log noise and accidental regeneration calls for non-existent types from
    // crafted requests.
    let content_types = state.content_types().list_all().await;
    if !content_types
        .iter()
        .any(|ct| ct.machine_name == form.item_type)
    {
        return render_error("Unknown content type. Select a type from the list.");
    }

    let items = match Item::list_by_type(state.db(), &form.item_type).await {
        Ok(items) => items,
        Err(e) => {
            tracing::error!(error = %e, item_type = %form.item_type, "failed to list items for regeneration");
            return render_server_error("Failed to load items for alias regeneration.");
        }
    };

    // Guard against accidentally blocking a worker thread for an unbounded
    // duration. Sites that exceed this limit should run regeneration offline.
    if items.len() > MAX_REGENERATE_ITEMS {
        return render_error(&format!(
            "Too many items to regenerate in one request ({} found; limit is {}). \
             Run regeneration from the command line for large datasets.",
            items.len(),
            MAX_REGENERATE_ITEMS
        ));
    }

    let mut count: u64 = 0;
    for item in &items {
        match update_alias_item(
            state.db(),
            item.id,
            &item.title,
            &item.item_type,
            item.created,
        )
        .await
        {
            Ok(Some(_)) => count += 1,
            Ok(None) => {} // alias already matches or no pattern configured
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    item_id = %item.id,
                    "failed to update alias during pathauto regeneration"
                );
            }
        }
    }

    tracing::info!(
        item_type = %form.item_type,
        aliases_updated = count,
        "pathauto regeneration complete"
    );

    let msg = match count {
        1 => format!("1 alias created or updated for '{}'.", form.item_type),
        n => format!("{n} aliases created or updated for '{}'.", form.item_type),
    };
    let _ = session.insert(FLASH_KEY, msg).await;
    Redirect::to("/admin/config/pathauto").into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the pathauto admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/config/pathauto",
            get(pathauto_config_page).post(save_pathauto_config),
        )
        .route(
            "/admin/config/pathauto/regenerate",
            post(regenerate_aliases),
        )
}
