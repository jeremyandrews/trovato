#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plugin looks up public facts about users it is not serving (**BL-105**,
//! G-NO-USER-DIRECTORY): `user-api.load-users`.
//!
//! `user-api` answered only about the request's own user, so a plugin sending a
//! digest to its subscribers could not even learn their names. `load-users`
//! answers for a batch of ids, and only for a plugin that declares
//! `user_directory = true`: declaring `user-api` alone does not grant it.
//!
//! Driven through the **real** `plugins/test_members` wasm over HTTP: the
//! shipped manifest declares the capability, and a copy of the plugin with the
//! line removed shows the refusal.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_members/`.

mod common;

use std::sync::OnceLock;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::TestApp;
use trovato_sdk::host_errors;
use uuid::Uuid;

const PLUGIN: &str = "test_members";
const BUCKET: &str = "user_directory_test";

static CAPABLE: OnceLock<TestApp> = OnceLock::new();
static WITHOUT_CAPABILITY: OnceLock<TestApp> = OnceLock::new();

async fn install_fixture() {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "0.100.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;
}

/// Build an app on another thread, the way `plugin_mail_test` does: the
/// construction future is not `Send`.
fn build(extra_dir: Option<std::path::PathBuf>) -> TestApp {
    let handle = common::shared_runtime_handle();
    std::thread::spawn(move || {
        handle.block_on(async move {
            install_fixture().await;
            TestApp::with_config(move |config| {
                if std::env::var_os("PLUGINS_DIR").is_none() {
                    config.plugins_dirs = vec![common::project_root().join("plugins")];
                }
                if let Some(dir) = extra_dir {
                    config.plugins_dirs.push(dir);
                }
            })
            .await
        })
    })
    .join()
    .expect("app init thread panicked")
}

/// The fixture as shipped: `user-api` and `user_directory = true`.
fn capable() -> &'static TestApp {
    CAPABLE.get_or_init(|| build(None))
}

/// The same wasm with `user_directory` removed from its manifest.
fn without_capability() -> &'static TestApp {
    WITHOUT_CAPABILITY.get_or_init(|| {
        let dir = common::plugin_copy::copy_with_manifest(PLUGIN, |manifest| {
            assert!(
                manifest.contains("user_directory = true"),
                "the shipped fixture declares the capability"
            );
            manifest.replace("user_directory = true", "")
        });
        build(Some(dir))
    })
}

