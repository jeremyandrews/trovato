//! A Postgres database created for one test and dropped when that test ends.
//!
//! # Why it drops itself
//!
//! Three test files had their own copy of create-use-drop, and all three
//! dropped the database on the last line of the test body. That is the line a
//! failing assertion skips, so every red run left a database behind on the
//! server — fourteen of them on one developer machine before anybody looked,
//! each a full migrated schema. The teardown belongs in [`Drop`], where
//! unwinding runs it too.
//!
//! Migrations are deliberately not run here: that would mean depending on the
//! kernel from a crate the kernel dev-depends on. The caller runs them, which
//! is one line and keeps the dependency pointing one way.

use sqlx::{AssertSqlSafe, Connection, Executor, PgConnection, PgPool};
use uuid::Uuid;

/// A database created for one test, dropped when this value is.
pub struct ScratchDb {
    /// Connection URL of the server, without the database name.
    server_url: String,
    name: String,
    pool: Option<PgPool>,
}

impl ScratchDb {
    /// Create a database named for `label`, and connect to it.
    ///
    /// Unmigrated: the caller decides what schema it wants, which for the
    /// kernel's own tests is `trovato_kernel::db::run_migrations(db.pool())`.
    ///
    /// # Panics
    ///
    /// If `DATABASE_URL` is unset, or the server refuses the creation.
    pub async fn create(label: &str) -> Self {
        let database_url = crate::env::database_url();

        // Split `postgres://user:pass@host:port/dbname` into server and name,
        // tolerating a query string after the database name.
        let without_query = database_url
            .split_once('?')
            .map_or(database_url.as_str(), |(base, _)| base);
        let cut = without_query
            .rfind('/')
            .expect("DATABASE_URL must include a database name");
        let server_url = without_query[..cut].to_string();

        let name = format!("trovato_{label}_{}", Uuid::now_v7().simple());

        let mut admin = PgConnection::connect(&format!("{server_url}/postgres"))
            .await
            .expect("failed to connect to the postgres maintenance database");
        admin
            .execute(AssertSqlSafe(format!(r#"CREATE DATABASE "{name}""#)))
            .await
            .unwrap_or_else(|e| panic!("failed to create scratch database {name}: {e}"));
        drop(admin);

        let pool = PgPool::connect(&format!("{server_url}/{name}"))
            .await
            .expect("failed to connect to the scratch database");

        Self {
            server_url,
            name,
            pool: Some(pool),
        }
    }

    /// Drop every database `create(label)` made that nobody is connected to.
    ///
    /// For a scratch database held in a `static`, which Rust never drops: the
    /// next run clears what the last one left. A database in use by a run
    /// going on now has a connection, so it is left alone.
    ///
    /// # Panics
    ///
    /// If `DATABASE_URL` is unset or the server cannot be reached.
    pub async fn drop_stale(label: &str) {
        let database_url = crate::env::database_url();
        let without_query = database_url
            .split_once('?')
            .map_or(database_url.as_str(), |(base, _)| base);
        let cut = without_query
            .rfind('/')
            .expect("DATABASE_URL must include a database name");
        let server_url = &without_query[..cut];
        let mut admin = PgConnection::connect(&format!("{server_url}/postgres"))
            .await
            .expect("failed to connect to the postgres maintenance database");
        let pattern = format!("trovato\\_{}\\_%", label.replace('_', "\\_"));
        let stale: Vec<String> = sqlx::query_scalar(
            "SELECT datname FROM pg_database d WHERE datname LIKE $1 \
             AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname)",
        )
        .bind(&pattern)
        .fetch_all(&mut admin)
        .await
        .expect("list stale scratch databases");
        // No `FORCE`: a run that connects between the list and the drop keeps
        // its database, because a plain drop refuses one in use.
        for name in stale {
            let _ = admin
                .execute(AssertSqlSafe(format!(
                    r#"DROP DATABASE IF EXISTS "{name}""#
                )))
                .await;
        }
    }

    /// The pool connected to this database.
    pub fn pool(&self) -> &PgPool {
        self.pool.as_ref().expect("scratch pool was already closed")
    }

    /// The database's name on the server.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The full connection URL of this database.
    pub fn url(&self) -> String {
        format!("{}/{}", self.server_url, self.name)
    }
}

impl Drop for ScratchDb {
    fn drop(&mut self) {
        // Dropping the pool closes it; `WITH (FORCE)` deals with any connection
        // the test left open elsewhere.
        self.pool.take();

        let server_url = std::mem::take(&mut self.server_url);
        let name = std::mem::take(&mut self.name);

        // A dedicated thread with its own runtime. `Drop` here runs inside the
        // test's runtime, where `Runtime::block_on` panics, and dropping a
        // database is work that has to be awaited. Joined, so the database is
        // gone before the test function returns.
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async move {
                if let Ok(mut admin) =
                    PgConnection::connect(&format!("{server_url}/postgres")).await
                {
                    let _ = admin
                        .execute(AssertSqlSafe(format!(
                            r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#
                        )))
                        .await;
                }
            });
        })
        .join();
    }
}
