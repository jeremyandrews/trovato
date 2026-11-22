//! Appearance (B108): `/admin/appearance`.
//!
//! Lists the themes (`theme::registry`: the stock theme and every theme the
//! deployment provides), and sets the default theme and the admin theme. The
//! choice is stored in `site_config`, applied by the theme engine at once and
//! on every other server through the `Themes` cache generation: no restart.
//! Every route takes `administer themes`; the form goes through `FormService`.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::state::AppState;
use crate::theme::ActiveThemes;
use crate::theme::registry::{self, STOCK};

use super::admin_form;
use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_permission,
};

/// The permission every route here takes.
pub const PERMISSION: &str = "administer themes";

const BASE: &str = "/admin/appearance";
const FORM_ID: &str = "appearance_form";
const FLASH_KEY: &str = "appearance_flash";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(appearance).post(appearance_submit))
        .route(&format!("{BASE}/{{theme}}/screenshot"), get(screenshot))
}

#[derive(Debug, Serialize)]
struct ThemeCard {
    machine_name: String,
    name: String,
    description: String,
    has_screenshot: bool,
    is_default: bool,
    is_admin: bool,
}

fn appearance_form(state: &AppState, active: &ActiveThemes) -> crate::form::Form {
    let options: Vec<(String, String)> = state
        .theme()
        .themes()
        .into_iter()
        .map(|t| (t.machine_name, t.name))
        .collect();
    crate::form::Form::new(FORM_ID)
        .action(BASE)
        .element(
            "default_theme",
            FormElement::radio(options.clone())
                .title("Default theme")
                .description("Every page outside the administration section.")
                .default_value(active.default.clone())
                .required()
                .weight(0),
        )
        .element(
            "admin_theme",
            FormElement::select(options)
                .title("Administration theme")
                .description(
                    "Pages under /admin. The stock theme is the one the administration \
                     screens are built for.",
                )
                .default_value(active.admin.clone())
                .required()
                .weight(1),
        )
        .element("op", FormElement::submit("Save appearance").weight(10))
}

async fn render(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    shown: &ActiveThemes,
    errors: &[ValidationError],
) -> Response {
    let active = state.theme().active();
    let cards: Vec<ThemeCard> = state
        .theme()
        .themes()
        .into_iter()
        .map(|t| ThemeCard {
            has_screenshot: t.screenshot_path().is_some(),
            is_default: t.machine_name == active.default,
            is_admin: t.machine_name == active.admin,
            machine_name: t.machine_name,
            name: t.name,
            description: t.description,
        })
        .collect();
    let form_html =
        match admin_form::render(state, session, user, appearance_form(state, shown), errors).await
        {
            Ok(html) => html,
            Err(response) => return response,
        };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let viewer = super::helpers::user_context_for(state, user).await;
    let mut context = tera::Context::new();
    context.insert("themes", &cards);
    context.insert("form", &form_html);
    context.insert("flash", &flash);
    context.insert("path", BASE);
    render_admin_template(state, &viewer, "admin/appearance.html", context).await
}

/// GET /admin/appearance
async fn appearance(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let active = state.theme().active();
    render(&state, &session, &user, &active, &[]).await
}

/// POST /admin/appearance
async fn appearance_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let wanted = ActiveThemes {
        default: posted.get("default_theme").unwrap_or(STOCK).to_string(),
        admin: posted.get("admin_theme").unwrap_or(STOCK).to_string(),
    };
    let mut errors = Vec::new();
    for (field, name) in [
        ("default_theme", &wanted.default),
        ("admin_theme", &wanted.admin),
    ] {
        if state.theme().theme(name).is_none() {
            errors.push(ValidationError::field(
                field,
                format!("There is no theme named {name}."),
            ));
        }
    }
    match admin_form::check(&state, &session, &user, &posted, FORM_ID, errors).await {
        Ok(()) => {}
        Err(Ok(errors)) => return render(&state, &session, &user, &wanted, &errors).await,
        Err(Err(response)) => return response,
    }
    if let Err(e) = registry::save_active(state.db(), state.theme(), &wanted).await {
        tracing::error!(error = %e, "failed to save the theme setting");
        return render_server_error("The appearance settings could not be saved.");
    }
    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        default_theme = %wanted.default,
        admin_theme = %wanted.admin,
        "themes changed"
    );
    let _ = session
        .insert(FLASH_KEY, "The appearance settings were saved.")
        .await;
    Redirect::to(BASE).into_response()
}

/// GET /admin/appearance/{theme}/screenshot
async fn screenshot(
    State(state): State<AppState>,
    session: Session,
    Path(theme): Path<String>,
) -> Response {
    if let Err(refusal) = require_permission(&state, &session, PERMISSION).await {
        return refusal;
    }
    let Some(path) = state
        .theme()
        .theme(&theme)
        .and_then(|t| t.screenshot_path())
    else {
        return render_not_found();
    };
    let content_type = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        _ => return render_not_found(),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, content_type),
                (header::CACHE_CONTROL, "private, max-age=3600"),
            ],
            Body::from(bytes),
        )
            .into_response(),
        Err(_) => render_not_found(),
    }
}
