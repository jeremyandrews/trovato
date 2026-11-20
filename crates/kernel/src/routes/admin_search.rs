//! Search settings admin screen.
//!
//! `GET` and `POST /admin/config/search`: the ranking signals, type boosts,
//! priority pages, AI stage switches, analytics and sentiment settings, all one
//! [`SearchSettings`] value stored under one `site_config` key. Per type field
//! weights stay on each type's own screen (`/admin/structure/types/{type}/search`),
//! which this one links to rather than duplicates.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Serialize;
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::search::settings::{NUMBER_RANGES, TYPE_BOOST_RANGE};
use crate::search::{PriorityPage, SearchSettings};
use crate::state::AppState;

use super::helpers::{
    render_admin_template, render_server_error, require_csrf, require_permission,
};

/// Session key for the saved message.
const FLASH_KEY: &str = "search_settings_flash";

/// The screen's path.
const PATH: &str = "/admin/config/search";

/// Blank priority page rows offered below the configured ones.
const BLANK_PRIORITY_ROWS: usize = 3;

/// The switches, by form field name.
const SWITCHES: [&str; 5] = [
    "expand",
    "summarize",
    "followup",
    "analytics_enabled",
    "sentiment_enabled",
];

/// Build the router.
pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(show_form).post(save_form))
}

/// One numeric input as the template draws it.
#[derive(Debug, Serialize)]
struct NumberInput {
    field: &'static str,
    label: &'static str,
    min: f64,
    max: f64,
    value: String,
}

/// One item type's boost row.
#[derive(Debug, Serialize)]
struct TypeRow {
    machine_name: String,
    label: String,
    value: String,
}

/// What the form shows: the saved settings, or what was just submitted.
struct FormValues {
    numbers: BTreeMap<&'static str, String>,
    type_boosts: BTreeMap<String, String>,
    priority_pages: Vec<PriorityPage>,
    switches: BTreeMap<&'static str, bool>,
}

impl FormValues {
    fn from_settings(settings: &SearchSettings) -> Self {
        Self {
            numbers: settings
                .numbers()
                .into_iter()
                .map(|(field, value)| (field, value.to_string()))
                .collect(),
            type_boosts: settings
                .type_boosts
                .iter()
                .map(|(t, b)| (t.clone(), b.to_string()))
                .collect(),
            priority_pages: settings.priority_pages.clone(),
            switches: BTreeMap::from([
                ("expand", settings.expand),
                ("summarize", settings.summarize),
                ("followup", settings.followup),
                ("analytics_enabled", settings.analytics_enabled),
                ("sentiment_enabled", settings.sentiment_enabled),
            ]),
        }
    }
}

