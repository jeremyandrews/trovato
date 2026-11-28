//! Reports: the status report, the cron report and the site log (B107, B161).
//!
//! `/admin/reports/status` checks the versions, the database, the cache
//! server, cron, the file system and the security settings, now, and links to
//! running cron by hand; it takes `administer site`.
//! `/admin/reports/cron` shows the last cron run (when, on which server, how
//! long, what it did) and runs cron now, in this process, from a button; it
//! takes `run cron` or `administer site`.
//! `/admin/reports/log` pages through `system_log`, filtered by message text,
//! level and type, and sorted by time, level or type. It takes
//! `administer site`, as `/cron/status` does, and also opens to
//! `access site reports`, as Drupal's recent log messages do.

use crate::redis_conn::BoundedRedis;
use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::cron::{CRON_INTERVAL_CONFIG_KEY, CronIntervalSource, CronResult};
use crate::form::{FormElement, FormState, ProcessError, ValidationError};
use crate::models::SiteConfig;
use crate::state::AppState;
use crate::system_log::LogFilter;

use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    render_admin_template, render_error, render_server_error, require_any_permission,
    require_permission,
};

/// The permission both reports take.
const PERMISSION: &str = "administer site";

/// The site log's own permission, Drupal's: it opens the log and nothing else.
pub const LOG_PERMISSION: &str = "access site reports";

/// Session key for the cron report's message after Run now.
const CRON_FLASH_KEY: &str = "cron_report_flash";

/// Either runs cron by hand. `administer site` is what ran it before B161.
const CRON_PERMISSIONS: &[&str] = &["run cron", PERMISSION];

/// The levels the log filter offers.
const LEVELS: &[&str] = &["error", "warn", "info"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/reports/cron", get(cron_report).post(run_cron_now))
        .route(
            "/admin/reports/cron/scheduler",
            axum::routing::post(save_scheduler_settings),
        )
        .route("/admin/reports/status", get(status_report))
        .route("/admin/reports/log", get(log_report))
        .route("/admin/reports/accessibility", get(accessibility_report))
}

// =============================================================================
// Cron
// =============================================================================

/// The last run, as the template shows it.
#[derive(Debug, Serialize)]
struct LastRunView {
    timestamp: i64,
    hostname: String,
    status: String,
    duration_ms: Option<u64>,
    tasks: Vec<String>,
    /// The raw result, for a record written before the fields above existed.
    result: String,
}

/// The Run now form, through the forms pipeline.
fn run_now_form() -> crate::form::Form {
    crate::form::Form::new("cron_run_now_form")
        .action("/admin/reports/cron")
        .element("op", FormElement::submit("Run cron now").weight(0))
}

/// The scheduler settings form's id.
const SCHEDULER_FORM_ID: &str = "cron_scheduler_settings_form";

/// The built-in scheduler's one setting, through the forms pipeline.
fn scheduler_settings_form(interval_secs: u64) -> crate::form::Form {
    crate::form::Form::new(SCHEDULER_FORM_ID)
        .action("/admin/reports/cron/scheduler")
        .element(
            "interval_secs",
            FormElement::textfield()
                .title("Scheduled interval (seconds)")
                .description(
                    "How often this server runs cron on its own, with no external caller \
                     needed. The default is 300 (five minutes); 0 turns the built-in \
                     scheduler off.",
                )
                .default_value(interval_secs.to_string())
                .attribute("inputmode", "numeric")
                .required()
                .weight(0),
        )
        .element(
            "op",
            FormElement::submit("Save scheduler setting").weight(10),
        )
}

