#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A tile's `audience` visibility rule: anonymous-only or signed-in-only (B174).
//!
//! The classic case is a login tile: a site used to need a template check
//! (`{% if not viewer.authenticated %}`) to hide it once a visitor signed in,
//! because `{"roles": ["anonymous user"]}` matches every viewer — signed in or
//! not, everyone carries that role in the list tile rendering is matched
//! against. `audience` is decided from the viewer's actual signed-in state
//! instead, so the rule means what it says.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, user_holding};
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::tile::{CreateTile, Tile, UpdateTile};
use uuid::Uuid;

fn stamp() -> String {
    format!("m{}", Uuid::now_v7().simple())
}

/// A `custom_html` tile in the sidebar with the given `visibility`, whose body
/// is its own machine name (so a test can find it by text). Removed when the
/// test ends.
async fn tile(app: &TestApp, machine_name: &str, visibility: serde_json::Value) -> Uuid {
    let tile = Tile::create(
        &app.db,
        CreateTile {
            machine_name: machine_name.to_string(),
            label: format!("Tile {machine_name}"),
            region: Some("sidebar".to_string()),
            tile_type: Some("custom_html".to_string()),
            config: Some(serde_json::json!({
                "body": format!("marker {machine_name}"),
                "format": "plain_text",
            })),
            visibility: Some(visibility),
            weight: Some(0),
            status: Some(1),
            plugin: None,
            stage_id: Some(LIVE_STAGE_ID),
        },
    )
    .await
    .expect("create tile");
    let pool = app.db.clone();
    let id = tile.id;
    defer_cleanup(move || async move {
        let _ = Tile::delete(&pool, id).await;
    });
    id
}

async fn get(app: &TestApp, path: &str, cookies: &str) -> String {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    assert_eq!(response.status(), StatusCode::OK, "GET {path}");
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

#[test]
fn an_anonymous_only_tile_shows_for_an_anonymous_request_and_not_for_a_signed_in_one() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let machine = format!("b174_anon_{marker}");
        tile(
            app,
            &machine,
            serde_json::json!({ "audience": "anonymous" }),
        )
        .await;

        let anon_html = get(app, "/", "").await;
        assert!(
            anon_html.contains(&format!("marker {machine}")),
            "an anonymous-only tile must render for an anonymous visitor:\n{anon_html}"
        );

        let (_, cookies) = user_holding(app, "b174anon", &[]).await;
        let signed_in_html = get(app, "/", &cookies).await;
        assert!(
            !signed_in_html.contains(&format!("marker {machine}")),
            "an anonymous-only tile must not render for a signed-in viewer:\n{signed_in_html}"
        );
    });
}

#[test]
fn a_signed_in_only_tile_shows_for_a_signed_in_request_and_not_for_an_anonymous_one() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let machine = format!("b174_auth_{marker}");
        tile(
            app,
            &machine,
            serde_json::json!({ "audience": "authenticated" }),
        )
        .await;

        let anon_html = get(app, "/", "").await;
        assert!(
            !anon_html.contains(&format!("marker {machine}")),
            "a signed-in-only tile must not render for an anonymous visitor:\n{anon_html}"
        );

        let (_, cookies) = user_holding(app, "b174auth", &[]).await;
        let signed_in_html = get(app, "/", &cookies).await;
        assert!(
            signed_in_html.contains(&format!("marker {machine}")),
            "a signed-in-only tile must render for a signed-in viewer:\n{signed_in_html}"
        );
    });
}

#[test]
fn a_tile_with_no_audience_rule_shows_to_everyone() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let machine = format!("b174_everyone_{marker}");
        tile(app, &machine, serde_json::json!({})).await;

        let anon_html = get(app, "/", "").await;
        assert!(
            anon_html.contains(&format!("marker {machine}")),
            "{anon_html}"
        );

        let (_, cookies) = user_holding(app, "b174every", &[]).await;
        let signed_in_html = get(app, "/", &cookies).await;
        assert!(
            signed_in_html.contains(&format!("marker {machine}")),
            "{signed_in_html}"
        );
    });
}

