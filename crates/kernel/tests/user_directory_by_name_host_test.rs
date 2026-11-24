#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `find-user-by-name` and `find-users-by-name-prefix` (B179): a plugin
//! resolves a site account by the name a person typed, not just by an id it
//! already knows, so a plugin form like "add member by name" works for every
//! account on the site and not just the ones the plugin has already seen.
//!
//! As with `load-users` (`user_directory_host_test.rs`), these tests read the
//! exact JSON the host writes, so a leaked `mail`, `pass` or `data` fails
//! them directly rather than through a struct that would quietly drop it.
//!
//! Requires Postgres.

mod common;

use sqlx::Connection;
use trovato_kernel::models::user::ANONYMOUS_USER_ID;
use trovato_sdk::host_errors;
use uuid::Uuid;

async fn connect() -> sqlx::PgConnection {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&url).await;
    sqlx::PgConnection::connect(&url).await.expect("connect")
}

/// A user with a known address, password hash and private data, under a
/// caller-chosen name and status; returns the id.
async fn seeded_user(conn: &mut sqlx::PgConnection, name: &str, status: i16) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, status, data) \
         VALUES ($1, $2, 'argon2-hash-must-not-leak', $3, $4, '{\"secret\": \"private-data-must-not-leak\"}')",
    )
    .bind(id)
    .bind(name)
    .bind(format!("{name}@leak.example.test"))
    .bind(status)
    .execute(conn)
    .await
    .unwrap();
    id
}

async fn delete_user(conn: &mut sqlx::PgConnection, id: Uuid) {
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .ok();
}

/// A site-created account answers to its exact name.
#[tokio::test]
async fn exact_name_lookup_finds_a_site_created_account() {
    let mut conn = connect().await;
    let name = format!("dirname_{}", Uuid::now_v7().simple());
    let id = seeded_user(&mut conn, &name, 1).await;

    let json = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name)
        .await
        .expect("an answer");

    delete_user(&mut conn, id).await;

    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["id"], id.to_string(), "{json}");
    assert_eq!(value["name"], name, "{json}");
}

/// A name with no matching account answers JSON `null`, not an error: the
/// same "absent, not refused" answer `load-users` gives an id with no row.
#[tokio::test]
async fn exact_name_lookup_answers_null_for_no_such_user() {
    let mut conn = connect().await;
    let name = format!("nosuchuser_{}", Uuid::now_v7().simple());

    let json = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name)
        .await
        .expect("an answer");

    assert_eq!(json, "null", "{json}");
}

/// Case is handled exactly the way login handles it
/// (`User::find_by_name`'s `LOWER(name) = LOWER($1)`, see
/// `crates/kernel/src/models/user.rs`): `Dirname_Mixed` and `dirname_mixed`
/// both name the account registered as `DirMixed_...`.
#[tokio::test]
async fn exact_name_lookup_is_case_insensitive_like_login() {
    let mut conn = connect().await;
    let name = format!("DirMixed_{}", Uuid::now_v7().simple());
    let id = seeded_user(&mut conn, &name, 1).await;

    let upper = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name.to_uppercase())
        .await
        .expect("an answer");
    let lower = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name.to_lowercase())
        .await
        .expect("an answer");

    delete_user(&mut conn, id).await;

    let upper: serde_json::Value = serde_json::from_str(&upper).unwrap();
    let lower: serde_json::Value = serde_json::from_str(&lower).unwrap();
    assert_eq!(upper["id"], id.to_string(), "{upper}");
    assert_eq!(lower["id"], id.to_string(), "{lower}");
}

/// No address, hash or other private field ever reaches the answer, for
/// either call.
#[tokio::test]
async fn no_private_field_is_ever_returned() {
    let mut conn = connect().await;
    let name = format!("dirleak_{}", Uuid::now_v7().simple());
    let id = seeded_user(&mut conn, &name, 1).await;

    let exact = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name)
        .await
        .expect("an answer");
    let prefix_src = &name[..name.len() - 4];
    let prefix = trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, prefix_src)
        .await
        .expect("an answer");

    delete_user(&mut conn, id).await;

    for json in [&exact, &prefix] {
        for forbidden in [
            "\"mail\"",
            "\"pass\"",
            "\"data\"",
            "\"is_admin\"",
            "@leak.example.test",
            "argon2-hash-must-not-leak",
            "private-data-must-not-leak",
        ] {
            assert!(!json.contains(forbidden), "{forbidden} leaked: {json}");
        }
    }
    let exact_obj: serde_json::Value = serde_json::from_str(&exact).unwrap();
    let keys: std::collections::BTreeSet<&str> = exact_obj
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
        "{exact}"
    );
}

