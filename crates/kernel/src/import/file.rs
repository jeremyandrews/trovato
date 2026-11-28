//! Files already copied into the file store, registered as `file_managed`
//! rows with the id, owner and date the record gives.
//!
//! A migrated site's files arrive in the store before the importer ever runs:
//! a port copies them in (a directory, `UPLOADS_DIR`, or an S3 compatible
//! bucket — whatever `FILE_STORAGE` names), then writes a `file` record
//! naming where each one landed. The importer never writes, moves or rewrites
//! a byte of them — it reads the file at `path` through the site's own
//! [`FileStorage`](crate::file::FileStorage) to check it the way an upload is
//! checked ([`FileService::validate_file`]) and to measure its own size, then
//! writes one `file_managed` row whose `uri` is that same path. An item's File
//! field or embedded `local://`/`/files/` reference finds it exactly as it
//! would find an upload, because `file_refs::sync_item_file_references` only
//! ever looks the file up by `file_managed` id or uri.

use std::path::{Component, Path};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::FileRecord;
use super::{Outcome, Run, require_id, require_user};
use crate::file::{FileService, FileStatus, is_not_found};

/// A stored file, as a record would describe it, with the storage URI import
/// does not let a record change.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredFile {
    uri: String,
    owner_id: Uuid,
    filename: String,
    filemime: String,
    filesize: i64,
    status: i16,
    created: i64,
}

/// Refuse a `path` that is not a relative path with no `..` component: the
/// same shape `config_storage::yaml::read_file_bytes` requires of a config
/// set's files, checked here against a `file` record's path instead.
fn reject_unsafe_shape(path: &str) -> Result<()> {
    let rel_path = Path::new(path);
    if rel_path.is_absolute()
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        anyhow::bail!("must be a relative path inside the files directory, with no '..'");
    }
    Ok(())
}

/// Confirm `rel` names a regular file inside `root`, once any symlink on its
/// path is followed. Local storage only: an object store has no directory for
/// a symlink to escape, so [`FileStorage::read`](crate::file::FileStorage::read)
/// is this import's one check there; on a directory, that same read does not
/// resolve symlinks itself, so containment is checked here first.
async fn check_local_containment(root: &Path, rel: &str) -> Result<()> {
    let root_real = tokio::fs::canonicalize(root)
        .await
        .with_context(|| format!("failed to resolve the files directory {}", root.display()))?;
    let resolved = tokio::fs::canonicalize(root_real.join(rel))
        .await
        .context("not found in the files directory")?;
    if !resolved.starts_with(&root_real) {
        anyhow::bail!("leads outside the files directory");
    }
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .with_context(|| format!("failed to read '{rel}'"))?;
    if !metadata.is_file() {
        anyhow::bail!("'{rel}' is not a file");
    }
    Ok(())
}

/// Import one `file` record: a file already in the store, registered under
/// the id, owner and date the record gives. Never copies, moves or rewrites
/// the file.
pub(crate) async fn import_file(run: &mut Run<'_>, r: FileRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    require_user(run, r.owner_id, "owner_id").await?;
    let filename = r.filename.trim();
    if filename.is_empty() {
        anyhow::bail!("filename is required");
    }
    let path = r.path.trim();
    if path.is_empty() {
        anyhow::bail!("path is required");
    }
    reject_unsafe_shape(path).with_context(|| format!("'{path}'"))?;

    let storage = run.state.files().storage();
    if let Some(root) = storage.local_dir() {
        check_local_containment(root, path)
            .await
            .with_context(|| format!("'{path}'"))?;
    }

    let uri = format!("local://{path}");
    let data = storage.read(&uri).await.map_err(|e| {
        if is_not_found(&e) {
            anyhow::anyhow!("'{path}' not found in the file store")
        } else {
            e.context(format!("failed to read '{path}'"))
        }
    })?;
    FileService::validate_file(filename, &r.mime_type, &data)
        .with_context(|| format!("'{path}' is not a valid {}", r.mime_type))?;

    let wanted = StoredFile {
        uri: uri.clone(),
        owner_id: r.owner_id,
        filename: filename.to_string(),
        filemime: r.mime_type.clone(),
        filesize: data.len() as i64,
        status: FileStatus::Permanent as i16,
        created: r.created,
    };

    let stored: Option<StoredFile> = sqlx::query_as(
        "SELECT uri, owner_id, filename, filemime, filesize, status, created FROM file_managed \
         WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the file")?;
    if let Some(stored) = &stored
        && stored.uri != wanted.uri
    {
        anyhow::bail!(
            "file {} is stored at '{}', not '{uri}': a file's path cannot be changed by import",
            r.id,
            stored.uri
        );
    }
    if stored.as_ref() == Some(&wanted) {
        return Ok(Outcome::Unchanged);
    }

    // The storage URI is unique: another id cannot already hold this path.
    let holder: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM file_managed WHERE uri = $1 AND id <> $2")
            .bind(&uri)
            .bind(r.id)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the path")?;
    if let Some(holder) = holder {
        anyhow::bail!("'{path}' is already file {holder}");
    }

    if stored.is_some() {
        sqlx::query(
            "UPDATE file_managed SET owner_id = $2, filename = $3, filemime = $4, filesize = $5, \
             status = $6, created = $7, changed = $7 WHERE id = $1",
        )
        .bind(r.id)
        .bind(wanted.owner_id)
        .bind(&wanted.filename)
        .bind(&wanted.filemime)
        .bind(wanted.filesize)
        .bind(wanted.status)
        .bind(wanted.created)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the file")?;
        return Ok(Outcome::Updated);
    }

    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, \
         created, changed) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)",
    )
    .bind(r.id)
    .bind(wanted.owner_id)
    .bind(&wanted.filename)
    .bind(&uri)
    .bind(&wanted.filemime)
    .bind(wanted.filesize)
    .bind(wanted.status)
    .bind(wanted.created)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the file")?;
    Ok(Outcome::Created)
}
