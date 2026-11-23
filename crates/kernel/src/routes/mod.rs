//! HTTP route handlers.

pub mod admin;
pub mod admin_access_rules;
pub mod admin_ai_assistant;
pub mod admin_ai_budget;
pub mod admin_ai_chat;
pub mod admin_ai_provider;
pub mod admin_alias;
pub mod admin_appearance;
pub mod admin_audit;
pub mod admin_config;
pub mod admin_content;
pub mod admin_content_type;
pub mod admin_embed;
pub mod admin_form;
pub mod admin_menu;
pub mod admin_nav;
pub mod admin_pathauto;
pub mod admin_queue;
pub mod admin_record_type;
pub mod admin_reports;
pub mod admin_search;
pub mod admin_search_report;
pub mod admin_stage;
pub mod admin_taxonomy;
pub mod admin_text_format;
pub mod admin_translation;
pub mod admin_user;
pub mod api_ai_assist;
pub mod api_chat;
pub mod api_search;
pub mod api_token;
pub mod api_v1;
pub mod assistant;
pub mod auth;
pub mod batch;
pub mod category;
pub mod comment;
pub mod cron;
pub mod feed;
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
pub mod item_form;
pub mod lock;
pub mod mail_preference;
pub mod metrics;
pub mod oauth;
pub mod owned_vocabulary;
pub mod password_reset;
pub mod plugin_admin;
pub mod plugin_api;
pub mod recovery;
pub mod route_metadata;
pub mod search;
pub mod sessions;
pub mod sitemap;
pub mod static_files;
pub mod tile_admin;
pub mod user_data_export;
pub mod user_delete;
pub mod webauthn;

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
                crate::error::AppError::with_status(
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
plugin_gate!(gate_audit_log, "trovato_audit_log");

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
    "trovato_audit_log",
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
        .merge(
            admin_audit::router().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                gate_audit_log,
            )),
        )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    /// Every `routes::*::router()` is reachable from `main.rs`.
    ///
    /// A module with a `router()` nobody merges is a set of handlers no request
    /// can reach, and documentation that names its paths describes a site that
    /// does not exist (`admin_ai_features` was exactly that). A module counts as
    /// mounted when `main.rs`, this file (whose `gated_plugin_routes` `main.rs`
    /// merges), or an already-mounted module calls `<module>::router()`.
    #[test]
    fn every_router_is_mounted() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut modules: BTreeMap<String, String> = BTreeMap::new();
        for entry in std::fs::read_dir(src.join("routes")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            modules.insert(name, std::fs::read_to_string(&path).unwrap());
        }
        let with_router: BTreeSet<&String> = modules
            .iter()
            .filter(|(name, text)| {
                name.as_str() != "mod" && text.contains("pub fn router() -> Router<AppState>")
            })
            .map(|(name, _)| name)
            .collect();

        let mut sources = vec![
            std::fs::read_to_string(src.join("main.rs")).unwrap(),
            modules["mod"].clone(),
        ];
        let mut mounted: BTreeSet<&String> = BTreeSet::new();
        loop {
            let before = mounted.len();
            for name in &with_router {
                let call = format!("{name}::router()");
                if !mounted.contains(name) && sources.iter().any(|s| s.contains(&call)) {
                    mounted.insert(name);
                    sources.push(modules[*name].clone());
                }
            }
            if mounted.len() == before {
                break;
            }
        }

        let unmounted: Vec<&&String> = with_router.difference(&mounted).collect();
        assert!(
            unmounted.is_empty(),
            "routes with a router() that nothing mounts: {unmounted:?}"
        );
    }
}
