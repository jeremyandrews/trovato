#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `load-users` host function's answer, as bytes (**BL-105**).
//!
//! The end-to-end test reads the answer through the SDK's typed struct, which
//! would quietly drop a field the host should never have sent. These tests read
//! the exact JSON the host writes into the plugin's memory, so a leaked `mail`,
//! `pass` or `data` fails them.
//!
//! Requires Postgres.

mod common;

use sqlx::Connection;
use trovato_sdk::host_errors;
use uuid::Uuid;

async fn connect() -> sqlx::PgConnection {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&url).await;
    sqlx::PgConnection::connect(&url).await.expect("connect")
}

/// A user with a known address, password hash and private data; returns the id.
async fn seeded_user(conn: &mut sqlx::PgConnection) -> Uuid {
    let id = Uuid::now_v7();
    let name = format!("dirhost_{}", id.simple());
    sqlx::query(
        "INSERT INTO users (id, name, pass, mail, data) \
         VALUES ($1, $2, 'argon2-hash-must-not-leak', $3, '{\"secret\": \"private-data-must-not-leak\"}')",
    )
    .bind(id)
    .bind(&name)
    .bind(format!("{name}@leak.example.test"))
    .execute(conn)
    .await
    .unwrap();
    id
}

/// The serialized answer names the six public fields and nothing else: no
/// address, no password hash, no `data`, no credential.
#[tokio::test]
async fn the_answer_carries_public_fields_only() {
    let mut conn = connect().await;
    let id = seeded_user(&mut conn).await;
    let ids = serde_json::json!([id.to_string()]).to_string();

    let json = trovato_kernel::host::user::load_users_json(&mut conn, &ids)
        .await
        .expect("an answer");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(&mut conn)
        .await
        .ok();

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
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let user = &value.as_array().expect("a JSON array")[0];
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
        "{json}"
    );
    assert_eq!(user["status"], "active");
}

/// One database round trip for the whole batch.
///
/// `load_users_json` takes its executor by value, and a `&mut PgConnection` is
/// spent by the first query it runs, so a body that queried once per id would
/// not compile against this call. The runtime half: a batch of 100 is answered
/// from that one connection.
#[tokio::test]
async fn a_batch_is_one_round_trip() {
    let mut conn = connect().await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(seeded_user(&mut conn).await);
    }
    let mut batch: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    while batch.len() < 100 {
        batch.push(Uuid::now_v7().to_string());
    }
    let ids_json = serde_json::to_string(&batch).unwrap();

    let json = trovato_kernel::host::user::load_users_json(&mut conn, &ids_json)
        .await
        .expect("an answer");

    for id in &ids {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&mut conn)
            .await
            .ok();
    }
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value.as_array().unwrap().len(), 3, "{json}");
}

/// The limit and the shape are checked before any query.
#[tokio::test]
async fn too_many_or_malformed_ids_are_refused() {
    let mut conn = connect().await;
    let too_many: Vec<String> = (0..101).map(|_| Uuid::now_v7().to_string()).collect();
    assert_eq!(
        trovato_kernel::host::user::load_users_json(
            &mut conn,
            &serde_json::to_string(&too_many).unwrap()
        )
        .await,
        Err(host_errors::ERR_USER_IDS_TOO_MANY)
    );
    assert_eq!(
        trovato_kernel::host::user::load_users_json(&mut conn, r#"["not-a-uuid"]"#).await,
        Err(host_errors::ERR_USER_IDS_INVALID)
    );
    assert_eq!(
        trovato_kernel::host::user::load_users_json(&mut conn, r#"{"ids": []}"#).await,
        Err(host_errors::ERR_USER_IDS_INVALID)
    );
}
