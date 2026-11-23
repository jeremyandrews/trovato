#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `COOKIE_SECURE` (B124): every cookie the kernel sets takes its `Secure`
//! flag from this one setting.
//!
//! The session cookie (`id`) is the only cookie the kernel sets: CSRF tokens,
//! "remember me" and the signed-in user live inside the session, in Redis. So
//! "every cookie" is every `Set-Cookie` the kernel answers with, and these tests
//! check all of them on every response of a full sign-in: the anonymous login
//! form, the login itself (which rotates the session id), and logout.
//!
//! - `always`, the default, sets `Secure` on every cookie.
//! - `never` drops it from every cookie, and a plain HTTP sign-in works end to
//!   end: log in through the form, see the profile, log out.
//!
//! The one exception is the removal cookie logout sends (`id=; Max-Age=0`),
//! which tower-sessions builds without any flags; see [`is_removal`].
//!
//! Startup refusal of `never` with an `https://` `SITE_URL` is a configuration
//! error, tested in `config.rs` (`cookie_secure_never_with_an_https_site_url_*`)
//! and by starting the binary.
//!
//! Requires Postgres + Redis.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use common::{TestApp, extract_cookies, run_test, shared_app, test_ip_for, username};
use trovato_kernel::session::CookieSecure;

const PASSWORD: &str = "cookie-secure-test-pw-1";

/// Every `Set-Cookie` value on `response`.
fn set_cookies(response: &Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect()
}

/// Whether a `Set-Cookie` value is a removal: an empty value that expires now.
///
/// On logout tower-sessions (0.14) removes the cookie with a `Set-Cookie`
/// built from the request's name alone, so it carries no `Secure`,
/// `HttpOnly` or `SameSite` whatever the layer is configured with. It holds
/// no session id and expires at once, so it can leak nothing.
fn is_removal(set_cookie: &str) -> bool {
    let mut parts = set_cookie.split(';');
    parts.next().is_some_and(|kv| kv.trim() == "id=")
        && parts.any(|attr| attr.trim().eq_ignore_ascii_case("max-age=0"))
}

/// Whether a `Set-Cookie` value carries the `Secure` attribute.
fn is_secure(set_cookie: &str) -> bool {
    set_cookie
        .split(';')
        .skip(1)
        .any(|attr| attr.trim().eq_ignore_ascii_case("secure"))
}

/// Whether a `Set-Cookie` value carries `HttpOnly` and `SameSite=Strict`, the
/// flags this setting must not change.
fn keeps_http_only_and_same_site(set_cookie: &str) -> bool {
    let attrs: Vec<String> = set_cookie
        .split(';')
        .skip(1)
        .map(|a| a.trim().to_ascii_lowercase())
        .collect();
    attrs.iter().any(|a| a == "httponly") && attrs.iter().any(|a| a == "samesite=strict")
}