/// Read the submitted form into settings, collecting every problem.
///
/// Raw pairs rather than a struct, because the type boost and priority page
/// rows repeat. A number left empty takes its default; a switch not submitted
/// is off, as an unticked checkbox is.
fn parse_form(pairs: &[(String, String)]) -> (FormValues, Result<SearchSettings, Vec<String>>) {
    let mut values = FormValues {
        numbers: BTreeMap::new(),
        type_boosts: BTreeMap::new(),
        priority_pages: Vec::new(),
        switches: SWITCHES.iter().map(|s| (*s, false)).collect(),
    };
    let mut keywords = Vec::new();
    let mut paths = Vec::new();

    for (key, value) in pairs {
        if let Some(range) = NUMBER_RANGES.iter().find(|r| r.field == key) {
            values.numbers.insert(range.field, value.trim().to_string());
        } else if let Some(item_type) = key
            .strip_prefix("type_boost[")
            .and_then(|k| k.strip_suffix(']'))
        {
            values
                .type_boosts
                .insert(item_type.to_string(), value.trim().to_string());
        } else if key == "priority_keyword" {
            keywords.push(value.trim().to_string());
        } else if key == "priority_path" {
            paths.push(value.trim().to_string());
        } else if let Some(switch) = SWITCHES.iter().find(|s| **s == key) {
            values.switches.insert(switch, true);
        }
    }

    for i in 0..keywords.len().max(paths.len()) {
        let keyword = keywords.get(i).cloned().unwrap_or_default();
        let path = paths.get(i).cloned().unwrap_or_default();
        if keyword.is_empty() && path.is_empty() {
            continue;
        }
        values.priority_pages.push(PriorityPage { keyword, path });
    }

    let mut errors = Vec::new();
    let mut settings = SearchSettings::default();

    for range in NUMBER_RANGES {
        let Some(raw) = values.numbers.get(range.field).filter(|v| !v.is_empty()) else {
            continue;
        };
        match raw.parse::<f64>() {
            Ok(n) => {
                if let Err(message) = set_number(&mut settings, range.field, n) {
                    errors.push(format!("{} {message}", range.label));
                }
            }
            Err(_) => errors.push(format!("{} must be a number.", range.label)),
        }
    }

    let (min, max) = TYPE_BOOST_RANGE;
    for (item_type, raw) in &values.type_boosts {
        if raw.is_empty() {
            continue;
        }
        match raw.parse::<f64>() {
            Ok(n) => {
                // 1.0 is what an unlisted type gets; storing it changes nothing.
                if n != 1.0 {
                    settings.type_boosts.insert(item_type.clone(), n);
                }
            }
            Err(_) => errors.push(format!(
                "The boost for {item_type} must be a number from {min} to {max}."
            )),
        }
    }

    settings.priority_pages = values.priority_pages.clone();
    settings.expand = values.switches["expand"];
    settings.summarize = values.switches["summarize"];
    settings.followup = values.switches["followup"];
    settings.analytics_enabled = values.switches["analytics_enabled"];
    settings.sentiment_enabled = values.switches["sentiment_enabled"];

    if let Err(more) = settings.validate() {
        errors.extend(more);
    }
    errors.dedup();

    if errors.is_empty() {
        (values, Ok(settings))
    } else {
        (values, Err(errors))
    }
}

/// Assign one parsed number to its field. Whole number fields refuse a
/// fraction; ranges are checked afterwards by [`SearchSettings::validate`].
fn set_number(settings: &mut SearchSettings, field: &str, n: f64) -> Result<(), &'static str> {
    let whole = || -> Result<u32, &'static str> {
        if n.is_finite() && n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&n) {
            Ok(n as u32)
        } else {
            Err("must be a whole number in range.")
        }
    };
    match field {
        "recency_boost_max" => settings.recency_boost_max = n,
        "recency_half_life_days" => settings.recency_half_life_days = n,
        "recency_penalty_after_days" => settings.recency_penalty_after_days = n,
        "recency_max_penalty" => settings.recency_max_penalty = n,
        "title_match_boost" => settings.title_match_boost = n,
        "title_all_terms_multiplier" => settings.title_all_terms_multiplier = n,
        "content_match_boost" => settings.content_match_boost = n,
        "priority_boost" => settings.priority_boost = n,
        "excerpt_length" => settings.excerpt_length = whole()?,
        "results_per_page" => settings.results_per_page = whole()?,
        "max_pagefind_results" => settings.max_pagefind_results = whole()?,
        "ai_summary_top_n" => settings.ai_summary_top_n = whole()?,
        "ai_summary_max_chars" => settings.ai_summary_max_chars = whole()?,
        "max_followups" => settings.max_followups = whole()?,
        "analytics_retention_days" => settings.analytics_retention_days = whole()?,
        _ => {}
    }
    Ok(())
}

