//! Reports: precise timers (B278).
//!
//! `/admin/reports/timers` lists every timer, filtered by owner, status and
//! name, a page at a time: next run in the viewer's time zone and as a
//! relative time, repeat, status, last fire, its lateness, attempts and the
//! last error. It takes `view timers` or `administer timers`.
//!
//! With `administer timers` each row can be paused, resumed, run now or
//! cancelled, and `/admin/reports/timers/{id}/edit` moves its next run or
//! changes its repeat interval. `/admin/reports/timers/settings` edits the
//! throttles. No form here creates a timer: what a timer runs comes from
//! code only. Every change is permission checked, takes the page's form
//! token, and is recorded in the security log with who made it.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;
use tower_sessions::Session;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::form::csrf::generate_csrf_token;
use crate::i18n::UiMessage;
use crate::models::User;
use crate::state::AppState;
use crate::timer::{
    AdminChange, Timer, TimerFilter, TimerOverrides, TimerRefused, TimerSettings, TimerStatus,
};

use super::admin_form::FieldError;
use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_any_permission,
    require_csrf, require_permission,
};

/// Opens the listing.
const VIEW: &[&str] = &["view timers", ADMINISTER];

/// Changes a timer or the throttles.
const ADMINISTER: &str = "administer timers";

/// The listing's address.
const PATH: &str = "/admin/reports/timers";

/// Session key for the message after a change.
const FLASH_KEY: &str = "timers_report_flash";

/// The listing's sortable columns: key and SQL.
const SORTS: &[(&str, &str)] = &[
    ("next", "run_at"),
    ("owner", "owner"),
    ("name", "name"),
    ("status", "status"),
    ("fired", "last_fired_at"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(PATH, get(list_timers))
        .route(
            "/admin/reports/timers/settings",
            get(settings_form).post(save_settings),
        )
        .route(
            "/admin/reports/timers/{id}/edit",
            get(edit_form).post(edit_submit),
        )
        .route("/admin/reports/timers/{id}/{action}", post(row_action))
}

// =============================================================================
// Times in the viewer's zone
// =============================================================================

/// The viewer's time zone, if Postgres knows it, else UTC.
async fn viewer_zone(state: &AppState, user: &User) -> String {
    let Some(zone) = user
        .timezone
        .as_deref()
        .map(str::trim)
        .filter(|z| !z.is_empty())
    else {
        return "UTC".to_string();
    };
    match sqlx::query("SELECT now() AT TIME ZONE $1")
        .bind(zone)
        .execute(state.db())
        .await
    {
        Ok(_) => zone.to_string(),
        Err(_) => "UTC".to_string(),
    }
}

/// `times` in `zone`, as `YYYY-MM-DD HH:MM:SS`, in order. Postgres holds the
/// zone rules, so the kernel needs no time zone library of its own.
async fn local_times(
    state: &AppState,
    zone: &str,
    times: &[Option<DateTime<Utc>>],
) -> Vec<Option<String>> {
    if times.is_empty() {
        return Vec::new();
    }
    sqlx::query_scalar(
        "SELECT to_char(t AT TIME ZONE $1, 'YYYY-MM-DD HH24:MI:SS') \
         FROM unnest($2::timestamptz[]) WITH ORDINALITY AS u(t, i) ORDER BY i",
    )
    .bind(zone)
    .bind(times)
    .fetch_all(state.db())
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "timers report: format times");
        times
            .iter()
            .map(|t| t.map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()))
            .collect()
    })
}

/// A wall clock time in `zone`, typed into a `datetime-local` field, as Unix
/// seconds; `None` when it is not a time.
async fn parse_local(state: &AppState, zone: &str, typed: &str) -> Option<i64> {
    let typed = typed.trim();
    let naive = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(typed, f).ok())?;
    sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM ($1::timestamp AT TIME ZONE $2))::bigint")
        .bind(naive)
        .bind(zone)
        .fetch_one(state.db())
        .await
        .ok()
}

/// How far a time is from now, in the largest whole unit, for the template
/// to say ("in 3 minutes", "2 hours ago").
#[derive(Debug, Serialize)]
struct Relative {
    count: i64,
    /// `second`, `minute`, `hour` or `day`.
    unit: &'static str,
    future: bool,
}

