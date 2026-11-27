#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Access rules (B108): the service, registration and sign in refusals, the
//! admin creation warning, and `/admin/people/rules`.
//!
//! Every rule a test makes uses a pattern no other test's value can match (a
//! unique prefix, or a TEST-NET-3 address), and is deleted when the test ends.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::models::{RegistrationMode, SiteConfig};
use trovato_kernel::services::access_rule::{self, RuleStatus, RuleType};
use uuid::Uuid;

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn post_form(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    body: String,
) -> axum::response::Response {
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The token and build id of the form whose `action` is `action`.
fn form_pairs(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    let scoped = &scoped[..scoped.find("</form>").unwrap()];
    let mut pairs = format!(
        "_token={}",
        urlencoding::encode(&input_value(scoped, "_token").unwrap())
    );
    if let Some(build) = input_value(scoped, "_form_build_id") {
        pairs.push_str(&format!("&_form_build_id={}", urlencoding::encode(&build)));
    }
    pairs
}

async fn admin_cookies(app: &TestApp, prefix: &str) -> String {
    let name = common::username(prefix);
    app.create_and_login_admin(
        &name,
        "correct-horse-battery-staple",
        &format!("{name}@test.local"),
    )
    .await
}

/// A rule that is deleted when the test ends.
async fn rule(app: &TestApp, rule_type: RuleType, pattern: &str, status: RuleStatus) -> Uuid {
    let id = access_rule::create(&app.db, rule_type, pattern, status)
        .await
        .expect("create rule");
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = access_rule::delete(&db, id).await;
    });
    id
}

fn delete_user_on_exit(app: &TestApp, name: &str) {
    let db = app.db.clone();
    let name = name.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM users WHERE name = $1")
            .bind(&name)
            .execute(&db)
            .await;
    });
}

/// Open registration for the test, and put the settings back after. Only one
/// test in this file opens it, so no other test here sees it change.
async fn open_registration(app: &TestApp) {
    let mut saved = Vec::new();
    for key in ["user_registration", "allow_user_registration"] {
        saved.push((key, SiteConfig::get(&app.db, key).await.ok().flatten()));
    }
    let db = app.db.clone();
    common::defer_cleanup(move || async move {
        for (key, value) in saved {
            match value {
                Some(value) => {
                    let _ = SiteConfig::set(&db, key, value).await;
                }
                None => {
                    let _ = SiteConfig::delete(&db, key).await;
                }
            }
        }
    });
    RegistrationMode::Open
        .save(&app.db)
        .await
        .expect("open registration");
}

