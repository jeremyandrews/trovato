#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The request profiler's security properties, pinned before it was built.
//!
//! A profiler sees everything a request does, which makes it the one part of
//! the kernel best placed to leak what a request carries. These tests state
//! what it may show and to whom:
//!
//! - its pages are for holders of `view request profiles` (the superuser
//!   bypass applies), an anonymous visitor is sent to log in, anyone else is
//!   refused;
//! - switched off, the pages do not exist and nothing is recorded;
//! - a profile never holds a cookie, a header, a session id, a request body, a
//!   query string value or a cache key;
//! - a path is stored as the route it matched, so a secret path segment (the
//!   password reset token) never reaches a profile;
//! - SQL is stored with its literal values replaced by a placeholder unless an
//!   operator opts in.
//!
//! Requires Postgres + Redis (the shared `TestApp` and one profiled app); runs
//! in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use tokio::sync::OnceCell;
use tracing::Dispatch;
use tracing::instrument::{Instrument, WithSubscriber};
use tracing_subscriber::layer::SubscriberExt;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// The permission the profiler pages ask for.
const PERMISSION: &str = "view request profiles";

/// The list of recent requests.
const LIST: &str = "/admin/reports/requests";

/// Its own client address, so this file does not spend the shared rate-limit
/// bucket that requests without `X-Forwarded-For` fall into.
const CLIENT_IP: &str = "10.76.1.1";

/// An app with the profiler on, and the subscriber its layer runs in.
///
/// The layer is installed per request (`with_subscriber`) rather than as the
/// process default, so nothing else in this binary is profiled into it.
struct Profiled {
    app: TestApp,
    dispatch: Dispatch,
}

static PROFILED: OnceCell<Profiled> = OnceCell::const_new();

async fn profiled() -> &'static Profiled {
    PROFILED
        .get_or_init(|| async {
            let app = TestApp::with_config(|config| {
                config.runtime.profiler.enabled = true;
            })
            .await;
            let store = app
                .state
                .profiler()
                .expect("PROFILER_ENABLED gives the app a profile store")
                .clone();
            let layer = trovato_kernel::profiler::layer(store, app.state.metrics().clone());
            let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));
            Profiled { app, dispatch }
        })
        .await
}

/// Send `request` as `cookies` (empty for anonymous) through `app`, profiled
/// when `dispatch` is given.
async fn send(
    app: &TestApp,
    dispatch: Option<&Dispatch>,
    mut request: Request<Body>,
    cookies: &str,
) -> (StatusCode, Option<String>, String) {
    request
        .headers_mut()
        .insert("x-forwarded-for", CLIENT_IP.parse().unwrap());
    let response = match dispatch {
        Some(d) => {
            app.request_with_cookies(request, cookies)
                .with_subscriber(d.clone())
                .await
        }
        None => app.request_with_cookies(request, cookies).await,
    };
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        location,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

/// Every stored profile, serialised, newest first.
fn stored(p: &Profiled) -> Vec<(String, String)> {
    p.app
        .state
        .profiler()
        .expect("profiler enabled")
        .recent()
        .iter()
        .map(|profile| {
            (
                profile.route.clone(),
                serde_json::to_string(profile.as_ref()).expect("a profile serialises"),
            )
        })
        .collect()
}

/// The id the detail page of the newest stored profile is addressed by.
fn newest_id(p: &Profiled) -> u64 {
    p.app
        .state
        .profiler()
        .expect("profiler enabled")
        .recent()
        .first()
        .expect("at least one profile is stored")
        .id
}

#[test]
fn an_anonymous_visitor_is_sent_to_log_in() {
    run_test(async {
        let p = profiled().await;
        send(&p.app, Some(&p.dispatch), get("/"), "").await;
        let id = newest_id(p);

        for path in [LIST.to_string(), format!("{LIST}/{id}")] {
            let (status, location, _) = send(&p.app, None, get(&path), "").await;
            assert!(
                status.is_redirection(),
                "anonymous GET {path} answered {status}, not a redirect to login"
            );
            assert!(
                location
                    .as_deref()
                    .is_some_and(|l| l.contains("/user/login")),
                "anonymous GET {path} redirected to {location:?}, not /user/login"
            );
        }
    });
}

#[test]
fn a_user_without_the_permission_is_refused() {
    run_test(async {
        let p = profiled().await;
        let (_, cookies) = user_holding(&p.app, "prof-none", &[]).await;
        send(&p.app, Some(&p.dispatch), get("/"), "").await;
        let id = newest_id(p);

        for path in [LIST.to_string(), format!("{LIST}/{id}")] {
            let (status, _, _) = send(&p.app, None, get(&path), &cookies).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "GET {path} without {PERMISSION}"
            );
        }
    });
}

