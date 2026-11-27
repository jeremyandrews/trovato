#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The 404 log (B306): every address a visitor requested that the site could
//! not find, aggregated per `(path, language)` in `not_found_log`, with a
//! report at `/admin/reports/page-not-found` that turns an entry into a
//! redirect in one click.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use tower::ServiceExt;
use trovato_kernel::middleware::language::ResolvedLanguage;
use trovato_kernel::middleware::not_found_log;
use uuid::Uuid;

/// A tiny router: the 404 log layer wrapping a fallback that always answers
/// 404, exactly as it sits directly outside `check_redirect` in `main.rs`.
/// `ResolvedLanguage` is set on the request directly rather than through
/// `negotiate_language`, which needs a session layer this does not build —
/// `check_redirect`'s own tests (`redirect_query_test.rs`) use the same
/// narrower-than-the-whole-stack shape.
fn router(app: &TestApp) -> Router {
    Router::new()
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(axum::middleware::from_fn_with_state(
            app.state.clone(),
            not_found_log::record_not_found,
        ))
}

fn get(path: &str, language: &str, referrer: Option<&str>) -> Request<Body> {
    let mut builder = Request::get(path);
    if let Some(referrer) = referrer {
        builder = builder.header(header::REFERER, referrer);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ResolvedLanguage(language.to_string()));
    request
}

async fn row(app: &TestApp, path: &str, language: &str) -> Option<(i64, i64, i64, Option<String>)> {
    sqlx::query_as::<_, (i64, i64, i64, Option<String>)>(
        "SELECT hits, first_seen, last_seen, referrer FROM not_found_log \
         WHERE path = $1 AND language = $2",
    )
    .bind(path)
    .bind(language)
    .fetch_optional(&app.db)
    .await
    .unwrap()
}

fn forget(app: &TestApp, path: &str, language: &str) {
    let pool = app.db.clone();
    let path = path.to_string();
    let language = language.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM not_found_log WHERE path = $1 AND language = $2")
            .bind(&path)
            .bind(&language)
            .execute(&pool)
            .await;
    });
}

fn unique_path(prefix: &str) -> String {
    format!("/{prefix}-{}", Uuid::now_v7().simple())
}

/// A single 404 is recorded, keyed on the path and the resolved language, its
/// referrer cut to its own path.
#[test]
fn a_404_is_recorded_with_its_path_language_and_referrer() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = unique_path("b306missing");
        forget(app, &path, "en");

        let response = router
            .oneshot(get(
                &path,
                "en",
                Some("https://example.com/came-from?utm=1#x"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let written = app.state.not_found_log().flush_for_test(&app.db).await;
        assert_eq!(written, 1);

        let (hits, first_seen, last_seen, referrer) = row(app, &path, "en")
            .await
            .expect("a row for the missing path");
        assert_eq!(hits, 1);
        assert_eq!(first_seen, last_seen);
        assert_eq!(referrer.as_deref(), Some("https://example.com/came-from"));
    });
}

/// Two hits to the same path and language coalesce into one row, with hits
/// added rather than a second row.
#[test]
fn repeated_hits_to_the_same_path_coalesce() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = unique_path("b306repeat");
        forget(app, &path, "en");

        for _ in 0..3 {
            let response = router.clone().oneshot(get(&path, "en", None)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        app.state.not_found_log().flush_for_test(&app.db).await;

        let (hits, ..) = row(app, &path, "en").await.expect("a row");
        assert_eq!(hits, 3);
    });
}

/// The same path in two languages is two rows.
#[test]
fn the_same_path_in_two_languages_is_two_rows() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = unique_path("b306lang");
        forget(app, &path, "en");
        forget(app, &path, "it");

        router.clone().oneshot(get(&path, "en", None)).await.unwrap();
        router.clone().oneshot(get(&path, "it", None)).await.unwrap();
        app.state.not_found_log().flush_for_test(&app.db).await;

        assert!(row(app, &path, "en").await.is_some());
        assert!(row(app, &path, "it").await.is_some());
    });
}