/// GET /admin/reports/cron
async fn cron_report(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_any_permission(&state, &session, CRON_PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let last_run = match state.cron().last_run().await {
        Ok(run) => run.map(|run| LastRunView {
            timestamp: run.timestamp,
            hostname: run.hostname,
            status: run.status,
            duration_ms: run.duration_ms,
            tasks: run.tasks,
            result: run.result,
        }),
        Err(e) => {
            tracing::warn!(error = %e, "failed to read the last cron run");
            None
        }
    };

    // The builder's words ("Run cron now") in the viewer's admin language.
    let text = super::admin_nav::AdminText::for_page(&state, &viewer, "/admin/").await;
    let mut form_state = FormState::new("cron_run_now_form", "");
    form_state.user_id = Some(user.id);
    let form_html = match state
        .forms()
        .build(
            run_now_form(),
            &session,
            &super::item_form::tap_state(&state, &viewer),
            form_state,
        )
        .await
        .and_then(|form| text.scope(|| state.theme().render_form(&form)))
    {
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the cron form");
            return render_server_error("The cron report could not be displayed.");
        }
    };

    // Changing the schedule is a site configuration decision, not merely
    // running cron by hand: a viewer holding only `run cron` (B161) sees the
    // current interval but not an edit form that `save_scheduler_settings`
    // (gated on `PERMISSION` alone) would refuse anyway.
    let scheduler_config = state.cron().scheduler_config().await;
    // With `CRON_INTERVAL_SECS` set the saved setting has no effect, so the
    // form is not offered; the page says where the interval comes from.
    let may_configure_scheduler =
        viewer.can(PERMISSION) && scheduler_config.source != CronIntervalSource::Environment;
    let scheduler_form_html = if may_configure_scheduler {
        let mut scheduler_form_state = FormState::new(SCHEDULER_FORM_ID, "");
        scheduler_form_state.user_id = Some(user.id);
        match state
            .forms()
            .build(
                scheduler_settings_form(scheduler_config.interval_secs),
                &session,
                &super::item_form::tap_state(&state, &viewer),
                scheduler_form_state,
            )
            .await
            .and_then(|form| text.scope(|| state.theme().render_form(&form)))
        {
            Ok(html) => Some(html),
            Err(e) => {
                tracing::error!(error = %e, "failed to build the cron scheduler settings form");
                return render_server_error("The cron report could not be displayed.");
            }
        }
    } else {
        None
    };

    let flash: Option<serde_json::Value> = session.remove(CRON_FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("last_run", &last_run);
    context.insert("run_now_form", &form_html);
    context.insert("scheduler_enabled", &scheduler_config.enabled());
    context.insert("scheduler_interval_secs", &scheduler_config.interval_secs);
    context.insert(
        "scheduler_from_environment",
        &(scheduler_config.source == CronIntervalSource::Environment),
    );
    context.insert("scheduler_form", &scheduler_form_html);
    context.insert("may_configure_scheduler", &may_configure_scheduler);
    context.insert("flash", &flash);
    context.insert("path", "/admin/reports/cron");
    render_admin_template(&state, &viewer, "admin/reports/cron.html", context).await
}

/// POST /admin/reports/cron: run cron in this process, then back to the
/// report.
async fn run_cron_now(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_any_permission(&state, &session, CRON_PERMISSIONS).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
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
                if form_state.form_id == "cron_run_now_form" {
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
        Err(ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        Err(ProcessError::Invalid(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
    }

    tracing::info!(user_id = %user.id, "cron run from the admin report");
    // On its own task, as `POST /cron/{key}` runs it: a client that leaves
    // must not tear a run apart half way.
    let cron = state.cron().clone();
    let result = match tokio::spawn(async move { cron.run().await }).await {
        Ok(result) => result,
        Err(e) => CronResult::Failed(format!("cron task did not finish: {e}")),
    };
    let message = match result {
        CronResult::Completed { duration_ms, .. } => {
            format!("Cron ran in {duration_ms} ms.")
        }
        CronResult::Skipped => "Cron is already running on another server.".to_string(),
        CronResult::Failed(error) => format!("Cron failed: {error}"),
    };
    let _ = session.insert(CRON_FLASH_KEY, message).await;
    Redirect::to("/admin/reports/cron").into_response()
}

/// POST /admin/reports/cron/scheduler: save the built-in scheduler's interval
/// (B178), then back to the report.
async fn save_scheduler_settings(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let interval_secs = match state
        .forms()
        .process(
            posted.get("_form_build_id").unwrap_or_default(),
            &posted,
            &session,
            &super::item_form::tap_state(&state, &viewer),
            |form_state| {
                if form_state.form_id != SCHEDULER_FORM_ID {
                    return Err(vec![ValidationError::form(crate::form::FORM_EXPIRED)]);
                }
                match posted
                    .get("interval_secs")
                    .unwrap_or_default()
                    .trim()
                    .parse::<u64>()
                {
                    Ok(n) => Ok(n),
                    Err(_) => Err(vec![ValidationError::field(
                        "interval_secs",
                        "Enter a whole number of seconds, 0 or more.",
                    )]),
                }
            },
        )
        .await
    {
        Ok((_, interval_secs)) => interval_secs,
        Err(ProcessError::Csrf) => {
            return render_error("Invalid or expired form token. Please try again.");
        }
        // A value that is not a whole number: say so on the report, where the
        // form is, rather than calling the form expired.
        Err(ProcessError::Invalid(errors)) => {
            match errors.into_iter().find(|e| e.field.is_some()) {
                Some(error) => {
                    let flash = serde_json::json!({ "kind": "error", "message": error.message });
                    let _ = session.insert(CRON_FLASH_KEY, flash).await;
                    return Redirect::to("/admin/reports/cron").into_response();
                }
                None => {
                    return render_error("The form has expired. Reload the page and try again.");
                }
            }
        }
    };

    if let Err(e) = SiteConfig::set(
        state.db(),
        CRON_INTERVAL_CONFIG_KEY,
        serde_json::json!(interval_secs),
    )
    .await
    {
        tracing::error!(error = %e, "failed to save the cron scheduler setting");
        return render_server_error("The scheduler setting could not be saved.");
    }

    tracing::info!(
        user_id = %user.id,
        interval_secs,
        "built-in cron scheduler setting changed"
    );
    let message = if interval_secs == 0 {
        "Built-in cron scheduler turned off.".to_string()
    } else {
        format!("Built-in cron scheduler set to run every {interval_secs} seconds.")
    };
    let _ = session.insert(CRON_FLASH_KEY, message).await;
    Redirect::to("/admin/reports/cron").into_response()
}

// =============================================================================
// Log
// =============================================================================

/// The log's sortable columns: key and SQL. `time` is the row id, which is
/// the order rows were written in; `level` orders by severity, least first.
const LOG_SORTS: &[(&str, &str)] = &[
    ("time", "id"),
    (
        "level",
        "CASE level WHEN 'error' THEN 3 WHEN 'warn' THEN 2 ELSE 1 END",
    ),
    ("type", "log_type"),
];

/// GET /admin/reports/log
///
/// The parameters: `q` (message contains), `level`, `type`, `sort` and
/// `page`.
async fn log_report(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_any_permission(&state, &session, &[LOG_PERMISSION, PERMISSION]).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let query = ListQuery::from_params(&params);
    let sort = query.sort(LOG_SORTS, ("time", true));
    let wanted_level = params.get("level").map(String::as_str).unwrap_or("");
    let level = LEVELS
        .iter()
        .find(|l| **l == wanted_level)
        .map(|l| (*l).to_string());
    let log_type = params
        .get("type")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let filter = LogFilter {
        level: level.clone(),
        log_type: log_type.clone(),
        message_like: query.like(),
    };

    let (entries, total) = match crate::system_log::list(
        state.db(),
        &filter,
        Some(&sort.order_by()),
        PER_PAGE,
        query.offset(),
    )
    .await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the system log");
            return render_server_error("The log could not be read.");
        }
    };
    let types = crate::system_log::types(state.db())
        .await
        .unwrap_or_default();

    // A level is a fixed word the select translates; a type is data.
    let level_options: Vec<(String, String)> = LEVELS
        .iter()
        .map(|l| ((*l).to_string(), (*l).to_string()))
        .collect();
    let type_options: Vec<(String, String)> =
        types.iter().map(|t| (t.clone(), t.clone())).collect();
    let list = query.view(total, entries.len(), Some(&sort));
    let forms = match super::admin_list::render_forms(
        &state,
        &session,
        &user,
        &list,
        "/admin/reports/log",
        "Filter the log",
        "Message contains",
        vec![
            (
                "level",
                crate::form::FormElement::select(level_options)
                    .title("Level")
                    .placeholder("- Any -")
                    .default_value(level.unwrap_or_default()),
            ),
            (
                "type",
                crate::form::FormElement::select(type_options)
                    .title("Type")
                    .placeholder("- Any -")
                    .default_value(log_type.unwrap_or_default()),
            ),
        ],
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };

    let mut context = tera::Context::new();
    context.insert("list", &list);
    forms.insert_into(&mut context);
    context.insert("entries", &entries);
    context.insert("path", "/admin/reports/log");
    render_admin_template(&state, &viewer, "admin/reports/log.html", context).await
}

/// An image file that has no text alternative.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct MissingAlt {
    id: uuid::Uuid,
    filename: String,
    created: i64,
}

/// Most images the accessibility report lists.
const MISSING_ALT_ROWS: i64 = 50;

/// An item whose rich text holds images with no text alternative, as the
/// "In content" section lists it.
#[derive(Debug, Serialize)]
struct MissingAltItem {
    title: String,
    item_type: String,
    count: i32,
    href: String,
}

/// Most items the accessibility report's "In content" section lists.
const MISSING_ALT_ITEM_ROWS: i64 = 50;

/// GET /admin/reports/accessibility: images without a text alternative,
/// uploaded or written into rich text, the viewer's own accessibility
/// settings, and how the administration is checked.
async fn accessibility_report(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let missing_total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM file_managed WHERE filemime LIKE 'image/%' \
         AND (alt_text IS NULL OR btrim(alt_text) = '')",
    )
    .fetch_one(state.db())
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "accessibility report: count images");
        0
    });
    let missing: Vec<MissingAlt> = sqlx::query_as(
        "SELECT id, filename, created FROM file_managed WHERE filemime LIKE 'image/%' \
         AND (alt_text IS NULL OR btrim(alt_text) = '') \
         ORDER BY created DESC, id DESC LIMIT $1",
    )
    .bind(MISSING_ALT_ROWS)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();
    // A file's page is where its text alternative is set; link to it only
    // for someone who may open it.
    let may_fix = viewer.can("access files");

    let (content_missing_total, content_missing) = content_missing_alt(&state, &viewer).await;

    let prefs = super::admin_nav::Preferences::load(state.db(), user.id).await;

    let mut context = tera::Context::new();
    context.insert("missing_total", &missing_total);
    context.insert("missing", &missing);
    context.insert("may_fix", &may_fix);
    context.insert("content_missing_total", &content_missing_total);
    context.insert("content_missing", &content_missing);
    context.insert("pref_theme", prefs.theme.as_deref().unwrap_or("system"));
    context.insert("pref_slash", &!prefs.search_slash_off);
    context.insert("pref_key", prefs.search_key());
    context.insert("path", "/admin/reports/accessibility");
    render_admin_template(&state, &viewer, "admin/reports/accessibility.html", context).await
}

