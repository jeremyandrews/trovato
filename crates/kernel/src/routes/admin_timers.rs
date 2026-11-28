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
//! code only. Every change is permission checked, goes through
//! `FormService` (the page's form token and saved form state), and is
//! recorded in the security log with who made it.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Serialize;
use tower_sessions::Session;

use crate::audit::{SecurityEvent, SecurityEventKind};
use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::i18n::UiMessage;
use crate::models::User;
use crate::state::AppState;
use crate::timer::{
    AdminChange, Timer, TimerFilter, TimerOverrides, TimerRefused, TimerSettings, TimerStatus,
};

use super::admin_form;
use super::admin_list::{ListQuery, PER_PAGE};
use super::admin_nav::AdminText;
use super::helpers::{
    render_admin_template, render_not_found, render_server_error, require_any_permission,
    require_permission,
};

/// Opens the listing.
const VIEW: &[&str] = &["view timers", ADMINISTER];

/// Changes a timer or the throttles.
const ADMINISTER: &str = "administer timers";

/// The listing's address.
const PATH: &str = "/admin/reports/timers";

/// The throttles form's address.
const SETTINGS_PATH: &str = "/admin/reports/timers/settings";

/// Session key for the message after a change.
const FLASH_KEY: &str = "timers_report_flash";

/// The form the listing's pause, resume and run buttons submit: one for the
/// whole table, each button naming its own address (`formaction`).
const ACTIONS_FORM_ID: &str = "timer_actions_form";

/// The form on the page that asks before a timer is cancelled.
const CANCEL_FORM_ID: &str = "timer_cancel_form";

/// The form that moves a timer's next run or changes its repeat.
const EDIT_FORM_ID: &str = "timer_edit_form";

/// The throttles form.
const SETTINGS_FORM_ID: &str = "timer_settings_form";

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
        .route(
            "/admin/reports/timers/{id}/{action}",
            get(cancel_form).post(row_action),
        )
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

/// A repeat interval, for the template to say ("every 7 days").
#[derive(Debug, Serialize)]
struct Every {
    count: i64,
    /// `second`, `minute`, `hour` or `day`.
    unit: &'static str,
}

/// `secs` in the largest unit that divides it exactly: 604800 is 7 days, 90 is
/// 90 seconds.
fn every(secs: i32) -> Every {
    let secs = i64::from(secs);
    let (count, unit) = [(86_400, "day"), (3600, "hour"), (60, "minute")]
        .into_iter()
        .find(|(size, _)| secs % size == 0)
        .map_or((secs, "second"), |(size, unit)| (secs / size, unit));
    Every { count, unit }
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
    /// The repeat in the largest unit that divides it exactly.
    repeat: Option<Every>,
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
            repeat: t.repeat_secs.map(every),
            repeat_secs: t.repeat_secs,
            status: t.status,
            last_lateness_ms: t.last_lateness_ms,
            attempts: t.attempts,
            last_error: t.last_error,
        })
        .collect();

    let owner_options: Vec<(String, String)> =
        owners.iter().map(|o| (o.clone(), o.clone())).collect();
    let status_options: Vec<(String, String)> = TimerStatus::ALL
        .iter()
        .map(|s| (s.as_str().to_string(), status_label(*s).to_string()))
        .collect();
    let settings = crate::timer::effective_settings(state.db())
        .await
        .unwrap_or_default();
    let flash: Option<UiMessage> = session.remove(FLASH_KEY).await.ok().flatten();

    let list = query.view(total, rows.len(), Some(&sort));
    let forms = match super::admin_list::render_forms(
        &state,
        &session,
        &user,
        &list,
        PATH,
        "Filter the timers",
        "Name contains",
        vec![
            (
                "owner",
                FormElement::select(owner_options)
                    .title("Owner")
                    .placeholder("- Any -")
                    .default_value(owner.unwrap_or_default()),
            ),
            (
                "status",
                FormElement::select(status_options)
                    .title("Status")
                    .placeholder("- Any -")
                    .default_value(status.map(|s| s.as_str()).unwrap_or("")),
            ),
        ],
    )
    .await
    {
        Ok(forms) => forms,
        Err(response) => return response,
    };
    let may_administer = viewer.can(ADMINISTER);
    // The row buttons' form, only where there are buttons.
    let actions_form = if may_administer && !rows.is_empty() {
        let form = crate::form::Form::new(ACTIONS_FORM_ID)
            .action(PATH)
            .attribute("hidden", "hidden");
        match admin_form::render(&state, &session, &user, form, &[]).await {
            Ok(html) => html,
            Err(response) => return response,
        }
    } else {
        String::new()
    };

    let mut context = tera::Context::new();
    context.insert("list", &list);
    forms.insert_into(&mut context);
    context.insert("timers", &rows);
    context.insert("zone", &zone);
    context.insert("may_administer", &may_administer);
    context.insert("actions_form", &actions_form);
    context.insert("actions_form_id", ACTIONS_FORM_ID);
    context.insert("settings", &settings);
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

