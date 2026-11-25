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

/// Postgres truncates an identifier over this many bytes rather than refusing
/// it (`NAMEDATALEN` - 1, true of every build), so a name built from a label
/// long enough to cross it is silently split from what `CREATE DATABASE` ran:
/// the admin connection creates the truncated name, and the very next
/// connect, which asks for the name this module still holds in full, is
/// refused with "database ... does not exist" — the create appeared to
/// succeed and the failure lands one line later, on an unrelated-looking
/// assertion.
const POSTGRES_MAX_IDENTIFIER_BYTES: usize = 63;

/// `label`, cut down so `trovato_<label>_<32 hex chars>` fits in
/// [`POSTGRES_MAX_IDENTIFIER_BYTES`]. The UUID suffix is never shortened:
/// [`ScratchDb::drop_stale`] parses it back out to read the database's age.
///
/// Every label in the tree is plain ASCII, so byte and character boundaries
/// are the same thing; this still only ever cuts at a char boundary, so a
/// label that were not ASCII could not come back out an invalid `&str`.
fn fitting_label(label: &str) -> &str {
    const FIXED: usize = "trovato_".len() + "_".len() + 32; // the non-label parts
    let max_label_bytes = POSTGRES_MAX_IDENTIFIER_BYTES.saturating_sub(FIXED);
    if label.len() <= max_label_bytes {
        return label;
    }
    let cut = (0..=max_label_bytes)
        .rev()
        .find(|&i| label.is_char_boundary(i))
        .unwrap_or(0);
    &label[..cut]
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

        let name = format!(
            "trovato_{}_{}",
            fitting_label(label),
            Uuid::now_v7().simple()
        );

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

    /// Drop every database `create(label)` made more than five minutes ago that
    /// nobody is connected to.
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
        let prefix = format!("trovato_{}_", fitting_label(label));
        let pattern = format!("{}%", prefix.replace('_', "\\_"));
        let listed: Vec<String> = sqlx::query_scalar(
            "SELECT datname FROM pg_database d WHERE datname LIKE $1 \
             AND NOT EXISTS (SELECT 1 FROM pg_stat_activity a WHERE a.datname = d.datname)",
        )
        .bind(&pattern)
        .fetch_all(&mut admin)
        .await
        .expect("list stale scratch databases");
        // Only names `create(label)` makes, so `perf` does not match `perf_x_...`,
        // and only ones made a while ago: a run that has created its database
        // but not yet connected to it has no connection to protect it.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let stale = listed.into_iter().filter(|name| {
            name.strip_prefix(&prefix)
                .and_then(|id| Uuid::try_parse(id).ok())
                .and_then(|id| id.get_timestamp())
                .is_some_and(|ts| {
                    let (secs, nanos) = ts.to_unix();
                    let made_ms = u128::from(secs) * 1000 + u128::from(nanos / 1_000_000);
                    now_ms.saturating_sub(made_ms) > 5 * 60 * 1000
                })
        });
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

#[cfg(test)]
mod tests {
    use super::fitting_label;

    #[test]
    fn a_label_within_the_limit_is_unchanged() {
        assert_eq!(fitting_label("roleperms"), "roleperms");
        assert_eq!(fitting_label(""), "");
        // Exactly the longest label `trovato_<label>_<32 hex>` fits whole:
        // 63 - len("trovato_") - len("_") - 32 == 22.
        let exactly_fits = "a".repeat(22);
        assert_eq!(fitting_label(&exactly_fits), exactly_fits);
    }

    #[test]
    fn a_label_over_the_limit_is_cut_to_it() {
        // One byte over: cut by exactly one.
        let one_over = "a".repeat(23);
        assert_eq!(fitting_label(&one_over), "a".repeat(22));
        // The two labels that first exposed this: each produced a
        // "database ... does not exist" on the very next connect, because
        // CREATE DATABASE truncated the name but the client kept asking for
        // it in full.
        assert_eq!(
            fitting_label("cfgimport_rolepluginperm").len(),
            22,
            "trovato_<this>_<32 hex> must be at most 63 bytes"
        );
        assert_eq!(fitting_label("bg10_plugin_incompat_cli").len(), 22);
    }
}
