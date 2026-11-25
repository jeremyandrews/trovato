//! The pages the admin shell itself owns: each group's landing page, the
//! viewer's admin preferences, and admin search.
//!
//! - `GET /admin/{group}/overview`: every page in the group the viewer may
//!   open, each with its one sentence description, from the registry in
//!   [`super::admin_nav`]. The way to browse the admin without JavaScript.
//! - `GET` and `POST /admin/preferences`: the colour theme and admin language.
//! - `POST /admin/preferences/nav`: which sidebar groups are collapsed, sent
//!   by the sidebar's disclosure buttons (JSON, CSRF header).
//! - `GET /admin/search?q=`: the registry pages matching the query.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::state::AppState;

use super::admin_nav::{self, GROUPS, Group, Preferences};
use super::helpers::{
    admin_user_context, render_admin_template, render_not_found, require_csrf, require_permission,
    require_permission_json,
};

/// What every page here asks for: admission to the admin.
const ADMISSION: &str = "access administration pages";

/// Session key for the saved message.
const FLASH_KEY: &str = "admin_preferences_flash";

/// Build the router.
pub fn router() -> Router<AppState> {
    let mut router = Router::new()
        .route(
            admin_nav::PREFERENCES,
            get(preferences_form).post(preferences_save),
        )
        .route("/admin/preferences/nav", post(save_nav_state))
        .route(admin_nav::SEARCH, get(search));
    // One static route per group rather than `/admin/{group}/overview`, so
    // no path parameter competes with the existing `/admin/content/...` and
    // `/admin/people/...` routes.
    for g in GROUPS {
        router = router.route(&g.group.landing_href(), get(landing));
    }
    router
}

/// GET /admin/{group}/overview
async fn landing(
    State(state): State<AppState>,
    session: Session,
    uri: axum::http::Uri,
) -> Response {
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let path = uri.path().to_string();
    let Some(group) = admin_nav::landing_group(&path) else {
        return render_not_found();
    };
    let viewer = admin_user_context(&state, &user).await;
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    let nav = state.plugin_admin_nav();
    let items = admin_nav::landing(&viewer, &enabled, group, nav.links(), nav.cards());
    // A group with nothing the viewer may open does not exist for them, as a
    // page they may not open does not.
    if items.is_empty() {
        return render_not_found();
    }

    let mut context = tera::Context::new();
    context.insert("group", group.info().title);
    context.insert("group_description", group.info().description);
    context.insert("items", &items);
    context.insert("path", &path);
    render_admin_template(&state, &viewer, "admin/landing.html", context).await
}

/// One choice in a radio group or select.
#[derive(Debug, Serialize)]
struct Choice {
    value: String,
    label: String,
    /// Whether the label is a language's own name, not interface text.
    native: bool,
}

/// GET /admin/preferences
async fn preferences_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let prefs = Preferences::load(state.db(), user.id).await;
    render_preferences(&state, &session, &user, &prefs, None).await
}

async fn render_preferences(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    prefs: &Preferences,
    error: Option<&str>,
) -> Response {
    let viewer = admin_user_context(state, user).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut languages: Vec<Choice> = crate::models::Language::list_all(state.db())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|l| Choice {
            value: l.id,
            label: l.label,
            native: true,
        })
        .collect();
    if crate::i18n::pseudo_locales_enabled() {
        languages.push(Choice {
            value: crate::i18n::PSEUDO_ACCENTED.to_string(),
            label: "Pseudo (accented, longer)".to_string(),
            native: false,
        });
        languages.push(Choice {
            value: crate::i18n::PSEUDO_RTL.to_string(),
            label: "Pseudo (right to left)".to_string(),
            native: false,
        });
    }

    let mut context = tera::Context::new();
    context.insert("csrf_token", &generate_csrf_token(session).await);
    context.insert("theme", prefs.theme());
    context.insert(
        "language",
        &prefs
            .language
            .clone()
            .unwrap_or_else(|| state.default_language().to_string()),
    );
    context.insert("languages", &languages);
    context.insert("default_language", state.default_language());
    context.insert("flash", &flash);
    context.insert("error_message", &error);
    context.insert("path", admin_nav::PREFERENCES);
    render_admin_template(state, &viewer, "admin/preferences.html", context).await
}

