//! Files already copied into the files directory, registered as
//! `file_managed` rows with the id, owner and date the record gives.
//!
//! A migrated site's files arrive on disk before the importer ever runs: a
//! port copies them into `UPLOADS_DIR`, then writes a `file` record naming
//! where each one landed. The importer never writes, moves or renames a byte
//! of them — it reads the file at `path` to check it the way an upload is
//! checked ([`FileService::validate_file`]) and to measure its own size, then
//! writes one `file_managed` row whose `uri` is that same path. An item's File
//! field or embedded `local://`/`/files/` reference finds it exactly as it
//! would find an upload, because `file_refs::sync_item_file_references` only
//! ever looks the file up by `file_managed` id or uri.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::FileRecord;
use super::{Outcome, Run, require_id, require_user};
use crate::file::{FileService, FileStatus};

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

/// Resolve `rel` as a path inside `root`: relative, no `..`, and — once any
/// symlink is followed — still inside `root`. The same containment rule
/// `config_storage::yaml::read_file_bytes` applies to a config set's files,
/// checked here against the live files directory instead.
async fn resolve_in_dir(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || !rel_path
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        anyhow::bail!("must be a relative path inside the files directory, with no '..'");
    }
    let root = tokio::fs::canonicalize(root)
        .await
        .with_context(|| format!("failed to resolve the files directory {}", root.display()))?;
    let resolved = tokio::fs::canonicalize(root.join(rel_path))
        .await
        .context("not found in the files directory")?;
    if !resolved.starts_with(&root) {
        anyhow::bail!("leads outside the files directory");
    }
    Ok(resolved)
}

/// Import one `file` record: a file already on disk, registered under the id,
/// owner and date the record gives. Never copies, moves or rewrites the file.
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

    let Some(root) = run.state.files().local_dir() else {
        anyhow::bail!(
            "files are not stored in a local directory on this server; a `file` record needs one"
        );
    };
    let resolved = resolve_in_dir(root, path)
        .await
        .with_context(|| format!("'{path}'"))?;
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .with_context(|| format!("failed to read '{path}'"))?;
    if !metadata.is_file() {
        anyhow::bail!("'{path}' is not a file");
    }
    let data = tokio::fs::read(&resolved)
        .await
        .with_context(|| format!("failed to read '{path}'"))?;
    FileService::validate_file(filename, &r.mime_type, &data)
        .with_context(|| format!("'{path}' is not a valid {}", r.mime_type))?;

    let uri = format!("local://{path}");
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

/// Import one `delete` record naming a file: unregister its `file_managed`
/// row, through [`FileService::delete_row_in`] — not [`FileService::delete`],
/// which also removes the bytes from storage: that is not transactional, and
/// import never touches bytes it did not place on disk itself. An id that is
/// not a stored file is unchanged, so a rerun is idempotent.
pub(crate) async fn delete_file(run: &mut Run<'_>, id: Uuid) -> Result<Outcome> {
    require_id(id, "id")?;
    let stored: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM file_managed WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *run.tx)
            .await
            .context("failed to look up the file")?;
    if stored.is_none() {
        return Ok(Outcome::Unchanged);
    }
    FileService::delete_row_in(&mut run.tx, id).await?;
    Ok(Outcome::Deleted)
}
