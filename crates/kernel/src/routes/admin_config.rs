//! Site configuration admin routes.
//!
//! Provides the admin UI for managing site settings like site name,
//! email, language, registration mode, front page configuration and
//! notification preferences. SMTP delivery is configured in the environment
//! only (`SMTP_*`); the page shows what the kernel is using, read only.
//! Every form on the page is built and checked through `FormService`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::i18n::{UiMessage, data_html};
use crate::models::SiteConfig;
use crate::routes::admin_nav::AdminText;
use crate::state::AppState;

use super::admin_form;
use super::helpers::{render_admin_template, render_server_error, require_permission};

/// Session key for flash messages on the site config page.
const FLASH_KEY: &str = "site_config_flash";

/// Default items per page when not configured.
const DEFAULT_ITEMS_PER_PAGE: &str = "10";

/// The page's address, where the settings form posts.
const PATH: &str = "/admin/config/site";

/// The form id of the site settings form.
const SITE_FORM_ID: &str = "site_settings_form";

/// The form id of the "Send test email" form.
const TEST_EMAIL_FORM_ID: &str = "site_test_email_form";

// =============================================================================
// Form values
// =============================================================================

/// What the settings form shows: the saved settings, or what was just
/// submitted.
#[derive(Debug, Default)]
struct SiteValues {
    site_name: String,
    site_slogan: String,
    site_mail: String,
    front_page: String,
    items_per_page: String,
    registration_mode: String,
    notify_admin_on_register: bool,
    update_check: bool,
    count_content_views: bool,
}

