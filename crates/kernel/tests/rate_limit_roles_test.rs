#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Rate limits by role, counted once per request, through the production stack.
//!
//! Every bucket had one count for everyone (`api`: 100 a minute), so a brief of
//! "60 a minute anonymous, 300 authenticated" could not be written down. And
//! `POST /user/login*` and `/user/register*` were counted twice, once by the
//! per-IP middleware and again by their handlers, so `register: 3 an hour`
//! admitted one registration and `login: 5 a minute` two.
//!
//! These run against [`TestApp::with_rate_limits`], which has both rate-limit
//! layers where `main.rs` has them. The shared app has neither, which is why the
//! double count was invisible to the suite.
//!
//! The per-role counts are site configuration (`rate_limit.<bucket>.<role key>`),
//! read once when the app is built, so the keys are written, the app is built,
//! and the keys are removed again before any test runs: nothing global outlives
//! construction. The role key is the role's name, lower-cased, with every run of
//! characters other than a letter or a digit replaced by one underscore.
//!
//! Every request comes from an address of its own (`X-Forwarded-For` from the
//! trusted loopback peer) and every user is new, so no two tests, and no two
//! runs against the same Redis, share a counter.
//!
//! Requires Postgres and Redis; runs in CI.

mod common;

use std::sync::LazyLock;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test};
use trovato_kernel::models::api_token::ApiToken;
use trovato_kernel::models::{RegistrationMode, Role, SiteConfig};
use uuid::Uuid;

/// A suffix unique to this run of this binary, so the configured role keys name
/// roles no other run has.
static RUN: LazyLock<String> =
    LazyLock::new(|| Uuid::now_v7().simple().to_string()[20..].to_string());

/// The `api` count for a request that presents no credential.
const ANONYMOUS_API: u32 = 5;
/// A role whose holders get more than an anonymous client. Two roles carry it
/// (`generous` and `roaming`), one per test, because a role is removed when the
/// test that made it ends.
const GENEROUS_API: u32 = 12;
/// Two roles held together: the user gets the larger.
const PAIR_LOW_API: u32 = 7;
const PAIR_HIGH_API: u32 = 10;
/// The role of the user a token authenticates.
const TOKEN_API: u32 = 6;

fn role_name(label: &str) -> String {
    format!("b69 rl {label} {}", *RUN)
}

/// The role key the configuration names a role by.
fn role_key(label: &str) -> String {
    format!("b69_rl_{label}_{}", *RUN)
}

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("rate-limited app init thread panicked")
    })
}

/// Configure the per-role `api` counts, build the rate-limited app, and remove
/// the keys again.
async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::PgPool::connect(&database_url)
        .await
        .expect("connect for fixture setup");

    let keys = [
        ("rate_limit.api.anonymous_user".to_string(), ANONYMOUS_API),
        (
            format!("rate_limit.api.{}", role_key("generous")),
            GENEROUS_API,
        ),
        (
            format!("rate_limit.api.{}", role_key("roaming")),
            GENEROUS_API,
        ),
        (format!("rate_limit.api.{}", role_key("low")), PAIR_LOW_API),
        (
            format!("rate_limit.api.{}", role_key("high")),
            PAIR_HIGH_API,
        ),
        (format!("rate_limit.api.{}", role_key("token")), TOKEN_API),
    ];
    for (key, count) in &keys {
        SiteConfig::set(&pool, key, serde_json::json!(count.to_string()))
            .await
            .expect("configure a per-role count");
    }

    let app = TestApp::with_rate_limits(|_| {}).await;

    for (key, _) in &keys {
        SiteConfig::delete(&pool, key)
            .await
            .expect("remove a per-role count");
    }
    pool.close().await;
    app
}

/// An address no other test or run uses.
fn fresh_ip() -> String {
    common::test_ip_for(&Uuid::now_v7().to_string())
}

/// A user holding the roles named by `labels` (created here, removed when the
/// test ends), logged in. Returns the user's id and session cookies.
async fn user_with_roles(app: &TestApp, labels: &[&str]) -> (Uuid, String) {
    let name = common::username("b69rl");
    app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    let user_id = common::user_id_of(app, &name).await;
    for label in labels {
        let role_id = common::create_test_role(app, &role_name(label)).await;
        Role::assign_to_user(&app.db, user_id, role_id)
            .await
            .expect("assign role");
    }
    app.state.permissions().invalidate_user(user_id);
    let cookies = app.login(&name, "test-password-123").await;
    (user_id, cookies)
}

