//! `trovato files migrate`: move every file from one physical backend
//! (`local` or `s3`) to another, keeping each file's scheme (public/private)
//! and relative path, verifying size and checksum before the database row is
//! rewritten to point at the new copy.
//!
//! Resumable with no separate journal: each batch is selected by querying for
//! rows still on the `from` scheme, so an interrupted run simply leaves some
//! rows already rewritten (skipped on the next run, since they no longer
//! match) and others not yet touched (picked up again). A row is only
//! rewritten after its new copy is read back and verified, and the source
//! object is only deleted after that rewrite commits — a crash at any point
//! leaves either the original untouched, or both copies present (an orphan on
//! the source backend, harmless, cleaned up by a subsequent run attempting
//! the same row again under a fresh query only if it is still on `from`,
//! which it no longer is — the one sanctioned leftover is a stray object on
//! the OLD backend when the process dies between the commit and the delete;
//! operators can always re-run `migrate` with the same `--from`/`--to` to
//! converge, and harmless because nothing references it by its old URI once
//! the row is rewritten).

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use sqlx::PgPool;
use uuid::Uuid;

use super::sigv4::sha256_hex;
use super::storage::FileStorage;

/// How many files one query-and-process round handles, so a large table does
/// not have to fit in memory and progress is visible between rounds.
const BATCH_SIZE: i64 = 50;

/// The four backend instances a migration between two physical kinds needs:
/// each of the two schemes (public, private), built against both the source
/// and the destination kind, since a file's scheme decides its key prefix
/// (for S3) or directory (for local) and must carry over unchanged.
pub struct MigrationBackends {
    pub public_from: Arc<dyn FileStorage>,
    pub public_to: Arc<dyn FileStorage>,
    pub private_from: Arc<dyn FileStorage>,
    pub private_to: Arc<dyn FileStorage>,
}

impl MigrationBackends {
    fn for_visibility(&self, visibility: i16) -> (&Arc<dyn FileStorage>, &Arc<dyn FileStorage>) {
        if visibility == 1 {
            (&self.private_from, &self.private_to)
        } else {
            (&self.public_from, &self.public_to)
        }
    }
}

/// What one `migrate_files` call did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MigrateSummary {
    /// Files copied, verified and rewritten this run.
    pub moved: u64,
    /// Rows a concurrent run had already finished by the time this run
    /// reached them (its own copy was discarded, not double-counted).
    pub already_done_elsewhere: u64,
}

/// Move every `file_managed` row currently on `from_scheme` to `to_scheme`.
/// See the module doc for the resume and failure-safety argument.
pub async fn migrate_files(
    pool: &PgPool,
    from_scheme: &str,
    to_scheme: &str,
    backends: &MigrationBackends,
    mut progress: impl FnMut(&str),
) -> Result<MigrateSummary> {
    if from_scheme == to_scheme {
        bail!("--from and --to must name different backends");
    }
    let like = format!("{from_scheme}://%");
    let mut summary = MigrateSummary::default();

    loop {
        let batch: Vec<(Uuid, String, i16)> = sqlx::query_as(
            "SELECT id, uri, visibility FROM file_managed WHERE uri LIKE $1 ORDER BY id LIMIT $2",
        )
        .bind(&like)
        .bind(BATCH_SIZE)
        .fetch_all(pool)
        .await
        .context("failed to query files to migrate")?;
        if batch.is_empty() {
            break;
        }

        for (id, uri, visibility) in batch {
            let (source, dest) = backends.for_visibility(visibility);
            let path = uri
                .strip_prefix(&like[..like.len() - 1]) // "{from_scheme}://" (drop the trailing '%')
                .with_context(|| format!("{uri} lost its {from_scheme}:// prefix mid-run"))?;
            let new_uri = format!("{to_scheme}://{path}");

            let data = source
                .read(&uri)
                .await
                .with_context(|| format!("reading {uri} from {from_scheme}"))?;
            let source_checksum = sha256_hex(&data);

            dest.write(&new_uri, &data)
                .await
                .with_context(|| format!("writing {new_uri} to {to_scheme}"))?;
            let written = dest
                .read(&new_uri)
                .await
                .with_context(|| format!("reading {new_uri} back to verify"))?;
            if written.len() != data.len() || sha256_hex(&written) != source_checksum {
                bail!(
                    "{uri}: the copy on {to_scheme} did not verify (size or checksum mismatch); \
                     left both copies in place for inspection, nothing was rewritten"
                );
            }

            let updated = sqlx::query(
                "UPDATE file_managed SET uri = $1, changed = EXTRACT(EPOCH FROM NOW())::bigint \
                 WHERE id = $2 AND uri = $3",
            )
            .bind(&new_uri)
            .bind(id)
            .bind(&uri)
            .execute(pool)
            .await
            .with_context(|| format!("rewriting file_managed.uri for {id}"))?;

            if updated.rows_affected() == 0 {
                // A concurrent migrate run already moved this row between our
                // read and our write: drop our redundant copy and move on.
                let _ = dest.delete(&new_uri).await;
                summary.already_done_elsewhere += 1;
                continue;
            }

            if let Err(e) = source.delete(&uri).await {
                tracing::warn!(
                    error = %e, uri = %uri,
                    "migrated file's source copy could not be deleted (orphaned, harmless)"
                );
            }
            summary.moved += 1;
            progress(&format!("{id} {uri} -> {new_uri}"));
        }
    }

    Ok(summary)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::file::storage::LocalFileStorage;

    /// A lazy pool never actually connects until a query runs — right for
    /// this one test, whose refusal happens before any query, and which
    /// therefore needs no live database at all.
    fn lazy_pool() -> PgPool {
        PgPool::connect_lazy("postgres://trovato:trovato@localhost:5432/unused")
            .expect("a syntactically valid URL parses without connecting")
    }

    fn local(dir: &std::path::Path) -> Arc<dyn FileStorage> {
        Arc::new(LocalFileStorage::new(dir, "/files"))
    }

    #[tokio::test]
    async fn refuses_to_migrate_a_backend_to_itself() {
        let dir = std::env::temp_dir().join(format!("trovato-relocate-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = local(&dir);
        let backends = MigrationBackends {
            public_from: storage.clone(),
            public_to: storage.clone(),
            private_from: storage.clone(),
            private_to: storage,
        };
        let err = migrate_files(&lazy_pool(), "local", "local", &backends, |_| {})
            .await
            .expect_err("same scheme must be refused");
        assert!(err.to_string().contains("must name different backends"));
        std::fs::remove_dir_all(&dir).ok();
    }
}

// The migration's actual byte-moving behavior (copy, verify, rewrite,
// resume after a simulated interruption) is covered end to end in
// `tests/file_migrate_test.rs`, against a real database and the in-process
// mock S3 server (`tests/support/mock_s3.rs`) — that is a true two-backend
// test (local disk and a second, independent backend), which this module's
// unit tests cannot be without either a live network service or pretending
// one physical backend is two, which would not exercise anything this
// function does not already get right by construction.
