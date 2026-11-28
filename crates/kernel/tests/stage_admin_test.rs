#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Route-level tests for the editorial stage administration screens.
//!
//! Stages were configuration-import only: `KNOWN-ISSUES.md` and `ROADMAP.md` both
//! listed the missing form as pre-1.0 work. Everything here goes through a real
//! request against the real router with a real session and CSRF token, because the
//! guard rails under test are only partly in the handlers — most of them are on
//! `Stage::update` and `Stage::delete`, and what the route contributes is turning a
//! refusal into a sentence.
//!
//! These tests write to a **shared** fixture database, so every stage they create
//! carries a UUID-derived machine name and is deleted at the end. Stage names are
//! globally unique (`stage_config.machine_name` is `UNIQUE`), so a fixed name would
//! pass once and fail on the second run.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, extract_cookies, run_test, shared_app};
use uuid::Uuid;

/// The Live stage's fixed UUID.
const LIVE: &str = "0193a5a0-0000-7000-8000-000000000001";

async fn admin_session(app: &TestApp) -> String {
    let name = format!("stageadm_{}", Uuid::now_v7().simple());
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

/// The id of the add and edit form.
const STAGE_FORM: &str = "stage_form";

/// The id of the form that confirms a stage's deletion.
const DELETE_FORM: &str = "stage_delete_form";

/// The CSRF token and the build id of the pipeline form `form_id` on a
/// rendered page: what a browser posts back with the form's fields.
fn pipeline_fields(html: &str, form_id: &str) -> (String, String) {
    let form = common::form_in(html, form_id)
        .unwrap_or_else(|| panic!("the page must carry the form {form_id}"));
    let token = common::input_value_in(form, "_token").expect("the form carries a CSRF token");
    (token, common::form_build_id(html, form_id))
}

async fn get_page(app: &TestApp, cookies: &str, path: &str) -> (String, String) {
    let response = app
        .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    let refreshed = extract_cookies(&response);
    let cookies = if refreshed.is_empty() {
        cookies.to_string()
    } else {
        refreshed
    };
    let body = body_text(response).await;
    assert_eq!(status, StatusCode::OK, "GET {path} should render: {body}");
    (cookies, body)
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

async fn post_form(
    app: &TestApp,
    cookies: &str,
    path: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    app.request_with_cookies(
        Request::post(path)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

/// The cookies, CSRF token and build id of the delete confirmation for the
/// stage `id`, from the page the row's Delete link opens.
async fn delete_fields(app: &TestApp, cookies: &str, id: Uuid) -> (String, String, String) {
    let (cookies, html) = get_page(
        app,
        cookies,
        &format!("/admin/structure/stages/{id}/delete"),
    )
    .await;
    let (token, build) = pipeline_fields(&html, DELETE_FORM);
    (cookies, token, build)
}

/// A machine name unique to one test.
fn fresh_name() -> String {
    format!("s{}", Uuid::now_v7().simple())
}

/// Create a stage through the form and return its id.
async fn create_stage(app: &TestApp, cookies: &str, machine_name: &str, label: &str) -> Uuid {
    let (cookies, form) = get_page(app, cookies, "/admin/structure/stages/add").await;
    let (token, build) = pipeline_fields(&form, STAGE_FORM);
    let response = post_form(
        app,
        &cookies,
        "/admin/structure/stages/add",
        &[
            ("_token", &token),
            ("_form_build_id", &build),
            ("machine_name", machine_name),
            ("label", label),
            ("description", ""),
            ("visibility", "internal"),
            ("weight", "0"),
        ],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "creating {machine_name} should redirect"
    );

    sqlx::query_scalar("SELECT tag_id FROM stage_config WHERE machine_name = $1")
        .bind(machine_name)
        .fetch_one(&app.db)
        .await
        .expect("the created stage must be a row")
}

async fn drop_stage(app: &TestApp, id: Uuid) {
    let _ = sqlx::query("DELETE FROM category_tag WHERE id = $1")
        .bind(id)
        .execute(&app.db)
        .await;
}

// =============================================================================
// Access control
// =============================================================================

#[test]
fn a_non_admin_is_refused_every_stage_screen() {
    run_test(async {
        let app = shared_app().await;
        let name = format!("stageusr_{}", Uuid::now_v7().simple());
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let cookies = app.login(&name, "test-password-123").await;

        for path in [
            "/admin/structure/stages",
            "/admin/structure/stages/add",
            &format!("/admin/structure/stages/{LIVE}/edit"),
        ] {
            let response = app
                .request_with_cookies(Request::get(path).body(Body::empty()).unwrap(), &cookies)
                .await;
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{path} must be forbidden to a non-admin"
            );
        }
    });
}

#[test]
fn a_write_without_a_valid_csrf_token_is_rejected() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();

        let response = post_form(
            app,
            &cookies,
            "/admin/structure/stages/add",
            &[
                ("_token", "not-a-valid-token"),
                ("machine_name", &name),
                ("label", "Should not exist"),
                ("description", ""),
                ("visibility", "internal"),
                ("weight", "0"),
            ],
        )
        .await;

        assert_ne!(response.status(), StatusCode::SEE_OTHER);

        let landed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE machine_name = $1")
                .bind(&name)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(landed, 0, "a CSRF-rejected write must write nothing");
    });
}

