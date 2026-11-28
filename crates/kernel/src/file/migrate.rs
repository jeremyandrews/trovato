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
//! either side. The local directory stays as it was: the way back only until
//! the first upload or delete on the bucket, after which it holds files deleted
//! since, with no row, which `/files/` would serve to anyone.

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
    name.starts_with(".trovato-write-check-") || name.starts_with(".trovato-tmp-")
}

/// Every regular file under `root`, as `/`-separated paths relative to it,
/// and every subdirectory that could not be listed (a run reports those and
/// goes on; only `root` itself failing stops it).
///
/// Symbolic links are not followed: a link could name a file outside the
/// directory, which must never be copied into the store. A file whose path is not
/// UTF-8 is reported with the directories that could not be listed.
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
                // A key is text: a name that is not UTF-8 has no key of its
                // own (a lossy spelling could be another file's name), so it
                // is reported, never copied under a guess.
                let parts: Option<Vec<&str>> =
                    rel.components().map(|c| c.as_os_str().to_str()).collect();
                match parts {
                    Some(parts) => out.push(parts.join("/")),
                    None => unlisted.push((
                        rel.display().to_string(),
                        "the name is not UTF-8, so it has no file store key; rename it and run again"
                            .to_string(),
                    )),
                }
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

/// Read the regular file at `key` under `root`, refusing anything that is no
/// longer a regular file inside `root` by the time it is opened.
///
/// On Unix every directory on the way is opened relative to the one before,
/// none of them through a symbolic link (`O_NOFOLLOW`), and the file itself
/// the same way, so a directory or file swapped for a link at any moment is
/// refused rather than followed out of the directory: there is no window
/// between a check and the open. Elsewhere the file's directory is checked to
/// resolve under `root` and the file not to be a link, just before opening.
fn read_inside(root: &Path, key: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let parts: Vec<&str> = key.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        anyhow::bail!("not a plain path under the directory");
    }
    let mut file = open_inside(root, &parts)?;
    if !file.metadata()?.file_type().is_file() {
        anyhow::bail!("no longer a regular file");
    }
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(data)
}

#[cfg(unix)]
fn open_inside(root: &Path, parts: &[&str]) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, open, openat};
    let (name, dirs) = parts.split_last().context("an empty path")?;
    let step = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    for part in dirs {
        dir = openat(&dir, *part, step, Mode::empty()).with_context(|| {
            format!("{part}/ is no longer a directory inside the uploads directory")
        })?;
    }
    // Non-blocking, so a FIFO put in the file's place is opened (and then
    // refused as not a regular file) rather than waited on.
    let fd = openat(
        &dir,
        *name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .context("no longer a regular file inside the uploads directory")?;
    Ok(std::fs::File::from(fd))
}

#[cfg(not(unix))]
fn open_inside(root: &Path, parts: &[&str]) -> Result<std::fs::File> {
    let path = parts
        .iter()
        .fold(root.to_path_buf(), |p, part| p.join(part));
    let parent = path.parent().context("a file with no directory")?;
    if !parent.canonicalize()?.starts_with(root.canonicalize()?) {
        anyhow::bail!("its directory is no longer inside the uploads directory");
    }
    if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
        anyhow::bail!("no longer a regular file");
    }
    Ok(std::fs::File::open(&path)?)
}

async fn copy_one(root: &Path, key: &str, dest: &dyn FileStorage) -> Result<Copied> {
    // Checked again as it is opened, not only when listed: a file or
    // directory swapped for a symbolic link in between is refused, not followed.
    let (root, owned_key) = (root.to_path_buf(), key.to_string());
    let data = tokio::task::spawn_blocking(move || read_inside(&root, &owned_key))
        .await
        .context("the read task failed")?
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
        assert!(is_internal_name(".trovato-tmp-0197abc"));
        assert!(!is_internal_name(".well-known"));
        assert!(!is_internal_name("photo.jpg"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_link_or_moved_outside_is_not_read() {
        let base = std::env::temp_dir().join(format!(
            "trovato-readinside-{}",
            uuid::Uuid::now_v7().simple()
        ));
        let root = base.join("uploads");
        let outside = base.join("outside");
        std::fs::create_dir_all(root.join("2026")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"not an upload").unwrap();
        std::fs::write(root.join("2026/plain.txt"), b"upload").unwrap();
        assert_eq!(read_inside(&root, "2026/plain.txt").unwrap(), b"upload");

        // The file itself replaced by a link.
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("2026/link.txt")).unwrap();
        assert!(read_inside(&root, "2026/link.txt").is_err());

        // A directory on its path replaced by a link to outside.
        std::os::unix::fs::symlink(&outside, root.join("swapped")).unwrap();
        assert!(read_inside(&root, "swapped/secret.txt").is_err());

        // A FIFO in a file's place is refused, not waited on.
        let fifo = std::ffi::CString::new(
            root.join("2026/fifo.txt")
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: `fifo` is a valid NUL-terminated path for the call's duration.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_inside(&root, "2026/fifo.txt").is_err());
        assert!(read_inside(&root, "2026/../2026/plain.txt").is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Linux file names are bytes; macOS refuses names that are not UTF-8, so
    /// this runs where such a name can exist.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_name_that_is_not_utf8_is_reported_not_copied_under_a_guess() {
        use std::os::unix::ffi::OsStrExt;
        let base =
            std::env::temp_dir().join(format!("trovato-notutf8-{}", uuid::Uuid::now_v7().simple()));
        let root = base.join("uploads");
        std::fs::create_dir_all(&root).unwrap();
        let bad = std::ffi::OsStr::from_bytes(b"photo-\xff.jpg");
        std::fs::write(root.join(bad), b"the real bytes").unwrap();
        // The name a lossy conversion would give it, holding other bytes.
        std::fs::write(root.join("photo-\u{fffd}.jpg"), b"a different file").unwrap();

        let (keys, failed) = list_files(&root).await.unwrap();
        assert_eq!(keys, vec!["photo-\u{fffd}.jpg".to_string()]);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert!(failed[0].1.contains("not UTF-8"), "{failed:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