fn relative(at: DateTime<Utc>, now: DateTime<Utc>) -> Relative {
    let secs = (at - now).num_seconds();
    let future = secs >= 0;
    let n = secs.abs();
    let (count, unit) = if n < 60 {
        (n, "second")
    } else if n < 3600 {
        (n / 60, "minute")
    } else if n < 86_400 {
        (n / 3600, "hour")
    } else {
        (n / 86_400, "day")
    };
    Relative {
        count,
        unit,
        future,
    }
}

// =============================================================================
// The listing
// =============================================================================

/// One row of the listing.
#[derive(Debug, Serialize)]
struct Row {
    id: i64,
    owner: String,
    name: String,
    next_local: String,
    next_utc: String,
    next_relative: Relative,
    repeat_secs: Option<i32>,
    status: String,
    last_fired_local: Option<String>,
    last_lateness_ms: Option<i64>,
    attempts: i32,
    last_error: Option<String>,
}

/// GET /admin/reports/timers: `owner`, `status`, `q` (name contains),
/// `sort` and `page`.
async fn list_timers(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_any_permission(&state, &session, VIEW).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let query = ListQuery::from_params(&params);
    let sort = query.sort(SORTS, ("next", false));
    let owner = params
        .get("owner")
        .map(|o| o.trim().to_string())
        .filter(|o| !o.is_empty());
    let status = params.get("status").and_then(|s| TimerStatus::parse(s));
    let filter = TimerFilter {
        owner: owner.clone(),
        status,
        name_like: query.like(),
    };
    let (timers, total) = match crate::timer::list(
        state.db(),
        &filter,
        &sort.order_by(),
        PER_PAGE,
        query.offset(),
    )
    .await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(error = %e, "failed to list timers");
            return render_server_error("The timers could not be read.");
        }
    };
    let owners = crate::timer::owners(state.db()).await.unwrap_or_default();
    let zone = viewer_zone(&state, &user).await;
    let mut times = Vec::with_capacity(timers.len() * 2);
    for t in &timers {
        times.push(Some(t.run_at));
        times.push(t.last_fired_at);
    }
    let local = local_times(&state, &zone, &times).await;
    let now = Utc::now();
    let rows: Vec<Row> = timers
        .into_iter()
        .enumerate()
        .map(|(i, t)| Row {
            id: t.id,
            next_local: local.get(i * 2).cloned().flatten().unwrap_or_default(),
            next_utc: t.run_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            next_relative: relative(t.run_at, now),
            last_fired_local: local.get(i * 2 + 1).cloned().flatten(),
            owner: t.owner,
            name: t.name,
            repeat_secs: t.repeat_secs,
            status: t.status,
            last_lateness_ms: t.last_lateness_ms,
            attempts: t.attempts,
            last_error: t.last_error,
        })
        .collect();

    let owner_options: Vec<(&str, &str)> =
        owners.iter().map(|o| (o.as_str(), o.as_str())).collect();
    let status_options: Vec<(&str, &str)> = TimerStatus::ALL
        .iter()
        .map(|s| (s.as_str(), status_label(*s)))
        .collect();
    let settings = crate::timer::effective_settings(state.db())
        .await
        .unwrap_or_default();
    let flash: Option<UiMessage> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, rows.len(), Some(&sort)));
    context.insert("timers", &rows);
    context.insert("zone", &zone);
    context.insert("owner_options", &owner_options);
    context.insert("owner_filter", &owner.unwrap_or_default());
    context.insert("status_options", &status_options);
    context.insert("status_filter", &status.map(|s| s.as_str()).unwrap_or(""));
    context.insert("may_administer", &viewer.can(ADMINISTER));
    context.insert("settings", &settings);
    context.insert("csrf_token", &generate_csrf_token(&session).await);
    context.insert("flash", &flash);
    context.insert("path", PATH);
    render_admin_template(&state, &viewer, "admin/reports/timers.html", context).await
}

/// The English word for a status, translated by the template.
fn status_label(status: TimerStatus) -> &'static str {
    match status {
        TimerStatus::Scheduled => "Scheduled",
        TimerStatus::Paused => "Paused",
        TimerStatus::Running => "Running",
        TimerStatus::Failed => "Failed",
    }
}

// =============================================================================
// Changes
// =============================================================================