// =============================================================================
// Create and edit
// =============================================================================

#[test]
fn a_stage_is_created_and_listed() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();

        let id = create_stage(app, &cookies, &name, "Created Stage").await;

        let (machine_name, visibility, is_default): (String, String, bool) = sqlx::query_as(
            "SELECT machine_name, visibility, is_default FROM stage_config WHERE tag_id = $1",
        )
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(machine_name, name);
        assert_eq!(visibility, "internal");
        assert!(!is_default, "a new stage is not the default unless asked");

        let label: String = sqlx::query_scalar("SELECT label FROM category_tag WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(label, "Created Stage");

        let (_, html) = get_page(app, &cookies, "/admin/structure/stages").await;
        assert!(html.contains(&name), "the new stage must be listed");
        assert!(html.contains("Created Stage"));

        drop_stage(app, id).await;
    });
}

#[test]
fn editing_a_stage_changes_its_label_visibility_and_weight() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();
        let id = create_stage(app, &cookies, &name, "Before").await;

        let edit = format!("/admin/structure/stages/{id}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let (token, build) = pipeline_fields(&form, STAGE_FORM);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", &name),
                ("label", "After"),
                ("description", "A described stage"),
                ("visibility", "accessible"),
                ("weight", "7"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let (label, description, weight): (String, Option<String>, i16) =
            sqlx::query_as("SELECT label, description, weight FROM category_tag WHERE id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(label, "After");
        assert_eq!(description.as_deref(), Some("A described stage"));
        assert_eq!(weight, 7);

        let visibility: String =
            sqlx::query_scalar("SELECT visibility FROM stage_config WHERE tag_id = $1")
                .bind(id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(visibility, "accessible");

        drop_stage(app, id).await;
    });
}

/// The add form is a pipeline form: a submission without its build id is
/// refused as expired, and a refused field is marked and explained beside it.
#[test]
fn the_stage_form_is_a_pipeline_form_and_marks_the_field_it_refuses() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();
        let add = "/admin/structure/stages/add";

        let (cookies, form) = get_page(app, &cookies, add).await;
        assert!(
            common::form_in(&form, STAGE_FORM).is_some(),
            "the add form must be built by the pipeline"
        );
        let (token, build) = pipeline_fields(&form, STAGE_FORM);

        let response = post_form(
            app,
            &cookies,
            add,
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", &name),
                ("label", ""),
                ("visibility", "internal"),
                ("weight", "0"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        let form = common::form_in(&html, STAGE_FORM).expect("the form is shown again");
        assert!(
            form.contains(r#"id="label-error""#)
                && form.contains(r#"aria-invalid="true""#)
                && form.contains("Label is required."),
            "the refusal must be on the label field, got: {form}"
        );

        // Without its build id a submission is refused as expired. The token
        // is the one the form was shown again with: each is used once.
        let (token, _) = pipeline_fields(&html, STAGE_FORM);
        let response = post_form(
            app,
            &cookies,
            add,
            &[
                ("_token", &token),
                ("machine_name", &name),
                ("label", "No build id"),
                ("visibility", "internal"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_text(response).await.contains("expired"));

        let landed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE machine_name = $1")
                .bind(&name)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(landed, 0, "no refused stage may have been written");
    });
}

/// A machine name is a machine name, and a duplicate is refused by name.
#[test]
fn an_invalid_or_duplicate_machine_name_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let taken = fresh_name();
        let id = create_stage(app, &cookies, &taken, "Taken").await;

        for (machine_name, expected) in [
            ("Has Spaces", "Machine name must start with a letter"),
            ("9leading", "Machine name must start with a letter"),
            ("", "required"),
            (taken.as_str(), "already exists"),
        ] {
            let (fresh_cookies, form) =
                get_page(app, &cookies, "/admin/structure/stages/add").await;
            let (token, build) = pipeline_fields(&form, STAGE_FORM);
            let response = post_form(
                app,
                &fresh_cookies,
                "/admin/structure/stages/add",
                &[
                    ("_token", &token),
                    ("_form_build_id", &build),
                    ("machine_name", machine_name),
                    ("label", "Rejected"),
                    ("description", ""),
                    ("visibility", "internal"),
                    ("weight", "0"),
                ],
            )
            .await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{machine_name:?} should re-render the form"
            );
            let html = body_text(response).await;
            assert!(
                html.contains(expected),
                "{machine_name:?} should be refused with {expected:?}, got: {html}"
            );
        }

        drop_stage(app, id).await;
    });
}