/// `GET /api/tokens` from `ip`, with whatever credential `auth` adds.
async fn api_get(app: &TestApp, ip: &str, auth: Option<(&str, &str)>) -> StatusCode {
    let mut request = Request::get("/api/tokens").header("x-forwarded-for", ip);
    if let Some((name, value)) = auth {
        request = request.header(name, value);
    }
    app.request(request.body(Body::empty()).unwrap())
        .await
        .status()
}

/// How many of `ceiling` requests were answered before the first 429, and the
/// statuses of those that were.
async fn answered_before_429(
    app: &TestApp,
    ip: &str,
    auth: Option<(&str, &str)>,
    ceiling: u32,
) -> (u32, Vec<StatusCode>) {
    let mut statuses = Vec::new();
    for n in 0..ceiling {
        let status = api_get(app, ip, auth).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            return (n, statuses);
        }
        statuses.push(status);
    }
    (ceiling, statuses)
}

/// An anonymous client gets the `anonymous user` role's count, not the bucket
/// default of 100.
#[test]
fn an_anonymous_client_is_refused_at_the_anonymous_count() {
    run_test(async {
        let app = app();
        let (answered, _) = answered_before_429(app, &fresh_ip(), None, 30).await;
        assert_eq!(
            answered, ANONYMOUS_API,
            "an anonymous client must be refused after the anonymous count of {ANONYMOUS_API}"
        );
    });
}

/// A signed-in user whose role has a larger count than anonymous is not refused
/// where an anonymous client would be, and is refused at their role's count.
#[test]
fn a_role_with_a_higher_count_is_not_refused_at_the_anonymous_count() {
    run_test(async {
        let app = app();
        let (_, cookies) = user_with_roles(app, &["generous"]).await;
        let ip = fresh_ip();

        let (answered, statuses) =
            answered_before_429(app, &ip, Some(("cookie", &cookies)), 30).await;
        assert!(
            statuses.iter().all(|s| *s == StatusCode::OK),
            "a signed-in user's requests must be served: {statuses:?}"
        );
        assert_eq!(
            answered, GENEROUS_API,
            "the user must get their role's count of {GENEROUS_API}, not the anonymous \
             {ANONYMOUS_API} or the default"
        );
    });
}

/// A user holding two roles with counts gets the more generous of the two.
#[test]
fn a_user_with_two_roles_gets_the_higher_count() {
    run_test(async {
        let app = app();
        let (_, cookies) = user_with_roles(app, &["low", "high"]).await;

        let (answered, _) =
            answered_before_429(app, &fresh_ip(), Some(("cookie", &cookies)), 30).await;
        assert_eq!(
            answered, PAIR_HIGH_API,
            "the higher of the two roles' counts ({PAIR_HIGH_API}) must apply"
        );
    });
}

/// The per-user count follows the user, not the address: the budget is spent
/// from two addresses together.
#[test]
fn a_users_count_is_shared_across_addresses() {
    run_test(async {
        let app = app();
        let (_, cookies) = user_with_roles(app, &["roaming"]).await;
        let (first_ip, second_ip) = (fresh_ip(), fresh_ip());

        for _ in 0..GENEROUS_API {
            assert_eq!(
                api_get(app, &first_ip, Some(("cookie", &cookies))).await,
                StatusCode::OK
            );
        }
        assert_eq!(
            api_get(app, &second_ip, Some(("cookie", &cookies))).await,
            StatusCode::TOO_MANY_REQUESTS,
            "a user who spent their count from one address is refused from another"
        );
    });
}

/// A request authenticated by an API token is counted as the token's user, at
/// that user's role count.
#[test]
fn a_token_request_gets_its_users_role_count() {
    run_test(async {
        let app = app();
        let (user_id, _) = user_with_roles(app, &["token"]).await;
        let (_, raw) = ApiToken::create(&app.db, user_id, "b69 rate limit", None)
            .await
            .expect("create token");
        let bearer = format!("Bearer {raw}");

        let (answered, statuses) =
            answered_before_429(app, &fresh_ip(), Some(("authorization", &bearer)), 30).await;
        assert!(
            statuses.iter().all(|s| *s == StatusCode::OK),
            "a valid token must authenticate: {statuses:?}"
        );
        assert_eq!(
            answered, TOKEN_API,
            "a token request must get its user's role count of {TOKEN_API}"
        );
    });
}

