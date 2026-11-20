#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The Pagefind export honours field weights and indexes titles.
//!
//! The export joined every configured field into one `data-pagefind-body` div in
//! an order guessed from field names, and put the title in an `<h1>` outside that
//! div. Pagefind indexes only inside `data-pagefind-body` when a page has one, so
//! a title alone was not findable, and a match in a city ranked like a match in a
//! summary.
//!
//! This builds the export list from the database exactly as the cron job does
//! (`indexable_documents`) and renders one item's fragment (`render_fragment`),
//! so no Pagefind CLI is needed.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{run_test, shared_app};
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// A fixture type of its own, so configuring its search fields touches no other
/// test. Created once and kept, like the other fixture types, so a second pass
/// over the same database adds nothing.
const FIXTURE_TYPE: &str = "pf_weight_probe";

async fn ensure_fixture_type(app: &common::TestApp) {
    sqlx::query(
        "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
         VALUES ($1, 'Pagefind weight probe', 'Pagefind fixture', true, 'Title', 'pagefind_test', \
                 '{\"fields\": []}'::jsonb) \
         ON CONFLICT (type) DO NOTHING",
    )
    .bind(FIXTURE_TYPE)
    .execute(&app.db)
    .await
    .expect("seed the fixture type");

    // B and C: two letters with different weights, neither the title's A.
    for (field, weight) in [("field_summary", 'B'), ("field_city", 'C')] {
        app.state
            .search()
            .configure_field(FIXTURE_TYPE, field, weight)
            .await
            .expect("configure a search field");
    }
}

/// The opening tag of the element whose text content starts at `text`.
fn opening_tag_before<'a>(html: &'a str, text: &str) -> &'a str {
    let at = html
        .find(text)
        .unwrap_or_else(|| panic!("{text:?} must be in the fragment: {html}"));
    let open = html[..at]
        .rfind('<')
        .unwrap_or_else(|| panic!("{text:?} must sit in an element: {html}"));
    &html[open..at]
}

/// The `data-pagefind-weight` an opening tag carries.
fn weight_of(tag: &str) -> f32 {
    let needle = "data-pagefind-weight=\"";
    let start = tag
        .find(needle)
        .map(|i| i + needle.len())
        .unwrap_or_else(|| panic!("{tag:?} must carry data-pagefind-weight"));
    let end = tag[start..]
        .find('"')
        .expect("attribute must be terminated");
    tag[start..start + end]
        .parse()
        .unwrap_or_else(|_| panic!("weight in {tag:?} must be a number"))
}

#[test]
fn the_fragment_indexes_the_title_and_weights_each_field() {
    run_test(async {
        let app = shared_app().await;
        ensure_fixture_type(app).await;

        let tag = format!("zqxpfw{}", Uuid::now_v7().simple());
        let title = format!("Title {tag}");
        let summary = format!("Summary {tag}");
        let city = format!("City {tag}");

        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let id = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: FIXTURE_TYPE.to_string(),
                    title: title.clone(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({
                        "field_city": city,
                        "field_summary": { "value": summary },
                    })),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("pagefind weight test".to_string()),
                },
                &admin,
            )
            .await
            .expect("create")
            .id;

        let docs = trovato_kernel::cron::indexable_documents(&app.db, app.state.items())
            .await
            .expect("build the export list");
        let doc = docs
            .iter()
            .find(|d| d.id == id)
            .expect("a published live item is exported");
        let html = trovato_kernel::cron::render_fragment(doc);

        // The indexed region: Pagefind indexes only inside data-pagefind-body.
        let body_at = html
            .find("data-pagefind-body")
            .unwrap_or_else(|| panic!("the fragment must mark its indexed region: {html}"));
        let region_start = html[body_at..].find('>').unwrap() + body_at + 1;
        let region_end = html[region_start..]
            .find("</div>")
            .map(|i| i + region_start)
            .expect("the indexed region must be closed");
        let region = &html[region_start..region_end];

        // The title is inside it, at A's weight, and still the result's title.
        let title_tag = opening_tag_before(region, &title);
        assert!(
            title_tag.contains("data-pagefind-meta=\"title\""),
            "the title element keeps its meta role: {title_tag}"
        );
        assert_eq!(weight_of(title_tag), 10.0, "the title is weight A");

        // Each configured field in its own element, at its letter's weight.
        let summary_tag = opening_tag_before(region, &summary);
        let city_tag = opening_tag_before(region, &city);
        assert!(
            region.find(&summary).unwrap() < region.find(&city).unwrap(),
            "fields are ordered by weight, heaviest first: {region}"
        );
        assert_eq!(weight_of(summary_tag), 4.0, "B maps to 4.0: {region}");
        assert_eq!(weight_of(city_tag), 2.0, "C maps to 2.0: {region}");
        assert!(
            !region[region.find(&summary).unwrap()..].starts_with(&format!("{summary}. {city}")),
            "the fields are separate elements, not one joined string: {region}"
        );
    });
}
