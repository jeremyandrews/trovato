#![allow(clippy::unwrap_used, clippy::expect_used)]
//! "Who can do this?" names a role's members (B252).
//!
//! An administrator sees up to 20 names, each linked to the account, then
//! "and N more" linking to the People listing filtered to the role; a role
//! with no more than 20 members shows all of them and no "more" link.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, defer_cleanup, run_test, shared_app, test_ip_for};
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-staple";
const PATH: &str = "/admin/reports/dead-jobs";

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b252-who-can-names"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A role holding `administer site` (the dead jobs page's own permission),
/// with `count` members named `{tag}-NN`, so alphabetical order is numeric
/// order. Returns the role id and the member names in that order. Torn down
/// when the test ends.
async fn role_with_members(app: &TestApp, tag: &str, count: usize) -> (Uuid, Vec<String>) {
    let role = app
        .state
        .roles()
        .create(&format!("who-can-role-{tag}"))
        .await
        .unwrap();
    app.state
        .roles()
        .add_permission(role.id, "administer site")
        .await
        .unwrap();

    let names: Vec<String> = (0..count).map(|i| format!("{tag}-{i:02}")).collect();
    for name in &names {
        let user_id = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, name, pass, mail) VALUES ($1, $2, '', $3)")
            .bind(user_id)
            .bind(name)
            .bind(format!("{name}@test.local"))
            .execute(&app.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)")
            .bind(user_id)
            .bind(role.id)
            .execute(&app.db)
            .await
            .unwrap();
    }

    let role_id = role.id;
    let pool = app.db.clone();
    let tag = tag.to_string();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
            .bind(format!("{tag}-%"))
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM roles WHERE id = $1")
            .bind(role_id)
            .execute(&pool)
            .await;
    });
    (role_id, names)
}

async fn admin(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    let prefix = prefix.to_string();
    let pool = app.db.clone();
    defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name LIKE $1")
            .bind(format!("{prefix}-%"))
            .execute(&pool)
            .await;
    });
    app.create_and_login_admin(&name, PASSWORD, &format!("{name}@test.local"))
        .await
}

#[test]
fn names_members_for_an_administrator_capped_at_20_with_and_n_more() {
    run_test(async {
        let app = shared_app().await;
        let tag = format!("wcn{}", Uuid::now_v7().simple());
        let (role_id, names) = role_with_members(app, &tag, 25).await;
        let cookies = admin(app, "wcnadmin").await;

        let (status, html) =
            get(app, &cookies, &format!("/admin/people/who-can?path={PATH}")).await;
        assert_eq!(status, StatusCode::OK);

        for name in &names[..20] {
            assert!(html.contains(name), "{name} is named");
        }
        for name in &names[20..] {
            assert!(!html.contains(name), "{name} is not named past the cap");
        }
        assert!(
            html.contains("and 5 more"),
            "the remainder is named as a count"
        );
        assert!(
            html.contains(&format!("/admin/people?role={role_id}")),
            "the remainder links to the People listing filtered by role"
        );
    });
}

#[test]
fn a_role_at_or_under_the_cap_shows_every_member_and_no_more_link() {
    run_test(async {
        let app = shared_app().await;
        let tag = format!("wcnfew{}", Uuid::now_v7().simple());
        let (role_id, names) = role_with_members(app, &tag, 3).await;
        let cookies = admin(app, "wcnfewadmin").await;

        let (status, html) =
            get(app, &cookies, &format!("/admin/people/who-can?path={PATH}")).await;
        assert_eq!(status, StatusCode::OK);
        for name in &names {
            assert!(html.contains(name), "{name} is named");
        }
        assert!(
            !html.contains(&format!("/admin/people?role={role_id}")),
            "nothing is left over to name as a count"
        );
    });
}
