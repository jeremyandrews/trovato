#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Alias lookup for every language and character set (B161).
//!
//! Before B161 the alias fallback looked a path up exactly as requested, so an
//! alias with a non-ASCII character (`quiche-mediterráneo-profundo`, which a
//! browser sends percent-encoded) was a 404, a differently cased old link
//! (`/BLOG/TILLICUM`) was a 404, and another language's alias under a prefix
//! (`/es/recipes/borscht-with-pork-ribs`) was served as a second address of a
//! page that has its own Spanish one. Each of these resolves now, in the same
//! single indexed query, and an alias that resolved before still does.
//!
//! The aliases here point at `/user/login`, a real route, so a resolved
//! alias is a 200 with the login form and nothing else needs creating.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;

async fn get(app: &TestApp, uri: &str) -> axum::response::Response {
    app.request(Request::get(uri).body(Body::empty()).unwrap())
        .await
}

fn location(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

/// Insert an alias of `/user/login`, deleted when the test ends.
async fn alias(app: &TestApp, path: &str, language: &str) {
    sqlx::query(
        "INSERT INTO url_alias (id, source, alias, language, stage_id, created, changed) \
         VALUES ($1, '/user/login', $2, $3, $4, 0, 0)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(path)
    .bind(language)
    .bind(LIVE_STAGE_ID)
    .execute(&app.db)
    .await
    .expect("insert alias");
    let db = app.db.clone();
    let path = path.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM url_alias WHERE alias = $1")
            .bind(&path)
            .execute(&db)
            .await;
    });
}

fn stamp() -> String {
    format!("b161-{}", &uuid::Uuid::now_v7().simple().to_string()[20..])
}

/// Make sure Spanish is a language of the site for this test, and remove it
/// again afterwards if this test added it.
async fn ensure_spanish(app: &TestApp) {
    let added = sqlx::query(
        "INSERT INTO language (id, label, weight, is_default, direction) \
         VALUES ('es', 'Spanish', 1, false, 'ltr') ON CONFLICT (id) DO NOTHING",
    )
    .execute(&app.db)
    .await
    .expect("insert language")
    .rows_affected()
        > 0;
    app.state.reload_languages().await.expect("reload");
    if added {
        let db = app.db.clone();
        let state = app.state.clone();
        common::defer_cleanup(move || async move {
            let _ =
                sqlx::query("DELETE FROM url_alias WHERE language = 'es' AND alias LIKE '/b161-%'")
                    .execute(&db)
                    .await;
            let _ = sqlx::query("DELETE FROM language WHERE id = 'es'")
                .execute(&db)
                .await;
            let _ = state.reload_languages().await;
        });
    }
}

/// **The finding (K-alias-utf8).** An alias with non-ASCII characters is
/// requested percent-encoded, and resolves; an alias stored percent-encoded,
/// which resolved before, still does.
#[test]
fn an_alias_with_non_ascii_characters_resolves() {
    run_test(async {
        let app = shared_app().await;
        let s = stamp();
        alias(app, &format!("/{s}/quiche-mediterráneo-profundo"), "en").await;
        alias(app, &format!("/{s}/caf%C3%A9-stored-encoded"), "en").await;

        let response = get(app, &format!("/{s}/quiche-mediterr%C3%A1neo-profundo")).await;
        assert_eq!(response.status(), StatusCode::OK, "the decoded alias");

        let response = get(app, &format!("/{s}/caf%C3%A9-stored-encoded")).await;
        assert_eq!(response.status(), StatusCode::OK, "an alias stored encoded");
    });
}

/// **The finding (K-alias-case).** An alias asked for in another letter case
/// is a permanent redirect to the stored spelling, query kept; non-ASCII in
/// the stored alias is percent-encoded in the `Location`.
#[test]
fn an_alias_in_another_letter_case_redirects_to_the_stored_one() {
    run_test(async {
        let app = shared_app().await;
        let s = stamp();
        alias(app, &format!("/{s}/blog/tillicum"), "en").await;
        alias(app, &format!("/{s}/blog/reposteria-dulce-ñ"), "en").await;

        let upper = s.to_uppercase();
        let response = get(app, &format!("/{upper}/BLOG/TILLICUM?nocache=1")).await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(location(&response), format!("/{s}/blog/tillicum?nocache=1"));

        let response = get(app, &format!("/{s}/blog/tillicum")).await;
        assert_eq!(response.status(), StatusCode::OK);

        let response = get(app, &format!("/{upper}/BLOG/REPOSTERIA-DULCE-%C3%B1")).await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(
            location(&response),
            format!("/{s}/blog/reposteria-dulce-%C3%B1")
        );
    });
}

