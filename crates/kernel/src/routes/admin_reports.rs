//! Reports: the cron report and the site log (B107).
//!
//! `/admin/reports/cron` shows the last cron run (when, on which server, how
//! long, what it did) and runs cron now, in this process, from a button.
//! `/admin/reports/log` pages through `system_log`, filtered by level and
//! type. Both take `administer site`, as `/cron/status` does.

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::cron::CronResult;
use crate::form::{FormElement, FormState, ProcessError};
use crate::state::AppState;
use crate::system_log::LogFilter;

use super::helpers::{
    render_admin_template, render_error, render_server_error, require_permission,
};

/// The permission both reports take.
const PERMISSION: &str = "administer site";

/// Session key for the cron report's message after Run now.
const CRON_FLASH_KEY: &str = "cron_report_flash";

/// Entries per page on the log screen.
const PER_PAGE: i64 = 50;

/// The levels the log filter offers.
const LEVELS: &[&str] = &["error", "warn", "info"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/reports/cron", get(cron_report).post(run_cron_now))
        .route("/admin/reports/log", get(log_report))
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
        .and_then(|form| state.theme().render_form(&form))
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

#[derive(Debug, Deserialize)]
struct LogQuery {
    #[serde(default)]
    level: String,
    #[serde(default, rename = "type")]
    log_type: String,
    page: Option<i64>,
}

/// GET /admin/reports/log
async fn log_report(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<LogQuery>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let level = LEVELS
        .iter()
        .find(|l| **l == query.level)
        .map(|l| (*l).to_string());
    let log_type = Some(query.log_type.trim().to_string()).filter(|t| !t.is_empty());
    let filter = LogFilter {
        level: level.clone(),
        log_type: log_type.clone(),
    };
    let page = query.page.unwrap_or(1).max(1);

    let (entries, total) =
        match crate::system_log::list(state.db(), &filter, PER_PAGE, (page - 1) * PER_PAGE).await {
            Ok(found) => found,
            Err(e) => {
                tracing::error!(error = %e, "failed to read the system log");
                return render_server_error("The log could not be read.");
            }
        };
    let types = crate::system_log::types(state.db())
        .await
        .unwrap_or_default();

    // The query string for the pager links, so a page keeps its filter.
    let mut filter_query: Vec<(&str, String)> = Vec::new();
    if let Some(level) = &level {
        filter_query.push(("level", level.clone()));
    }
    if let Some(log_type) = &log_type {
        filter_query.push(("type", log_type.clone()));
    }
    let filter_qs = filter_query
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let mut context = tera::Context::new();
    context.insert("entries", &entries);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("per_page", &PER_PAGE);
    context.insert("total_pages", &((total + PER_PAGE - 1) / PER_PAGE).max(1));
    context.insert("levels", LEVELS);
    context.insert("types", &types);
    context.insert("level_filter", &level.unwrap_or_default());
    context.insert("type_filter", &log_type.unwrap_or_default());
    context.insert("filter_qs", &filter_qs);
    context.insert("path", "/admin/reports/log");
    render_admin_template(&state, &viewer, "admin/reports/log.html", context).await
}