/// What the security log keeps of a timer's state.
fn snapshot(t: &Timer) -> serde_json::Value {
    serde_json::json!({
        "run_at": t.run_at.timestamp(),
        "repeat_secs": t.repeat_secs,
        "status": t.status,
    })
}

/// Record an administrator's change to a timer in the security log.
async fn audit_change(
    state: &AppState,
    user: &User,
    action: &str,
    before: &Timer,
    after: Option<&Timer>,
) {
    state
        .security_audit()
        .emit(
            SecurityEvent::new(SecurityEventKind::TimerChanged)
                .actor(user.id)
                .with_request_context(&crate::services::audit::AuditActor::current())
                .detail("action", action)
                .detail("timer_id", before.id)
                .detail("owner", &before.owner)
                .detail("name", &before.name)
                .detail("before", snapshot(before))
                .detail("after", after.map(snapshot)),
        )
        .await;
    tracing::info!(
        user_id = %user.id,
        action,
        owner = %before.owner,
        name = %before.name,
        "timer changed from the admin"
    );
}

/// POST /admin/reports/timers/{id}/{action}: `pause`, `resume`, `run` or
/// `cancel` (which asks first, through the confirmation page).
async fn row_action(
    State(state): State<AppState>,
    session: Session,
    Path((id, action)): Path<(i64, String)>,
    Form(form): Form<super::helpers::CsrfOnlyForm>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = require_csrf(&session, &form.token).await {
        return refusal;
    }
    let change = match action.as_str() {
        "pause" => AdminChange::Pause,
        "resume" => AdminChange::Resume,
        "run" => AdminChange::RunNow,
        "cancel" => AdminChange::Cancel,
        _ => return render_not_found(),
    };
    match state.timers().admin_change(id, &change).await {
        Ok((before, after)) => {
            audit_change(&state, &user, change.action(), &before, after.as_ref()).await;
            let message = match change {
                AdminChange::Pause => UiMessage::new("Paused {name}."),
                AdminChange::Resume => UiMessage::new("Resumed {name}."),
                AdminChange::RunNow => UiMessage::new("{name} runs now."),
                AdminChange::Cancel => UiMessage::new("Cancelled {name}."),
                AdminChange::Edit { .. } => UiMessage::new("Saved {name}."),
            }
            .arg("name", before.name.clone());
            let _ = session.insert(FLASH_KEY, message).await;
            Redirect::to(PATH).into_response()
        }
        Err(TimerRefused::NotFound) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to change a timer");
            render_server_error("The timer could not be changed.")
        }
    }
}

/// The edit form's fields as typed.
#[derive(Debug, Default, Serialize, serde::Deserialize)]
struct EditValues {
    run_at: String,
    repeat_secs: String,
}

async fn render_edit(
    state: &AppState,
    session: &Session,
    user: &User,
    timer: &Timer,
    zone: &str,
    values: &EditValues,
    errors: &[FieldError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let settings = crate::timer::effective_settings(state.db())
        .await
        .unwrap_or_default();
    let mut context = tera::Context::new();
    context.insert("timer", timer);
    context.insert("zone", zone);
    context.insert("values", values);
    context.insert("errors", errors);
    context.insert("min_repeat_secs", &settings.min_repeat_secs);
    context.insert(
        "action",
        &format!("/admin/reports/timers/{}/edit", timer.id),
    );
    context.insert("csrf_token", &generate_csrf_token(session).await);
    context.insert("path", &format!("/admin/reports/timers/{}/edit", timer.id));
    render_admin_template(state, &viewer, "admin/reports/timer-edit.html", context).await
}

/// GET /admin/reports/timers/{id}/edit
async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let timer = match crate::timer::find(state.db(), id).await {
        Ok(Some(t)) => t,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load a timer");
            return render_server_error("The timer could not be read.");
        }
    };
    let zone = viewer_zone(&state, &user).await;
    let local = local_times(&state, &zone, &[Some(timer.run_at)]).await;
    let values = EditValues {
        run_at: local
            .first()
            .cloned()
            .flatten()
            .unwrap_or_default()
            .replacen(' ', "T", 1),
        repeat_secs: timer.repeat_secs.map(|r| r.to_string()).unwrap_or_default(),
    };
    render_edit(&state, &session, &user, &timer, &zone, &values, &[]).await
}

