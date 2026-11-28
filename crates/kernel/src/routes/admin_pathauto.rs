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
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::i18n::UiMessage;
use crate::models::{Item, SiteConfig, User};
use crate::services::pathauto::update_alias_item;
use crate::state::AppState;
use trovato_sdk::types::ContentTypeDefinition;

use super::admin_form;
use super::helpers::{
    is_valid_machine_name, render_admin_template, render_error, render_server_error,
    require_permission,
};

/// Session key for flash messages on the pathauto settings page.
const FLASH_KEY: &str = "pathauto_flash";

/// The settings page, where every form here returns.
const BASE: &str = "/admin/config/pathauto";

/// The id of the patterns form.
const PATTERNS_FORM_ID: &str = "pathauto_form";

/// The id of the regenerate form. There is one for every content type: each
/// type's button submits it with its own `item_type`, so the page builds two
/// forms however many types there are.
const REGENERATE_FORM_ID: &str = "pathauto_regenerate_form";

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
// Forms
// =============================================================================

/// The patterns form: one field per content type, `pattern.{machine_name}`,
/// filled from `patterns`, each with the type's regenerate button beside it.
/// `t` translates the words built here into the viewer's admin language.
fn patterns_form(
    types: &[ContentTypeDefinition],
    patterns: &HashMap<String, String>,
    t: &dyn Fn(UiMessage) -> String,
) -> crate::form::Form {
    let mut form = crate::form::Form::new(PATTERNS_FORM_ID).action(BASE);
    for (weight, ct) in (0_i32..).zip(types) {
        // The button submits the regenerate form, drawn outside this one
        // since forms do not nest, naming its type as the submitter's value.
        let button = format!(
            r#"<button type="submit" form="{REGENERATE_FORM_ID}" name="item_type" value="{}" class="button button--secondary button--small">{}</button>"#,
            tera::escape_html(&ct.machine_name),
            tera::escape_html(&t(
                UiMessage::new("Regenerate aliases for {type}").arg("type", ct.label.clone())
            ))
        );
        let placeholder = t(UiMessage::new("e.g. {example}")
            .arg("example", format!("{}s/[title]", ct.machine_name)));
        form = form.element(
            format!("pattern.{}", ct.machine_name),
            FormElement::textfield()
                .title(ct.label.clone())
                .description(ct.machine_name.clone())
                .default_value(patterns.get(&ct.machine_name).cloned().unwrap_or_default())
                .attribute("placeholder", placeholder)
                .suffix(button)
                .weight(weight),
        );
    }
    form.element(
        "op",
        FormElement::submit("Save configuration").weight(i32::MAX),
    )
}

/// The regenerate form. It has no button of its own: the patterns form draws
/// one beside each type's field, which posts the type as `item_type`. It
/// asks first (`?confirm`), with the question and the confirming button's
/// words already translated, as the confirmation page expects them.
fn regenerate_form(t: &dyn Fn(UiMessage) -> String) -> crate::form::Form {
    crate::form::Form::new(REGENERATE_FORM_ID)
        .action(format!("{BASE}/regenerate?confirm"))
        .element(
            "_confirm",
            FormElement::hidden().default_value(t(UiMessage::new(
                "Save pattern changes first. Regenerate aliases for every item of this content \
                 type using the saved pattern?",
            ))),
        )
        .element(
            "_confirm_label",
            FormElement::hidden().default_value(t(UiMessage::new("Regenerate aliases"))),
        )
}

// =============================================================================
// Handlers
// =============================================================================

