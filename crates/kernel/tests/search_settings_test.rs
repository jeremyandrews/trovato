#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The search settings screen, and the backends that read what it saves.
//!
//! Ranking used to be a JSON literal in `templates/search.html`, the AI stage
//! switches existed only in the browser, and server search knew nothing of type
//! boosts. These tests go through the real router with a real session and CSRF
//! token: the screen at `/admin/config/search` saves one settings model, and the
//! search page, the AI endpoints and `SearchService::search` all read it.
//!
//! The settings are site wide, so this file serializes its own tests and puts the
//! defaults back when each one ends, whether it passed or not.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, extract_cookies, run_test, shared_app};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateItem, SiteConfig};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// Search settings are site wide, so this file serializes its own tests.
static SETTINGS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The `site_config` key the settings live under, and the cache key they are
/// read through.
const SETTINGS_KEY: &str = "search";
const SETTINGS_CACHE_KEY: &str = "search_settings";

/// Put the defaults back when the running test ends, pass or fail: the key is
/// removed and the cached copy dropped, so the next reader sees the defaults.
fn restore_defaults_on_exit(app: &'static TestApp) {
    defer_cleanup(move || async move {
        let _ = SiteConfig::delete(&app.db, SETTINGS_KEY).await;
        app.state.cache().invalidate(SETTINGS_CACHE_KEY).await;
    });
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Pull the CSRF token out of a rendered form.
fn csrf_from(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .expect("a form on this page must carry a CSRF token");
    let end = html[start..].find('"').expect("token must be terminated");
    html[start..start + end].to_string()
}

async fn admin_session(app: &TestApp) -> String {
    let name = common::username("srchadm");
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

async fn get(app: &TestApp, cookies: &str, path: &str) -> axum::response::Response {
    app.request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await
}

/// Save the settings form as an administrator: GET the screen for its token,
/// then POST `fields` with it. Returns the POST's status.
async fn save_settings(app: &TestApp, fields: &[(&str, &str)]) -> StatusCode {
    let cookies = admin_session(app).await;
    let response = get(app, &cookies, "/admin/config/search").await;
    let refreshed = extract_cookies(&response);
    let cookies = if refreshed.is_empty() {
        cookies
    } else {
        refreshed
    };
    let status = response.status();
    let html = body_text(response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the settings screen must render for an administrator: {html}"
    );
    let token = csrf_from(&html);

    let mut body = format!("_token={}", encode(&token));
    for (key, value) in fields {
        body.push_str(&format!("&{}={}", encode(key), encode(value)));
    }
    let response = app
        .request_with_cookies(
            Request::post("/admin/config/search")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
            &cookies,
        )
        .await;
    let status = response.status();
    let html = body_text(response).await;
    assert!(
        status.is_redirection(),
        "a valid settings form must save and redirect, got {status}: {html}"
    );
    status
}

/// The JSON in the search page's `scolta-config` block.
async fn rendered_scolta_config(app: &TestApp) -> serde_json::Value {
    let response = app
        .request(Request::get("/search").body(Body::empty()).unwrap())
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    let open = r#"<script id="scolta-config" type="application/json">"#;
    let start = html
        .find(open)
        .map(|i| i + open.len())
        .expect("the search page must carry its config block");
    let end = html[start..]
        .find("</script>")
        .expect("the config block must be closed");
    serde_json::from_str(html[start..start + end].trim())
        .unwrap_or_else(|e| panic!("the config block must be valid JSON ({e}): {html}"))
}

async fn post_json(app: &TestApp, path: &str, body: serde_json::Value) -> (StatusCode, String) {
    let response = app
        .request(
            Request::post(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    let status = response.status();
    (status, body_text(response).await)
}

/// The three AI stages switched on, so a test that turns one off leaves the
/// other two as they were.
const ALL_STAGES_ON: [(&str, &str); 4] = [
    ("expand", "on"),
    ("summarize", "on"),
    ("followup", "on"),
    ("analytics_enabled", "on"),
];

// =============================================================================
// (a) The screen
// =============================================================================

/// An administrator gets the screen; a signed-in visitor without `administer
/// site` and an anonymous visitor do not.
#[test]
fn the_settings_screen_is_for_site_administrators_only() {
    run_test(async {
        let app = shared_app().await;

        let admin = admin_session(app).await;
        let response = get(app, &admin, "/admin/config/search").await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains(r#"action="/admin/config/search""#),
            "the screen must carry its own form: {html}"
        );

        let (_, plain) = common::user_holding(app, "srchusr", &[]).await;
        let status = get(app, &plain, "/admin/config/search").await.status();
        assert!(
            status == StatusCode::FORBIDDEN
                || status == StatusCode::UNAUTHORIZED
                || status.is_redirection(),
            "a member without `administer site` must be refused, got {status}"
        );

        let status = get(app, "", "/admin/config/search").await.status();
        assert!(
            status == StatusCode::FORBIDDEN
                || status == StatusCode::UNAUTHORIZED
                || status.is_redirection(),
            "an anonymous visitor must be refused, got {status}"
        );
    });
}

/// A POST without the administrator's permission saves nothing.
#[test]
fn saving_requires_the_permission_too() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        let (_, plain) = common::user_holding(app, "srchpost", &[]).await;
        let response = app
            .request_with_cookies(
                Request::post("/admin/config/search")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("_token=x&recency_half_life_days=7"))
                    .unwrap(),
                &plain,
            )
            .await;
        let status = response.status();
        assert!(
            status == StatusCode::FORBIDDEN
                || status == StatusCode::UNAUTHORIZED
                || status.is_redirection(),
            "got {status}"
        );
        assert_eq!(
            SiteConfig::get(&app.db, SETTINGS_KEY).await.unwrap(),
            None,
            "a refused POST must not write the settings"
        );
    });
}