/// **The finding (K-alias-language).** Under a language prefix, another
/// language's alias of a page that has its own alias in that language is
/// redirected to it; the page's own alias is served; a page with no alias in
/// that language is still served under the prefix by the alias it has.
#[test]
fn another_languages_alias_redirects_to_the_pages_own() {
    run_test(async {
        let app = shared_app().await;
        ensure_spanish(app).await;
        let s = stamp();
        alias(app, &format!("/{s}/about"), "en").await;
        alias(app, &format!("/{s}/acerca"), "es").await;

        let response = get(app, &format!("/es/{s}/about")).await;
        assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(location(&response), format!("/es/{s}/acerca"));

        let response = get(app, &format!("/es/{s}/acerca")).await;
        assert_eq!(response.status(), StatusCode::OK, "its own alias");
        let response = get(app, &format!("/{s}/about")).await;
        assert_eq!(response.status(), StatusCode::OK, "the English alias");

        // A page with only an English alias is read in Spanish under it.
        let t = stamp();
        sqlx::query(
            "INSERT INTO url_alias (id, source, alias, language, stage_id, created, changed) \
             VALUES ($1, '/search', $2, 'en', $3, 0, 0)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(format!("/{t}/forgot"))
        .bind(LIVE_STAGE_ID)
        .execute(&app.db)
        .await
        .unwrap();
        let db = app.db.clone();
        let forgot = format!("/{t}/forgot");
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM url_alias WHERE alias = $1")
                .bind(&forgot)
                .execute(&db)
                .await;
        });
        let response = get(app, &format!("/es/{t}/forgot")).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "no Spanish alias: served"
        );
    });
}

/// The lookup is one query: an aliased page runs exactly one alias statement.
#[test]
fn an_aliased_page_runs_one_alias_query() {
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::layer::SubscriberExt;

    run_test(async {
        let app = shared_app().await;
        let s = stamp();
        alias(app, &format!("/{s}/one-query-ñ"), "en").await;

        common::scoped_subscribers_see_every_callsite();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(Statements(seen.clone()));
        let response = get(app, &format!("/{s}/one-query-%C3%B1"))
            .with_subscriber(subscriber)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let statements = seen.lock().unwrap().clone();
        assert!(!statements.is_empty(), "the counter sees statements");
        let alias_lookups: Vec<&String> = statements
            .iter()
            .filter(|sql| sql.contains("FROM url_alias a"))
            .collect();
        assert_eq!(alias_lookups.len(), 1, "{alias_lookups:#?}");
    });
}

/// Records every statement sqlx runs, from its `sqlx::query` events.
struct Statements(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct StatementText(String);

impl tracing::field::Visit for StatementText {
    fn record_debug(&mut self, _field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{value:?} "));
    }

    fn record_str(&mut self, _field: &tracing::field::Field, value: &str) {
        self.0.push_str(value);
        self.0.push(' ');
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Statements {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() == "sqlx::query" {
            let mut text = StatementText(String::new());
            event.record(&mut text);
            self.0.lock().unwrap().push(text.0);
        }
    }
}

/// **The finding (K-pathauto-terms).** A tag created without a slug gets one
/// from its name, unique in its category, so its term page has an address at
/// once; a slug given is kept.
#[test]
fn a_new_tag_gets_a_slug_from_its_name() {
    use trovato_kernel::models::{Category, CreateCategory, CreateTag};

    run_test(async {
        let app = shared_app().await;
        let id = format!("b161c{}", &uuid::Uuid::now_v7().simple().to_string()[24..]);
        Category::create(
            &app.db,
            CreateCategory {
                id: id.clone(),
                label: "B161 tags".into(),
                description: None,
                hierarchy: None,
                weight: None,
            },
        )
        .await
        .expect("category");
        let db = app.db.clone();
        let cleanup_id = id.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM category WHERE id = $1")
                .bind(&cleanup_id)
                .execute(&db)
                .await;
        });

        let tag = |label: &str, slug: Option<&str>| CreateTag {
            category_id: id.clone(),
            label: label.to_string(),
            description: None,
            slug: slug.map(str::to_string),
            weight: None,
            parent_ids: None,
        };
        let first = app
            .state
            .categories()
            .create_tag(tag("Pv2 Term", None))
            .await
            .unwrap();
        assert_eq!(first.slug.as_deref(), Some("pv2-term"));
        let second = app
            .state
            .categories()
            .create_tag(tag("Pv2 term!", None))
            .await
            .unwrap();
        assert_eq!(
            second.slug.as_deref(),
            Some("pv2-term-1"),
            "unique in the category"
        );
        let given = app
            .state
            .categories()
            .create_tag(tag("Other", Some("chosen")))
            .await
            .unwrap();
        assert_eq!(
            given.slug.as_deref(),
            Some("chosen"),
            "a given slug is kept"
        );
        let nothing = app
            .state
            .categories()
            .create_tag(tag("¡¿", None))
            .await
            .unwrap();
        assert_eq!(nothing.slug, None, "nothing to slugify: no slug");
    });
}