/// Content items whose rich text holds images with no text alternative: the
/// total with a finding, and the newest [`MISSING_ALT_ITEM_ROWS`], each linked
/// to the item's edit form for a viewer who may edit it, to the item for a
/// viewer who may only view it, and left out for anyone else (B199's listing
/// rule, `routes::admin_dashboard::your_content`). Reads only the stored scan
/// ([`crate::content::accessibility_scan`]), written on save: no item's HTML
/// is parsed here.
async fn content_missing_alt(
    state: &AppState,
    viewer: &crate::tap::UserContext,
) -> (i64, Vec<MissingAltItem>) {
    let total: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_accessibility WHERE images_missing_alt > 0")
            .fetch_one(state.db())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "accessibility report: count items");
                0
            });

    let rows: Vec<(uuid::Uuid, i32)> = sqlx::query_as(
        "SELECT item_accessibility.item_id, item_accessibility.images_missing_alt \
         FROM item_accessibility JOIN item ON item.id = item_accessibility.item_id \
         WHERE item_accessibility.images_missing_alt > 0 \
         ORDER BY item.changed DESC, item.id DESC LIMIT $1",
    )
    .bind(MISSING_ALT_ITEM_ROWS)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();
    let counts: HashMap<uuid::Uuid, i32> = rows.into_iter().collect();
    let ids: Vec<uuid::Uuid> = counts.keys().copied().collect();

    let mut items = crate::models::Item::find_by_ids(state.db(), &ids)
        .await
        .unwrap_or_default();
    items.sort_by_key(|i| std::cmp::Reverse((i.changed, i.id)));

    let editor = viewer.can("edit any content");
    let mut content_missing = Vec::new();
    for item in items {
        let href = if editor {
            format!("/admin/content/{}/edit", item.id)
        } else if state
            .items()
            .check_access(&item, "edit", viewer)
            .await
            .unwrap_or(false)
        {
            format!("/item/{}/edit", item.id)
        } else if state
            .items()
            .check_access(&item, "view", viewer)
            .await
            .unwrap_or(false)
        {
            format!("/item/{}", item.id)
        } else {
            continue;
        };
        let count = counts.get(&item.id).copied().unwrap_or(0);
        content_missing.push(MissingAltItem {
            title: item.title.clone(),
            item_type: item.item_type.clone(),
            count,
            href,
        });
    }
    (total, content_missing)
}