/// Saving the form changes what `/search` renders in its config block.
#[test]
fn saving_the_form_changes_the_search_page_config() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        // Before any save the page carries today's literal values, so an upgraded
        // site ranks exactly as it did.
        let before = rendered_scolta_config(app).await;
        let scoring = &before["scoring"];
        assert_eq!(scoring["RECENCY_BOOST_MAX"], serde_json::json!(0.5));
        assert_eq!(scoring["RECENCY_HALF_LIFE_DAYS"], serde_json::json!(365.0));
        assert_eq!(
            scoring["TITLE_ALL_TERMS_MULTIPLIER"],
            serde_json::json!(1.5)
        );
        assert_eq!(scoring["AI_MAX_FOLLOWUPS"], serde_json::json!(3));
        assert_eq!(scoring["AI_EXPAND_QUERY"], serde_json::json!(true));

        let mut fields: Vec<(&str, &str)> = ALL_STAGES_ON.to_vec();
        fields.extend([
            ("recency_half_life_days", "730"),
            ("title_match_boost", "2.5"),
            ("type_boost[conference]", "1.75"),
            ("priority_keyword", "team"),
            ("priority_path", "/about/team"),
            ("priority_boost", "6"),
            ("max_followups", "2"),
        ]);
        save_settings(app, &fields).await;

        let after = rendered_scolta_config(app).await;
        let scoring = &after["scoring"];
        assert_eq!(scoring["RECENCY_HALF_LIFE_DAYS"], serde_json::json!(730.0));
        assert_eq!(scoring["TITLE_MATCH_BOOST"], serde_json::json!(2.5));
        assert_eq!(scoring["AI_MAX_FOLLOWUPS"], serde_json::json!(2));
        assert_eq!(
            scoring["TYPE_BOOSTS"]["conference"],
            serde_json::json!(1.75),
            "type boosts reach the browser: {after}"
        );
        assert_eq!(
            scoring["PRIORITY_PAGES"]["boost"],
            serde_json::json!(6.0),
            "{after}"
        );
        assert_eq!(
            scoring["PRIORITY_PAGES"]["pages"][0],
            serde_json::json!({"keyword": "team", "path": "/about/team"}),
            "{after}"
        );
    });
}

/// An invalid value re-renders the form with a message and saves nothing.
#[test]
fn an_invalid_value_is_refused_with_a_message() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        let cookies = admin_session(app).await;
        let response = get(app, &cookies, "/admin/config/search").await;
        let refreshed = extract_cookies(&response);
        let cookies = if refreshed.is_empty() {
            cookies
        } else {
            refreshed
        };
        let token = csrf_from(&body_text(response).await);

        let body = format!(
            "_token={}&recency_half_life_days=NaN&priority_keyword=team&priority_path=no-slash",
            encode(&token)
        );
        let response = app
            .request_with_cookies(
                Request::post("/admin/config/search")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "errors re-render the form"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("must start with"),
            "a priority page path without a leading slash is named: {html}"
        );
        assert_eq!(
            SiteConfig::get(&app.db, SETTINGS_KEY).await.unwrap(),
            None,
            "nothing is saved while the form has errors"
        );
    });
}

// =============================================================================
// (b) Stage switches are enforced on the server
// =============================================================================