async fn register_json(
    app: &TestApp,
    ip: &str,
    username: &str,
    mail: &str,
) -> (StatusCode, String) {
    app.state.rate_limiter().reset("register", ip).await.ok();
    let response = app
        .request(
            Request::post("/user/register/json")
                .header("content-type", "application/json")
                .header("x-forwarded-for", ip)
                .body(Body::from(
                    serde_json::json!({
                        "username": username,
                        "mail": mail,
                        "password": "correct-horse-battery-staple",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

async fn login_json(app: &TestApp, ip: &str, username: &str, password: &str) -> StatusCode {
    app.state.lockout().clear_all(username).await.ok();
    app.state.rate_limiter().reset("login", ip).await.ok();
    app.request(
        Request::post("/user/login/json")
            .header("content-type", "application/json")
            .header("x-forwarded-for", ip)
            .body(Body::from(
                serde_json::json!({"username": username, "password": password}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .status()
}

// =============================================================================
// The service
// =============================================================================

/// Each type is checked against stored rules of its own type, and allow
/// overrides deny.
#[test]
fn the_service_checks_each_type_and_allow_overrides_deny() {
    run_test(async {
        let app = shared_app().await;
        let tag = Uuid::now_v7().simple().to_string();
        let user_prefix = format!("svc{tag}");
        rule(
            app,
            RuleType::Username,
            &format!("{user_prefix}%"),
            RuleStatus::Deny,
        )
        .await;
        rule(
            app,
            RuleType::Email,
            &format!("%@{tag}.test"),
            RuleStatus::Deny,
        )
        .await;
        rule(
            app,
            RuleType::Email,
            &format!("editor@{tag}.test"),
            RuleStatus::Allow,
        )
        .await;
        rule(app, RuleType::Host, "203.0.113.%", RuleStatus::Deny).await;
        rule(app, RuleType::Host, "203.0.113.200", RuleStatus::Allow).await;

        let db = &app.db;
        assert!(
            access_rule::is_blocked(db, RuleType::Username, &format!("{user_prefix}-bot")).await
        );
        assert!(
            access_rule::is_blocked(
                db,
                RuleType::Username,
                &format!("{}-BOT", user_prefix.to_uppercase())
            )
            .await,
            "case is ignored"
        );
        assert!(!access_rule::is_blocked(db, RuleType::Username, &format!("x{user_prefix}")).await);
        // A username rule says nothing about an email address.
        assert!(!access_rule::is_blocked(db, RuleType::Email, &format!("{user_prefix}-bot")).await);

        assert!(access_rule::is_blocked(db, RuleType::Email, &format!("writer@{tag}.test")).await);
        assert!(
            !access_rule::is_blocked(db, RuleType::Email, &format!("editor@{tag}.test")).await,
            "allow beats deny"
        );
        assert!(!access_rule::is_blocked(db, RuleType::Email, "writer@elsewhere.test").await);

        assert!(access_rule::is_blocked(db, RuleType::Host, "203.0.113.7").await);
        assert!(
            !access_rule::is_blocked(db, RuleType::Host, "203.0.113.200").await,
            "allow beats deny"
        );
        assert!(!access_rule::is_blocked(db, RuleType::Host, "198.51.100.7").await);

        let blocked = access_rule::blocked_types(
            db,
            Some(&format!("{user_prefix}1")),
            Some(&format!("a@{tag}.test")),
            Some("203.0.113.9"),
        )
        .await;
        assert_eq!(
            blocked,
            vec![RuleType::Username, RuleType::Email, RuleType::Host]
        );
    });
}

// =============================================================================
// Registration and sign in
// =============================================================================

/// Registration is refused for a denied username, email address or client
/// address, and goes through when an allow rule lifts the deny; the HTML form
/// shows the refusal.
#[test]
fn registration_is_refused_by_each_rule_type() {
    run_test(async {
        let app = shared_app().await;
        open_registration(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let ip = "198.51.100.23";

        // Username.
        let spam = format!("reg{tag}");
        rule(
            app,
            RuleType::Username,
            &format!("{spam}%"),
            RuleStatus::Deny,
        )
        .await;
        let (status, body) =
            register_json(app, ip, &format!("{spam}-a"), &format!("a@{tag}.ok")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("This username is not allowed"), "{body}");

        // Email.
        rule(
            app,
            RuleType::Email,
            &format!("%@{tag}.bad"),
            RuleStatus::Deny,
        )
        .await;
        let name = common::username("b108rege");
        let (status, body) = register_json(app, ip, &name, &format!("x@{tag}.bad")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("This email address is not allowed"), "{body}");

        // Host.
        rule(app, RuleType::Host, "203.0.113.41", RuleStatus::Deny).await;
        let (status, body) =
            register_json(app, "203.0.113.41", &name, &format!("x@{tag}.ok")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.contains("Registration from your address is not allowed"),
            "{body}"
        );

        // An allow rule for this one address lifts the email deny.
        rule(
            app,
            RuleType::Email,
            &format!("vip@{tag}.bad"),
            RuleStatus::Allow,
        )
        .await;
        delete_user_on_exit(app, &name);
        let (status, body) = register_json(app, ip, &name, &format!("vip@{tag}.bad")).await;
        assert_eq!(status, StatusCode::OK, "allowed address registers: {body}");

        // The HTML registration form shows the refusal and creates no account.

        let response = app
            .request(Request::get("/user/register").body(Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookies = common::extract_cookies(&response);
        let html = body_string(response).await;
        let fields = common::form_fields(&html, "user_register");
        let name = common::username("b108regf");
        delete_user_on_exit(app, &name);
        app.state
            .rate_limiter()
            .reset("register", "127.0.0.1")
            .await
            .ok();
        let response = post_form(
            app,
            &cookies,
            "/user/register",
            format!(
                "{fields}&username={name}&mail={}&password=correct-horse-battery-staple&confirm_password=correct-horse-battery-staple",
                urlencoding::encode(&format!("someone@{tag}.bad")),
            ),
        )
        .await;
        let html = body_string(response).await;
        assert!(
            html.contains("This email address is not allowed"),
            "the refusal is shown"
        );
        let created: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_optional(&app.db)
            .await
            .unwrap();
        assert!(created.is_none(), "no account is created");
    });
}

/// Sign in is refused for a denied username or client address, before the
/// password is checked, and works again once the rule is gone.
#[test]
fn sign_in_is_refused_by_username_and_host_rules() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b108login");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let ip = "198.51.100.61";
        assert_eq!(
            login_json(app, ip, &name, "test-password-123").await,
            StatusCode::OK
        );

        let id = rule(app, RuleType::Username, &name, RuleStatus::Deny).await;
        assert_eq!(
            login_json(app, ip, &name, "test-password-123").await,
            StatusCode::FORBIDDEN,
            "a denied username cannot sign in"
        );
        access_rule::delete(&app.db, id).await.unwrap();
        assert_eq!(
            login_json(app, ip, &name, "test-password-123").await,
            StatusCode::OK
        );

        rule(app, RuleType::Host, "203.0.113.62", RuleStatus::Deny).await;
        assert_eq!(
            login_json(app, "203.0.113.62", &name, "test-password-123").await,
            StatusCode::FORBIDDEN,
            "a denied address cannot sign in"
        );
        assert_eq!(
            login_json(app, ip, &name, "test-password-123").await,
            StatusCode::OK
        );
    });
}

/// An administrator creating an account a rule blocks is warned, not refused.
#[test]
fn admin_user_creation_warns_but_creates() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app, "b108adm").await;
        let tag = Uuid::now_v7().simple().to_string();
        rule(
            app,
            RuleType::Email,
            &format!("%@{tag}.bad"),
            RuleStatus::Deny,
        )
        .await;

        let (status, html) = get(app, &cookies, "/admin/people/add").await;
        assert_eq!(status, StatusCode::OK);
        let pairs = form_pairs(&html, "/admin/people/add");
        let name = common::username("b108warned");
        delete_user_on_exit(app, &name);
        let response = post_form(
            app,
            &cookies,
            "/admin/people/add",
            format!(
                "{pairs}&name={name}&mail={}&password=correct-horse-battery-staple&status=1",
                urlencoding::encode(&format!("new@{tag}.bad"))
            ),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "created: {}",
            response.status()
        );
        let created: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(&name)
            .fetch_optional(&app.db)
            .await
            .unwrap();
        assert!(created.is_some(), "the account is created");

        let (_, html) = get(app, &cookies, "/admin/people").await;
        assert!(
            html.contains("an access rule blocks its email address"),
            "the warning is shown"
        );
    });
}

// =============================================================================
// The admin screen
// =============================================================================

/// Every route refuses an anonymous visitor and a signed-in user without
/// `administer access rules`, GET and POST.
#[test]
fn the_rules_screens_refuse_anonymous_and_plain_users() {
    run_test(async {
        let app = shared_app().await;
        let (_, plain) =
            user_holding(app, "b108rplain", &["access content", "administer users"]).await;
        let id = rule(
            app,
            RuleType::Username,
            &format!("refuse{}", Uuid::now_v7().simple()),
            RuleStatus::Deny,
        )
        .await;
        let gets = [
            "/admin/people/rules".to_string(),
            "/admin/people/rules/add".to_string(),
            format!("/admin/people/rules/{id}/edit"),
            format!("/admin/people/rules/{id}/delete"),
        ];
        let posts = [
            "/admin/people/rules/check".to_string(),
            "/admin/people/rules/add".to_string(),
            format!("/admin/people/rules/{id}/edit"),
            format!("/admin/people/rules/{id}/delete"),
        ];
        for (who, cookies) in [
            ("an anonymous visitor", ""),
            ("a plain user", plain.as_str()),
        ] {
            for uri in &gets {
                let (status, _) = get(app, cookies, uri).await;
                assert_ne!(status, StatusCode::OK, "GET {uri} served {who}");
                assert!(
                    status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
                    "GET {uri} for {who}: {status}"
                );
            }
            for uri in &posts {
                let response = post_form(
                    app,
                    cookies,
                    uri,
                    "rule_type=username&pattern=x&status=deny".into(),
                )
                .await;
                let status = response.status();
                assert!(
                    status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED,
                    "POST {uri} for {who}: {status}"
                );
            }
        }
        // Nothing was changed by the refused posts.
        assert!(access_rule::find(&app.db, id).await.unwrap().is_some());
        let (status, _) = get(app, &plain, "/admin/people/rules").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a signed-in user without the permission gets 403"
        );
    });
}

/// A user holding only `administer access rules` adds, edits, checks and
/// deletes a rule through the forms, and a post without the form's token
/// changes nothing.
#[test]
fn a_rules_administrator_manages_rules_through_the_forms() {
    run_test(async {
        let app = shared_app().await;
        let (_, cookies) = user_holding(app, "b108rman", &["administer access rules"]).await;
        let pattern = format!("%@{}.test", Uuid::now_v7().simple());

        let (status, html) = get(app, &cookies, "/admin/people/rules/add").await;
        assert_eq!(status, StatusCode::OK);
        // Without the token, nothing is saved.
        let response = post_form(
            app,
            &cookies,
            "/admin/people/rules/add",
            format!(
                "rule_type=email&status=deny&pattern={}",
                urlencoding::encode(&pattern)
            ),
        )
        .await;
        assert!(!response.status().is_redirection());
        assert!(
            access_rule::list(&app.db)
                .await
                .unwrap()
                .iter()
                .all(|r| r.pattern != pattern),
            "no token, no rule"
        );

        // An empty mask is shown again with its error.
        let pairs = form_pairs(&html, "/admin/people/rules/add");
        let response = post_form(
            app,
            &cookies,
            "/admin/people/rules/add",
            format!("{pairs}&rule_type=email&status=deny&pattern="),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = body_string(response).await;
        assert!(html.contains("Mask is required"), "the error is shown");

        let pairs = form_pairs(&html, "/admin/people/rules/add");
        let response = post_form(
            app,
            &cookies,
            "/admin/people/rules/add",
            format!(
                "{pairs}&rule_type=email&status=deny&pattern={}",
                urlencoding::encode(&pattern)
            ),
        )
        .await;
        assert!(
            response.status().is_redirection(),
            "saved: {}",
            response.status()
        );
        let saved = access_rule::list(&app.db)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.pattern == pattern)
            .expect("the rule is stored");
        let id = saved.id;
        let db = app.db.clone();
        common::defer_cleanup(move || async move {
            let _ = access_rule::delete(&db, id).await;
        });
        assert_eq!(saved.rule_type, RuleType::Email);
        assert_eq!(saved.status, RuleStatus::Deny);

        let (status, html) = get(app, &cookies, "/admin/people/rules").await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&pattern), "the list shows the rule");

        // The check box tests a value against the rules.
        let pairs = form_pairs(&html, "/admin/people/rules/check");
        let blocked_value = pattern.replace('%', "someone");
        let response = post_form(
            app,
            &cookies,
            "/admin/people/rules/check",
            format!(
                "{pairs}&rule_type=email&value={}",
                urlencoding::encode(&blocked_value)
            ),
        )
        .await;
        let html = body_string(response).await;
        assert!(
            html.contains("is denied by the rules"),
            "the check reports the block"
        );

        // Edit: make it an allow rule.
        let (status, html) = get(app, &cookies, &format!("/admin/people/rules/{id}/edit")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains(&pattern), "the form carries the mask");
        let pairs = form_pairs(&html, &format!("/admin/people/rules/{id}/edit"));
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/people/rules/{id}/edit"),
            format!(
                "{pairs}&rule_type=email&status=allow&pattern={}",
                urlencoding::encode(&pattern)
            ),
        )
        .await;
        assert!(response.status().is_redirection());
        assert_eq!(
            access_rule::find(&app.db, id)
                .await
                .unwrap()
                .unwrap()
                .status,
            RuleStatus::Allow
        );

        // Delete, through the confirmation form.
        let (status, html) = get(app, &cookies, &format!("/admin/people/rules/{id}/delete")).await;
        assert_eq!(status, StatusCode::OK);
        let pairs = form_pairs(&html, &format!("/admin/people/rules/{id}/delete"));
        let response = post_form(
            app,
            &cookies,
            &format!("/admin/people/rules/{id}/delete"),
            pairs,
        )
        .await;
        assert!(response.status().is_redirection());
        assert!(
            access_rule::find(&app.db, id).await.unwrap().is_none(),
            "deleted"
        );
    });
}
