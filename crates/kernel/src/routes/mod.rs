//! HTTP route handlers.

pub mod admin;
pub mod admin_alias;
pub mod admin_content;
pub mod admin_content_type;
pub mod admin_taxonomy;
pub mod admin_user;
pub mod api_token;
pub mod auth;
pub mod batch;
pub mod category;
pub mod comment;
pub mod cron;
pub mod file;
pub mod front;
pub mod gather;
pub mod gather_admin;
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
pub mod search;
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

plugin_gate!(gate_categories, "categories");
plugin_gate!(gate_comments, "comments");
plugin_gate!(gate_content_locking, "content_locking");
plugin_gate!(gate_image_styles, "image_styles");
plugin_gate!(gate_oauth2, "oauth2");

/// Plugin names that are runtime-gated in [`gated_plugin_routes`].
///
/// Must be kept in sync with [`crate::plugin::gate::GATED_ROUTE_PLUGINS`].
/// A unit test in `plugin::gate` enforces this invariant.
pub(crate) const RUNTIME_GATED_NAMES: &[&str] = &[
    "categories",
    "comments",
    "content_locking",
    "image_styles",
    "oauth2",
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
}