#[test]
fn a_user_with_the_permission_reads_both_pages() {
    run_test(async {
        let p = profiled().await;
        let (_, cookies) = user_holding(&p.app, "prof-holder", &[PERMISSION]).await;
        send(&p.app, Some(&p.dispatch), get("/user/login"), "").await;
        let id = newest_id(p);

        let (status, _, list) = send(&p.app, None, get(LIST), &cookies).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GET {LIST} with {PERMISSION}: {list}"
        );
        assert!(
            list.contains("/user/login"),
            "the list does not show the request just made"
        );

        let (status, _, detail) = send(&p.app, None, get(&format!("{LIST}/{id}")), &cookies).await;
        assert_eq!(status, StatusCode::OK, "GET the newest profile: {detail}");

        let (status, _, _) =
            send(&p.app, None, get(&format!("{LIST}/{}", u64::MAX)), &cookies).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "a profile that is not stored"
        );
    });
}

#[test]
fn switched_off_the_pages_do_not_exist_and_nothing_is_recorded() {
    run_test(async {
        let app = shared_app().await;
        assert!(
            app.state.profiler().is_none(),
            "PROFILER_ENABLED defaults to off, and off means no profile store"
        );

        let (_, holder) = user_holding(app, "prof-off-holder", &[PERMISSION]).await;
        let (_, nobody) = user_holding(app, "prof-off-none", &[]).await;
        let admin_name = common::username("prof-off-admin");
        let admin = app
            .create_and_login_admin(
                &admin_name,
                "test-password-123",
                &format!("{admin_name}@example.com"),
            )
            .await;

        for cookies in ["", nobody.as_str(), holder.as_str(), admin.as_str()] {
            for path in [LIST.to_string(), format!("{LIST}/1")] {
                let (status, _, _) = send(app, None, get(&path), cookies).await;
                assert_eq!(
                    status,
                    StatusCode::NOT_FOUND,
                    "GET {path} with the profiler off"
                );
            }
        }
        assert!(
            app.state.profiler().is_none(),
            "nothing came to record into"
        );
    });
}