/// Render the form with `values`, and `errors` if any.
async fn render(
    state: &AppState,
    session: &Session,
    values: FormValues,
    errors: Option<Vec<String>>,
) -> Response {
    let csrf_token = generate_csrf_token(session).await;

    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear search settings flash message");
    }

    let numbers: BTreeMap<&str, NumberInput> = NUMBER_RANGES
        .iter()
        .map(|r| {
            (
                r.field,
                NumberInput {
                    field: r.field,
                    label: r.label,
                    min: r.min,
                    max: r.max,
                    value: values.numbers.get(r.field).cloned().unwrap_or_default(),
                },
            )
        })
        .collect();

    let mut types: Vec<TypeRow> = state
        .content_types()
        .list_all()
        .await
        .into_iter()
        .map(|t| TypeRow {
            value: values
                .type_boosts
                .get(&t.machine_name)
                .cloned()
                .unwrap_or_default(),
            machine_name: t.machine_name,
            label: t.label,
        })
        .collect();
    types.sort_by(|a, b| a.label.cmp(&b.label));

    let mut priority_rows = values.priority_pages;
    priority_rows.extend(std::iter::repeat_n(
        PriorityPage {
            keyword: String::new(),
            path: String::new(),
        },
        BLANK_PRIORITY_ROWS,
    ));

    let mut context = tera::Context::new();
    context.insert("csrf_token", &csrf_token);
    context.insert("numbers", &numbers);
    context.insert("types", &types);
    context.insert("type_boost_min", &TYPE_BOOST_RANGE.0);
    context.insert("type_boost_max", &TYPE_BOOST_RANGE.1);
    context.insert("priority_rows", &priority_rows);
    context.insert("switches", &values.switches);
    context.insert("flash", &flash);
    context.insert("errors", &errors);
    context.insert("path", PATH);

    render_admin_template(
        state,
        &crate::routes::helpers::get_user_context(session, state).await,
        "admin/config/search.html",
        context,
    )
    .await
}

/// GET /admin/config/search
async fn show_form(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    render(&state, &session, FormValues::from_settings(&settings), None).await
}

/// POST /admin/config/search
async fn save_form(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }
    let token = pairs
        .iter()
        .find(|(k, _)| k == "_token")
        .map(|(_, v)| v.as_str())
        .unwrap_or_default();
    if let Err(resp) = require_csrf(&session, token).await {
        return resp;
    }

    let (values, parsed) = parse_form(&pairs);
    let settings = match parsed {
        Ok(settings) => settings,
        Err(errors) => return render(&state, &session, values, Some(errors)).await,
    };

    if let Err(e) = settings.save(state.db(), state.cache()).await {
        tracing::error!(error = %e, "failed to save search settings");
        return render_server_error("Failed to save search settings.");
    }

    let _ = session.insert(FLASH_KEY, "Search settings saved.").await;
    tracing::info!("search settings updated");
    Redirect::to(PATH).into_response()
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn an_empty_form_is_the_defaults_with_every_switch_off() {
        let (_, parsed) = parse_form(&pairs(&[("_token", "t")]));
        let s = parsed.unwrap();
        assert!(!s.expand && !s.summarize && !s.followup);
        assert!(!s.analytics_enabled && !s.sentiment_enabled);
        assert_eq!(s.recency_half_life_days, 365.0);
    }

    #[test]
    fn rows_pair_up_and_blank_rows_are_skipped() {
        let (_, parsed) = parse_form(&pairs(&[
            ("priority_keyword", "team"),
            ("priority_path", "/team"),
            ("priority_keyword", ""),
            ("priority_path", ""),
            ("type_boost[conference]", "2"),
            ("type_boost[page]", "1"),
            ("type_boost[blog]", ""),
        ]));
        let s = parsed.unwrap();
        assert_eq!(s.priority_pages.len(), 1);
        assert_eq!(s.type_boosts.len(), 1, "1.0 and blank are not stored");
        assert_eq!(s.type_boosts["conference"], 2.0);
    }

    #[test]
    fn bad_numbers_are_named() {
        let (_, parsed) = parse_form(&pairs(&[
            ("results_per_page", "2.5"),
            ("recency_boost_max", "lots"),
            ("type_boost[page]", "x"),
        ]));
        let errors = parsed.unwrap_err();
        assert_eq!(errors.len(), 3, "{errors:?}");
    }

    #[test]
    fn a_half_filled_priority_row_is_an_error() {
        let (_, parsed) = parse_form(&pairs(&[("priority_keyword", "team")]));
        assert!(parsed.is_err());
    }
}
