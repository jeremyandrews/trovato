//! File management service.
//!
//! Handles file uploads, metadata storage, and cleanup.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::storage::FileStorage;

/// Maximum file size (10 MB).
pub const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Allowed MIME types for upload.
pub const ALLOWED_MIME_TYPES: &[&str] = &[
    // Images (SVG excluded: XML-based format enables stored XSS)
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    // Documents
    "application/pdf",
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "text/plain",
    "text/csv",
    // Archives
    "application/zip",
    "application/gzip",
];

/// File status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i16)]
pub enum FileStatus {
    /// Temporary file, not yet attached to content.
    Temporary = 0,
    /// Permanent file, attached to content.
    Permanent = 1,
}

impl From<i16> for FileStatus {
    fn from(v: i16) -> Self {
        match v {
            1 => FileStatus::Permanent,
            _ => FileStatus::Temporary,
        }
    }
}

/// File metadata from database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub filename: String,
    pub uri: String,
    pub filemime: String,
    pub filesize: i64,
    pub status: FileStatus,
    pub created: i64,
    pub changed: i64,
}

/// Database row for file.
#[derive(sqlx::FromRow)]
struct FileRow {
    id: Uuid,
    owner_id: Uuid,
    filename: String,
    uri: String,
    filemime: String,
    filesize: i64,
    status: i16,
    created: i64,
    changed: i64,
}

impl From<FileRow> for FileInfo {
    fn from(row: FileRow) -> Self {
        Self {
            id: row.id,
            owner_id: row.owner_id,
            filename: row.filename,
            uri: row.uri,
            filemime: row.filemime,
            filesize: row.filesize,
            status: FileStatus::from(row.status),
            created: row.created,
            changed: row.changed,
        }
    }
}

/// File upload result.
#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub id: Uuid,
    pub filename: String,
    pub uri: String,
    pub url: String,
    pub size: i64,
    pub mime_type: String,
}

/// File service for managing uploads.
pub struct FileService {
    pool: PgPool,
    storage: Arc<dyn FileStorage>,
}

impl FileService {
    /// Create a new file service.
    pub fn new(pool: PgPool, storage: Arc<dyn FileStorage>) -> Self {
        Self { pool, storage }
    }

    /// Upload a file.
    ///
    /// Validates size and MIME type, stores the file, and creates a database record.
    /// File is created with temporary status until attached to content.
    pub async fn upload(
        &self,
        owner_id: Uuid,
        filename: &str,
        mime_type: &str,
        data: &[u8],
    ) -> Result<UploadResult> {
        // Validate size
        if data.len() > MAX_FILE_SIZE {
            bail!(
                "file too large: {} bytes (max {} bytes)",
                data.len(),
                MAX_FILE_SIZE
            );
        }

        // Validate MIME type
        if !ALLOWED_MIME_TYPES.contains(&mime_type) {
            bail!("file type not allowed: {mime_type}");
        }

        // Validate file content matches declared MIME type (magic byte check).
        // This prevents uploading executables disguised as images, etc.
        validate_magic_bytes(data, mime_type)?;

        // Generate storage URI
        let uri = match self.storage.scheme() {
            "local" => {
                // Cast to LocalFileStorage to access generate_uri
                // For now, generate a simple URI
                let now = chrono::Utc::now();
                let unique_id = Uuid::now_v7().simple().to_string();
                let safe_name = sanitize_filename(filename);
                format!(
                    "local://{}/{}/{}_{}",
                    now.format("%Y"),
                    now.format("%m"),
                    &unique_id[..16],
                    safe_name
                )
            }
            "s3" => {
                let now = chrono::Utc::now();
                let unique_id = Uuid::now_v7().simple().to_string();
                let safe_name = sanitize_filename(filename);
                format!(
                    "s3://{}/{}/{}_{}",
                    now.format("%Y"),
                    now.format("%m"),
                    &unique_id[..16],
                    safe_name
                )
            }
            scheme => bail!("unsupported storage scheme: {scheme}"),
        };

        // Write to storage
        self.storage
            .write(&uri, data)
            .await
            .context("failed to write file to storage")?;

        // Create database record
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(id)
        .bind(owner_id)
        .bind(filename)
        .bind(&uri)
        .bind(mime_type)
        .bind(data.len() as i64)
        .bind(FileStatus::Temporary as i16)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to create file record")?;

        let url = self.storage.public_url(&uri);

        debug!(
            id = %id,
            filename = %filename,
            uri = %uri,
            size = data.len(),
            "file uploaded"
        );

        Ok(UploadResult {
            id,
            filename: filename.to_string(),
            uri,
            url,
            size: data.len() as i64,
            mime_type: mime_type.to_string(),
        })
    }

