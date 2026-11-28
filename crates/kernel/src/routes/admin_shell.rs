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

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::state::AppState;

use super::admin_form;
use super::admin_nav::{self, GROUPS, Group, Preferences};
use super::admin_search_index;
use super::helpers::{
    admin_user_context, render_admin_template, render_not_found, require_permission,
    require_permission_json,
};

/// What every page here asks for: admission to the admin.
const ADMISSION: &str = "access administration pages";

/// Session key for the saved message.
const FLASH_KEY: &str = "admin_preferences_flash";

/// The admin preferences form.
const PREFERENCES_FORM_ID: &str = "admin_preferences_form";

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

/// GET /admin/preferences
async fn preferences_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let prefs = Preferences::load(state.db(), user.id).await;
    render_preferences(&state, &session, &user, &prefs, &[]).await
}

/// The preferences page: its form showing `prefs` and `errors`, and under
/// it a sign out button that sends the form's CSRF token to `/user/logout`.
async fn render_preferences(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    prefs: &Preferences,
    errors: &[ValidationError],
) -> Response {
    let viewer = admin_user_context(state, user).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut languages: Vec<(String, String)> = crate::models::Language::list_all(state.db())
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|l| (l.id, l.label))
        .collect();
    if crate::i18n::pseudo_locales_enabled() {
        languages.push((
            crate::i18n::PSEUDO_ACCENTED.to_string(),
            "Pseudo (accented, longer)".to_string(),
        ));
        languages.push((
            crate::i18n::PSEUDO_RTL.to_string(),
            "Pseudo (right to left)".to_string(),
        ));
    }
    let language = prefs
        .language
        .clone()
        .unwrap_or_else(|| state.default_language().to_string());

    let text = admin_nav::AdminText::for_page(state, &viewer, admin_nav::PREFERENCES).await;
    let effect = format!(
        r#"<p class="form__effect"><strong class="form__effect-label">{}</strong> {}</p>"#,
        text.html("What this changes:", &[]),
        text.html(
            "only how this administration looks and reads for you. Nobody else is affected.",
            &[]
        )
    );
    let form = preferences_form_for(prefs, &language, languages, effect);
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("form_id", PREFERENCES_FORM_ID);
    context.insert("default_language", &state.default_language());
    context.insert("flash", &flash);
    context.insert("path", admin_nav::PREFERENCES);
    render_admin_template(state, &viewer, "admin/preferences.html", context).await
}

/// The preferences form, showing `prefs` with `language` (the stored admin
/// language, or the site's) chosen among `languages`. `effect` is the
/// translated sentence above the button saying what saving changes.
fn preferences_form_for(
    prefs: &Preferences,
    language: &str,
    languages: Vec<(String, String)>,
    effect: String,
) -> crate::form::Form {
    let choice = |value: &str, label: &str| (value.to_string(), label.to_string());
    crate::form::Form::new(PREFERENCES_FORM_ID)
        .action(admin_nav::PREFERENCES)
        .element(
            "theme",
            FormElement::radio(vec![
                choice("system", "System"),
                choice("light", "Light"),
                choice("dark", "Dark"),
            ])
            .title("Colour theme")
            .description("System follows your device's light or dark setting.")
            .default_value(prefs.theme())
            .weight(0),
        )
        .element(
            "language",
            // Required: the list is every language and has no empty choice.
            FormElement::select(languages)
                .title("Administration language")
                .description(
                    "The language these pages are shown in. It does not change the language of \
                     your content.",
                )
                .default_value(language)
                .required()
                .weight(1),
        )
        .element(
            "search_shortcuts",
            FormElement::fieldset()
                .title("Search shortcuts")
                .weight(2)
                .child(
                    "search_slash",
                    FormElement::checkbox()
                        .title("Open search with /")
                        .description(
                            "Pressing / anywhere outside a text field opens search. Turn it off \
                             if it gets in the way of your screen reader or other keys.",
                        )
                        .default_value(!prefs.search_slash_off)
                        .weight(0),
                )
                .child(
                    "search_key",
                    // The empty choice, "None", is the select's placeholder.
                    FormElement::select(vec![
                        choice("ctrl-k", "Control+K (Command+K on a Mac)"),
                        choice("alt-k", "Alt+K (Option+K on a Mac)"),
                    ])
                    .title("Another key to open search")
                    .description(
                        "Works in text fields too. Control+K replaces the browser's own use of \
                         that key on these pages.",
                    )
                    .placeholder("None")
                    .default_value(prefs.search_key())
                    .weight(1),
                ),
        )
        .element("effect", FormElement::markup(effect).weight(3))
        .element("op", FormElement::submit("Save preferences").weight(10))
}

