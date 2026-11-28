//! Admin routes for plugin management (enable/disable).
//!
//! The listing's enable and disable buttons all submit one form, built and
//! checked through `FormService`: each button is named for what it does and
//! carries its plugin's name.

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::plugin::runtime::PluginRuntime;
use crate::plugin::status::{self, STATUS_DISABLED, STATUS_ENABLED};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{render_admin_template, require_admin};

const FLASH_KEY: &str = "plugin_admin_flash";

/// The one form the listing's enable and disable buttons submit.
const TOGGLE_FORM_ID: &str = "plugin_toggle_form";

/// When an enable or disable reaches the running site.
const RESTART_NOTICE: &str =
    "Changes take effect when the server restarts (all servers, if more than one).";

// =============================================================================
// Template data
// =============================================================================

#[derive(Debug, Serialize)]
struct PluginRow {
    name: String,
    description: String,
    version: String,
    dependencies: String,
    status: String,
    is_installed: bool,
    default_enabled: bool,
    /// The manifest grants `raw_sql`; the listing marks it.
    raw_sql: bool,
    /// The stored status differs from what this server is running: the
    /// change waits for a restart.
    pending_restart: bool,
}

/// What a toggle submission asks: the plugin and `enable` or `disable`.
///
/// A listing button posts its action as its name and the plugin as its
/// value (`enable=blog`); `plugin_name` and `action` say the same in two
/// fields.
fn toggle_request(posted: &PostedForm) -> (String, String) {
    if let (Some(name), Some(action)) = (posted.get("plugin_name"), posted.get("action")) {
        return (name.to_string(), action.to_string());
    }
    ["enable", "disable"]
        .iter()
        .find_map(|action| {
            posted
                .get(action)
                .map(|name| (name.to_string(), (*action).to_string()))
        })
        .unwrap_or_default()
}

// =============================================================================
// Handlers
// =============================================================================

/// List all plugins.
///
/// GET /admin/plugins
async fn list_plugins(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let discovered = PluginRuntime::discover_plugins(state.plugins_dirs()).await;
    let statuses = match status::get_all_statuses(state.db()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to get plugin statuses");
            return render_plugin_error(&state, &session, "Failed to load plugin statuses.").await;
        }
    };
    let status_map: std::collections::HashMap<String, &status::PluginStatus> =
        statuses.iter().map(|s| (s.name.clone(), s)).collect();

    let mut rows: Vec<PluginRow> = Vec::new();

    // All discovered plugins
    let mut names: Vec<&String> = discovered.keys().collect();
    names.sort();
    for name in names {
        let (info, _dir) = &discovered[name];
        let (status_str, is_installed) = match status_map.get(name) {
            Some(s) if s.status == STATUS_ENABLED => ("Enabled".to_string(), true),
            Some(_) => ("Disabled".to_string(), true),
            None => ("Not installed".to_string(), false),
        };

        rows.push(PluginRow {
            name: name.clone(),
            description: info.description.clone(),
            version: info.version.clone(),
            dependencies: if info.dependencies.is_empty() {
                "None".to_string()
            } else {
                info.dependencies.join(", ")
            },
            status: status_str,
            is_installed,
            default_enabled: info.default_enabled,
            raw_sql: info.holds_raw_sql(),
            pending_restart: status_map
                .get(name)
                .is_some_and(|s| (s.status == STATUS_ENABLED) != state.is_plugin_enabled(name)),
        });
    }

    // Installed plugins not on disk
    for ps in &statuses {
        if !discovered.contains_key(&ps.name) {
            let status_str = if ps.status == STATUS_ENABLED {
                "Enabled"
            } else {
                "Disabled"
            };
            rows.push(PluginRow {
                name: ps.name.clone(),
                description: "Not found on disk".to_string(),
                version: ps.version.clone(),
                dependencies: "?".to_string(),
                status: status_str.to_string(),
                is_installed: true,
                default_enabled: true,
                raw_sql: false,
                pending_restart: (ps.status == STATUS_ENABLED) != state.is_plugin_enabled(&ps.name),
            });
        }
    }

    // The buttons' form, only where there are buttons.
    let toggle_form = if rows.iter().any(|row| row.is_installed) {
        let form = crate::form::Form::new(TOGGLE_FORM_ID)
            .action("/admin/plugins/toggle")
            .attribute("hidden", "hidden");
        match admin_form::render(&state, &session, &user, form, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        }
    } else {
        String::new()
    };

    // Read and clear flash message
    // A plain sentence, or an i18n::UiMessage for a failure.
    let flash: Option<serde_json::Value> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some() {
        let _ = session.remove::<serde_json::Value>(FLASH_KEY).await;
    }

    let mut context = tera::Context::new();
    let raw_sql_count = rows.iter().filter(|row| row.raw_sql).count();
    context.insert("plugins", &rows);
    context.insert("raw_sql_count", &raw_sql_count);
    context.insert("toggle_form", &toggle_form);
    context.insert("toggle_form_id", TOGGLE_FORM_ID);
    context.insert("flash", &flash);
    context.insert("restart_notice", RESTART_NOTICE);
    context.insert("path", "/admin/plugins");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/plugin-list.html",
        context,
    )
    .await
}

