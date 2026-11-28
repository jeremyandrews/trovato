//! Search settings admin screen.
//!
//! `GET` and `POST /admin/config/search`: the ranking signals, type boosts,
//! priority pages, AI stage switches, analytics and sentiment settings, all one
//! [`SearchSettings`] value stored under one `site_config` key. Per type field
//! weights stay on each type's own screen (`/admin/structure/types/{type}/search`),
//! which this one links to rather than duplicates. The form is built and
//! checked through `FormService`.

use std::collections::BTreeMap;

use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use tower_sessions::Session;

use crate::content::submission::PostedForm;
use crate::form::{FormElement, ValidationError};
use crate::i18n::{UiMessage, data_html};
use crate::routes::admin_nav::AdminText;
use crate::search::settings::{NUMBER_RANGES, NumberRange, TYPE_BOOST_RANGE};
use crate::search::{PriorityPage, SearchSettings};
use crate::state::AppState;

use super::admin_form;
use super::helpers::{render_admin_template, render_server_error, require_permission};

/// Session key for the saved message.
const FLASH_KEY: &str = "search_settings_flash";

/// The screen's path.
const PATH: &str = "/admin/config/search";

/// The form id of the search settings form.
const FORM_ID: &str = "search_settings_form";

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

/// The number fields that take a whole number; the rest take any.
const WHOLE_NUMBERS: [&str; 7] = [
    "excerpt_length",
    "results_per_page",
    "max_pagefind_results",
    "ai_summary_top_n",
    "ai_summary_max_chars",
    "max_followups",
    "analytics_retention_days",
];