// =============================================================================
// Status report
// =============================================================================

/// One line of the status report: what was checked, what it found, and how
/// much it matters.
#[derive(Debug, Serialize)]
struct Requirement {
    /// What was checked, in the source language.
    title: &'static str,
    /// What it found: a version, a time, a word. Data, not translated.
    value: String,
    /// `ok`, `warning` or `error`.
    severity: &'static str,
    /// What to do about it, in the source language, when there is anything.
    description: Option<&'static str>,
}

impl Requirement {
    fn new(title: &'static str, value: impl Into<String>, severity: &'static str) -> Self {
        Self {
            title,
            value: value.into(),
            severity,
            description: None,
        }
    }

    fn because(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }
}

/// How old the last cron run may be before the report warns.
const CRON_STALE_SECS: i64 = 24 * 60 * 60;

/// GET /admin/reports/status: the versions, the database, the cache server,
/// cron, the file system and the security notices, each checked now.
///
/// No value here is a secret: no connection string, key, token or password
/// is read into the page, only versions, times, a directory and yes or no.
async fn status_report(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let mut rows = vec![Requirement::new(
        "Trovato",
        crate::update_status::running_version(),
        "ok",
    )];
    rows.push(update_requirement(&state).await);
    rows.extend(database_requirements(&state).await);
    rows.push(redis_requirement(&state).await);
    let (cron_row, last_cron) = cron_requirement(&state).await;
    rows.push(cron_row);
    rows.push(files_requirement(&state).await);
    rows.push(field_index_requirement(&state).await);
    rows.push(match state.runtime().cookie_secure {
        crate::session::CookieSecure::Never => Requirement::new(
            "Session cookies",
            "Sent over plain HTTP",
            "warning",
        )
        .because("COOKIE_SECURE=never sends the sign-in cookie without HTTPS. Use it only on a private network or for local development."),
        _ => Requirement::new("Session cookies", "HTTPS only", "ok"),
    });
    let maintenance = crate::models::SiteConfig::get(
        state.db(),
        crate::middleware::maintenance::MAINTENANCE_MODE_KEY,
    )
    .await
    .ok()
    .flatten()
    .and_then(|v| v.as_bool())
    .unwrap_or(false);
    rows.push(if maintenance {
        Requirement::new("Maintenance mode", "On", "warning")
            .because("Visitors see the maintenance message. Turn it off under Site settings.")
    } else {
        Requirement::new("Maintenance mode", "Off", "ok")
    });

    let errors = rows.iter().filter(|r| r.severity == "error").count();
    let warnings = rows.iter().filter(|r| r.severity == "warning").count();

    let mut context = tera::Context::new();
    context.insert("rows", &rows);
    context.insert("errors", &errors);
    context.insert("warnings", &warnings);
    context.insert("last_cron", &last_cron);
    context.insert("may_run_cron", &may_run_cron(&viewer));
    context.insert("path", "/admin/reports/status");
    render_admin_template(&state, &viewer, "admin/reports/status.html", context).await
}

