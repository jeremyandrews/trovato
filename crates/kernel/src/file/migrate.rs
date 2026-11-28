//! Copy every file under a local uploads directory into a file store:
//! `trovato files migrate-to-s3` (BG16).
//!
//! The copy keeps each file's key (`local://<path under the directory>`), so
//! no row, field or item body changes: switching `FILE_STORAGE` to `s3`
//! afterwards serves the same URIs from the bucket.
//!
//! Every copy is checked: the file's SHA-256 is taken from the source, the
//! object is read back after it is written, and a file whose read-back differs
//! is reported as failed. The run is resumable and can be repeated: a file the
//! store already holds with the same SHA-256 is counted as already there and
//! not written again, so a run that was interrupted, or that failed on some
//! files, is finished by running it again. Nothing is ever deleted, from
//! either side; the local directory stays as it was, as the way back.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use super::storage::{FileStorage, is_not_found};

/// What a run did, file by file.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// Files found under the directory.
    pub total: u64,
    /// Files written to the store and verified.
    pub copied: u64,
    /// Files the store already held with the same SHA-256.
    pub already_there: u64,
    /// Files that could not be copied, with the reason; the run should be
    /// repeated once the cause is fixed.
    pub failed: Vec<(String, String)>,
}

/// Names Trovato writes for itself and never serves: the startup write check
/// and a local write's temporary file.
fn is_internal_name(name: &str) -> bool {
    name.starts_with(".trovato-write-check-")
        || (name.starts_with('.') && name.contains(".trovato-tmp-"))
}

/// Every regular file under `root`, as `/`-separated paths relative to it,
/// and every subdirectory that could not be listed (a run reports those and
/// goes on; only `root` itself failing stops it).
///
/// Symbolic links are not followed: a link could name a file outside the
/// directory, which must never be copied into the store.
async fn list_files(root: &Path) -> Result<(Vec<String>, Vec<(String, String)>)> {
    let mut out = Vec::new();
    let mut unlisted = Vec::new();
    let mut dirs: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let listed = list_dir(&dir).await;
        let entries = match listed {
            Ok(entries) => entries,
            Err(e) if dir == root => return Err(e),
            Err(e) => {
                let rel = dir.strip_prefix(root).unwrap_or(&dir).display().to_string();
                unlisted.push((format!("{rel}/"), format!("{e:#}")));
                continue;
            }
        };
        for (path, kind) in entries {
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if is_internal_name(&name) {
                    continue;
                }
                let rel = path
                    .strip_prefix(root)
                    .context("listed file outside the directory")?;
                let parts: Vec<String> = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    out.sort();
    Ok((out, unlisted))
}

/// The entries of one directory with their types (not following links).
async fn list_dir(dir: &Path) -> Result<Vec<(PathBuf, std::fs::FileType)>> {
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("failed to list {}", dir.display()))?;
    let mut out = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .with_context(|| format!("failed to list {}", dir.display()))?
    {
        out.push((entry.path(), entry.file_type().await?));
    }
    Ok(out)
}

/// What happened to one file.
enum Copied {
    Written,
    AlreadyThere,
}

async fn copy_one(root: &Path, key: &str, dest: &dyn FileStorage) -> Result<Copied> {
    // Checked again just before reading, not only when listed: a file swapped
    // for a symbolic link in between is refused, not followed.
    let path = root.join(key);
    let meta = tokio::fs::symlink_metadata(&path)
        .await
        .context("failed to read the local file")?;
    if !meta.file_type().is_file() {
        anyhow::bail!("no longer a regular file");
    }
    let data = tokio::fs::read(&path)
        .await
        .context("failed to read the local file")?;
    let want = Sha256::digest(&data);
    let uri = format!("local://{key}");
    match dest.read(&uri).await {
        Ok(stored) if Sha256::digest(&stored) == want => return Ok(Copied::AlreadyThere),
        Ok(_) => {}
        Err(e) if is_not_found(&e) => {}
        Err(e) => return Err(e.context("failed to check the store")),
    }
    dest.write(&uri, &data)
        .await
        .context("failed to write to the store")?;
    let back = dest
        .read(&uri)
        .await
        .context("failed to read back from the store")?;
    if Sha256::digest(&back) != want {
        anyhow::bail!("the copy read back from the store has a different SHA-256");
    }
    Ok(Copied::Written)
}

/// Copy every file under `root` into `dest`, `parallel` at a time.
///
/// Fails only when the directory cannot be listed; a file that cannot be
/// copied is recorded in [`MigrationReport::failed`] and the run goes on.
pub async fn copy_dir_to_storage(
    root: &Path,
    dest: Arc<dyn FileStorage>,
    parallel: usize,
) -> Result<MigrationReport> {
    let (keys, unlisted) = list_files(root).await?;
    tracing::info!(files = keys.len(), dir = %root.display(), "listed files to copy");
    let mut report = MigrationReport {
        total: keys.len() as u64,
        failed: unlisted,
        ..MigrationReport::default()
    };
    let root = Arc::new(root.to_path_buf());
    let mut tasks = JoinSet::new();
    let mut pending = keys.into_iter();
    let parallel = parallel.max(1);
    let mut done: u64 = 0;
    loop {
        while tasks.len() < parallel {
            let Some(key) = pending.next() else { break };
            let (root, dest) = (root.clone(), dest.clone());
            tasks.spawn(async move {
                let result = copy_one(&root, &key, dest.as_ref()).await;
                (key, result)
            });
        }
        let Some(joined) = tasks.join_next().await else {
            break;
        };
        let (key, result) = joined.context("a copy task panicked")?;
        match result {
            Ok(Copied::Written) => report.copied += 1,
            Ok(Copied::AlreadyThere) => report.already_there += 1,
            Err(e) => {
                tracing::warn!(key = %key, error = %format!("{e:#}"), "file not copied");
                report.failed.push((key, format!("{e:#}")));
            }
        }
        done += 1;
        if done.is_multiple_of(1000) {
            tracing::info!(done, total = report.total, "copying files");
        }
    }
    report.failed.sort();
    Ok(report)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn internal_names_are_skipped_and_ordinary_dotfiles_are_not() {
        assert!(is_internal_name(".trovato-write-check-0197"));
        assert!(is_internal_name(".photo.jpg.trovato-tmp-0197abc"));
        assert!(!is_internal_name(".well-known"));
        assert!(!is_internal_name("photo.jpg"));
    }
}