    /// List all files.
    pub async fn list(&self) -> Result<Vec<FileInfo>> {
        let rows: Vec<FileRow> = sqlx::query_as(
            "SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed FROM file_managed ORDER BY created DESC"
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list files")?;

        Ok(rows.into_iter().map(FileInfo::from).collect())
    }

    /// List files with pagination.
    pub async fn list_paginated(&self, limit: i64, offset: i64) -> Result<Vec<FileInfo>> {
        let rows: Vec<FileRow> = sqlx::query_as(
            "SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed FROM file_managed ORDER BY created DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .context("failed to list files")?;

        Ok(rows.into_iter().map(FileInfo::from).collect())
    }

    /// List files with optional status filter.
    pub async fn list_by_status(
        &self,
        status: Option<FileStatus>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FileInfo>> {
        let rows: Vec<FileRow> = match status {
            Some(s) => {
                sqlx::query_as(
                    "SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed FROM file_managed WHERE status = $1 ORDER BY created DESC LIMIT $2 OFFSET $3"
                )
                .bind(s as i16)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .context("failed to list files by status")?
            }
            None => {
                sqlx::query_as(
                    "SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed FROM file_managed ORDER BY created DESC LIMIT $1 OFFSET $2"
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .context("failed to list files")?
            }
        };

        Ok(rows.into_iter().map(FileInfo::from).collect())
    }

    /// Count all files.
    pub async fn count(&self) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_managed")
            .fetch_one(&self.pool)
            .await
            .context("failed to count files")?;

        Ok(count)
    }

    /// Count files by status.
    pub async fn count_by_status(&self, status: FileStatus) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_managed WHERE status = $1")
            .bind(status as i16)
            .fetch_one(&self.pool)
            .await
            .context("failed to count files by status")?;

        Ok(count)
    }

    /// Get file info by ID.
    pub async fn get(&self, id: Uuid) -> Result<Option<FileInfo>> {
        let row = sqlx::query_as::<_, FileRow>(
            "SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed FROM file_managed WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch file")?;

        Ok(row.map(FileInfo::from))
    }

    /// Mark a file as permanent (attached to content).
    pub async fn mark_permanent(&self, id: Uuid) -> Result<bool> {
        let result = sqlx::query("UPDATE file_managed SET status = $1, changed = $2 WHERE id = $3")
            .bind(FileStatus::Permanent as i16)
            .bind(chrono::Utc::now().timestamp())
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to update file status")?;

        Ok(result.rows_affected() > 0)
    }

    /// Mark multiple files as permanent.
    pub async fn mark_permanent_batch(&self, ids: &[Uuid]) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }

        let result = sqlx::query(
            r#"
            UPDATE file_managed
            SET status = $1, changed = $2
            WHERE id = ANY($3)
            "#,
        )
        .bind(FileStatus::Permanent as i16)
        .bind(chrono::Utc::now().timestamp())
        .bind(ids)
        .execute(&self.pool)
        .await
        .context("failed to update file statuses")?;

        Ok(result.rows_affected())
    }