/// Whether every field marked "Filterable and sortable" has a ready, valid
/// index (B207). A field without one still answers, by reading every item of
/// its type.
async fn field_index_requirement(state: &AppState) -> Requirement {
    const TITLE: &str = "Field indexes";
    match crate::gather::field_index::missing(state.db()).await {
        Ok(missing) if missing.is_empty() => Requirement::new(TITLE, "All ready", "ok"),
        Ok(missing) => {
            let failed = missing.iter().any(|(_, _, s)| s == "failed");
            let names: Vec<String> = missing
                .iter()
                .map(|(t, f, s)| match f.as_str() {
                    "*" => format!("{t}, several fields at once ({s})"),
                    "+" => format!("{t}, newest first ({s})"),
                    _ => format!("{t}.{f} ({s})"),
                })
                .collect();
            if failed {
                Requirement::new(TITLE, names.join(", "), "error").because(
                    "Building an index failed. The content type's fields page shows the error; cron tries again after an hour. Listings on these fields read every item of the type until then.",
                )
            } else {
                Requirement::new(TITLE, names.join(", "), "warning").because(
                    "These indexes are queued or being built. Listings on these fields read every item of the type until they are ready.",
                )
            }
        }
        Err(_) => Requirement::new(TITLE, "Could not be read", "warning"),
    }
}