/// The edit form as posted.
#[derive(Debug, serde::Deserialize)]
struct EditForm {
    #[serde(rename = "_token")]
    token: String,
    #[serde(default)]
    run_at: String,
    #[serde(default)]
    repeat_secs: String,
}

/// POST /admin/reports/timers/{id}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<EditForm>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = require_csrf(&session, &form.token).await {
        return refusal;
    }
    let timer = match crate::timer::find(state.db(), id).await {
        Ok(Some(t)) => t,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load a timer");
            return render_server_error("The timer could not be read.");
        }
    };
    let zone = viewer_zone(&state, &user).await;
    let values = EditValues {
        run_at: form.run_at.trim().to_string(),
        repeat_secs: form.repeat_secs.trim().to_string(),
    };
    let mut errors = Vec::new();
    let run_at = parse_local(&state, &zone, &values.run_at).await;
    if run_at.is_none() {
        errors.push(FieldError::on(
            "run_at",
            UiMessage::new("Enter the next run as a date and time."),
        ));
    }
    let repeat_secs = if values.repeat_secs.is_empty() {
        Ok(None)
    } else {
        values.repeat_secs.parse::<i64>().map(Some)
    };
    if repeat_secs.is_err() {
        errors.push(FieldError::on(
            "repeat_secs",
            UiMessage::new("Enter the repeat interval as a whole number of seconds, or leave it empty to run once."),
        ));
    }
    let (Some(run_at), Ok(repeat_secs), true) = (run_at, repeat_secs, errors.is_empty()) else {
        return render_edit(&state, &session, &user, &timer, &zone, &values, &errors).await;
    };
    match state
        .timers()
        .admin_change(
            id,
            &AdminChange::Edit {
                run_at,
                repeat_secs,
            },
        )
        .await
    {
        Ok((before, after)) => {
            audit_change(&state, &user, "edit", &before, after.as_ref()).await;
            let _ = session
                .insert(
                    FLASH_KEY,
                    UiMessage::new("Saved {name}.").arg("name", before.name.clone()),
                )
                .await;
            Redirect::to(PATH).into_response()
        }
        Err(TimerRefused::RepeatTooShort { min }) => {
            let errors = [FieldError::on(
                "repeat_secs",
                UiMessage::new("The repeat interval must be at least {min} seconds.")
                    .arg("min", min.to_string()),
            )];
            render_edit(&state, &session, &user, &timer, &zone, &values, &errors).await
        }
        Err(TimerRefused::InvalidTime) => {
            let errors = [FieldError::on(
                "run_at",
                UiMessage::new("Enter the next run as a date and time."),
            )];
            render_edit(&state, &session, &user, &timer, &zone, &values, &errors).await
        }
        Err(TimerRefused::NotFound) => render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to edit a timer");
            render_server_error("The timer could not be changed.")
        }
    }
}

// =============================================================================
// Throttles
// =============================================================================

/// One throttle, as the settings form shows it.
#[derive(Debug, Serialize)]
struct Throttle {
    name: &'static str,
    label: &'static str,
    hint: &'static str,
    value: String,
    default: u32,
    variable: &'static str,
    from_environment: bool,
}

fn throttles(
    saved: &TimerSettings,
    overrides: &TimerOverrides,
    typed: Option<&HashMap<String, String>>,
) -> Vec<Throttle> {
    let defaults = TimerSettings::default();
    let effective = overrides.apply(*saved);
    let set = overrides.set();
    let rows = [
        (
            "max_concurrent",
            "Timers firing at once, per server",
            "The most timers one server runs at the same time; the rest wait their turn.",
            saved.max_concurrent,
            effective.max_concurrent,
            defaults.max_concurrent,
        ),
        (
            "max_per_plugin",
            "Timers per plugin",
            "The most timers one plugin may hold. A plugin past it is refused a new one, with an error it can see.",
            saved.max_per_plugin,
            effective.max_per_plugin,
            defaults.max_per_plugin,
        ),
        (
            "min_repeat_secs",
            "Shortest repeat interval (seconds)",
            "A repeating timer may not fire more often than this.",
            saved.min_repeat_secs,
            effective.min_repeat_secs,
            defaults.min_repeat_secs,
        ),
        (
            "max_payload_bytes",
            "Largest payload (bytes)",
            "The most JSON a timer may carry to its handler.",
            saved.max_payload_bytes,
            effective.max_payload_bytes,
            defaults.max_payload_bytes,
        ),
    ];
    rows.into_iter()
        .enumerate()
        .map(
            |(i, (name, label, hint, saved, effective, default))| Throttle {
                name,
                label,
                hint,
                value: typed
                    .and_then(|t| t.get(name).cloned())
                    .unwrap_or_else(|| if set[i] { effective } else { saved }.to_string()),
                default,
                variable: TimerOverrides::VARIABLES[i],
                from_environment: set[i],
            },
        )
        .collect()
}