    /// Delete a file (both storage and database record).
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        // Get file info first
        let Some(file) = self.get(id).await? else {
            return Ok(false);
        };

        // Delete from storage
        if let Err(e) = self.storage.delete(&file.uri).await {
            warn!(error = %e, uri = %file.uri, "failed to delete file from storage");
            // Continue to delete database record anyway
        }

        // Delete database record
        let result = sqlx::query("DELETE FROM file_managed WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete file record")?;

        debug!(id = %id, "file deleted");
        Ok(result.rows_affected() > 0)
    }

    /// Cleanup temporary files older than the given age.
    ///
    /// Returns the number of files deleted.
    pub async fn cleanup_temp_files(&self, max_age_secs: i64) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp() - max_age_secs;

        // Get files to delete
        let files: Vec<FileRow> = sqlx::query_as(
            r#"
            SELECT id, owner_id, filename, uri, filemime, filesize, status, created, changed
            FROM file_managed
            WHERE status = $1 AND created < $2
            "#,
        )
        .bind(FileStatus::Temporary as i16)
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch temp files")?;

        let count = files.len() as u64;

        for file in files {
            // Delete from storage
            if let Err(e) = self.storage.delete(&file.uri).await {
                warn!(error = %e, uri = %file.uri, "failed to delete temp file from storage");
            }

            // Delete database record
            if let Err(e) = sqlx::query("DELETE FROM file_managed WHERE id = $1")
                .bind(file.id)
                .execute(&self.pool)
                .await
            {
                warn!(error = %e, id = %file.id, "failed to delete temp file record");
            }
        }

        if count > 0 {
            info!(count = count, "cleaned up temporary files");
        }

        Ok(count)
    }

    /// Get the storage backend.
    pub fn storage(&self) -> &Arc<dyn FileStorage> {
        &self.storage
    }

    /// Load file data by path (convenience for image processing, etc.).
    pub async fn load_file_data(&self, path: &str) -> Result<Option<Vec<u8>>> {
        match self.storage.read(path).await {
            Ok(data) => Ok(Some(data)),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not found") || msg.contains("No such file") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// Validate that file content matches the declared MIME type using magic bytes.
///
/// For file types with well-known magic byte signatures (images, PDF, ZIP, GZIP),
/// this verifies the actual content matches. For document types without reliable
/// magic bytes (CSV, plain text, Office XML), validation is skipped since these
/// are inherently safe (non-executable).
fn validate_magic_bytes(data: &[u8], declared_mime: &str) -> Result<()> {
    // Types that require magic byte validation
    let expected_mimes: &[&str] = match declared_mime {
        "image/jpeg" => &["image/jpeg"],
        "image/png" => &["image/png"],
        "image/gif" => &["image/gif"],
        "image/webp" => &["image/webp"],
        "application/pdf" => &["application/pdf"],
        "application/zip" => &["application/zip"],
        "application/gzip" => &["application/gzip"],
        // Office formats (docx/xlsx) are ZIP archives internally.
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        | "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
            &["application/zip"]
        }
        // Legacy Office formats have OLE magic bytes detected as application/x-ole-storage
        "application/msword" | "application/vnd.ms-excel" => return Ok(()),
        // Text-based formats have no reliable magic bytes
        "text/plain" | "text/csv" => return Ok(()),
        _ => return Ok(()),
    };

    let detected = infer::get(data);
    match detected {
        Some(kind) => {
            if expected_mimes.contains(&kind.mime_type()) {
                Ok(())
            } else {
                bail!(
                    "file content does not match declared type: declared {declared_mime}, detected {}",
                    kind.mime_type()
                );
            }
        }
        None => {
            // infer couldn't detect the type. For types that should have detectable magic
            // bytes (images, PDF, archives), this is suspicious.
            match declared_mime {
                "text/plain" | "text/csv" => Ok(()),
                _ => bail!("could not verify file content for declared type {declared_mime}"),
            }
        }
    }
}

/// Sanitize a filename for safe storage.
pub(crate) fn sanitize_filename(filename: &str) -> String {
    use std::path::Path;

    // Get just the filename part (no path)
    let name = Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);

    // Replace unsafe characters
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            ' ' => '_',
            _ => '_',
        })
        .collect::<String>()
        .chars()
        .take(200)
        .collect()
}