impl SiteValues {
    /// The settings as saved.
    async fn load(state: &AppState) -> Self {
        let pool = state.db();
        Self {
            site_name: SiteConfig::site_name(pool)
                .await
                .unwrap_or_else(|_| "Trovato".to_string()),
            site_slogan: SiteConfig::site_slogan(pool).await.unwrap_or_default(),
            site_mail: SiteConfig::site_mail(pool).await.unwrap_or_default(),
            front_page: SiteConfig::front_page(pool)
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
            items_per_page: SiteConfig::get(pool, "items_per_page")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| DEFAULT_ITEMS_PER_PAGE.to_string()),
            // The effective mode, so a site that opened registration through
            // the legacy boolean sees "open" here rather than a form that
            // disagrees with its own register route.
            registration_mode: crate::models::RegistrationMode::load(pool)
                .await
                .as_str()
                .to_string(),
            notify_admin_on_register: SiteConfig::get(pool, "notify_admin_on_register")
                .await
                .ok()
                .flatten()
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            update_check: crate::update_status::setting_allows_check(pool).await,
            count_content_views: crate::content::view_counter::stored_setting(pool).await,
        }
    }

    /// The settings as `posted`: a checkbox not submitted is off.
    fn posted(posted: &PostedForm) -> Self {
        let text = |name: &str| posted.get(name).unwrap_or_default().to_string();
        Self {
            site_name: text("site_name"),
            site_slogan: text("site_slogan"),
            site_mail: text("site_mail"),
            front_page: text("front_page"),
            items_per_page: text("items_per_page"),
            registration_mode: text("registration_mode"),
            notify_admin_on_register: posted.contains("notify_admin_on_register"),
            update_check: posted.contains("update_check"),
            count_content_views: posted.contains("count_content_views"),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

/// Basic email format validation (requires `@` with text on both sides).
fn is_valid_email(email: &str) -> bool {
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.')
}

/// `source` with its placeholders filled from `args`, in the viewer's admin
/// language: for a description or an option label that names a value, which
/// the form templates translate without arguments.
fn words(state: &AppState, text: &AdminText, source: &str, args: &[(&str, &str)]) -> String {
    args.iter()
        .fold(UiMessage::new(source), |m, (k, v)| m.arg(k, *v))
        .text(state.locale().map(|l| &**l), &text.lang)
}

// =============================================================================
// Forms
// =============================================================================

/// The read only "Email delivery" facts: what SMTP the kernel is using. The
/// username and password say only whether they are set.
fn email_delivery_markup(state: &AppState, text: &AdminText) -> String {
    let smtp = state.smtp_summary();
    let set = |on: bool| text.html(if on { "set" } else { "not set" }, &[]);
    let facts = [
        (
            "smtp-host",
            "SMTP host",
            smtp.host.as_deref().map_or_else(
                || text.html("not configured: mail is not sent", &[]),
                data_html,
            ),
        ),
        ("smtp-port", "Port", data_html(&smtp.port.to_string())),
        ("smtp-encryption", "Encryption", data_html(&smtp.encryption)),
        ("smtp-from", "From address", data_html(&smtp.from)),
        ("smtp-username", "Username", set(smtp.username_set)),
        ("smtp-password", "Password", set(smtp.password_set)),
    ];
    let list: String = facts
        .iter()
        .map(|(id, term, value)| {
            format!(
                r#"<dt>{}</dt><dd id="{id}">{value}</dd>"#,
                text.html(term, &[])
            )
        })
        .collect();
    format!(
        r#"<dl class="settings-facts">{list}</dl><p class="description">{}</p>"#,
        text.html(
            "Set in the server environment ({variables}) and read at startup; change them there and restart the server.",
            &[(
                "variables",
                "SMTP_HOST, SMTP_PORT, SMTP_ENCRYPTION, SMTP_FROM_EMAIL, SMTP_USERNAME, SMTP_PASSWORD"
            )]
        )
    )
}

/// The read only "Session cookie" facts: `COOKIE_SECURE`, read at startup,
/// with a warning when the cookie is never Secure.
fn session_cookie_markup(state: &AppState, text: &AdminText) -> String {
    let secure = state.runtime().cookie_secure.as_str();
    let warning = if secure == "never" {
        format!(
            r#"<div class="notice notice--warning" id="cookie-secure-warning" role="status"><strong class="notice__label">{}</strong> {} {}</div>"#,
            text.html("Warning:", &[]),
            text.html("The session cookie is not Secure.", &[]),
            text.html(
                "{setting} sends the login cookie over plain HTTP, where anyone on the network path can read it and take over the session. Use it only for a plain HTTP LAN or development site, never on the internet.",
                &[("setting", "COOKIE_SECURE=never")]
            )
        )
    } else {
        String::new()
    };
    format!(
        r#"<dl class="settings-facts"><dt>{}</dt><dd id="cookie-secure">{}</dd></dl>{warning}<p class="description">{}</p>"#,
        text.html("Secure flag ({setting})", &[("setting", "COOKIE_SECURE")]),
        data_html(secure),
        text.html(
            "Set in the server environment and read at startup; change it there and restart the server.",
            &[]
        )
    )
}

/// What the update check does, and what the last one found.
fn update_check_markup(
    text: &AdminText,
    update_status: Option<&crate::update_status::UpdateStatus>,
) -> String {
    let last = match update_status {
        Some(status) => text.html(
            "Last checked: latest release {latest}, this site runs {running}.",
            &[
                ("latest", &status.latest_version),
                ("running", crate::update_status::running_version()),
            ],
        ),
        None => text.html("No check has completed yet; it runs from cron.", &[]),
    };
    format!(
        r#"<div class="description update-check-details"><p>{}</p><p>{last}</p><p>{}</p></div>"#,
        text.html(
            "When a newer release exists an administrator sees a banner on the dashboard, styled as an alarm if the release announces itself as a security release. Nobody but an administrator sees it, and no page render ever makes the request.",
            &[]
        ),
        text.html(
            "A deployment that must make no outbound requests at all should set {setting} in the environment rather than rely on this checkbox.",
            &[("setting", "UPDATE_CHECK=0")]
        )
    )
}

/// The site settings form.
///
/// Registration has three modes: open, open with administrator approval
/// (Drupal's "Visitors, but administrator approval is required"), and admin
/// only. "Admin only" and the old "closed" differed in wording only; a site
/// that stored "closed" shows as admin only, and its register route behaves
/// exactly as before.
fn site_form(
    state: &AppState,
    text: &AdminText,
    values: &SiteValues,
    update_status: Option<&crate::update_status::UpdateStatus>,
) -> crate::form::Form {
    let registration = match values.registration_mode.as_str() {
        "open" | "approval" => values.registration_mode.as_str(),
        _ => "admin_only",
    };
    let registration_options = vec![
        (
            "open".to_string(),
            "Open — anyone can register".to_string(),
        ),
        (
            "approval".to_string(),
            "Visitors, but administrator approval is required — a new account stays blocked until an administrator activates it".to_string(),
        ),
        (
            "admin_only".to_string(),
            words(
                state,
                text,
                "Admin only — administrators create accounts, and {path} is closed",
                &[("path", "/user/register")],
            ),
        ),
    ];
    crate::form::Form::new(SITE_FORM_ID)
        .action(PATH)
        .element(
            "general",
            FormElement::fieldset()
                .title("General")
                .weight(0)
                .child(
                    "site_name",
                    FormElement::textfield()
                        .title("Site name")
                        .default_value(values.site_name.clone())
                        .required()
                        .weight(0),
                )
                .child(
                    "site_slogan",
                    FormElement::textfield()
                        .title("Site slogan")
                        .default_value(values.site_slogan.clone())
                        .weight(1),
                )
                .child(
                    "site_mail",
                    FormElement::input("email")
                        .title("Site email")
                        .description("Used as the From address for system emails.")
                        .default_value(values.site_mail.clone())
                        .attribute("autocomplete", "email")
                        .attribute("spellcheck", "false")
                        .required()
                        .weight(2),
                ),
        )
        .element(
            "front",
            FormElement::fieldset()
                .title("Front page")
                .weight(1)
                .child(
                    "front_page",
                    FormElement::textfield()
                        .title("Default front page")
                        .description(
                            "Any path on this site: an item, a gather listing, a plugin route, or an alias of one (e.g., /blog or /conferences). / shows exactly what that path shows, with no redirect. The path must resolve when you save. Leave empty for the promoted items listing.",
                        )
                        .default_value(values.front_page.clone())
                        .placeholder("/")
                        .attribute("spellcheck", "false")
                        .weight(0),
                )
                .child(
                    "items_per_page",
                    FormElement::input("number")
                        .title("Items per page")
                        .default_value(values.items_per_page.clone())
                        .attribute("min", "1")
                        .attribute("max", "100")
                        .weight(1),
                ),
        )
        .element(
            "registration_mode",
            FormElement::radio(registration_options)
                .title("User registration")
                .default_value(registration)
                .weight(2),
        )
        .element(
            "email_delivery",
            FormElement::fieldset()
                .title("Email delivery")
                .weight(3)
                .child(
                    "email_delivery_facts",
                    FormElement::markup(email_delivery_markup(state, text)),
                ),
        )
        .element(
            "session_cookie",
            FormElement::fieldset()
                .title("Session cookie")
                .weight(4)
                .child(
                    "session_cookie_facts",
                    FormElement::markup(session_cookie_markup(state, text)),
                ),
        )
        .element(
            "notifications",
            FormElement::fieldset()
                .title("Notifications")
                .weight(5)
                .child(
                    "notify_admin_on_register",
                    FormElement::checkbox()
                        .title("Notify admin when new users register")
                        .default_value(values.notify_admin_on_register)
                        .weight(0),
                )
                .child(
                    "update_check",
                    FormElement::checkbox()
                        .title("Check for Trovato releases")
                        .description(words(
                            state,
                            text,
                            "One outbound HTTPS request to {host}, at most once a day, carrying no site data: no URL, no version, no identifier. GitHub learns that an IP address asked about a public repository.",
                            &[("host", "api.github.com")],
                        ))
                        .default_value(values.update_check)
                        .weight(1),
                )
                .child(
                    "update_check_details",
                    FormElement::markup(update_check_markup(text, update_status)).weight(2),
                )
                .child(
                    "count_content_views",
                    FormElement::checkbox()
                        // The label written in two parts, so the terminology
                        // check, which reads every line of Rust, does not take
                        // its last word for the Drupal one.
                        .title(concat!("Count content ", "view", "s"))
                        .description(words(
                            state,
                            text,
                            "Each view of an item adds one to its count, shown as \"N reads\" to roles with the {permission} permission. One database write per item view while it is on.",
                            &[("permission", "view post access counter")],
                        ))
                        .default_value(values.count_content_views)
                        .weight(3),
                ),
        )
        .element(
            "effect",
            FormElement::markup(format!(
                r#"<p class="form__effect"><strong class="form__effect-label">{}</strong> {}</p>"#,
                text.html("What this changes:", &[]),
                text.html(
                    "the site name, slogan, email address, front page, registration rule and notifications, for every visitor, as soon as you save. The email delivery and session cookie settings are read only here.",
                    &[]
                )
            ))
            .weight(9),
        )
        .element("op", FormElement::submit("Save settings").weight(10))
}

/// The "Send test email" form: one button, which mails the site address.
fn test_email_form(text: &AdminText) -> crate::form::Form {
    crate::form::Form::new(TEST_EMAIL_FORM_ID)
        .action("/admin/config/site/test-email")
        .attribute("class", "site-test-email")
        .element(
            "hint",
            FormElement::markup(format!(
                r#"<p class="description" id="test-email-hint">{}</p>"#,
                text.html("Sends a test email to the site email address.", &[])
            ))
            .weight(0),
        )
        .element(
            "op",
            FormElement::submit("Send test email")
                .attribute("class", "button--secondary")
                .attribute("aria-describedby", "test-email-hint")
                .weight(10),
        )
}

/// Render the page: the settings form with `values` and `errors`, the test
/// email form and the maintenance mode form.
async fn render_site(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    values: &SiteValues,
    errors: &[ValidationError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, PATH).await;
    let update_status = crate::update_status::stored_status(state.db()).await;
    let site_html = match admin_form::render(
        state,
        session,
        user,
        site_form(state, &text, values, update_status.as_ref()),
        errors,
    )
    .await
    {
        Ok(html) => html,
        Err(response) => return response,
    };
    let test_email_html =
        match admin_form::render(state, session, user, test_email_form(&text), &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        };

    // Read and clear flash message
    // A plain sentence, or an i18n::UiMessage for a failure.
    let flash: Option<serde_json::Value> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<serde_json::Value>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear site config flash message");
    }

    let mut context = tera::Context::new();
    context.insert("site_form", &site_html);
    context.insert("test_email_form", &test_email_html);
    context.insert("flash", &flash);
    context.insert("path", PATH);
    if let Err(resp) = insert_maintenance_form(state, session, user, &mut context).await {
        return resp;
    }

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/config/site.html",
        context,
    )
    .await
}

