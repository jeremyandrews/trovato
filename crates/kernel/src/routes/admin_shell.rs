//! The pages the admin shell itself owns: each group's landing page, the
//! viewer's admin preferences, and admin search.
//!
//! - `GET /admin/{group}`: for a group whose bare address no page holds
//!   (structure, extend, config, reports), a redirect to its landing page.
//! - `GET /admin/{group}/overview`: every page in the group the viewer may
//!   open, each with its one sentence description, from the registry in
//!   [`super::admin_nav`]. The way to browse the admin without JavaScript.
//! - `GET` and `POST /admin/preferences`: the colour theme and admin language.
//! - `POST /admin/preferences/nav`: one sidebar group the viewer opened or
//!   closed by hand (`{ group, open }`), sent by its disclosure button (JSON,
//!   same-site requests only). Only the current page's group opens by
//!   default; the groups opened by hand are remembered.
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
use super::admin_search_index;
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
        .route(admin_nav::SEARCH, get(search))
        .route("/admin/search/suggest", get(suggest));
    // A group's bare address, where no page already has it (`/admin/content`
    // and `/admin/people` are their listings, `/admin/appearance` its page):
    // the address Drupal gives each overview, so an old bookmark lands there.
    router = router
        .route("/admin/structure", get(group_index))
        .route("/admin/extend", get(group_index))
        .route("/admin/config", get(group_index))
        .route("/admin/reports", get(group_index));
    // One static route per group rather than `/admin/{group}/overview`, so
    // no path parameter competes with the existing `/admin/content/...` and
    // `/admin/people/...` routes.
    for g in GROUPS {
        router = router.route(&g.group.landing_href(), get(landing));
    }
    router
}

/// GET /admin/{group}: a permanent redirect to the group's landing page.
/// Nothing is shown before the landing page checks its own gate.
async fn group_index(uri: axum::http::Uri) -> Response {
    Redirect::permanent(&format!("{}/overview", uri.path())).into_response()
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
    let mut landing = admin_nav::landing(&viewer, &enabled, group, nav.links(), nav.cards());
    // A group with nothing the viewer may open does not exist for them, as a
    // page they may not open does not.
    if landing.is_empty() {
        return render_not_found();
    }
    // Each tile's state, one after another in this request's own task, so
    // the request profile counts every statement. Only offered tiles carry
    // a status, and `status_line` applies its own visibility rules.
    for item in &mut landing.items {
        if let Some(status) = item.status {
            item.status_line = admin_nav::status_line(status, &viewer, &state).await;
        }
    }

    let mut context = tera::Context::new();
    context.insert("group", group.info().title);
    context.insert("group_description", group.info().description);
    context.insert("items", &landing.items);
    context.insert("primary", &landing.primary);
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
    context.insert("search_slash", &!prefs.search_slash_off);
    context.insert("search_key", prefs.search_key());
    context.insert(
        "search_key_options",
        &[
            ("", "None"),
            ("ctrl-k", "Control+K (Command+K on a Mac)"),
            ("alt-k", "Alt+K (Option+K on a Mac)"),
        ],
    );
    context.insert("default_language", &state.default_language());
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
    /// The `/` shortcut checkbox: present when ticked.
    #[serde(default)]
    search_slash: Option<String>,
    #[serde(default)]
    search_key: String,
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

    prefs.search_slash_off = form.search_slash.is_none();
    prefs.search_key = match form.search_key.as_str() {
        "ctrl-k" | "alt-k" => Some(form.search_key.clone()),
        _ => None,
    };
    let changes = serde_json::json!({
        "theme": prefs.theme,
        "language": prefs.language,
        "search_slash_off": prefs.search_slash_off,
        "search_key": prefs.search_key,
    });
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
struct NavToggle {
    /// The slug of the group the viewer opened or closed.
    group: String,
    /// Whether it is now open.
    open: bool,
}

/// POST /admin/preferences/nav: the viewer opened or closed one sidebar
/// group by hand, remembered so the next page renders it that way with no
/// flash. The group holding the current page is always open, so the sidebar
/// never sends it.
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
    Json(body): Json<NavToggle>,
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
    if Group::from_slug(&body.group).is_none() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match Preferences::toggle_group(state.db(), user.id, &body.group, body.open).await {
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

/// The viewer, their admin language, and what `q` finds for them; or the
/// refusal when they may not use the administration.
async fn run_search(
    state: &AppState,
    session: &Session,
    raw: &str,
) -> Result<
    (
        crate::tap::UserContext,
        String,
        String,
        Vec<admin_search_index::HitGroup>,
    ),
    Response,
> {
    let user = require_permission(state, session, ADMISSION).await?;
    let viewer = admin_user_context(state, &user).await;
    let q: String = raw.trim().chars().take(MAX_QUERY_LEN).collect();

    let prefs = Preferences::load(state.db(), user.id).await;
    let lang = admin_nav::admin_language(state, &prefs);
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
    let groups = admin_search_index::search(state, &viewer, &q, &translate).await;
    Ok((viewer, q, lang, groups))
}

/// GET /admin/search?q=: the results page, grouped under headings.
async fn search(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<SearchQuery>,
) -> Response {
    let (viewer, q, _, groups) = match run_search(&state, &session, &query.q).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let mut context = tera::Context::new();
    context.insert("q", &q);
    context.insert("count", &admin_search_index::count(&groups));
    context.insert("groups", &groups);
    context.insert("path", admin_nav::SEARCH);
    render_admin_template(&state, &viewer, "admin/search.html", context).await
}

/// One result as the command palette shows it: words already in the
/// viewer's language.
#[derive(Debug, Serialize)]
struct SuggestHit {
    label: String,
    context: String,
    href: String,
}

#[derive(Debug, Serialize)]
struct SuggestGroup {
    key: &'static str,
    title: String,
    hits: Vec<SuggestHit>,
}

#[derive(Debug, Serialize)]
struct Suggestions {
    q: String,
    count: usize,
    /// "N results", in the viewer's language, for the live region.
    summary: String,
    groups: Vec<SuggestGroup>,
}

/// GET /admin/search/suggest?q=: the same results as the page, as JSON for
/// the command palette.
async fn suggest(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<SearchQuery>,
) -> Response {
    let Ok((_, q, lang, groups)) = run_search(&state, &session, &query.q).await else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let locale = state.locale().cloned();
    let text =
        |m: &crate::i18n::UiMessage| crate::i18n::strip_markers(&m.text(locale.as_deref(), &lang));
    let count = admin_search_index::count(&groups);
    let summary = text(
        &crate::i18n::UiMessage::new("{count} result")
            .plural("{count} results", i64::try_from(count).unwrap_or(i64::MAX)),
    );
    let groups = groups
        .into_iter()
        .map(|g| SuggestGroup {
            key: g.key,
            title: text(&crate::i18n::UiMessage::new(g.title)),
            hits: g
                .hits
                .into_iter()
                .map(|h| SuggestHit {
                    label: if h.translate {
                        text(&crate::i18n::UiMessage::new(h.label))
                    } else {
                        h.label
                    },
                    context: text(&h.context),
                    href: h.href,
                })
                .collect(),
        })
        .collect();
    Json(Suggestions {
        q,
        count,
        summary,
        groups,
    })
    .into_response()
}