async fn body_text(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// The value of the hidden `_token` field in the first form whose `action`
/// is `action`.
fn form_token(html: &str, action: &str) -> String {
    let form = html
        .find(&format!("action=\"{action}\""))
        .unwrap_or_else(|| panic!("a form posting to {action}"));
    let marker = "name=\"_token\" value=\"";
    let start = html[form..]
        .find(marker)
        .map(|p| form + p + marker.len())
        .expect("a _token field in the form");
    let end = html[start..].find('"').map(|p| start + p).unwrap();
    html[start..end].to_string()
}

/// A full sign-in over plain HTTP through the HTML forms: the login form, the
/// login, the profile, logout. Returns every `Set-Cookie` seen on the way.
async fn sign_in_see_profile_sign_out(app: &TestApp, prefix: &str) -> Vec<String> {
    let name = username(prefix);
    app.create_test_user(&name, PASSWORD, &format!("{name}@example.com"))
        .await;
    let ip = test_ip_for(&name);
    app.state.lockout().clear_all(&name).await.ok();
    app.state.rate_limiter().reset("login", &ip).await.ok();
    let mut seen = Vec::new();

    // The anonymous login form: its CSRF token is stored in a new session,
    // so this is where an anonymous visitor first gets a cookie.
    let form = app
        .request(
            Request::get("/user/login")
                .header("x-forwarded-for", &ip)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(form.status(), StatusCode::OK);
    seen.extend(set_cookies(&form));
    let anonymous = extract_cookies(&form);
    assert!(
        anonymous.starts_with("id="),
        "the login form starts a session"
    );
    let token = form_token(&body_text(form).await, "/user/login");

    // Log in through the form.
    let login = app
        .request_with_cookies(
            Request::post("/user/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-forwarded-for", &ip)
                .body(Body::from(format!(
                    "username={name}&password={PASSWORD}&_token={token}"
                )))
                .unwrap(),
            &anonymous,
        )
        .await;
    assert_eq!(
        login.status(),
        StatusCode::SEE_OTHER,
        "the form login redirects"
    );
    seen.extend(set_cookies(&login));
    let cookies = extract_cookies(&login);
    assert!(
        cookies.starts_with("id="),
        "login rotates the session id and sends the new cookie"
    );

    // See the profile, signed in.
    let profile = app
        .request_with_cookies(
            Request::get("/user/profile").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(profile.status(), StatusCode::OK, "the profile is reachable");
    seen.extend(set_cookies(&profile));
    let html = body_text(profile).await;
    assert!(html.contains(&name), "the profile names the account");
    let logout_token = form_token(&html, "/user/logout");

    // Log out.
    let logout = app
        .request_with_cookies(
            Request::post("/user/logout")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("_token={logout_token}")))
                .unwrap(),
            &cookies,
        )
        .await;
    assert_eq!(logout.status(), StatusCode::SEE_OTHER, "logout redirects");
    seen.extend(set_cookies(&logout));

    // Signed out: the old cookie reaches the profile no more.
    let after = app
        .request_with_cookies(
            Request::get("/user/profile").body(Body::empty()).unwrap(),
            &cookies,
        )
        .await;
    assert!(
        after.status() == StatusCode::UNAUTHORIZED,
        "signed out after logout (status {})",
        after.status()
    );

    seen
}

#[test]
fn the_default_sets_secure_on_every_cookie() {
    run_test(async {
        let app = shared_app().await;
        assert_eq!(
            app.state.runtime().cookie_secure,
            CookieSecure::Always,
            "the shared app runs with the default"
        );
        let seen = sign_in_see_profile_sign_out(app, "cookie-secure-always").await;

        let (removals, set): (Vec<_>, Vec<_>) = seen.iter().partition(|c| is_removal(c));
        assert!(
            set.len() >= 2,
            "the login form and the login set a cookie: {seen:?}"
        );
        assert_eq!(
            removals.len(),
            1,
            "logout removes the cookie once: {seen:?}"
        );
        for cookie in set {
            assert!(is_secure(cookie), "Secure by default: {cookie}");
            assert!(
                keeps_http_only_and_same_site(cookie),
                "flags kept: {cookie}"
            );
        }
    });
}

#[test]
fn never_drops_secure_from_every_cookie_and_plain_http_sign_in_works() {
    run_test(async {
        let app = TestApp::with_config(|config| {
            config.runtime.cookie_secure = CookieSecure::Never;
        })
        .await;
        let seen = sign_in_see_profile_sign_out(&app, "cookie-secure-never").await;

        let (removals, set): (Vec<_>, Vec<_>) = seen.iter().partition(|c| is_removal(c));
        assert!(
            set.len() >= 2,
            "the login form and the login set a cookie: {seen:?}"
        );
        assert_eq!(
            removals.len(),
            1,
            "logout removes the cookie once: {seen:?}"
        );
        for cookie in set {
            assert!(!is_secure(cookie), "no Secure under never: {cookie}");
            assert!(
                keeps_http_only_and_same_site(cookie),
                "flags kept: {cookie}"
            );
        }
    });
}

/// The site settings page shows the setting, and warns under `never`.
#[test]
fn the_site_settings_page_shows_the_setting_and_warns_under_never() {
    run_test(async {
        async fn page(app: &TestApp, prefix: &str) -> String {
            let name = username(prefix);
            let cookies = app
                .create_and_login_admin(&name, PASSWORD, &format!("{name}@example.com"))
                .await;
            let response = app
                .request_with_cookies(
                    Request::get("/admin/config/site")
                        .body(Body::empty())
                        .unwrap(),
                    &cookies,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            body_text(response).await
        }

        let html = page(shared_app().await, "cookie-secure-page-always").await;
        assert!(html.contains("<dd id=\"cookie-secure\">always</dd>"));
        assert!(!html.contains("cookie-secure-warning"));

        let never = TestApp::with_config(|config| {
            config.runtime.cookie_secure = CookieSecure::Never;
        })
        .await;
        let html = page(&never, "cookie-secure-page-never").await;
        assert!(html.contains("<dd id=\"cookie-secure\">never</dd>"));
        assert!(
            html.contains("id=\"cookie-secure-warning\""),
            "never is called out on the page"
        );
    });
}
