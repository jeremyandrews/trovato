//! HTTP route handlers.

pub mod admin;
pub mod admin_ai_budget;
pub mod admin_ai_chat;
pub mod admin_ai_features;
pub mod admin_ai_provider;
pub mod admin_alias;
pub mod admin_config;
pub mod admin_content;
pub mod admin_content_type;
pub mod admin_pathauto;
pub mod admin_taxonomy;
pub mod admin_translation;
pub mod admin_user;
pub mod api_ai_assist;
pub mod api_chat;
pub mod api_search;
pub mod api_token;
pub mod api_v1;
pub mod auth;
pub mod batch;
pub mod category;
pub mod comment;
pub mod cron;
pub mod file;
pub mod front;
pub mod gather;
pub mod gather_admin;
pub mod gather_routes;
pub mod health;
pub mod helpers;
pub mod image_style;
pub mod install;
pub mod item;
pub mod lock;
pub mod metrics;
pub mod oauth;
pub mod password_reset;
pub mod plugin_admin;
pub mod route_metadata;
pub mod search;
pub mod sitemap;
pub mod static_files;
pub mod tile_admin;

use axum::Router;

use crate::state::AppState;

/// Generate a middleware function that returns 404 when the named plugin is disabled.
macro_rules! plugin_gate {
    ($fn_name:ident, $plugin:expr) => {
        async fn $fn_name(
            axum::extract::State(state): axum::extract::State<AppState>,
            req: axum::extract::Request,
            next: axum::middleware::Next,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            if state.is_plugin_enabled($plugin) {
                next.run(req).await
            } else {
                (
                    axum::http::StatusCode::NOT_FOUND,
                    format!("Plugin '{}' is not enabled.", $plugin),
                )
                    .into_response()
            }
        }
    };
}

plugin_gate!(gate_categories, "trovato_categories");
plugin_gate!(gate_comments, "trovato_comments");
plugin_gate!(gate_content_locking, "trovato_content_locking");
plugin_gate!(gate_content_translation, "trovato_content_translation");
plugin_gate!(gate_image_styles, "trovato_image_styles");
plugin_gate!(gate_oauth2, "trovato_oauth2");
plugin_gate!(gate_block_editor, "trovato_block_editor");

/// Plugin names that are runtime-gated in [`gated_plugin_routes`].
///
/// Must be kept in sync with [`crate::plugin::gate::GATED_ROUTE_PLUGINS`].
/// A unit test in `plugin::gate` enforces this invariant.
pub(crate) const RUNTIME_GATED_NAMES: &[&str] = &[
    "trovato_categories",
    "trovato_comments",
    "trovato_content_locking",
    "trovato_content_translation",
    "trovato_image_styles",
    "trovato_oauth2",
    "trovato_block_editor",
];

/// Build the router fragment for plugin-gated routes.
///
/// Each gated plugin is listed in [`crate::plugin::gate::GATED_ROUTE_PLUGINS`].
/// This function is the **single source of truth** for which routes are
/// runtime-gated; both the binary (`main.rs`) and integration tests call it,
/// so adding a new gated plugin only requires updating this function
/// (and the `GATED_ROUTE_PLUGINS` documentation constant).
///
/// All routes are always registered. Per-plugin middleware checks the enabled
/// state at request time and returns 404 for disabled plugins.
pub fn gated_plugin_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .merge(
            admin::category_admin_router()
                .merge(category::router())
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    gate_categories,
                )),
        )
        .merge(
            admin::comment_admin_router()
                .merge(comment::router())
                .route_layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    gate_comments,
                )),
        )
        .merge(
            lock::router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_content_locking,
            )),
        )
        .merge(
            admin_translation::router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_content_translation,
            )),
        )
        .merge(
            image_style::router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_image_styles,
            )),
        )
        .merge(
            oauth::router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_oauth2,
            )),
        )
        .merge(
            file::block_editor_router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_block_editor,
            )),
        )
}