#[test]
fn an_audience_rule_combines_with_an_existing_roles_rule() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let machine = format!("b174_combo_{marker}");
        // Administrator-only *and* anonymous-only: nobody should ever see it,
        // since no viewer is both. Proves the two rules are ANDed, not ORed.
        tile(
            app,
            &machine,
            serde_json::json!({ "audience": "anonymous", "roles": ["administrator"] }),
        )
        .await;

        let anon_html = get(app, "/", "").await;
        assert!(
            !anon_html.contains(&format!("marker {machine}")),
            "{anon_html}"
        );

        let admin_cookies = {
            let name = format!("b174combo{}", stamp());
            app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
                .await;
            app.login(&name, "test-password-123").await
        };
        let admin_html = get(app, "/", &admin_cookies).await;
        assert!(
            !admin_html.contains(&format!("marker {machine}")),
            "an administrator is signed in, so the anonymous-only rule still excludes them:\n{admin_html}"
        );
    });
}

// -----------------------------------------------------------------------------
// The tile admin form's "Show to" control.
// -----------------------------------------------------------------------------

async fn admin_session(app: &TestApp) -> String {
    let name = format!("b174tileadm_{}", stamp());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

fn csrf_from(html: &str) -> String {
    let needle = r#"name="_token" value=""#;
    let start = html
        .find(needle)
        .map(|i| i + needle.len())
        .expect("a CSRF token");
    let end = html[start..].find('"').expect("token terminated");
    html[start..start + end].to_string()
}

/// Submit the add-tile form for a `custom_html` tile, `audience` as chosen
/// ("", "anonymous" or "authenticated").
async fn submit_tile_form(
    app: &TestApp,
    cookies: &str,
    machine_name: &str,
    audience: &str,
) -> axum::response::Response {
    let form_page = app
        .request_with_cookies(
            Request::get("/admin/structure/tiles/add")
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    assert_eq!(form_page.status(), StatusCode::OK);
    let token = csrf_from(&body_text(form_page).await);
    let fields = [
        ("_token", token.as_str()),
        ("machine_name", machine_name),
        ("label", "Audience form tile"),
        ("region", "sidebar"),
        ("tile_type", "custom_html"),
        ("body", "body"),
        ("format", "plain_text"),
        ("weight", "0"),
        ("audience", audience),
    ];
    let body = fields
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post("/admin/structure/tiles/add")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn forget_tile(app: &TestApp, machine_name: &str) {
    let pool = app.db.clone();
    let machine_name = machine_name.to_string();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM tile WHERE machine_name = $1")
            .bind(machine_name)
            .execute(&pool)
            .await;
    });
}

#[test]
fn the_tile_form_stores_and_redisplays_the_chosen_audience() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        for audience in ["anonymous", "authenticated"] {
            let machine = format!("b174_form_{audience}_{}", stamp());
            forget_tile(app, &machine);
            let response = submit_tile_form(app, &cookies, &machine, audience).await;
            assert_eq!(
                response.status(),
                StatusCode::SEE_OTHER,
                "audience={audience}"
            );

            let visibility: serde_json::Value =
                sqlx::query_scalar("SELECT visibility FROM tile WHERE machine_name = $1")
                    .bind(&machine)
                    .fetch_one(&app.db)
                    .await
                    .expect("tile saved");
            assert_eq!(visibility["audience"], serde_json::json!(audience));

            let id: Uuid = sqlx::query_scalar("SELECT id FROM tile WHERE machine_name = $1")
                .bind(&machine)
                .fetch_one(&app.db)
                .await
                .unwrap();
            let edit = app
                .request_with_cookies(
                    Request::get(format!("/admin/structure/tiles/{id}/edit"))
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            let html = body_text(edit).await;
            let option = format!(r#"value="{audience}" selected"#);
            assert!(html.contains(&option), "{html}");
        }

        // "Everyone" (blank) clears any stored `audience`.
        let machine = format!("b174_form_everyone_{}", stamp());
        forget_tile(app, &machine);
        let response = submit_tile_form(app, &cookies, &machine, "").await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let visibility: serde_json::Value =
            sqlx::query_scalar("SELECT visibility FROM tile WHERE machine_name = $1")
                .bind(&machine)
                .fetch_one(&app.db)
                .await
                .expect("tile saved");
        assert!(visibility.get("audience").is_none(), "{visibility}");
    });
}