/// What records an administrator's change to a timer in the security log:
/// `timer.changed`, written in the change's own transaction.
fn change_record<'a>(
    state: &'a AppState,
    user: &User,
    action: &'static str,
) -> crate::timer::ChangeRecord<'a> {
    let actor = user.id;
    let request = crate::services::audit::AuditActor::current();
    (
        state.security_audit(),
        Box::new(move |before: &Timer, after: Option<&Timer>| {
            SecurityEvent::new(SecurityEventKind::TimerChanged)
                .actor(actor)
                .with_request_context(&request)
                .detail("action", action)
                .detail("timer_id", before.id)
                .detail("owner", &before.owner)
                .detail("name", &before.name)
                .detail("before", snapshot(before))
                .detail("after", after.map(snapshot))
        }),
    )
}

/// Log an administrator's committed change to a timer.
fn log_change(user: &User, action: &str, before: &Timer) {
    tracing::info!(
        user_id = %user.id,
        action,
        owner = %before.owner,
        name = %before.name,
        "timer changed from the admin"
    );
}

/// GET /admin/reports/timers/{id}/cancel: ask before cancelling the timer.
/// Any other action has no page of its own.
async fn cancel_form(
    State(state): State<AppState>,
    session: Session,
    Path((id, action)): Path<(i64, String)>,
) -> Response {
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if action != "cancel" {
        return render_not_found();
    }
    let timer = match crate::timer::find(state.db(), id).await {
        Ok(Some(t)) => t,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, "failed to load a timer");
            return render_server_error("The timer could not be read.");
        }
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let text = AdminText::for_page(&state, &viewer, PATH).await;
    let form = crate::form::Form::new(CANCEL_FORM_ID)
        .action(format!("{PATH}/{id}/cancel"))
        .element(
            "op",
            FormElement::submit("Cancel timer")
                .attribute("class", "button--danger")
                .suffix(format!(
                    r#"<a href="{PATH}" class="button button--secondary">{}</a>"#,
                    text.html("Cancel", &[])
                ))
                .weight(10),
        );
    let form_html = match admin_form::render(&state, &session, &user, form, &[]).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("timer", &timer);
    context.insert("form", &form_html);
    context.insert("path", &format!("{PATH}/{id}/cancel"));
    render_admin_template(&state, &viewer, "admin/reports/timer-cancel.html", context).await
}