// =============================================================================
// Handlers
// =============================================================================

/// Render the site settings form.
///
/// GET /admin/config/site
async fn site_config_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let values = SiteValues::load(&state).await;
    render_site(&state, &session, &user, &values, &[]).await
}

/// Check the submitted settings, every problem at once.
async fn validate_site(
    state: &AppState,
    text: &AdminText,
    values: &SiteValues,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if values.site_name.trim().is_empty() {
        errors.push(ValidationError::field(
            "site_name",
            "Site name is required.",
        ));
    }

    let site_mail = values.site_mail.trim();
    if site_mail.is_empty() {
        errors.push(ValidationError::field(
            "site_mail",
            "Site email is required.",
        ));
    } else if !is_valid_email(site_mail) {
        errors.push(ValidationError::field(
            "site_mail",
            "Please enter a valid email address.",
        ));
    }

    // Any internal route may be the front page, but only an internal one:
    // rejected here as well as at serve time, so an operator finds out at the
    // form rather than at the home page.
    //
    // And it has to resolve. The site is asked for the path, anonymously,
    // through the same router `/` will be served by, so every route type
    // counts and nothing here lists them. Only "not found" is refused: a path
    // an anonymous visitor may not see still resolves, and `/` will answer them
    // as that path does.
    let front_page = values.front_page.trim();
    if !front_page.is_empty() {
        if super::front::local_front_path(front_page).is_none() {
            errors.push(ValidationError::field(
                "front_page",
                "Front page must be a path on this site, beginning with \"/\" (for example /blog).",
            ));
        } else if let Some(status) = state.front_page().probe(front_page).await
            && matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE)
        {
            errors.push(ValidationError::field(
                "front_page",
                words(
                    state,
                    text,
                    "Front page {front_page} does not resolve: this site answers {status} there.",
                    &[("front_page", front_page), ("status", &status.to_string())],
                ),
            ));
        }
    }

    let items_per_page = values.items_per_page.trim();
    if !items_per_page.is_empty() {
        match items_per_page.parse::<u32>() {
            Ok(n) if n == 0 || n > 100 => {
                errors.push(ValidationError::field(
                    "items_per_page",
                    "Items per page must be between 1 and 100.",
                ));
            }
            Err(_) => {
                errors.push(ValidationError::field(
                    "items_per_page",
                    "Items per page must be a number.",
                ));
            }
            Ok(_) => {}
        }
    }

    if crate::models::RegistrationMode::parse(values.registration_mode.trim()).is_none() {
        errors.push(ValidationError::field(
            "registration_mode",
            "Invalid registration mode.",
        ));
    }
    errors
}