/// POST /admin/preferences
async fn preferences_save(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let theme = posted.get("theme").unwrap_or_default();
    let lang = posted
        .get("language")
        .unwrap_or_default()
        .trim()
        .to_string();
    let search_key = posted.get("search_key").unwrap_or_default();

    let mut prefs = Preferences::load(state.db(), user.id).await;
    prefs.theme = match theme {
        "light" | "dark" => Some(theme.to_string()),
        _ => None,
    };
    let known = state.known_languages().contains(&lang)
        || (crate::i18n::is_pseudo(&lang) && crate::i18n::pseudo_locales_enabled());
    let errors = if !lang.is_empty() && !known {
        vec![ValidationError::field(
            "language",
            "Choose one of the languages listed.",
        )]
    } else {
        Vec::new()
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        PREFERENCES_FORM_ID,
        errors,
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(errors)) => {
            return render_preferences(&state, &session, &user, &prefs, &errors).await;
        }
        Err(Err(refusal)) => return refusal,
    };
    prefs.language =
        (!lang.is_empty() && lang != state.default_language()).then(|| lang.to_string());

    prefs.search_slash_off = !posted.contains("search_slash");
    prefs.search_key = match search_key {
        "ctrl-k" | "alt-k" => Some(search_key.to_string()),
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
    admin_form::submitted(&state, &user, &form_state, &posted).await;
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

/// The id of the results page's own search form.
const SEARCH_PAGE_FORM_ID: &str = "admin_search_page_form";

/// The signed-in user and their viewer, their admin language, and what `q`
/// finds for them; or the refusal when they may not use the administration.
async fn run_search(
    state: &AppState,
    session: &Session,
    raw: &str,
) -> Result<
    (
        crate::models::User,
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
    Ok((user, viewer, q, lang, groups))
}

/// GET /admin/search?q=: the results page, grouped under headings, below
/// its own search form (a GET form through the form pipeline, B83c).
async fn search(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<SearchQuery>,
) -> Response {
    use crate::form::{Form, FormElement};
    let (user, viewer, q, lang, groups) = match run_search(&state, &session, &query.q).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    // The form's name for assistive technology; an attribute, so translated
    // here rather than by the templates.
    let again = crate::i18n::strip_markers(
        &crate::i18n::UiMessage::new("Search again").text(state.locale().map(|l| &**l), &lang),
    );
    let form = Form::new(SEARCH_PAGE_FORM_ID)
        .method("get")
        .action(admin_nav::SEARCH)
        .attribute("class", "search-page-form")
        .attribute("role", "search")
        .attribute("aria-label", again)
        .element(
            "q",
            FormElement::input("search")
                .title("Search for")
                .default_value(q.clone())
                .attribute("autocomplete", "off")
                .weight(0),
        )
        .element("op", FormElement::submit("Search").weight(1));
    let form = match super::admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(refusal) => return refusal,
    };
    let mut context = tera::Context::new();
    context.insert("form", &form);
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
    let Ok((_, _, q, lang, groups)) = run_search(&state, &session, &query.q).await else {
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