/// POST /admin/reports/timers/{id}/{action}: `pause`, `resume` or `run`
/// (from the listing's buttons), or `cancel` (from the page that asks
/// first).
async fn row_action(
    State(state): State<AppState>,
    session: Session,
    Path((id, action)): Path<(i64, String)>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let change = match action.as_str() {
        "pause" => AdminChange::Pause,
        "resume" => AdminChange::Resume,
        "run" => AdminChange::RunNow,
        "cancel" => AdminChange::Cancel,
        _ => return render_not_found(),
    };
    let form_id = if matches!(change, AdminChange::Cancel) {
        CANCEL_FORM_ID
    } else {
        ACTIONS_FORM_ID
    };
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, form_id, Vec::new()).await {
            Ok(form_state) => form_state,
            Err(Ok(_)) => {
                return super::helpers::render_error(
                    "The form has expired. Reload the page and try again.",
                );
            }
            Err(Err(response)) => return response,
        };
    match state
        .timers()
        .admin_change_recorded(id, &change, change_record(&state, &user, change.action()))
        .await
    {
        Ok((before, _)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            log_change(&user, change.action(), &before);
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
    errors: &[ValidationError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let settings = crate::timer::effective_settings(state.db())
        .await
        .unwrap_or_default();
    let action = format!("/admin/reports/timers/{}/edit", timer.id);
    let text = AdminText::for_page(state, &viewer, &action).await;
    let min = settings.min_repeat_secs.to_string();
    let form = crate::form::Form::new(EDIT_FORM_ID)
        .action(action.clone())
        .element(
            "owner",
            FormElement::markup(format!(
                "<p>{}</p>",
                text.html("Owner: {owner}", &[("owner", &timer.owner)])
            ))
            .weight(0),
        )
        .element(
            "run_at",
            FormElement::input("datetime-local")
                .title("Next run")
                .description(words(
                    state,
                    &text,
                    "In {zone}. The timer fires at this second.",
                    &[("zone", zone)],
                ))
                .default_value(values.run_at.clone())
                .attribute("step", "1")
                .required()
                .weight(1),
        )
        .element(
            "repeat_secs",
            FormElement::input("number")
                .title("Repeat every (seconds)")
                .description(words(
                    state,
                    &text,
                    "Leave empty to run once. At least {min} seconds.",
                    &[("min", &min)],
                ))
                .default_value(values.repeat_secs.clone())
                .attribute("min", "1")
                .attribute("step", "1")
                .weight(2),
        )
        .element("op", submit_with_cancel(&text, "Save"));
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("timer", timer);
    context.insert("form", &form_html);
    context.insert("path", &action);
    render_admin_template(state, &viewer, "admin/reports/timer-edit.html", context).await
}

/// `source` with its placeholders filled from `args`, in the viewer's admin
/// language: for a description that names a value, which the form templates
/// translate without arguments.
fn words(state: &AppState, text: &AdminText, source: &str, args: &[(&str, &str)]) -> String {
    args.iter()
        .fold(UiMessage::new(source), |m, (k, v)| m.arg(k, *v))
        .text(state.locale().map(|l| &**l), &text.lang)
}

/// A form's primary button labelled `label`, with a Cancel link back to the
/// listing beside it.
fn submit_with_cancel(text: &AdminText, label: &str) -> FormElement {
    FormElement::submit(label)
        .suffix(format!(
            r#"<a href="{PATH}" class="button button--secondary">{}</a>"#,
            text.html("Cancel", &[])
        ))
        .weight(10)
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

/// POST /admin/reports/timers/{id}/edit
async fn edit_submit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
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
    let values = EditValues {
        run_at: posted.get("run_at").unwrap_or_default().trim().to_string(),
        repeat_secs: posted
            .get("repeat_secs")
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let mut errors = Vec::new();
    let run_at = parse_local(&state, &zone, &values.run_at).await;
    if run_at.is_none() {
        errors.push(ValidationError::field(
            "run_at",
            "Enter the next run as a date and time.",
        ));
    }
    let repeat_secs = if values.repeat_secs.is_empty() {
        Ok(None)
    } else {
        values.repeat_secs.parse::<i64>().map(Some)
    };
    if repeat_secs.is_err() {
        errors.push(ValidationError::field(
            "repeat_secs",
            "Enter the repeat interval as a whole number of seconds, or leave it empty to run once.",
        ));
    }
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, EDIT_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_edit(&state, &session, &user, &timer, &zone, &values, &errors).await;
            }
            Err(Err(response)) => return response,
        };
    let (Some(run_at), Ok(repeat_secs)) = (run_at, repeat_secs) else {
        return render_not_found();
    };
    match state
        .timers()
        .admin_change_recorded(
            id,
            &AdminChange::Edit {
                run_at,
                repeat_secs,
            },
            change_record(&state, &user, "edit"),
        )
        .await
    {
        Ok((before, _)) => {
            admin_form::submitted(&state, &user, &form_state, &posted).await;
            log_change(&user, "edit", &before);
            let _ = session
                .insert(
                    FLASH_KEY,
                    UiMessage::new("Saved {name}.").arg("name", before.name.clone()),
                )
                .await;
            Redirect::to(PATH).into_response()
        }
        Err(TimerRefused::RepeatTooShort { min }) => {
            let viewer = super::helpers::user_context_for(&state, &user).await;
            let text = AdminText::for_page(&state, &viewer, PATH).await;
            let errors = [ValidationError::field(
                "repeat_secs",
                words(
                    &state,
                    &text,
                    "The repeat interval must be at least {min} seconds.",
                    &[("min", &min.to_string())],
                ),
            )];
            render_edit(&state, &session, &user, &timer, &zone, &values, &errors).await
        }
        Err(TimerRefused::InvalidTime) => {
            let errors = [ValidationError::field(
                "run_at",
                "Enter the next run as a date and time.",
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
            "The most JSON a timer may carry to its handler, up to 65536.",
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
    errors: &[ValidationError],
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let saved = match crate::timer::saved_settings(state.db()).await {
        Ok(saved) => saved,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the timer settings");
            return render_server_error("The timer settings could not be read.");
        }
    };
    let text = AdminText::for_page(state, &viewer, SETTINGS_PATH).await;
    let mut form = crate::form::Form::new(SETTINGS_FORM_ID).action(SETTINGS_PATH);
    for (weight, th) in (0..).zip(throttles(&saved, &crate::timer::env_overrides(), typed)) {
        let field = FormElement::input("number")
            .title(th.label)
            .default_value(th.value)
            .weight(weight);
        // A setting the environment holds is shown, read only, and not edited
        // here.
        let field = if th.from_environment {
            field
                .description(words(
                    state,
                    &text,
                    "Set by the {variable} environment variable, which overrides this form. Change or unset it and restart to change this.",
                    &[("variable", th.variable)],
                ))
                .attribute("readonly", "readonly")
        } else {
            field
                .description(format!(
                    "{} {}",
                    words(state, &text, th.hint, &[]),
                    words(
                        state,
                        &text,
                        "Default {n}.",
                        &[("n", &th.default.to_string())]
                    )
                ))
                .attribute("min", "1")
                .attribute("step", "1")
                .required()
        };
        form = form.element(th.name, field);
    }
    let form = form.element("op", submit_with_cancel(&text, "Save throttles"));
    let form_html = match admin_form::render(state, session, user, form, errors).await {
        Ok(html) => html,
        Err(response) => return response,
    };
    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("path", SETTINGS_PATH);
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
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let posted = PostedForm(pairs);
    let user = match require_permission(&state, &session, ADMINISTER).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let form: HashMap<String, String> = posted.0.iter().cloned().collect();
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
                errors.push(ValidationError::field(
                    name,
                    "Enter a whole number above 0.",
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
    if !set[3] && after.max_payload_bytes > crate::timer::MAX_PAYLOAD_LIMIT {
        let viewer = super::helpers::user_context_for(&state, &user).await;
        let text = AdminText::for_page(&state, &viewer, SETTINGS_PATH).await;
        errors.push(ValidationError::field(
            "max_payload_bytes",
            words(
                &state,
                &text,
                "The largest payload can be at most {max} bytes.",
                &[("max", &crate::timer::MAX_PAYLOAD_LIMIT.to_string())],
            ),
        ));
    }
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, SETTINGS_FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => {
                return render_settings(&state, &session, &user, Some(&form), &errors).await;
            }
            Err(Err(response)) => return response,
        };
    if after != before {
        // The settings and `timer.settings_changed` commit together.
        let event = SecurityEvent::new(SecurityEventKind::TimerSettingsChanged)
            .actor(user.id)
            .with_request_context(&crate::services::audit::AuditActor::current())
            .detail("before", before)
            .detail("after", after);
        if let Err(e) =
            crate::timer::save_settings_recorded(state.db(), after, state.security_audit(), event)
                .await
        {
            tracing::error!(error = %e, "failed to save the timer settings");
            return render_server_error("The timer settings could not be saved.");
        }
        tracing::info!(user_id = %user.id, ?before, ?after, "timer settings changed");
    }
    admin_form::submitted(&state, &user, &form_state, &posted).await;
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
    fn a_repeat_is_said_in_the_largest_unit_that_divides_it() {
        let e = every(604_800);
        assert_eq!((e.count, e.unit), (7, "day"));
        let e = every(5400);
        assert_eq!((e.count, e.unit), (90, "minute"));
        let e = every(90);
        assert_eq!((e.count, e.unit), (90, "second"));
    }

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