async fn render_settings(
    state: &AppState,
    session: &Session,
    user: &User,
    typed: Option<&HashMap<String, String>>,
    errors: &[FieldError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let saved = match crate::timer::saved_settings(state.db()).await {
        Ok(saved) => saved,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the timer settings");
            return render_server_error("The timer settings could not be read.");
        }
    };
    let mut context = tera::Context::new();
    context.insert(
        "throttles",
        &throttles(&saved, &crate::timer::env_overrides(), typed),
    );
    context.insert("errors", errors);
    context.insert("csrf_token", &generate_csrf_token(session).await);
    context.insert("path", "/admin/reports/timers/settings");
    render_admin_template(state, &viewer, "admin/reports/timer-settings.html", context).await
}

/// GET /admin/reports/timers/settings
async fn settings_form(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    render_settings(&state, &session, &user, None, &[]).await
}

/// POST /admin/reports/timers/settings
async fn save_settings(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let token = form.get("_token").cloned().unwrap_or_default();
    if let Err(refusal) = require_csrf(&session, &token).await {
        return refusal;
    }
    let before = match crate::timer::saved_settings(state.db()).await {
        Ok(saved) => saved,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the timer settings");
            return render_server_error("The timer settings could not be read.");
        }
    };
    let overrides = crate::timer::env_overrides();
    let set = overrides.set();
    let mut errors = Vec::new();
    let mut read = |i: usize, name: &str, current: u32| -> u32 {
        // A setting the environment holds is not edited here.
        if set[i] {
            return current;
        }
        match form.get(name).map(|v| v.trim().parse::<u32>()) {
            Some(Ok(n)) if n > 0 => n,
            _ => {
                errors.push(FieldError::on(
                    name,
                    UiMessage::new("Enter a whole number above 0."),
                ));
                current
            }
        }
    };
    let after = TimerSettings {
        max_concurrent: read(0, "max_concurrent", before.max_concurrent),
        max_per_plugin: read(1, "max_per_plugin", before.max_per_plugin),
        min_repeat_secs: read(2, "min_repeat_secs", before.min_repeat_secs),
        max_payload_bytes: read(3, "max_payload_bytes", before.max_payload_bytes),
    };
    if !errors.is_empty() {
        return render_settings(&state, &session, &user, Some(&form), &errors).await;
    }
    if after != before {
        if let Err(e) = crate::timer::save_settings(state.db(), after).await {
            tracing::error!(error = %e, "failed to save the timer settings");
            return render_server_error("The timer settings could not be saved.");
        }
        state
            .security_audit()
            .emit(
                SecurityEvent::new(SecurityEventKind::TimerSettingsChanged)
                    .actor(user.id)
                    .with_request_context(&crate::services::audit::AuditActor::current())
                    .detail("before", before)
                    .detail("after", after),
            )
            .await;
        tracing::info!(user_id = %user.id, ?before, ?after, "timer settings changed");
    }
    let _ = session
        .insert(FLASH_KEY, UiMessage::new("Timer settings saved."))
        .await;
    Redirect::to(PATH).into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_pick_the_largest_whole_unit() {
        let now = Utc::now();
        let r = relative(now + chrono::Duration::seconds(30), now);
        assert_eq!((r.count, r.unit, r.future), (30, "second", true));
        let r = relative(now - chrono::Duration::seconds(7200), now);
        assert_eq!((r.count, r.unit, r.future), (2, "hour", false));
        let r = relative(now + chrono::Duration::days(3), now);
        assert_eq!((r.count, r.unit), (3, "day"));
    }
}