/// Whether the viewer may run cron by hand.
fn may_run_cron(viewer: &crate::tap::UserContext) -> bool {
    CRON_PERMISSIONS.iter().any(|p| viewer.can(p))
}

/// Whether a newer release exists, from the last update check.
async fn update_requirement(state: &AppState) -> Requirement {
    const TITLE: &str = "Updates";
    match crate::update_status::stored_status(state.db()).await {
        Some(status) if status.is_behind() && status.is_security => Requirement::new(
            TITLE,
            format!("{} available", status.latest_version),
            "error",
        )
        .because("A security release of Trovato is available. Update as soon as you can."),
        Some(status) if status.is_behind() => Requirement::new(
            TITLE,
            format!("{} available", status.latest_version),
            "warning",
        )
        .because("A newer release of Trovato is available."),
        Some(_) => Requirement::new(TITLE, "Up to date", "ok"),
        None if crate::update_status::setting_allows_check(state.db()).await => {
            Requirement::new(TITLE, "Not checked yet", "warning")
                .because("Cron checks for new releases once a day.")
        }
        None => Requirement::new(TITLE, "Not checked", "warning")
            .because("Update checks are off, so this site cannot tell you about a security release. Turn them on under Site settings."),
    }
}

/// The database server's version and the schema's last migration.
async fn database_requirements(state: &AppState) -> Vec<Requirement> {
    let version: Result<String, _> = sqlx::query_scalar("SELECT current_setting('server_version')")
        .fetch_one(state.db())
        .await;
    let mut rows = vec![match version {
        Ok(v) => Requirement::new("Database", format!("PostgreSQL {v}"), "ok"),
        Err(e) => {
            tracing::warn!(error = %e, "status report: database version");
            return vec![Requirement::new("Database", "Not reachable", "error")];
        }
    }];
    let schema: Result<(i64, Option<i64>), _> =
        sqlx::query_as("SELECT COUNT(*), MAX(version) FROM _sqlx_migrations WHERE success")
            .fetch_one(state.db())
            .await;
    rows.push(match schema {
        Ok((count, Some(latest))) => Requirement::new(
            "Database schema",
            format!("{count} migrations, the latest {latest}"),
            "ok",
        ),
        Ok((_, None)) | Err(_) => Requirement::new("Database schema", "Unknown", "warning")
            .because("The migration record could not be read."),
    });
    rows
}