#[derive(Debug, Deserialize)]
struct PreferencesForm {
    #[serde(rename = "_token", default)]
    token: String,
    #[serde(default)]
    theme: String,
    #[serde(default)]
    language: String,
}

/// POST /admin/preferences
async fn preferences_save(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<PreferencesForm>,
) -> Response {
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = require_csrf(&session, &form.token).await {
        return refusal;
    }
    let mut prefs = Preferences::load(state.db(), user.id).await;
    prefs.theme = match form.theme.as_str() {
        "light" | "dark" => Some(form.theme.clone()),
        _ => None,
    };
    let lang = form.language.trim();
    let known = state.known_languages().iter().any(|l| l == lang)
        || (crate::i18n::is_pseudo(lang) && crate::i18n::pseudo_locales_enabled());
    if !lang.is_empty() && !known {
        return render_preferences(
            &state,
            &session,
            &user,
            &prefs,
            Some("Choose one of the languages listed."),
        )
        .await;
    }
    prefs.language =
        (!lang.is_empty() && lang != state.default_language()).then(|| lang.to_string());

    let changes = serde_json::json!({ "theme": prefs.theme, "language": prefs.language });
    if let Err(e) = Preferences::merge(state.db(), user.id, changes).await {
        tracing::error!(error = %e, "failed to save admin preferences");
        return super::helpers::render_server_error("Your preferences could not be saved.");
    }
    if let Err(e) = session
        .insert(FLASH_KEY, "Your preferences were saved.")
        .await
    {
        tracing::warn!(error = %e, "failed to set the preferences message");
    }
    Redirect::to(admin_nav::PREFERENCES).into_response()
}

#[derive(Debug, Deserialize)]
struct NavState {
    /// The slugs of the groups now collapsed.
    collapsed: Vec<String>,
}

/// POST /admin/preferences/nav: the sidebar groups the viewer has collapsed,
/// remembered so the next page renders them that way with no flash.
///
/// No CSRF token, unlike every form: a token is issued per render and kept
/// in a short list, so giving every admin page one would expire the tokens of
/// forms open in other tabs. What stands in for it: the body must be JSON,
/// which no cross-site form can send and which a cross-site script can send
/// only after a CORS preflight this server never approves, and a browser's
/// `Sec-Fetch-Site` saying the request came from another site is refused
/// outright. The worst a forged request could do is fold someone's sidebar.
async fn save_nav_state(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<NavState>,
) -> Response {
    let user = match require_permission_json(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(e) => return e.into_response(),
    };
    let fetch_site = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("same-origin");
    if fetch_site != "same-origin" && fetch_site != "none" {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut collapsed: Vec<String> = body
        .collapsed
        .into_iter()
        .filter(|slug| Group::from_slug(slug).is_some())
        .collect();
    collapsed.sort();
    collapsed.dedup();
    let changes = serde_json::json!({ "collapsed": collapsed });
    match Preferences::merge(state.db(), user.id, changes).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to save the sidebar state");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    q: String,
}

/// Longest query read; the rest is ignored.
const MAX_QUERY_LEN: usize = 200;

/// GET /admin/search?q=
async fn search(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<SearchQuery>,
) -> Response {
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = admin_user_context(&state, &user).await;
    let q: String = query.q.trim().chars().take(MAX_QUERY_LEN).collect();

    let prefs = Preferences::load(state.db(), user.id).await;
    let lang = admin_nav::admin_language(&state, &prefs);
    if let Some(locale) = state.locale() {
        locale.ensure_loaded(&lang).await;
    }
    let locale = state.locale().cloned();
    let translate = |s: &str| {
        crate::i18n::strip_markers(&crate::i18n::translate(
            locale.as_deref(),
            &lang,
            &crate::i18n::Message::new(s),
            &[],
        ))
    };

    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    let nav = state.plugin_admin_nav();
    let hits = admin_nav::search(&viewer, &enabled, nav.links(), nav.cards(), &q, &translate);

    let mut context = tera::Context::new();
    context.insert("q", &q);
    context.insert("hits", &hits);
    context.insert("path", admin_nav::SEARCH);
    render_admin_template(&state, &viewer, "admin/search.html", context).await
}