// =============================================================================
// Guard rails
// =============================================================================

/// Exactly one stage is the default: making a new one default clears the old.
#[test]
fn making_a_stage_default_clears_the_previous_default() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let previous: Uuid =
            sqlx::query_scalar("SELECT tag_id FROM stage_config WHERE is_default = true")
                .fetch_one(&app.db)
                .await
                .expect("some stage must be the default");

        let name = fresh_name();
        let id = create_stage(app, &cookies, &name, "New Default").await;

        let edit = format!("/admin/structure/stages/{id}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let (token, build) = pipeline_fields(&form, STAGE_FORM);
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", &name),
                ("label", "New Default"),
                ("description", ""),
                ("visibility", "internal"),
                ("is_default", "1"),
                ("weight", "0"),
            ],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let defaults: Vec<Uuid> =
            sqlx::query_scalar("SELECT tag_id FROM stage_config WHERE is_default = true")
                .fetch_all(&app.db)
                .await
                .unwrap();
        assert_eq!(
            defaults,
            vec![id],
            "exactly one stage is the default, and it is the new one"
        );

        // Put it back before dropping the stage, or the site has no default.
        sqlx::query("UPDATE stage_config SET is_default = false WHERE tag_id = $1")
            .bind(id)
            .execute(&app.db)
            .await
            .unwrap();
        sqlx::query("UPDATE stage_config SET is_default = true WHERE tag_id = $1")
            .bind(previous)
            .execute(&app.db)
            .await
            .unwrap();
        drop_stage(app, id).await;
    });
}

/// The default cannot simply be cleared: new content has to land somewhere.
#[test]
fn clearing_the_only_default_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;

        let default_id: Uuid =
            sqlx::query_scalar("SELECT tag_id FROM stage_config WHERE is_default = true")
                .fetch_one(&app.db)
                .await
                .expect("some stage must be the default");
        let (machine_name, visibility): (String, String) =
            sqlx::query_as("SELECT machine_name, visibility FROM stage_config WHERE tag_id = $1")
                .bind(default_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        let label: String = sqlx::query_scalar("SELECT label FROM category_tag WHERE id = $1")
            .bind(default_id)
            .fetch_one(&app.db)
            .await
            .unwrap();

        let edit = format!("/admin/structure/stages/{default_id}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        let (token, build) = pipeline_fields(&form, STAGE_FORM);
        // Submitting without the is_default checkbox is how a browser clears it.
        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", &machine_name),
                ("label", &label),
                ("description", ""),
                ("visibility", &visibility),
                ("weight", "0"),
            ],
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a refused edit re-renders the form"
        );
        let html = body_text(response).await;
        assert!(
            html.contains("cannot clear the default stage"),
            "the refusal must say why, got: {html}"
        );

        let still_default: bool =
            sqlx::query_scalar("SELECT is_default FROM stage_config WHERE tag_id = $1")
                .bind(default_id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(still_default, "the default must not have been cleared");
    });
}

/// The Live stage stays public: published content is resolved through it.
#[test]
fn the_live_stage_cannot_be_made_internal() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let live: Uuid = LIVE.parse().unwrap();

        let edit = format!("/admin/structure/stages/{live}/edit");
        let (cookies, form) = get_page(app, &cookies, &edit).await;
        assert!(
            form.contains("This is the Live stage"),
            "the form must say what it is editing"
        );
        let (token, build) = pipeline_fields(&form, STAGE_FORM);

        let response = post_form(
            app,
            &cookies,
            &edit,
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", "live"),
                ("label", "Live"),
                ("description", ""),
                ("visibility", "internal"),
                ("is_default", "1"),
                ("weight", "0"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("must stay public"),
            "the refusal must say why, got: {html}"
        );

        let visibility: String =
            sqlx::query_scalar("SELECT visibility FROM stage_config WHERE tag_id = $1")
                .bind(live)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(visibility, "public", "Live must still be public");
    });
}

/// A second public stage is refused: the schema allows exactly one.
#[test]
fn a_second_public_stage_is_refused() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();

        let (cookies, form) = get_page(app, &cookies, "/admin/structure/stages/add").await;
        let (token, build) = pipeline_fields(&form, STAGE_FORM);
        let response = post_form(
            app,
            &cookies,
            "/admin/structure/stages/add",
            &[
                ("_token", &token),
                ("_form_build_id", &build),
                ("machine_name", &name),
                ("label", "Second Public"),
                ("description", ""),
                ("visibility", "public"),
                ("weight", "0"),
            ],
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let html = body_text(response).await;
        assert!(
            html.contains("Only one stage can be public"),
            "the refusal must say why, got: {html}"
        );

        let landed: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE machine_name = $1")
                .bind(&name)
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert_eq!(landed, 0, "nothing may have been written");
    });
}

