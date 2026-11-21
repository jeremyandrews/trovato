#![allow(clippy::unwrap_used, clippy::expect_used)]
//! What decides whether `raw_sql` can be split into a read gate and a write gate.
//!
//! `docs/design/plugin-query-surface.md` section 9 keeps `raw_sql` one flag
//! unless a read-only `query-raw` can be shown to hold against a statement that
//! starts like a read and writes in the middle: a data-modifying CTE. These
//! tests are that showing, on a real PostgreSQL rather than on the parser's
//! opinion of PostgreSQL.
//!
//! - A first-keyword check cannot hold the line: PostgreSQL runs the `DELETE`
//!   inside `WITH gone AS (DELETE ...) SELECT ...` in an ordinary transaction.
//! - A transaction the server opens `READ ONLY`, which is what `query-raw` runs
//!   in (`crates/kernel/src/host/db.rs`, `Access::ReadOnly`), refuses the same
//!   statement with SQLSTATE `25006` and the row survives.
//! - The parser in front of it (`host::sql_guard`) refuses it before it reaches
//!   the server at all.
//!
//! Requires `DATABASE_URL`; runs in CI.

use sqlx::{Executor, PgPool};
use trovato_kernel::host::sql_guard;

/// A table of this file's own, so nothing else's rows are at stake.
const PROBE_TABLE: &str = "raw_sql_read_gate_probe";

const WRITING_CTE: &str =
    "WITH gone AS (DELETE FROM raw_sql_read_gate_probe RETURNING id) SELECT count(*) FROM gone";

async fn pool() -> PgPool {
    trovato_test_utils::env::load_dotenv();
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:5432/trovato".to_string());
    PgPool::connect(&url).await.expect("connect test DB")
}

/// A fresh probe table holding exactly one row.
async fn one_row(pool: &PgPool) {
    pool.execute("CREATE TABLE IF NOT EXISTS raw_sql_read_gate_probe (id integer PRIMARY KEY)")
        .await
        .expect("create probe table");
    pool.execute("DELETE FROM raw_sql_read_gate_probe")
        .await
        .expect("empty probe table");
    pool.execute("INSERT INTO raw_sql_read_gate_probe (id) VALUES (1)")
        .await
        .expect("seed probe row");
}

async fn rows(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM raw_sql_read_gate_probe")
        .fetch_one(pool)
        .await
        .expect("count probe rows")
}

/// The whole of the read/write split question, in one test so the table is
/// never shared between two tests running at once.
#[tokio::test]
async fn a_writing_cte_passes_a_keyword_check_and_not_a_read_only_transaction() {
    let pool = pool().await;
    assert!(WRITING_CTE.contains(PROBE_TABLE));

    // 1. It reads like a read: it starts with WITH, the keyword the old
    //    `query-raw` guard accepted. In an ordinary transaction PostgreSQL runs
    //    the DELETE, so a first-keyword check cannot stand between `query-raw`
    //    and a write.
    one_row(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    conn.execute("BEGIN").await.unwrap();
    let deleted: i64 = sqlx::query_scalar(WRITING_CTE)
        .fetch_one(&mut *conn)
        .await
        .expect("an ordinary transaction runs the writing CTE");
    conn.execute("COMMIT").await.unwrap();
    drop(conn);
    assert_eq!(deleted, 1, "the CTE reported the row it deleted");
    assert_eq!(rows(&pool).await, 0, "and the row is gone");

    // 2. The same statement in the transaction `query-raw` opens. The server
    //    refuses it, whatever any parser in front of it concluded.
    one_row(&pool).await;
    let mut conn = pool.acquire().await.unwrap();
    conn.execute("BEGIN READ ONLY").await.unwrap();
    let err = sqlx::query_scalar::<_, i64>(WRITING_CTE)
        .fetch_one(&mut *conn)
        .await
        .expect_err("a read-only transaction refuses the writing CTE");
    conn.execute("ROLLBACK").await.unwrap();
    drop(conn);
    let code = err
        .as_database_error()
        .and_then(|e| e.code().map(|c| c.to_string()));
    assert_eq!(
        code.as_deref(),
        Some("25006"),
        "read_only_sql_transaction, got {err}"
    );
    assert_eq!(rows(&pool).await, 1, "the row survives");

    // 3. The parser `query-raw` runs first refuses it too.
    assert_eq!(
        sql_guard::check_query_raw(WRITING_CTE),
        Err(sql_guard::RawSqlReject::WriteInsideQuery)
    );

    pool.execute("DROP TABLE raw_sql_read_gate_probe")
        .await
        .expect("drop probe table");
}