#[test]
fn a_profile_holds_no_cookie_header_session_body_query_value_or_cache_key() {
    run_test(async {
        let p = profiled().await;
        p.app.ensure_conference_type().await;
        let admin_ctx =
            UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let item = p
            .app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "conference".to_string(),
                    title: "Profiler secrets fixture".to_string(),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("profiler security test".to_string()),
                },
                &admin_ctx,
            )
            .await
            .expect("create item")
            .id;

        let (_, cookies) = user_holding(&p.app, "prof-secrets", &[PERMISSION]).await;
        let session_id = cookies
            .split(';')
            .filter_map(|pair| pair.split_once('='))
            .map(|(_, value)| value.trim().to_string())
            .find(|value| !value.is_empty())
            .expect("the login set a session cookie");

        // A page read twice (a cache miss, then a hit, keyed by the item id),
        // with a query string, a custom header and a bearer token.
        for _ in 0..2 {
            let request = Request::get(format!(
                "/item/{item}?q=b76-query-secret&token=b76-token-secret"
            ))
            .header("x-b76-probe", "b76-header-secret")
            .header(header::AUTHORIZATION, "Bearer b76-bearer-secret")
            .body(Body::empty())
            .unwrap();
            send(&p.app, Some(&p.dispatch), request, &cookies).await;
        }
        // A form post with a body.
        let request = Request::post("/user/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("name=b76-nobody&pass=b76-body-secret"))
            .unwrap();
        send(&p.app, Some(&p.dispatch), request, &cookies).await;

        let secrets = [
            "b76-query-secret",
            "b76-token-secret",
            "b76-header-secret",
            "b76-bearer-secret",
            "x-b76-probe",
            "b76-body-secret",
            "b76-nobody",
            session_id.as_str(),
            &item.to_string(),
        ];

        let profiles = stored(p);
        assert!(
            profiles.iter().any(|(route, _)| route == "/item/{id}"),
            "the item page was not profiled: {:?}",
            profiles.iter().map(|(r, _)| r).collect::<Vec<_>>()
        );
        for (route, json) in &profiles {
            for secret in &secrets {
                assert!(
                    !json.contains(secret),
                    "the profile of {route} holds {secret:?}: {json}"
                );
            }
        }

        // Nor do the pages that show them.
        let mut pages = vec![LIST.to_string()];
        pages.extend(
            p.app
                .state
                .profiler()
                .expect("profiler enabled")
                .recent()
                .iter()
                .map(|profile| format!("{LIST}/{}", profile.id)),
        );
        for page in pages {
            let (status, _, html) = send(&p.app, None, get(&page), &cookies).await;
            assert_eq!(status, StatusCode::OK, "GET {page}");
            for secret in &secrets {
                assert!(!html.contains(secret), "{page} shows {secret:?}");
            }
        }

        p.app.state.items().delete(item, &admin_ctx).await.ok();
    });
}

#[test]
fn a_secret_path_segment_is_stored_as_its_route_template() {
    run_test(async {
        let p = profiled().await;
        send(
            &p.app,
            Some(&p.dispatch),
            get("/user/password-reset/b76-reset-token-secret"),
            "",
        )
        .await;

        let profiles = stored(p);
        assert!(
            profiles
                .iter()
                .any(|(route, _)| route == "/user/password-reset/{token}"),
            "no profile under the reset route's template: {:?}",
            profiles.iter().map(|(r, _)| r).collect::<Vec<_>>()
        );
        for (route, json) in &profiles {
            assert!(
                !json.contains("b76-reset-token-secret"),
                "the profile of {route} holds the reset token: {json}"
            );
        }
    });
}

#[test]
fn sql_literals_are_replaced_by_a_placeholder_by_default() {
    run_test(async {
        let p = profiled().await;
        let route = "/b76-literal-probe";

        // A request span of the profiler's own, around a statement whose text
        // carries literals, as a handler's would.
        let span = tracing::dispatcher::with_default(&p.dispatch, || {
            trovato_kernel::profiler::request_span("GET", route)
        });
        async {
            sqlx::query("SELECT 'b76-literal-secret' AS s, 4276 AS n")
                .execute(&p.app.db)
                .await
                .expect("the probe statement runs");
        }
        .instrument(span)
        .with_subscriber(p.dispatch.clone())
        .await;

        let (_, json) = stored(p)
            .into_iter()
            .find(|(r, _)| r == route)
            .expect("the probe request was stored");
        let profile: serde_json::Value = serde_json::from_str(&json).unwrap();
        let statements: Vec<String> = profile["queries"]
            .as_array()
            .expect("a profile lists its queries")
            .iter()
            .filter_map(|q| q["statement"].as_str().map(str::to_string))
            .collect();
        let probe = statements
            .iter()
            .find(|s| s.contains("SELECT"))
            .unwrap_or_else(|| panic!("the probe statement was not recorded: {statements:?}"));
        assert!(
            !probe.contains("b76-literal-secret"),
            "a string literal survived: {probe}"
        );
        assert!(
            !probe.contains("4276"),
            "a number literal survived: {probe}"
        );
        assert!(
            probe.contains('?'),
            "no placeholder where the literals were: {probe}"
        );
    });
}