/// A system path (`/admin/...`) never gets a row, even when the fallback
/// answers 404 for it.
#[test]
fn a_system_path_is_never_recorded() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = format!("/admin/{}", Uuid::now_v7().simple());
        forget(app, &path, "en");

        router.oneshot(get(&path, "en", None)).await.unwrap();
        app.state.not_found_log().flush_for_test(&app.db).await;

        assert!(row(app, &path, "en").await.is_none());
    });
}

/// A `POST` that 404s is not recorded: only `GET` and `HEAD` are.
#[test]
fn a_post_404_is_never_recorded() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = unique_path("b306post");
        forget(app, &path, "en");

        let mut request = Request::post(&path).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ResolvedLanguage("en".to_string()));
        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        app.state.not_found_log().flush_for_test(&app.db).await;

        assert!(row(app, &path, "en").await.is_none());
    });
}

/// `/admin/reports/page-not-found` lists an entry, most frequent first, and
/// refuses a viewer without permission.
#[test]
fn the_report_lists_entries_and_is_gated() {
    run_test(async {
        let app = shared_app().await;
        let router = router(app);
        let path = unique_path("b306report");
        forget(app, &path, "en");
        router.oneshot(get(&path, "en", None)).await.unwrap();
        app.state.not_found_log().flush_for_test(&app.db).await;

        let (_, cookies) =
            user_holding(app, "b306reportviewer", &["access site reports"]).await;
        let response = app
            .request_with_cookies(
                Request::get("/admin/reports/page-not-found")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains(&path), "the report lists the path: {html}");

        let (_, no_perm_cookies) = user_holding(app, "b306reportdenied", &[]).await;
        let refused = app
            .request_with_cookies(
                Request::get("/admin/reports/page-not-found")
                    .body(Body::empty())
                    .unwrap(),
                &no_perm_cookies,
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    });
}

/// The settings form round-trips: saved values are read back on the next GET,
/// and only `administer site` may open it.
#[test]
fn settings_round_trip_and_are_gated() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b306settingsadmin", &["administer site"]).await;

        let get_response = app
            .request_with_cookies(
                Request::get("/admin/reports/page-not-found/settings")
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(get_response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(get_response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&bytes).replace("&#x2F;", "/");
        let token = extract_value(&html, "_token");
        let form_build_id = extract_value(&html, "_form_build_id");

        let body = format!(
            "_token={}&_form_build_id={}&enabled=1&retention_days=45&max_rows=250&ignore=foo%0Abar&op=Save",
            urlencoding::encode(&token),
            urlencoding::encode(&form_build_id),
        );
        let post_response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri("/admin/reports/page-not-found/settings")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(post_response.status().is_redirection(), "{}", post_response.status());

        let settings = app.state.not_found_log().settings();
        assert_eq!(settings.retention_days, 45);
        assert_eq!(settings.max_rows, 250);
        assert_eq!(settings.ignore, vec!["foo".to_string(), "bar".to_string()]);
        assert!(settings.enabled);

        // Restore the default settings so other tests in this binary are not
        // affected by this one's save.
        trovato_kernel::models::SiteConfig::set(
            &app.db,
            not_found_log::SETTINGS_KEY,
            not_found_log::NotFoundLogSettings::default().to_value(),
        )
        .await
        .unwrap();
        app.state.not_found_log().reload_settings(&app.db).await;

        let (_, no_perm_cookies) = user_holding(app, "b306settingsdenied", &[]).await;
        let refused = app
            .request_with_cookies(
                Request::get("/admin/reports/page-not-found/settings")
                    .body(Body::empty())
                    .unwrap(),
                &no_perm_cookies,
            )
            .await;
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    });
}