/// GET `/test-members/users?ids=...` and return the JSON answer.
async fn load(app: &TestApp, ids: &[String]) -> serde_json::Value {
    let response = app
        .request(
            Request::get(format!("/test-members/users?ids={}", ids.join(",")))
                .header("x-forwarded-for", common::test_ip_for(BUCKET))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "the fixture answers JSON: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// GET `/test-members/find-by-name?name=...` and return the JSON answer.
async fn find_by_name(app: &TestApp, name: &str) -> serde_json::Value {
    let response = app
        .request(
            Request::get(format!(
                "/test-members/find-by-name?name={}",
                urlencoding::encode(name)
            ))
            .header("x-forwarded-for", common::test_ip_for(BUCKET))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "the fixture answers JSON: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// GET `/test-members/find-by-name-prefix?prefix=...` and return the JSON answer.
async fn find_by_name_prefix(app: &TestApp, prefix: &str) -> serde_json::Value {
    let response = app
        .request(
            Request::get(format!(
                "/test-members/find-by-name-prefix?prefix={}",
                urlencoding::encode(prefix)
            ))
            .header("x-forwarded-for", common::test_ip_for(BUCKET))
            .body(Body::empty())
            .unwrap(),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or_else(|_| {
        panic!(
            "the fixture answers JSON: {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

/// An active user and a blocked one; returns their ids and names.
async fn two_users(app: &TestApp) -> ((Uuid, String), (Uuid, String)) {
    let active = common::username("dir-active");
    let blocked = common::username("dir-blocked");
    for name in [&active, &blocked] {
        app.create_test_user(
            name,
            "dir-Password-123",
            &format!("{name}@dir.example.test"),
        )
        .await;
    }
    let blocked_id = common::user_id_of(app, &blocked).await;
    sqlx::query("UPDATE users SET status = 0 WHERE id = $1")
        .bind(blocked_id)
        .execute(&app.db)
        .await
        .unwrap();
    (
        (common::user_id_of(app, &active).await, active),
        (blocked_id, blocked),
    )
}

/// With the capability, a batch answers public fields for the users that
/// exist, and leaves out an id that names nobody.
#[test]
fn a_batch_answers_public_fields_for_the_users_that_exist() {
    common::run_test(async {
        let app = capable();
        let ((active_id, active), (blocked_id, blocked)) = two_users(app).await;
        let nobody = Uuid::now_v7();

        let answer = load(
            app,
            &[
                active_id.to_string(),
                blocked_id.to_string(),
                nobody.to_string(),
            ],
        )
        .await;

        let users = answer["users"]
            .as_array()
            .unwrap_or_else(|| panic!("expected users: {answer}"));
        assert_eq!(users.len(), 2, "only the users that exist: {answer}");
        for user in users {
            let keys: std::collections::BTreeSet<&str> = user
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                ["created", "id", "language", "name", "status", "timezone"]
                    .into_iter()
                    .collect(),
                "public fields only: {user}"
            );
        }
        let by_id = |id: Uuid| {
            users
                .iter()
                .find(|u| u["id"] == id.to_string())
                .unwrap_or_else(|| panic!("{id} in {answer}"))
        };
        assert_eq!(by_id(active_id)["name"], active.as_str());
        assert_eq!(by_id(active_id)["status"], "active");
        assert_eq!(by_id(blocked_id)["name"], blocked.as_str());
        assert_eq!(by_id(blocked_id)["status"], "blocked");
        let text = answer.to_string();
        assert!(!text.contains("@dir.example.test"), "no address: {text}");
    });
}

/// Without `user_directory` the call is refused, though `user-api` is declared.
#[test]
fn without_the_capability_the_call_is_denied() {
    common::run_test(async {
        let app = without_capability();
        let ((active_id, _), _) = two_users(app).await;

        let answer = load(app, &[active_id.to_string()]).await;

        assert_eq!(
            answer["code"],
            host_errors::ERR_USER_DIRECTORY_DENIED,
            "expected the directory to be denied: {answer}"
        );
    });
}

/// More than the batch limit of 100 is refused.
#[test]
fn more_than_the_batch_limit_is_refused() {
    common::run_test(async {
        let app = capable();
        let ids: Vec<String> = (0..101).map(|_| Uuid::now_v7().to_string()).collect();

        let answer = load(app, &ids).await;

        assert_eq!(
            answer["code"],
            host_errors::ERR_USER_IDS_TOO_MANY,
            "{answer}"
        );
    });
}

/// A malformed id is refused, rather than skipped.
#[test]
fn a_malformed_id_is_refused() {
    common::run_test(async {
        let app = capable();

        let answer = load(app, &["not-a-user-id".to_string()]).await;

        assert_eq!(
            answer["code"],
            host_errors::ERR_USER_IDS_INVALID,
            "{answer}"
        );
    });
}

/// `find-user-by-name` answers the account's public facts, case-insensitively,
/// through the real wasm boundary.
#[test]
fn find_by_name_answers_public_fields_case_insensitively() {
    common::run_test(async {
        let app = capable();
        let ((active_id, active), _) = two_users(app).await;

        let answer = find_by_name(app, &active.to_uppercase()).await;

        let user = &answer["user"];
        assert_eq!(user["id"], active_id.to_string(), "{answer}");
        assert_eq!(user["name"], active.as_str(), "{answer}");
        let keys: std::collections::BTreeSet<&str> = user
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["created", "id", "language", "name", "status", "timezone"]
                .into_iter()
                .collect(),
            "public fields only: {user}"
        );
    });
}

/// A name with no account answers `null`, not an error.
#[test]
fn find_by_name_answers_null_for_no_such_user() {
    common::run_test(async {
        let app = capable();

        let answer = find_by_name(app, &format!("nosuchuser-{}", Uuid::now_v7())).await;

        assert!(answer["user"].is_null(), "{answer}");
    });
}

/// Without `user_directory` the call is refused, like `load-users`.
#[test]
fn find_by_name_without_the_capability_is_denied() {
    common::run_test(async {
        let app = without_capability();

        let answer = find_by_name(app, "anyone").await;

        assert_eq!(
            answer["code"],
            host_errors::ERR_USER_DIRECTORY_DENIED,
            "{answer}"
        );
    });
}

/// `find-users-by-name-prefix` answers a sorted, capped list through the real
/// wasm boundary, and never an address.
#[test]
fn find_by_name_prefix_answers_a_sorted_capped_list() {
    common::run_test(async {
        let app = capable();
        let stem = Uuid::now_v7().simple().to_string();
        let prefix = format!("dirpfx-{stem}-");
        for i in 0..3 {
            let name = format!("{prefix}{i}");
            app.create_test_user(
                &name,
                "dir-Password-123",
                &format!("{name}@dirpfx.example.test"),
            )
            .await;
        }

        let answer = find_by_name_prefix(app, &prefix).await;

        let users = answer["users"]
            .as_array()
            .unwrap_or_else(|| panic!("expected users: {answer}"));
        assert_eq!(users.len(), 3, "{answer}");
        let names: Vec<&str> = users.iter().map(|u| u["name"].as_str().unwrap()).collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|n| n.to_lowercase());
        assert_eq!(names, sorted, "not sorted: {names:?}");
        let text = answer.to_string();
        assert!(!text.contains("@dirpfx.example.test"), "no address: {text}");
    });
}

/// An empty prefix is refused, rather than answering every account.
#[test]
fn find_by_name_prefix_empty_is_refused() {
    common::run_test(async {
        let app = capable();

        let answer = find_by_name_prefix(app, "").await;

        assert_eq!(
            answer["code"],
            host_errors::ERR_USER_NAME_PREFIX_INVALID,
            "{answer}"
        );
    });
}

/// Leave the fixture plugin disabled so it does not load in other binaries.
#[test]
fn zz_leaves_the_fixture_plugin_disabled() {
    common::run_test(async {
        let app = capable();
        sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
            .bind(PLUGIN)
            .execute(&app.db)
            .await
            .ok();
    });
}