// =============================================================================
// Delete
// =============================================================================

/// An empty stage deletes.
#[test]
fn an_unused_stage_is_deleted() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();
        let id = create_stage(app, &cookies, &name, "Disposable").await;

        let (cookies, html) = get_page(app, &cookies, "/admin/structure/stages").await;
        assert!(
            html.contains(&format!(r#"href="/admin/structure/stages/{id}/delete""#)),
            "the row must link to its delete confirmation"
        );
        let (cookies, confirm) = get_page(
            app,
            &cookies,
            &format!("/admin/structure/stages/{id}/delete"),
        )
        .await;
        assert!(
            confirm.contains("Delete the Disposable stage?"),
            "the confirmation must ask the question, got: {confirm}"
        );
        let (cookies, token, build) = delete_fields(app, &cookies, id).await;

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/stages/{id}/delete"),
            &[("_token", &token), ("_form_build_id", &build)],
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM category_tag WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(left, 0, "the stage must be gone");
    });
}

/// The Live stage cannot be deleted, and the listing does not offer it.
#[test]
fn the_live_stage_cannot_be_deleted() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let live: Uuid = LIVE.parse().unwrap();

        let (cookies, html) = get_page(app, &cookies, "/admin/structure/stages").await;
        assert!(
            !html.contains(&format!("/admin/structure/stages/{live}/delete")),
            "the listing must not offer to delete the Live stage"
        );
        // Its confirmation page says why and offers no button.
        let (cookies, confirm) = get_page(
            app,
            &cookies,
            &format!("/admin/structure/stages/{live}/delete"),
        )
        .await;
        assert!(
            confirm.contains("Not deletable") && common::form_in(&confirm, DELETE_FORM).is_none(),
            "the Live stage's confirmation must refuse, not ask, got: {confirm}"
        );

        // The model refuses it even to a valid delete form, built for another
        // stage and posted at Live's address.
        let other = create_stage(app, &cookies, &fresh_name(), "Bystander").await;
        let (cookies, token, build) = delete_fields(app, &cookies, other).await;
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/stages/{live}/delete"),
            &[("_token", &token), ("_form_build_id", &build)],
        )
        .await;
        drop_stage(app, other).await;

        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "deleting Live must be refused"
        );
        let body = body_text(response).await;
        assert!(
            body.contains("live stage") || body.contains("public or default"),
            "the refusal must say which stage it is, got: {body}"
        );

        let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE tag_id = $1")
            .bind(live)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(still, 1, "Live must still be there");
    });
}

/// A stage holding content is refused, and the refusal counts what is in the way
/// — including a menu link, which the count used to miss.
#[test]
fn a_stage_holding_content_is_refused_with_a_count() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_session(app).await;
        let name = fresh_name();
        let id = create_stage(app, &cookies, &name, "Occupied").await;

        // A menu link, not an item: `Stage::delete` counted only items, so a stage
        // holding a link was refused by the foreign key with a message naming a
        // constraint instead of a count.
        let link_id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        sqlx::query(
            "INSERT INTO menu_link (id, menu_name, path, title, weight, hidden, plugin, stage_id, created, changed) \
             VALUES ($1, $2, $3, 'Occupying link', 0, false, 'core', $4, $5, $5)",
        )
        .bind(link_id)
        .bind(format!("m{}", Uuid::now_v7().simple()))
        .bind(format!("/occupied-{}", Uuid::now_v7().simple()))
        .bind(id)
        .bind(now)
        .execute(&app.db)
        .await
        .unwrap();

        let (cookies, html) = get_page(app, &cookies, "/admin/structure/stages").await;
        assert!(
            html.contains("1 menu link"),
            "the listing must say what is in the way, got: {html}"
        );
        let (cookies, confirm) = get_page(
            app,
            &cookies,
            &format!("/admin/structure/stages/{id}/delete"),
        )
        .await;
        assert!(
            confirm.contains("This stage is still used by 1 menu link"),
            "the confirmation must say what is in the way, got: {confirm}"
        );
        let (token, build) = pipeline_fields(&confirm, DELETE_FORM);

        let response = post_form(
            app,
            &cookies,
            &format!("/admin/structure/stages/{id}/delete"),
            &[("_token", &token), ("_form_build_id", &build)],
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_text(response).await;
        assert!(
            body.contains("1 menu link"),
            "the refusal must count what references the stage, got: {body}"
        );

        let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE tag_id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(still, 1, "the stage must survive the refused delete");

        sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(link_id)
            .execute(&app.db)
            .await
            .unwrap();
        drop_stage(app, id).await;
    });
}