/// Render the settings page: the patterns form, filled with `patterns` and
/// showing `errors`, and the regenerate form its buttons submit.
async fn render_page(
    state: &AppState,
    session: &Session,
    user: &User,
    content_types: &[ContentTypeDefinition],
    patterns: &HashMap<String, String>,
    errors: &[ValidationError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = super::admin_nav::AdminText::for_page(state, &viewer, BASE).await;
    let t = |message: UiMessage| message.text(state.locale().map(|l| &**l), &text.lang);

    let mut regenerate_html = String::new();
    let mut form_html = String::new();
    if !content_types.is_empty() {
        regenerate_html =
            match admin_form::render(state, session, user, regenerate_form(&t), &[]).await {
                Ok(html) => html,
                Err(response) => return response,
            };
        form_html = match admin_form::render(
            state,
            session,
            user,
            patterns_form(content_types, patterns, &t),
            errors,
        )
        .await
        {
            Ok(html) => html,
            Err(response) => return response,
        };
    }
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("content_types", &content_types);
    context.insert("regenerate_form", &regenerate_html);
    context.insert("form", &form_html);
    context.insert("path", BASE);
    if let Some(msg) = flash {
        context.insert("flash", &msg);
    }

    render_admin_template(state, &viewer, "admin/pathauto.html", context).await
}

/// Pathauto configuration page.
///
/// Renders a form listing all registered content types with a text input
/// for each type's URL pattern. Patterns are pre-populated from `site_config`.
///
/// GET /admin/config/pathauto
async fn pathauto_config_page(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    let content_types = state.content_types().list_all().await;

    // Load existing patterns map from site_config.
    let patterns_json = match SiteConfig::get(state.db(), "pathauto_patterns").await {
        Ok(v) => v.unwrap_or(serde_json::json!({})),
        Err(e) => {
            tracing::error!(error = %e, "failed to load pathauto patterns");
            return render_server_error("Failed to load pathauto configuration.");
        }
    };

    // Build a flat map of machine_name -> pattern string for the form.
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

    render_page(&state, &session, &user, &content_types, &patterns, &[]).await
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
/// `api/`) are refused on their field, since the alias middleware skips those
/// paths and such patterns would generate unreachable aliases.
///
/// POST /admin/config/pathauto
async fn save_pathauto_config(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Collect non-empty patterns from fields prefixed with "pattern.".
    let mut patterns = serde_json::Map::new();
    let mut submitted: HashMap<String, String> = HashMap::new();
    let mut errors = Vec::new();
    for (key, value) in &posted.0 {
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
        submitted.insert(type_name.to_string(), value.clone());

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
            errors.push(ValidationError::field(
                key.clone(),
                format!(
                    "Pattern for '{type_name}' begins with a reserved path prefix (e.g. admin/, \
                     api/, user/). The alias middleware skips these paths, so the alias would \
                     never resolve. Choose a different prefix."
                ),
            ));
            continue;
        }

        patterns.insert(
            type_name.to_string(),
            serde_json::Value::String(trimmed.to_string()),
        );
    }

    let form_state =
        match admin_form::check(&state, &session, &user, &posted, PATTERNS_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                let content_types = state.content_types().list_all().await;
                return render_page(&state, &session, &user, &content_types, &submitted, &errors)
                    .await;
            }
            Err(Err(response)) => return response,
        };

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

    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session.insert(FLASH_KEY, "Pathauto patterns saved.").await;
    Redirect::to(BASE).into_response()
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // The type is the value of the button that submitted the form.
    let item_type = posted.get("item_type").unwrap_or_default().to_string();
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        REGENERATE_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
        Err(Err(response)) => return response,
    };

    // Validate that the requested type is actually registered. This prevents
    // log noise and accidental regeneration calls for non-existent types from
    // crafted requests.
    let content_types = state.content_types().list_all().await;
    if !content_types.iter().any(|ct| ct.machine_name == item_type) {
        return render_error("Unknown content type. Select a type from the list.");
    }

    let items = match Item::list_by_type(state.db(), &item_type).await {
        Ok(items) => items,
        Err(e) => {
            tracing::error!(error = %e, item_type = %item_type, "failed to list items for regeneration");
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
        match update_alias_item(state.db(), item).await {
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

    admin_form::submitted(&state, &user, &form_state, &posted).await;
    tracing::info!(
        item_type = %item_type,
        aliases_updated = count,
        "pathauto regeneration complete"
    );

    let msg = match count {
        1 => format!("1 alias created or updated for '{item_type}'."),
        n => format!("{n} aliases created or updated for '{item_type}'."),
    };
    let _ = session.insert(FLASH_KEY, msg).await;
    Redirect::to(BASE).into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the pathauto admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(BASE, get(pathauto_config_page).post(save_pathauto_config))
        .route(
            "/admin/config/pathauto/regenerate",
            post(regenerate_aliases),
        )
}