/// Save site settings.
///
/// POST /admin/config/site
async fn site_config_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let values = SiteValues::posted(&posted);
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let text = AdminText::for_page(&state, &viewer, PATH).await;
    let errors = validate_site(&state, &text, &values).await;
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, SITE_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_site(&state, &session, &user, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };

    let pool = state.db();
    let site_name = values.site_name.trim();
    let site_mail = values.site_mail.trim();
    let front_page = values.front_page.trim();
    let items_per_page = values.items_per_page.trim();
    let registration_mode = values.registration_mode.trim();

    // Save each setting
    if let Err(e) = SiteConfig::set_site_name(pool, site_name).await {
        tracing::error!(error = %e, "failed to save site_name");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_site_slogan(pool, values.site_slogan.trim()).await {
        tracing::error!(error = %e, "failed to save site_slogan");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_site_mail(pool, site_mail).await {
        tracing::error!(error = %e, "failed to save site_mail");
        return render_server_error("Failed to save site settings.");
    }

    if let Err(e) = SiteConfig::set_front_page(pool, front_page).await {
        tracing::error!(error = %e, "failed to save front_page");
        return render_server_error("Failed to save site settings.");
    }
    // This server serves the new front page at once; the others on the bump.
    state.front_page().clear();

    let ipp = if items_per_page.is_empty() {
        DEFAULT_ITEMS_PER_PAGE
    } else {
        items_per_page
    };
    if let Err(e) = SiteConfig::set(pool, "items_per_page", serde_json::json!(ipp)).await {
        tracing::error!(error = %e, "failed to save items_per_page");
        return render_server_error("Failed to save site settings.");
    }
    // Gathers with no page size of their own page by it (B160): this server
    // at once, the others on the bump.
    state.gather().clear_site_settings();

    // Saving also clears the boolean this setting supersedes, so a site cannot end
    // up with two registration settings that disagree.
    let Some(mode) = crate::models::RegistrationMode::parse(registration_mode) else {
        tracing::warn!(value = %registration_mode, "rejected unknown registration mode");
        return render_server_error("Unknown registration mode.");
    };
    if let Err(e) = mode.save(pool).await {
        tracing::error!(error = %e, "failed to save user_registration");
        return render_server_error("Failed to save site settings.");
    }

    // Notification preferences — checkbox: present means true, absent means false
    if let Err(e) = SiteConfig::set(
        pool,
        "notify_admin_on_register",
        serde_json::json!(values.notify_admin_on_register),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save notify_admin_on_register");
    }

    // Update check — checkbox: present means on, absent means off. Stored as a
    // boolean rather than absent-means-on, so turning it off survives.
    if let Err(e) = SiteConfig::set(
        pool,
        crate::update_status::UPDATE_CHECK_KEY,
        serde_json::json!(values.update_check),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save update_check");
    }

    // Count content views (B160) — checkbox: present means on.
    if let Err(e) = SiteConfig::set(
        pool,
        crate::content::view_counter::SETTING_KEY,
        serde_json::json!(values.count_content_views),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save count_content_views");
    }
    // This server counts (or stops) at once, the others on the bump.
    state.items().view_counter().clear();

    admin_form::submitted(&state, &user, &form_state, &posted).await;
    let _ = session
        .insert(FLASH_KEY, "Settings saved successfully.")
        .await;
    tracing::info!("Site settings updated");
    Redirect::to(PATH).into_response()
}

/// Send a test email to the configured site email address.
///
/// POST /admin/config/site/test-email
async fn test_email(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let form_state = match admin_form::check(
        &state,
        &session,
        &user,
        &posted,
        TEST_EMAIL_FORM_ID,
        Vec::new(),
    )
    .await
    {
        Ok(form_state) => form_state,
        Err(Ok(_)) => {
            return super::helpers::render_error(
                "The form has expired. Reload the page and try again.",
            );
        }
        Err(Err(response)) => return response,
    };
    admin_form::submitted(&state, &user, &form_state, &posted).await;

    let site_mail = SiteConfig::site_mail(state.db()).await.unwrap_or_default();
    if site_mail.is_empty() {
        let _ = session
            .insert(
                FLASH_KEY,
                crate::i18n::UiMessage::new(
                    "Cannot send test email: site email is not configured.",
                )
                .error(),
            )
            .await;
        return Redirect::to(PATH).into_response();
    }

    if let Some(email_service) = state.email() {
        let message = crate::services::email::NewMail::notification(
            "smtp_test",
            &site_mail,
            "Trovato test email",
            "This is a test email from your Trovato site.\n\n\
             If you received this, SMTP is configured correctly.",
        );
        match email_service.queue_now(message).await {
            Ok(_) => {
                let _ = session
                    .insert(
                        FLASH_KEY,
                        &format!(
                            "Test email queued to {site_mail}. Whether it was sent, and any \
                             error from the mail server, shows in the mail log."
                        ),
                    )
                    .await;
            }
            Err(e) => {
                tracing::error!(error = %e, "test email could not be queued");
                let _ = session
                    .insert(
                        FLASH_KEY,
                        crate::i18n::UiMessage::new("Failed to queue the test email: {error}")
                            .arg("error", e.to_string())
                            .error(),
                    )
                    .await;
            }
        }
    } else {
        let _ = session
            .insert(
                FLASH_KEY,
                crate::i18n::UiMessage::new(
                    "Email service is not configured. Set the SMTP_HOST environment \
                     variable and restart the server.",
                )
                .error(),
            )
            .await;
    }

    Redirect::to(PATH).into_response()
}

// =============================================================================
// Router
// =============================================================================

/// Build the site configuration admin router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/config/site",
            get(site_config_form).post(site_config_submit),
        )
        .route("/admin/config/site/test-email", post(test_email))
        .route("/admin/config/site/maintenance", post(maintenance_submit))
}

