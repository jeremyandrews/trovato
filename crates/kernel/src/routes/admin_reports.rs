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
use crate::cron::{CRON_SCHEDULER_CONFIG_KEY, CronResult};
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

/// The levels the log filter offers.
const LEVELS: &[&str] = &["error", "warn", "info"];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/reports/cron", get(cron_report).post(run_cron_now))
        .route(
            "/admin/reports/cron/scheduler",
            axum::routing::post(save_scheduler_settings),
        )
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
                    "How often this server calls cron on its own, with no external caller \
                     needed. 0 turns the built-in scheduler off; an external caller to \
                     /cron/{key} (or the button above) still works either way, and the two \
                     never run at once.",
                )
                .default_value(interval_secs.to_string())
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

    let scheduler_config = state.cron().scheduler_config().await;
    let mut scheduler_form_state = FormState::new(SCHEDULER_FORM_ID, "");
    scheduler_form_state.user_id = Some(user.id);
    let scheduler_form_html = match state
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
        Ok(html) => html,
        Err(e) => {
            tracing::error!(error = %e, "failed to build the cron scheduler settings form");
            return render_server_error("The cron report could not be displayed.");
        }
    };

    let flash: Option<String> = session.remove(CRON_FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("last_run", &last_run);
    context.insert("run_now_form", &form_html);
    context.insert("scheduler_enabled", &scheduler_config.enabled());
    context.insert("scheduler_interval_secs", &scheduler_config.interval_secs);
    context.insert("scheduler_form", &scheduler_form_html);
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
        Err(ProcessError::Invalid(_)) => {
            return render_error("The form has expired. Reload the page and try again.");
        }
    };

    if let Err(e) = SiteConfig::set(
        state.db(),
        CRON_SCHEDULER_CONFIG_KEY,
        serde_json::json!({ "interval_secs": interval_secs }),
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