/// A prefix lookup returns at most `MAX_PREFIX_RESULTS`, sorted by name.
#[tokio::test]
async fn prefix_lookup_returns_at_most_the_fixed_limit_sorted() {
    let mut conn = connect().await;
    let prefix = format!("dirpfx_{}_", Uuid::now_v7().simple());
    let mut ids = Vec::new();
    // One more account than the fixed limit, so the cap is actually exercised.
    for i in 0..(trovato_kernel::host::user::MAX_PREFIX_RESULTS + 1) {
        let name = format!("{prefix}{i:03}");
        ids.push(seeded_user(&mut conn, &name, 1).await);
    }

    let json = trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, &prefix)
        .await
        .expect("an answer");

    for id in &ids {
        delete_user(&mut conn, *id).await;
    }

    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let array = value.as_array().expect("a JSON array");
    assert_eq!(
        array.len(),
        trovato_kernel::host::user::MAX_PREFIX_RESULTS as usize,
        "{json}"
    );
    let names: Vec<&str> = array.iter().map(|u| u["name"].as_str().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort_by_key(|n| n.to_lowercase());
    assert_eq!(names, sorted, "not sorted by name: {names:?}");
}

/// A prefix with no match answers an empty JSON array, not an error.
#[tokio::test]
async fn prefix_lookup_answers_empty_array_for_no_match() {
    let mut conn = connect().await;
    let prefix = format!("nomatch_{}_", Uuid::now_v7().simple());

    let json = trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, &prefix)
        .await
        .expect("an answer");

    assert_eq!(json, "[]", "{json}");
}

/// A `_` or `%` in the prefix matches itself, not any-character or
/// any-string: the prefix is a plugin-controlled value, not a pattern.
#[tokio::test]
async fn prefix_wildcards_are_escaped() {
    let mut conn = connect().await;
    let stem = Uuid::now_v7().simple().to_string();
    let literal_name = format!("dirwild_{stem}_x");
    let other_name = format!("dirwildzzz_{stem}_x");
    let literal_id = seeded_user(&mut conn, &literal_name, 1).await;
    let other_id = seeded_user(&mut conn, &other_name, 1).await;

    // A literal "_" right after "dirwild" must not act as a single-character
    // wildcard and match `other_name`'s "zzz" there too.
    let prefix = format!("dirwild_{stem}");
    let json = trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, &prefix)
        .await
        .expect("an answer");

    delete_user(&mut conn, literal_id).await;
    delete_user(&mut conn, other_id).await;

    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let names: Vec<&str> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&literal_name.as_str()), "{json}");
    assert!(!names.contains(&other_name.as_str()), "{json}");
}

/// Blocked accounts and the anonymous account follow the same visibility
/// rule `load-users` and the kernel's own `/admin/people` listing use for the
/// `user_directory` capability's calling context: no filter beyond the
/// capability gate itself. A blocked account answers like any other, and the
/// anonymous account (name `anonymous`, always present) is a real row that
/// answers to its own name too.
#[tokio::test]
async fn blocked_and_anonymous_accounts_are_visible_like_load_users() {
    let mut conn = connect().await;
    let name = format!("dirblocked_{}", Uuid::now_v7().simple());
    let id = seeded_user(&mut conn, &name, 0).await;

    let json = trovato_kernel::host::user::find_user_by_name_json(&mut conn, &name)
        .await
        .expect("an answer");

    delete_user(&mut conn, id).await;

    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["id"], id.to_string(), "{json}");
    assert_eq!(value["status"], "blocked", "{json}");

    let anon_json = trovato_kernel::host::user::find_user_by_name_json(&mut conn, "anonymous")
        .await
        .expect("an answer");
    let anon_value: serde_json::Value = serde_json::from_str(&anon_json).unwrap();
    assert_eq!(
        anon_value["id"],
        ANONYMOUS_USER_ID.to_string(),
        "{anon_json}"
    );
}

/// The length is checked before any query, for both calls.
#[tokio::test]
async fn too_short_or_too_long_input_is_refused() {
    let mut conn = connect().await;
    let too_long = "x".repeat(256);

    assert_eq!(
        trovato_kernel::host::user::find_user_by_name_json(&mut conn, "").await,
        Err(host_errors::ERR_USER_NAME_INVALID)
    );
    assert_eq!(
        trovato_kernel::host::user::find_user_by_name_json(&mut conn, &too_long).await,
        Err(host_errors::ERR_USER_NAME_INVALID)
    );
    assert_eq!(
        trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, "").await,
        Err(host_errors::ERR_USER_NAME_PREFIX_INVALID)
    );
    assert_eq!(
        trovato_kernel::host::user::find_users_by_name_prefix_json(&mut conn, &too_long).await,
        Err(host_errors::ERR_USER_NAME_PREFIX_INVALID)
    );
}