/// With expansion switched off, `/api/v1/search/expand` is refused before any
/// provider call. With no provider configured the old behaviour was a 503 from
/// the provider lookup; a switched-off stage is a 404 that never gets that far.
#[test]
fn expansion_switched_off_is_refused_before_any_provider_call() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        save_settings(
            app,
            &[
                ("summarize", "on"),
                ("followup", "on"),
                ("analytics_enabled", "on"),
            ],
        )
        .await;

        let (status, body) = post_json(
            app,
            "/api/v1/search/expand",
            serde_json::json!({"query": format!("off {}", Uuid::now_v7().simple())}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a switched-off stage is refused outright: {body}"
        );
        let json: serde_json::Value =
            serde_json::from_str(&body).expect("the refusal is a short JSON error");
        assert!(json["message"].is_string(), "{body}");

        // The browser is told too.
        let config = rendered_scolta_config(app).await;
        assert_eq!(
            config["scoring"]["AI_EXPAND_QUERY"],
            serde_json::json!(false)
        );
    });
}

/// Summary and follow-up honour their switches the same way.
#[test]
fn summary_and_followup_switched_off_are_refused() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        save_settings(app, &[("expand", "on"), ("analytics_enabled", "on")]).await;

        let (status, body) = post_json(
            app,
            "/api/v1/search/summarize",
            serde_json::json!({"query": "q", "context": "some text"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

        let (status, body) = post_json(
            app,
            "/api/v1/search/followup",
            serde_json::json!({"messages": [{"role": "user", "content": "q"}]}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    });
}

/// The follow-up turn cap is enforced on the server: the opening question plus
/// `max_followups` user turns is the most a conversation may hold.
#[test]
fn followup_rejects_a_conversation_over_the_turn_cap() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);

        let mut fields: Vec<(&str, &str)> = ALL_STAGES_ON.to_vec();
        fields.push(("max_followups", "1"));
        save_settings(app, &fields).await;

        let turn =
            |role: &str, content: &str| serde_json::json!({"role": role, "content": content});

        // Opening question, its answer, one follow-up: within the cap.
        let within = serde_json::json!({"messages": [
            turn("user", "first"), turn("assistant", "answer"), turn("user", "second"),
        ]});
        let (status, body) = post_json(app, "/api/v1/search/followup", within).await;
        assert_ne!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_ne!(status, StatusCode::NOT_FOUND, "{body}");

        // A second follow-up is one too many.
        let over = serde_json::json!({"messages": [
            turn("user", "first"), turn("assistant", "answer"),
            turn("user", "second"), turn("assistant", "answer"),
            turn("user", "third"),
        ]});
        let (status, body) = post_json(app, "/api/v1/search/followup", over).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // Counted on `history` too, where the new question is the extra turn.
        let over_history = serde_json::json!({
            "query": "third",
            "history": [
                turn("user", "first"), turn("assistant", "answer"),
                turn("user", "second"), turn("assistant", "answer"),
            ],
        });
        let (status, body) = post_json(app, "/api/v1/search/followup", over_history).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    });
}

// =============================================================================
// (d) Server search applies the type boost
// =============================================================================

async fn make_item(app: &TestApp, item_type: &str, title: &str) -> Uuid {
    let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
    app.state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({})),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("type boost test".to_string()),
            },
            &admin,
        )
        .await
        .expect("create")
        .id
}

async fn ranked_ids(app: &TestApp, term: &str) -> Vec<Uuid> {
    app.state
        .search()
        .search(
            term,
            &trovato_kernel::search::SearchVisibility::public([LIVE_STAGE_ID]),
            10,
            0,
        )
        .await
        .expect("search")
        .results
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// Two matches identical but for their type: the boosted type ranks first, in
/// either direction.
#[test]
fn server_search_orders_equal_matches_by_type_boost() {
    run_test(async {
        let app = shared_app().await;
        common::hold_until_cleanup(SETTINGS.lock().await);
        restore_defaults_on_exit(app);
        app.ensure_conference_type().await;

        let term = format!("zqxboost{}", Uuid::now_v7().simple());
        let title = format!("{term} probe");
        let conference = make_item(app, "conference", &title).await;
        let page = make_item(app, "page", &title).await;

        let mut fields: Vec<(&str, &str)> = ALL_STAGES_ON.to_vec();
        fields.push(("type_boost[conference]", "3"));
        save_settings(app, &fields).await;
        assert_eq!(
            ranked_ids(app, &term).await,
            vec![conference, page],
            "a conference boosted 3x outranks an otherwise equal page"
        );

        let mut fields: Vec<(&str, &str)> = ALL_STAGES_ON.to_vec();
        fields.push(("type_boost[page]", "3"));
        save_settings(app, &fields).await;
        assert_eq!(
            ranked_ids(app, &term).await,
            vec![page, conference],
            "and the other way round when the page is boosted"
        );
    });
}