/// Toggle a plugin's enabled/disabled status.
///
/// POST /admin/plugins/toggle
async fn toggle_plugin(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_admin(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, TOGGLE_FORM_ID, Vec::new()).await
        {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return super::helpers::render_error(
                    "The form has expired. Reload the page and try again.",
                );
            }
            Err(Err(response)) => return response,
        };
    let (plugin_name, action) = toggle_request(&posted);

    // Validate plugin name (alphanumeric + underscore only)
    if plugin_name.is_empty()
        || !plugin_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return render_plugin_error(&state, &session, "Invalid plugin name.").await;
    }

    // Check plugin is installed
    let installed = match status::is_installed(state.db(), &plugin_name).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "failed to check plugin install status");
            return render_plugin_error(&state, &session, "Failed to check plugin status.").await;
        }
    };

    if !installed {
        return render_plugin_error(
            &state,
            &session,
            "Plugin is not installed. Use the CLI to install it first: trovato plugin install",
        )
        .await;
    }

    let (new_status, action_word) = match action.as_str() {
        "enable" => (STATUS_ENABLED, "enabled"),
        "disable" => (STATUS_DISABLED, "disabled"),
        _ => return render_plugin_error(&state, &session, "Invalid action.").await,
    };

    // Check dependencies before enabling: every declared dependency must be
    // enabled in the stored statuses (mirrors the CLI's install-time check).
    if new_status == STATUS_ENABLED {
        let discovered = PluginRuntime::discover_plugins(state.plugins_dirs()).await;
        if let Some((info, _)) = discovered.get(&plugin_name) {
            let enabled = match status::get_enabled_names(state.db()).await {
                Ok(names) => names,
                Err(e) => {
                    tracing::error!(error = %e, "failed to read enabled plugins");
                    return render_plugin_error(&state, &session, "Failed to check plugin status.")
                        .await;
                }
            };
            for dep in &info.dependencies {
                if !enabled.contains(dep) {
                    let msg = crate::i18n::UiMessage::new(
                        "Cannot enable {plugin}: it depends on {dependency}, which is not enabled.",
                    )
                    .arg("plugin", plugin_name.clone())
                    .arg("dependency", dep.clone())
                    .error();
                    let _ = session.insert(FLASH_KEY, &msg).await;
                    return Redirect::to("/admin/plugins").into_response();
                }
            }
        }
    }

    // Only the stored status changes. The running server keeps the plugins it
    // started with: every server loads the enabled set once at boot, so a
    // change made here takes effect on each server when it restarts.
    match status::set_status(state.db(), &plugin_name, new_status).await {
        Ok(true) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            tracing::info!(
                log_type = "plugin",
                plugin = %plugin_name,
                "plugin {action_word}"
            );
            // Flash messages are rendered in Tera templates that autoescape
            // .html files, so we must NOT pre-escape here.
            let msg = format!("Plugin '{plugin_name}' {action_word}. {RESTART_NOTICE}");
            let _ = session.insert(FLASH_KEY, &msg).await;
        }
        Ok(false) => {
            let _ = session
                .insert(
                    FLASH_KEY,
                    crate::i18n::UiMessage::new("Plugin not found.").error(),
                )
                .await;
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to toggle plugin status");
            let _ = session
                .insert(
                    FLASH_KEY,
                    crate::i18n::UiMessage::new("Failed to update plugin status.").error(),
                )
                .await;
        }
    }

    Redirect::to("/admin/plugins").into_response()
}

// =============================================================================
// Helper functions
// =============================================================================

/// Render an error within the admin layout.
///
/// Unlike `helpers::render_error` (bare 400 page) this renders the error
/// inside the full admin chrome using the `admin/plugin-error.html` template.
///
/// The message is inserted raw into the Tera context because the template uses
/// Tera's default autoescape for `.html` files. Pre-escaping would cause
/// double-escaped output (e.g. `&amp;amp;`).
async fn render_plugin_error(state: &AppState, session: &Session, message: &str) -> Response {
    let mut context = tera::Context::new();
    context.insert("error_message", message);
    context.insert("path", "/admin/plugins");
    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/plugin-error.html",
        context,
    )
    .await
}

// =============================================================================
// Router
// =============================================================================

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/plugins", get(list_plugins))
        .route("/admin/plugins/toggle", post(toggle_plugin))
}