#[test]
fn the_tile_form_refuses_an_unknown_audience_value() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let machine = format!("b174_form_bad_{}", stamp());
        forget_tile(app, &machine);

        let response = submit_tile_form(app, &cookies, &machine, "everyone-but-tuesdays").await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an error page, not a redirect"
        );

        let stored: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM tile WHERE machine_name = $1")
                .bind(&machine)
                .fetch_optional(&app.db)
                .await
                .expect("query");
        assert!(stored.is_none(), "an invalid audience must write nothing");
    });
}

/// Editing a tile's audience through the form keeps a `paths` rule the tile
/// already carried (set here as a config import would set it): the admin
/// form's "Show to" control merges into `visibility`, it does not replace it.
#[test]
fn editing_the_audience_preserves_an_existing_paths_rule() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let machine = format!("b174_form_merge_{}", stamp());
        let id = tile(
            app,
            &machine,
            serde_json::json!({ "paths": ["/conferences*"] }),
        )
        .await;

        let edit_page = app
            .request_with_cookies(
                Request::get(format!("/admin/structure/tiles/{id}/edit"))
                    .body(Body::empty())
                    .unwrap(),
                &cookies,
            )
            .await;
        let token = csrf_from(&body_text(edit_page).await);
        let fields = [
            ("_token", token.as_str()),
            ("label", "Audience form tile"),
            ("region", "sidebar"),
            ("tile_type", "custom_html"),
            ("body", "body"),
            ("format", "plain_text"),
            ("weight", "0"),
            ("audience", "authenticated"),
        ];
        let body = fields
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let response = app
            .request_with_cookies(
                Request::post(format!("/admin/structure/tiles/{id}/edit"))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let visibility: serde_json::Value =
            sqlx::query_scalar("SELECT visibility FROM tile WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .expect("tile saved");
        assert_eq!(visibility["audience"], serde_json::json!("authenticated"));
        assert_eq!(visibility["paths"], serde_json::json!(["/conferences*"]));
    });
}

/// `Tile::update` can set `visibility` directly (what the admin form's
/// merge-in-place does under the hood): an existing `roles` rule survives an
/// update that only changes `audience`.
#[test]
fn updating_a_tiles_audience_preserves_its_other_visibility_rules() {
    run_test(async {
        let app = shared_app().await;
        let marker = stamp();
        let machine = format!("b174_update_{marker}");
        let id = tile(
            app,
            &machine,
            serde_json::json!({ "paths": ["/conferences*"] }),
        )
        .await;

        let mut existing: serde_json::Value =
            sqlx::query_scalar("SELECT visibility FROM tile WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .expect("read tile");
        existing["audience"] = serde_json::json!("authenticated");
        Tile::update(
            &app.db,
            id,
            UpdateTile {
                label: None,
                region: None,
                tile_type: None,
                config: None,
                visibility: Some(existing),
                weight: None,
                status: None,
                plugin: None,
                stage_id: None,
            },
        )
        .await
        .expect("update tile")
        .expect("tile exists");

        let stored: serde_json::Value =
            sqlx::query_scalar("SELECT visibility FROM tile WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .expect("read tile");
        assert_eq!(stored["audience"], serde_json::json!("authenticated"));
        assert_eq!(stored["paths"], serde_json::json!(["/conferences*"]));
    });
}