/// Whether the cache server answers, and its version.
async fn redis_requirement(state: &AppState) -> Requirement {
    const TITLE: &str = "Cache server";
    let info: redis::RedisResult<String> = async {
        let mut conn = state.redis().bounded_connection().await?;
        redis::cmd("INFO")
            .arg("server")
            .query_async(&mut conn)
            .await
    }
    .await;
    match info {
        Ok(info) => {
            let version = info
                .lines()
                .find_map(|l| l.strip_prefix("redis_version:"))
                .map(|v| format!("Redis {}", v.trim()))
                .unwrap_or_else(|| "Redis".to_string());
            Requirement::new(TITLE, version, "ok")
        }
        Err(e) => {
            tracing::warn!(error = %e, "status report: cache server");
            Requirement::new(TITLE, "Not reachable", "error")
                .because("Caches, sessions and cron need Redis.")
        }
    }
}

/// When cron last ran, and the row saying so.
async fn cron_requirement(state: &AppState) -> (Requirement, Option<i64>) {
    const TITLE: &str = "Cron maintenance tasks";
    let last = match state.cron().last_run().await {
        Ok(run) => run,
        Err(e) => {
            tracing::warn!(error = %e, "status report: last cron run");
            None
        }
    };
    let Some(run) = last else {
        return (
            Requirement::new(TITLE, "Not run in the last day", "warning")
                .because("Cron has not run in the last day. Schedule a request to the cron address with the cron key, or run it manually."),
            None,
        );
    };
    let age = chrono::Utc::now().timestamp() - run.timestamp;
    let value = format!(
        "Last run {} UTC",
        chrono::DateTime::from_timestamp(run.timestamp, 0)
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    );
    let row = if run.status == "failed" {
        Requirement::new(TITLE, value, "error").because("The last run failed. See the cron report.")
    } else if age > CRON_STALE_SECS {
        Requirement::new(TITLE, value, "warning").because("Cron has not run in the last day.")
    } else {
        Requirement::new(TITLE, value, "ok")
    };
    (row, Some(run.timestamp))
}

/// Whether this server can write to the uploads directory.
async fn files_requirement(state: &AppState) -> Requirement {
    const TITLE: &str = "File system";
    let Some(dir) = state.files().local_dir() else {
        let storage = state.files().storage();
        let place = storage.describe();
        // Bounded: a slow store must not hold the status page.
        let checked =
            tokio::time::timeout(std::time::Duration::from_secs(5), storage.check_usable())
                .await
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!("the store did not answer within 5 seconds"))
                });
        return match checked {
            Ok(()) => Requirement::new(TITLE, format!("Object storage ({place})"), "ok"),
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "status report: file store");
                Requirement::new(TITLE, format!("Not usable ({place})"), "error").because(
                    "Uploads will fail. Check the S3_* settings and this server's access to the bucket.",
                )
            }
        };
    };
    match crate::file::storage::check_writable(dir).await {
        Ok(()) => Requirement::new(TITLE, format!("Writable ({})", dir.display()), "ok"),
        Err(e) => {
            tracing::warn!(error = %e, "status report: uploads directory");
            Requirement::new(TITLE, format!("Not writable ({})", dir.display()), "error")
                .because("Uploads will fail. Set UPLOADS_DIR to a directory this server can write.")
        }
    }
}