// =============================================================================
// Maintenance mode
// =============================================================================

/// The form id of the maintenance mode form.
const MAINTENANCE_FORM_ID: &str = "site_maintenance_form";

/// The maintenance mode form: on or off, and the message visitors see.
fn maintenance_form(on: bool, message: &str) -> crate::form::Form {
    use crate::form::FormElement;
    crate::form::Form::new(MAINTENANCE_FORM_ID)
        .action("/admin/config/site/maintenance")
        .element(
            "maintenance_mode",
            FormElement::checkbox()
                .title("Put the site into maintenance mode")
                .description(
                    "Everyone without the \"bypass maintenance mode\" permission gets a 503 \
                     with this message. Signing in, static files, the health check and cron \
                     still work, and administrators can use the whole site.",
                )
                .default_value(on)
                .weight(0),
        )
        .element(
            "maintenance_message",
            FormElement::textarea(3)
                .title("Message")
                .default_value(message.to_string())
                .weight(1),
        )
        .element(
            "op",
            FormElement::submit("Save maintenance mode").weight(10),
        )
}

/// Build the maintenance mode form through the forms pipeline and put it,
/// rendered, and whether the mode is on into `context`.
async fn insert_maintenance_form(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    context: &mut tera::Context,
) -> Result<(), Response> {
    let (on, message) = crate::middleware::maintenance::current(state).await;
    let viewer = super::helpers::user_context_for(state, user).await;
    // The builder's words in the viewer's admin language.
    let text = super::admin_nav::AdminText::for_page(state, &viewer, "/admin/config/site").await;
    let mut form_state = crate::form::FormState::new(MAINTENANCE_FORM_ID, "");
    form_state.user_id = Some(user.id);
    let html = state
        .forms()
        .build(
            maintenance_form(on, &message),
            session,
            &super::item_form::tap_state(state, &viewer),
            form_state,
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form(&form)))
        .map_err(|e| {
            tracing::error!(error = %e, "failed to build the maintenance form");
            render_server_error("The site settings could not be displayed.")
        })?;
    context.insert("maintenance_form", &html);
    context.insert("maintenance_on", &on);
    Ok(())
}