/// Build the router.
pub fn router() -> Router<AppState> {
    Router::new().route(PATH, get(show_form).post(save_form))
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
/// is off, as an unticked checkbox is. `say` puts a message into the words
/// the form shows it in.
fn parse_form(
    pairs: &[(String, String)],
    say: impl Fn(UiMessage) -> String,
) -> (FormValues, Result<SearchSettings, Vec<ValidationError>>) {
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
        let label = say(UiMessage::new(range.label));
        match raw.parse::<f64>() {
            Ok(n) => {
                if let Err(message) = set_number(&mut settings, range.field, n) {
                    errors.push(ValidationError::field(
                        range.field,
                        say(UiMessage::new(format!("{{label}} {message}")).arg("label", label)),
                    ));
                }
            }
            Err(_) => errors.push(ValidationError::field(
                range.field,
                say(UiMessage::new("{label} must be a number.").arg("label", label)),
            )),
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
            Err(_) => errors.push(ValidationError::field(
                format!("type_boost_{item_type}"),
                say(
                    UiMessage::new("The boost for {type} must be a number from {min} to {max}.")
                        .arg("type", item_type)
                        .arg("min", min.to_string())
                        .arg("max", max.to_string()),
                ),
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
        errors.extend(more.into_iter().map(ValidationError::form));
    }
    errors.dedup_by(|a, b| a.field == b.field && a.message == b.message);

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
        "index_compaction_percent" => settings.index_compaction_percent = n,
        _ => {}
    }
    Ok(())
}

/// One item type's boost row.
struct TypeRow {
    machine_name: String,
    label: String,
    value: String,
}

/// What the form is built from, besides the values: the item types, and the
/// viewer's words.
struct FormContext<'a> {
    state: &'a AppState,
    text: &'a AdminText,
    types: Vec<TypeRow>,
}

impl FormContext<'_> {
    /// `source` with its placeholders filled from `args`, in the viewer's
    /// admin language: for a description that names a value, which the form
    /// templates translate without arguments.
    fn words(&self, source: &str, args: &[(&str, &str)]) -> String {
        args.iter()
            .fold(UiMessage::new(source), |m, (k, v)| m.arg(k, *v))
            .text(self.state.locale().map(|l| &**l), &self.text.lang)
    }

    /// A fieldset's explanation, as the paragraph it always was.
    fn explanation(&self, html: String) -> FormElement {
        FormElement::markup(format!(r#"<p class="description">{html}</p>"#)).weight(0)
    }

    /// The number field `field`, from the kernel's ranking table.
    fn number(&self, values: &FormValues, field: &str, weight: i32) -> FormElement {
        let Some(range) = NUMBER_RANGES.iter().find(|r| r.field == field) else {
            return FormElement::markup(String::new());
        };
        number_input(self, range, values, weight)
    }
}

/// A number field for `range`: its label, "From min to max.", its bounds,
/// and a step that says whether it takes a fraction.
fn number_input(
    cx: &FormContext<'_>,
    range: &NumberRange,
    values: &FormValues,
    weight: i32,
) -> FormElement {
    let step = if WHOLE_NUMBERS.contains(&range.field) {
        "1"
    } else if range.field == "index_compaction_percent" {
        "0.5"
    } else {
        "any"
    };
    let (min, max) = (range.min.to_string(), range.max.to_string());
    FormElement::input("number")
        .title(range.label)
        .description(cx.words("From {min} to {max}.", &[("min", &min), ("max", &max)]))
        .default_value(values.numbers.get(range.field).cloned().unwrap_or_default())
        .attribute("min", min.clone())
        .attribute("max", max.clone())
        .attribute("step", step)
        .weight(weight)
}

/// The type boost table: one number per item type, and a link to its field
/// weights. The inputs repeat by type, so they are markup in the form.
fn type_boost_table(cx: &FormContext<'_>) -> String {
    let text = cx.text;
    let (min, max) = (
        TYPE_BOOST_RANGE.0.to_string(),
        TYPE_BOOST_RANGE.1.to_string(),
    );
    let rows: String = cx
        .types
        .iter()
        .map(|ty| {
            let name = tera::escape_html(&ty.machine_name);
            format!(
                r#"<tr><th scope="row">{label} <code>{machine}</code></th><td><label class="visually-hidden" for="type_boost_{name}">{boost_for}</label><input type="number" id="type_boost_{name}" name="type_boost[{name}]" class="form-text" value="{value}" min="{min}" max="{max}" step="any" placeholder="1"></td><td><a href="/admin/structure/types/{name}/search">{weights_for}</a></td></tr>"#,
                label = data_html(&ty.label),
                machine = data_html(&ty.machine_name),
                boost_for = text.html("Boost for {type}", &[("type", &ty.label)]),
                value = tera::escape_html(&ty.value),
                weights_for = text.html("Field weights for {type}", &[("type", &ty.label)]),
            )
        })
        .collect();
    format!(
        r#"<div class="table-scroll" role="region" aria-labelledby="type-boosts-caption" tabindex="0"><table class="table"><caption id="type-boosts-caption">{caption}</caption><thead><tr><th scope="col">{type_}</th><th scope="col">{boost}</th><th scope="col">{weights}</th></tr></thead><tbody>{rows}</tbody></table></div>"#,
        caption = text.html("Boost per content type", &[]),
        type_ = text.html("Type", &[]),
        boost = text.html("Boost", &[]),
        weights = text.html("Field weights", &[]),
    )
}

/// The priority page table: a keyword and a path per row, the configured
/// rows and some empty ones. The inputs repeat, so they are markup.
fn priority_table(cx: &FormContext<'_>, rows: &[PriorityPage]) -> String {
    let text = cx.text;
    let blank = PriorityPage {
        keyword: String::new(),
        path: String::new(),
    };
    let body: String = rows
        .iter()
        .chain(std::iter::repeat_n(&blank, BLANK_PRIORITY_ROWS))
        .enumerate()
        .map(|(i, row)| {
            let n = (i + 1).to_string();
            format!(
                r#"<tr><td><input type="text" name="priority_keyword" class="form-text" value="{keyword}" maxlength="100" aria-label="{keyword_n}"></td><td><input type="text" name="priority_path" class="form-text" value="{path}" maxlength="255" placeholder="{about}" aria-label="{path_n}"></td></tr>"#,
                keyword = tera::escape_html(&row.keyword),
                keyword_n = text.html("Keyword {n}", &[("n", &n)]),
                path = tera::escape_html(&row.path),
                about = text.html("/about", &[]),
                path_n = text.html("Path {n}", &[("n", &n)]),
            )
        })
        .collect();
    format!(
        r#"<div class="table-scroll" role="region" aria-labelledby="priority-pages-caption" tabindex="0"><table class="table"><caption id="priority-pages-caption">{caption}</caption><thead><tr><th scope="col">{keyword}</th><th scope="col">{path}</th></tr></thead><tbody>{body}</tbody></table></div>"#,
        caption = text.html("Keywords and the pages they boost", &[]),
        keyword = text.html("Keyword", &[]),
        path = text.html("Path", &[]),
    )
}

/// The search settings form, with `values`.
fn settings_form(cx: &FormContext<'_>, values: &FormValues) -> crate::form::Form {
    let text = cx.text;
    let switch = |name: &str, title: &str, weight: i32| {
        FormElement::checkbox()
            .title(title)
            .default_value(values.switches.get(name).copied().unwrap_or(false))
            .weight(weight)
    };
    let mut ranking = FormElement::fieldset().title("Ranking signals").weight(0).child(
        "ranking_explanation",
        cx.explanation(text.html(
            "Applied in the browser to Pagefind results. A result's score is its Pagefind position plus the recency, title and excerpt signals, multiplied by its type boost and any priority page boost.",
            &[],
        )),
    );
    for (weight, field) in (1..).zip([
        "recency_boost_max",
        "recency_half_life_days",
        "recency_penalty_after_days",
        "recency_max_penalty",
        "title_match_boost",
        "title_all_terms_multiplier",
        "content_match_boost",
        "excerpt_length",
        "results_per_page",
        "max_pagefind_results",
    ]) {
        ranking = ranking.child(field, cx.number(values, field, weight));
    }
    let (min, max) = (
        TYPE_BOOST_RANGE.0.to_string(),
        TYPE_BOOST_RANGE.1.to_string(),
    );
    crate::form::Form::new(FORM_ID)
        .action(PATH)
        .element("ranking", ranking)
        .element(
            "type_boosts",
            FormElement::fieldset()
                .title("Type boosts")
                .weight(1)
                .child(
                    "type_boosts_explanation",
                    cx.explanation(text.html(
                        "A multiplier on every result of the type, in the browser and in server search alike. Leave empty for 1 (no boost); from {min} to {max}. Which fields of a type are searched, and at what weight, is set on that type's own search screen.",
                        &[("min", &min), ("max", &max)],
                    )),
                )
                .child(
                    "type_boost_table",
                    FormElement::markup(type_boost_table(cx)).weight(1),
                ),
        )
        .element(
            "priority_pages",
            FormElement::fieldset()
                .title("Priority pages")
                .weight(2)
                .child(
                    "priority_pages_explanation",
                    cx.explanation(text.html(
                        "When a query contains the keyword as a whole word, the result at that path is boosted. Applied in the browser. Paths start with {slash}; at most 50 rows.",
                        &[("slash", "/")],
                    )),
                )
                .child("priority_boost", cx.number(values, "priority_boost", 1))
                .child(
                    "priority_table",
                    FormElement::markup(priority_table(cx, &values.priority_pages)).weight(2),
                )
                .child(
                    "priority_pages_more",
                    FormElement::markup(format!(
                        r#"<p class="description">{}</p>"#,
                        text.html("Save to get more empty rows. Empty rows are ignored.", &[])
                    ))
                    .weight(3),
                ),
        )
        .element(
            "ai",
            FormElement::fieldset()
                .title("AI stages")
                .weight(3)
                .child(
                    "ai_explanation",
                    cx.explanation(text.html(
                        "Each stage calls the configured AI provider and costs tokens. A stage switched off here is refused by the server, not only hidden on the search page.",
                        &[],
                    )),
                )
                .child(
                    "expand",
                    switch(
                        "expand",
                        "Query expansion: related terms searched alongside the query",
                        1,
                    ),
                )
                .child(
                    "summarize",
                    switch("summarize", "Summary: an AI answer above the results", 2),
                )
                .child(
                    "followup",
                    switch("followup", "Follow-up questions after the summary", 3),
                )
                .child("max_followups", cx.number(values, "max_followups", 4))
                .child(
                    "ai_summary_top_n",
                    cx.number(values, "ai_summary_top_n", 5),
                )
                .child(
                    "ai_summary_max_chars",
                    cx.number(values, "ai_summary_max_chars", 6),
                ),
        )
        .element(
            "analytics",
            FormElement::fieldset()
                .title("Analytics")
                .weight(4)
                .child(
                    "analytics_explanation",
                    cx.explanation(text.html(
                        "Counts queries per day for the search report: the query text with email addresses and long numbers removed, how often it was searched and how often it found nothing. Nothing that identifies a visitor is stored: no IP address, account, session, browser or time finer than the day.",
                        &[],
                    )),
                )
                .child(
                    "analytics_enabled",
                    switch("analytics_enabled", "Record search analytics", 1),
                )
                .child(
                    "analytics_retention_days",
                    cx.number(values, "analytics_retention_days", 2),
                ),
        )
        .element(
            "index",
            FormElement::fieldset()
                .title("Search index")
                .weight(5)
                .child(
                    "index_explanation",
                    cx.explanation(text.html(
                        "Each change to content rewrites only its own part of the search index. A deleted or unpublished item leaves a freed slot that the next new item reuses; while freed slots are left over they shift ranking slightly, so past this share of a language's slots the whole index is rebuilt densely at the next cron run.",
                        &[],
                    )),
                )
                .child(
                    "index_compaction_percent",
                    cx.number(values, "index_compaction_percent", 1),
                ),
        )
        .element(
            "sentiment",
            FormElement::fieldset()
                .title("Sentiment")
                .weight(6)
                .child(
                    "sentiment_explanation",
                    cx.explanation(format!(
                        "<strong>{}</strong> {}",
                        text.html(
                            "Sends follow-up text to the configured AI provider and costs tokens.",
                            &[]
                        ),
                        text.html(
                            "Each follow-up question is classified as satisfied, confused or unmet, after the answer has been sent, and only the count of each label is kept. Needs analytics switched on. Off by default.",
                            &[]
                        )
                    )),
                )
                .child(
                    "sentiment_enabled",
                    switch("sentiment_enabled", "Classify follow-up sentiment", 1),
                ),
        )
        .element(
            "effect",
            FormElement::markup(format!(
                r#"<p class="form__effect"><strong class="form__effect-label">{}</strong> {}</p>"#,
                text.html("What this changes:", &[]),
                text.html(
                    "how every search on the site ranks its results, which AI stages it may call, and what the search report records, from the next search on.",
                    &[]
                )
            ))
            .weight(9),
        )
        .element("op", FormElement::submit("Save settings").weight(10))
}

/// Render the form with `values`, and `errors` if any.
async fn render(
    state: &AppState,
    session: &Session,
    user: &crate::models::User,
    values: FormValues,
    errors: &[ValidationError],
) -> Response {
    let flash: Option<String> = session.get(FLASH_KEY).await.ok().flatten();
    if flash.is_some()
        && let Err(e) = session.remove::<String>(FLASH_KEY).await
    {
        tracing::warn!(error = %e, "failed to clear search settings flash message");
    }

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

    let viewer = super::helpers::user_context_for(state, user).await;
    let text = AdminText::for_page(state, &viewer, PATH).await;
    let cx = FormContext {
        state,
        text: &text,
        types,
    };
    let form_html =
        match admin_form::render(state, session, user, settings_form(&cx, &values), errors).await {
            Ok(html) => html,
            Err(response) => return response,
        };

    let mut context = tera::Context::new();
    context.insert("form", &form_html);
    context.insert("flash", &flash);
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
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let settings = SearchSettings::load(state.db(), state.cache()).await;
    render(
        &state,
        &session,
        &user,
        FormValues::from_settings(&settings),
        &[],
    )
    .await
}

/// POST /admin/config/search
async fn save_form(
    State(state): State<AppState>,
    session: Session,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Response {
    let user = match require_permission(&state, &session, "administer site").await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let text = AdminText::for_page(&state, &viewer, PATH).await;
    let (values, parsed) = parse_form(&pairs, |m| m.text(state.locale().map(|l| &**l), &text.lang));
    let posted = PostedForm(pairs);
    let errors = parsed.as_ref().err().cloned().unwrap_or_default();
    let form_state =
        match admin_form::check(&state, &session, &user, &posted, FORM_ID, errors).await {
            Ok(form_state) => form_state,
            Err(Ok(errors)) => return render(&state, &session, &user, values, &errors).await,
            Err(Err(response)) => return response,
        };
    let Ok(settings) = parsed else {
        return render_server_error("Failed to save search settings.");
    };

    if let Err(e) = settings.save(state.db(), state.cache()).await {
        tracing::error!(error = %e, "failed to save search settings");
        return render_server_error("Failed to save search settings.");
    }

    admin_form::submitted(&state, &user, &form_state, &posted).await;
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

    fn english(m: UiMessage) -> String {
        m.text(None, "en")
    }

    #[test]
    fn an_empty_form_is_the_defaults_with_every_switch_off() {
        let (_, parsed) = parse_form(&pairs(&[("_token", "t")]), english);
        let s = parsed.unwrap();
        assert!(!s.expand && !s.summarize && !s.followup);
        assert!(!s.analytics_enabled && !s.sentiment_enabled);
        assert_eq!(s.recency_half_life_days, 365.0);
    }

    #[test]
    fn rows_pair_up_and_blank_rows_are_skipped() {
        let (_, parsed) = parse_form(
            &pairs(&[
                ("priority_keyword", "team"),
                ("priority_path", "/team"),
                ("priority_keyword", ""),
                ("priority_path", ""),
                ("type_boost[conference]", "2"),
                ("type_boost[page]", "1"),
                ("type_boost[blog]", ""),
            ]),
            english,
        );
        let s = parsed.unwrap();
        assert_eq!(s.priority_pages.len(), 1);
        assert_eq!(s.type_boosts.len(), 1, "1.0 and blank are not stored");
        assert_eq!(s.type_boosts["conference"], 2.0);
    }

    #[test]
    fn bad_numbers_are_named() {
        let (_, parsed) = parse_form(
            &pairs(&[
                ("results_per_page", "2.5"),
                ("recency_boost_max", "lots"),
                ("type_boost[page]", "x"),
            ]),
            english,
        );
        let errors = parsed.unwrap_err();
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(
            errors
                .iter()
                .any(|e| e.field.as_deref() == Some("recency_boost_max")
                    && e.message == "Recency boost, maximum must be a number."),
            "{errors:?}"
        );
    }

    #[test]
    fn a_half_filled_priority_row_is_an_error() {
        let (_, parsed) = parse_form(&pairs(&[("priority_keyword", "team")]), english);
        assert!(parsed.is_err());
    }
}
