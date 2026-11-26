//! Reports: the cron report and the site log (B107).
//!
//! `/admin/reports/cron` shows the last cron run (when, on which server, how
//! long, what it did) and runs cron now, in this process, from a button.
//! `/admin/reports/log` pages through `system_log`, filtered by message text,
//! level and type, and sorted by time, level or type. Both take
//! `administer site`, as `/cron/status` does; the log also opens to
//! `access site reports`, as Drupal's recent log messages do.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::cron::CronResult;
use crate::form::{FormElement, FormState, ProcessError};
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

/// The levels the log filter offers.
const LEVELS: &[&str] = &["error", "warn", "info"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/reports/cron", get(cron_report).post(run_cron_now))
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

/// GET /admin/reports/cron
async fn cron_report(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
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

    let flash: Option<String> = session.remove(CRON_FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("last_run", &last_run);
    context.insert("run_now_form", &form_html);
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
    let user = match require_permission(&state, &session, PERMISSION).await {
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

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, entries.len(), Some(&sort)));
    context.insert("entries", &entries);
    context.insert("levels", LEVELS);
    context.insert("types", &types);
    context.insert("level_filter", &level.unwrap_or_default());
    context.insert("type_filter", &log_type.unwrap_or_default());
    // [value, label] pairs for the form macros: a level is a fixed word the
    // template translates, a type is data.
    let level_options: Vec<(&str, &str)> = LEVELS.iter().map(|l| (*l, *l)).collect();
    context.insert("level_options", &level_options);
    let type_options: Vec<(&str, &str)> = types.iter().map(|t| (t.as_str(), t.as_str())).collect();
    context.insert("type_options", &type_options);
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

/// GET /admin/reports/accessibility: images without a text alternative, the
/// viewer's own accessibility settings, and how the administration is
/// checked.
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

    let prefs = super::admin_nav::Preferences::load(state.db(), user.id).await;

    let mut context = tera::Context::new();
    context.insert("missing_total", &missing_total);
    context.insert("missing", &missing);
    context.insert("may_fix", &may_fix);
    context.insert("pref_theme", prefs.theme.as_deref().unwrap_or("system"));
    context.insert("pref_slash", &!prefs.search_slash_off);
    context.insert("pref_key", prefs.search_key());
    context.insert("path", "/admin/reports/accessibility");
    render_admin_template(&state, &viewer, "admin/reports/accessibility.html", context).await
}