/// A bearer value that authenticates nobody does not escape the per-IP limit:
/// past the most generous configured count, the address is refused before the
/// value is looked up.
#[test]
fn an_invalid_bearer_value_is_limited_per_ip() {
    run_test(async {
        let app = app();
        let ip = fresh_ip();
        let bearer = format!("Bearer not-a-token-{}", Uuid::now_v7().simple());

        let (answered, statuses) =
            answered_before_429(app, &ip, Some(("authorization", &bearer)), 150).await;
        assert!(
            statuses.iter().all(|s| *s == StatusCode::UNAUTHORIZED),
            "an invalid bearer value must be refused as unauthenticated: {statuses:?}"
        );
        assert!(
            answered < 150,
            "an invalid bearer value must hit a per-IP limit within 150 requests"
        );
    });
}

/// `register` is three an hour per address, and three registrations succeed.
/// Counted twice, it admitted one.
#[test]
fn three_registrations_an_hour_succeed_and_the_fourth_is_refused() {
    run_test(async {
        let app = app();

        let previous = SiteConfig::get(&app.db, "user_registration")
            .await
            .expect("read registration mode");
        {
            let pool = app.db.clone();
            common::defer_cleanup(move || async move {
                match previous {
                    Some(value) => SiteConfig::set(&pool, "user_registration", value).await,
                    None => SiteConfig::delete(&pool, "user_registration")
                        .await
                        .map(|_| ()),
                }
                .expect("restore registration mode");
            });
        }
        RegistrationMode::Open
            .save(&app.db)
            .await
            .expect("open registration");

        let ip = fresh_ip();
        let mut statuses = Vec::new();
        for _ in 0..4 {
            let name = common::username("b69reg");
            {
                let pool = app.db.clone();
                let name = name.clone();
                common::defer_cleanup(move || async move {
                    let _ = sqlx::query("DELETE FROM users WHERE name = $1")
                        .bind(&name)
                        .execute(&pool)
                        .await;
                });
            }
            let response = app
                .request(
                    Request::post("/user/register/json")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", &ip)
                        .body(Body::from(
                            serde_json::json!({
                                "username": name,
                                "mail": format!("{name}@example.com"),
                                "password": "a-long-enough-password-1",
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await;
            statuses.push(response.status());
        }

        assert_eq!(
            statuses,
            vec![
                StatusCode::OK,
                StatusCode::OK,
                StatusCode::OK,
                StatusCode::TOO_MANY_REQUESTS
            ],
            "three registrations an hour must succeed and the fourth be refused"
        );
    });
}

/// Moved from `integration_test.rs`, unchanged but for the app it runs on: the
/// shared app has no rate-limit middleware, so there it only ever saw the
/// handler's count. The login limit is 5 a minute, so the first 5 attempts are
/// processed (401, bad credentials) and the 6th is refused with 429. Counted
/// twice, the third was refused.
#[test]
fn e2e_login_rate_limit_blocks_after_threshold() {
    run_test(async {
        let app = app();

        let unique = uuid::Uuid::now_v7().simple().to_string();
        let username = format!("rlburst_{}", &unique[..12]);
        // Unique bucket per test run (shared DB/Redis), reset to a clean slate.
        let fake_ip = format!(
            "10.77.{}.{}",
            unique.as_bytes()[0] % 255,
            (unique.as_bytes()[1] % 254) + 1
        );
        app.state.rate_limiter().reset("login", &fake_ip).await.ok();

        let mut statuses = Vec::new();
        for _ in 0..6 {
            let resp = app
                .request(
                    Request::post("/user/login/json")
                        .header("content-type", "application/json")
                        .header("x-forwarded-for", &fake_ip)
                        .body(Body::from(
                            serde_json::json!({ "username": username, "password": "wrongpassword" })
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await;
            statuses.push(resp.status());
        }

        assert!(
            statuses[..5].iter().all(|s| *s == StatusCode::UNAUTHORIZED),
            "first 5 attempts should be processed (401 bad creds), got {statuses:?}"
        );
        assert_eq!(
            statuses[5],
            StatusCode::TOO_MANY_REQUESTS,
            "the 6th attempt must be rate-limited (429), got {statuses:?}"
        );
    });
}