fn extract_value(html: &str, name: &str) -> String {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle).unwrap_or_else(|| panic!("a {name} field: {html}"));
    let start = html[..at].rfind('<').unwrap();
    let end = at + html[at..].find('>').unwrap();
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#).unwrap() + 7;
    let vend = tag[vat..].find('"').unwrap() + vat;
    tag[vat..vend].to_string()
}

/// Creating a redirect for a path clears any 404 log entry for it.
#[test]
fn creating_a_redirect_clears_the_matching_404_entry() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_redirects");
        let path = unique_path("b306redirectclear");
        forget(app, &path, "en");

        not_found_log::insert_batch(
            &app.db,
            &not_found_log::coalesce(&[not_found_log::NotFoundHit {
                path: path.clone(),
                language: "en".to_string(),
                referrer: None,
                when: chrono::Utc::now().timestamp(),
            }]),
        )
        .await
        .unwrap();
        assert!(row(app, &path, "en").await.is_some());

        let destination = unique_path("b306redirectdest");
        let redirect = app
            .state
            .redirects()
            .create(&path, &destination, 301, "en")
            .await
            .expect("create the redirect");
        common::defer_cleanup({
            let pool = app.db.clone();
            move || async move {
                let _ = sqlx::query("DELETE FROM redirect WHERE id = $1")
                    .bind(redirect.id)
                    .execute(&pool)
                    .await;
            }
        });

        assert!(
            row(app, &path, "en").await.is_none(),
            "the 404 entry is cleared once a redirect exists for it"
        );
    });
}

/// `clear_resolved` removes an entry that a Live URL alias or an existing
/// item's `/item/<uuid>` now answers for.
#[test]
fn clear_resolved_removes_entries_a_live_alias_or_item_now_answers_for() {
    run_test(async {
        let app = shared_app().await;

        let alias_path = unique_path("b306aliasclear");
        forget(app, &alias_path, "en");
        not_found_log::insert_batch(
            &app.db,
            &not_found_log::coalesce(&[not_found_log::NotFoundHit {
                path: alias_path.clone(),
                language: "en".to_string(),
                referrer: None,
                when: chrono::Utc::now().timestamp(),
            }]),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO url_alias (source, alias, language, stage_id, created) \
             VALUES ('/item/does-not-matter', $1, 'en', 'live', 0)",
        )
        .bind(&alias_path)
        .execute(&app.db)
        .await
        .unwrap();
        common::defer_cleanup({
            let pool = app.db.clone();
            let alias_path = alias_path.clone();
            move || async move {
                let _ = sqlx::query("DELETE FROM url_alias WHERE alias = $1")
                    .bind(&alias_path)
                    .execute(&pool)
                    .await;
            }
        });

        let removed = not_found_log::clear_resolved(&app.db, false).await.unwrap();
        assert!(removed >= 1);
        assert!(row(app, &alias_path, "en").await.is_none());
    });
}

/// `prune` removes rows past the retention window and all but the highest by
/// hits past the row cap.
#[test]
fn prune_keeps_the_smaller_of_its_day_and_row_caps() {
    run_test(async {
        let app = shared_app().await;
        let old_path = unique_path("b306pruneold");
        let new_path = unique_path("b306prunenew");
        forget(app, &old_path, "en");
        forget(app, &new_path, "en");

        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO not_found_log (path, language, hits, first_seen, last_seen) \
             VALUES ($1, 'en', 1, $2, $2)",
        )
        .bind(&old_path)
        .bind(now - 200 * 86_400)
        .execute(&app.db)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO not_found_log (path, language, hits, first_seen, last_seen) \
             VALUES ($1, 'en', 1, $2, $2)",
        )
        .bind(&new_path)
        .bind(now)
        .execute(&app.db)
        .await
        .unwrap();

        not_found_log::prune(&app.db, 90, 1_000_000).await.unwrap();
        assert!(row(app, &old_path, "en").await.is_none(), "older than retention");
        assert!(row(app, &new_path, "en").await.is_some(), "kept");
    });
}