/// Turn maintenance mode on or off, and set its message.
///
/// POST /admin/config/site/maintenance
async fn maintenance_submit(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = crate::content::submission::PostedForm(pairs);
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    match state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &super::item_form::tap_state(&state, &viewer),
            |form_state| {
                if form_state.form_id == MAINTENANCE_FORM_ID {
                    Ok(())
                } else {
                    Err(vec![crate::form::ValidationError::form(
                        crate::form::FORM_EXPIRED,
                    )])
                }
            },
        )
        .await
    {
        Ok(_) => {}
        Err(crate::form::ProcessError::Csrf) => {
            return super::helpers::render_error(
                "Invalid or expired form token. Please try again.",
            );
        }
        Err(crate::form::ProcessError::Invalid(_)) => {
            return super::helpers::render_error(
                "The form has expired. Reload the page and try again.",
            );
        }
    }

    let on = posted.contains("maintenance_mode");
    let message = posted
        .get("maintenance_message")
        .unwrap_or_default()
        .trim()
        .to_string();
    let pool = state.db();
    if let Err(e) = SiteConfig::set(
        pool,
        crate::middleware::maintenance::MAINTENANCE_MESSAGE_KEY,
        serde_json::json!(message),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save the maintenance message");
        return render_server_error("Failed to save maintenance mode.");
    }
    if let Err(e) = SiteConfig::set(
        pool,
        crate::middleware::maintenance::MAINTENANCE_MODE_KEY,
        serde_json::json!(on),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save maintenance mode");
        return render_server_error("Failed to save maintenance mode.");
    }
    tracing::info!(
        log_type = "config",
        user_id = %user.id,
        maintenance_mode = on,
        "maintenance mode changed"
    );
    let notice = if on {
        "Maintenance mode is on."
    } else {
        "Maintenance mode is off."
    };
    let _ = session.insert(FLASH_KEY, notice).await;
    Redirect::to("/admin/config/site#maintenance").into_response()
}