impl std::fmt::Debug for FileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileService").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;

    #[test]
    fn test_file_status_conversion() {
        assert_eq!(FileStatus::from(0), FileStatus::Temporary);
        assert_eq!(FileStatus::from(1), FileStatus::Permanent);
        assert_eq!(FileStatus::from(99), FileStatus::Temporary);
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: MIME allowlist blocks executables and SVG
    #[test]
    fn test_allowed_mime_types() {
        assert!(ALLOWED_MIME_TYPES.contains(&"image/jpeg"));
        assert!(ALLOWED_MIME_TYPES.contains(&"application/pdf"));
        assert!(!ALLOWED_MIME_TYPES.contains(&"application/x-executable"));
        assert!(!ALLOWED_MIME_TYPES.contains(&"application/x-sharedlib"));
        assert!(!ALLOWED_MIME_TYPES.contains(&"application/x-mach-binary"));
        // SVG excluded to prevent stored XSS
        assert!(!ALLOWED_MIME_TYPES.contains(&"image/svg+xml"));
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: valid PNG passes magic byte check
    #[test]
    fn magic_bytes_valid_png() {
        // PNG magic bytes: 89 50 4E 47 0D 0A 1A 0A
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        assert!(validate_magic_bytes(&png_data, "image/png").is_ok());
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: PNG content declared as JPEG rejected
    #[test]
    fn magic_bytes_mismatch_rejects() {
        // PNG magic bytes declared as JPEG should fail
        let png_data = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00];
        assert!(validate_magic_bytes(&png_data, "image/jpeg").is_err());
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: text formats skip validation
    #[test]
    fn magic_bytes_text_skips_validation() {
        // Plain text has no magic bytes, should pass
        assert!(validate_magic_bytes(b"Hello, world!", "text/plain").is_ok());
        assert!(validate_magic_bytes(b"col1,col2\nval1,val2", "text/csv").is_ok());
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: empty data rejected for binary types
    #[test]
    fn magic_bytes_empty_rejects_binary_types() {
        // Empty data declared as image should fail
        assert!(validate_magic_bytes(&[], "image/jpeg").is_err());
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: ELF binary disguised as JPEG rejected
    #[test]
    fn magic_bytes_elf_disguised_as_image() {
        // ELF magic bytes (Linux executable): 7F 45 4C 46
        let elf_data = [0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00, 0x00, 0x00];
        assert!(
            validate_magic_bytes(&elf_data, "image/jpeg").is_err(),
            "ELF binary declared as image/jpeg should be rejected"
        );
        assert!(
            validate_magic_bytes(&elf_data, "image/png").is_err(),
            "ELF binary declared as image/png should be rejected"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: PE executable disguised as image rejected
    #[test]
    fn magic_bytes_pe_disguised_as_image() {
        // PE/MZ magic bytes (Windows executable): 4D 5A
        let pe_data = [0x4D, 0x5A, 0x90, 0x00, 0x03, 0x00, 0x00, 0x00, 0x04, 0x00];
        assert!(
            validate_magic_bytes(&pe_data, "image/jpeg").is_err(),
            "PE executable declared as image/jpeg should be rejected"
        );
    }

    // SECURITY REGRESSION TEST — Story 27.6 Finding #1: valid JPEG passes magic byte check
    #[test]
    fn magic_bytes_valid_jpeg() {
        // JPEG magic bytes: FF D8 FF
        let jpeg_data = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
        assert!(validate_magic_bytes(&jpeg_data, "image/jpeg").is_ok());
    }
}
