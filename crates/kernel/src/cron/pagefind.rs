//! Pagefind search index builder.
//!
//! Checks the `pagefind_index_status` signal table (created by the
//! `trovato_search` plugin) and rebuilds the client-side search index
//! when requested. Exports published live-stage items as HTML fragments,
//! runs the Pagefind CLI, and atomically deploys the index to
//! `./static/pagefind/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::helpers::html_escape;

/// Maximum time allowed for the pagefind CLI to run (2 minutes).
const PAGEFIND_CLI_TIMEOUT: Duration = Duration::from_secs(120);

/// Row type for items to index.
#[derive(sqlx::FromRow)]
struct IndexableItem {
    id: Uuid,
    #[sqlx(rename = "type")]
    item_type: String,
    title: String,
    fields: serde_json::Value,
    created: i64,
}

/// Row type for search field configuration.
#[derive(sqlx::FromRow)]
struct FieldConfigRow {
    bundle: String,
    field_name: String,
}

/// Check if the `trovato_search` plugin has requested a rebuild and
/// perform it if so.
///
/// Returns `Ok(true)` if an index was built, `Ok(false)` if no rebuild
/// was needed (or the signal table doesn't exist), and `Err` on failure.
pub async fn maybe_rebuild_index(pool: &PgPool) -> Result<bool> {
    // Check if the signal table exists and a rebuild is requested.
    // If the table doesn't exist (plugin not installed), return early.
    let requested: Option<bool> =
        sqlx::query_scalar("SELECT rebuild_requested FROM pagefind_index_status WHERE id = 1")
            .fetch_optional(pool)
            .await
            .unwrap_or(None);

    let Some(true) = requested else {
        return Ok(false);
    };

    info!("pagefind rebuild requested, starting index build");

    // Record the query timestamp BEFORE fetching items. Using this
    // (rather than "now" at build completion) prevents a race where
    // content published during the build is silently missed.
    let query_timestamp = chrono::Utc::now().timestamp();

    // Clear signal immediately to prevent double builds
    sqlx::query("UPDATE pagefind_index_status SET rebuild_requested = false WHERE id = 1")
        .execute(pool)
        .await
        .context("failed to clear pagefind rebuild signal")?;

    // Run the actual build, recording errors in the status table
    match build_index(pool).await {
        Ok(count) => {
            sqlx::query(
                "UPDATE pagefind_index_status SET last_indexed_at = $1, last_error = NULL WHERE id = 1",
            )
            .bind(query_timestamp)
            .execute(pool)
            .await
            .context("failed to update pagefind last_indexed_at")?;

            info!(items = count, "pagefind index built successfully");
            Ok(true)
        }
        Err(e) => {
            let error_msg = format!("{e:#}");
            sqlx::query("UPDATE pagefind_index_status SET last_error = $1 WHERE id = 1")
                .bind(&error_msg)
                .execute(pool)
                .await
                .ok();

            warn!(error = %e, "pagefind index build failed");
            Err(e)
        }
    }
}

/// Build the Pagefind index from published live-stage items.
async fn build_index(pool: &PgPool) -> Result<usize> {
    let static_dir = std::env::var("STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./static"));

    // Create a temp directory inside static/ (same filesystem for atomic rename)
    let temp_dir = static_dir.join(format!(".pagefind_build_{}", std::process::id()));
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .context("failed to create pagefind temp directory")?;

    // Ensure cleanup on both success and failure
    let result = build_index_inner(pool, &static_dir, &temp_dir).await;

    // Clean up temp directory
    if let Err(e) = tokio::fs::remove_dir_all(&temp_dir).await {
        debug!(error = %e, path = %temp_dir.display(), "failed to remove pagefind temp dir");
    }

    result
}

/// Inner build logic, separated for cleanup guarantee.
async fn build_index_inner(pool: &PgPool, static_dir: &Path, temp_dir: &Path) -> Result<usize> {
    // Query all published live-stage items
    let items = sqlx::query_as::<_, IndexableItem>(
        r#"
        SELECT id, type, title, fields, created
        FROM item
        WHERE status = 1 AND stage_id = $1
        "#,
    )
    .bind(LIVE_STAGE_ID)
    .fetch_all(pool)
    .await
    .context("failed to query items for pagefind index")?;

    // Query search field configs so we export all searchable fields,
    // not just field_body (consistent with the trigger's indexing).
    let field_configs =
        sqlx::query_as::<_, FieldConfigRow>("SELECT bundle, field_name FROM search_field_config")
            .fetch_all(pool)
            .await
            .unwrap_or_default();

    let mut config_map: HashMap<String, Vec<String>> = HashMap::new();
    for row in &field_configs {
        config_map
            .entry(row.bundle.clone())
            .or_default()
            .push(row.field_name.clone());
    }

    let count = items.len();
    debug!(count = count, "exporting items for pagefind");

    // Write each item as an HTML fragment
    for item in &items {
        let body = extract_searchable_text(&item.fields, &item.item_type, &config_map);
        let html = format!(
            r#"<html><head><title>{title}</title></head>
<body>
<h1 data-pagefind-meta="title">{title}</h1>
<div data-pagefind-body>{body}</div>
<meta data-pagefind-meta="type:{item_type}" />
<meta data-pagefind-meta="date:{created}" />
<a data-pagefind-meta="url" href="/item/{id}"></a>
</body></html>"#,
            title = html_escape(&item.title),
            body = html_escape(&body),
            item_type = html_escape(&item.item_type),
            created = item.created,
            id = item.id,
        );

        let file_path = temp_dir.join(format!("{}.html", item.id));
        tokio::fs::write(&file_path, html)
            .await
            .context("failed to write pagefind HTML fragment")?;
    }

    // Check if pagefind CLI is available
    let pagefind_path = which_pagefind();
    let Some(pagefind) = pagefind_path else {
        warn!(
            "pagefind CLI not found in PATH; skipping index generation. \
               Install with: npm install -g pagefind"
        );
        return Ok(count);
    };

    // Run pagefind CLI with a timeout to prevent cron lock expiry
    let output_path = temp_dir.join("_pagefind");
    let output = tokio::time::timeout(
        PAGEFIND_CLI_TIMEOUT,
        tokio::process::Command::new(&pagefind)
            .arg("--site")
            .arg(temp_dir)
            .arg("--output-path")
            .arg(&output_path)
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("pagefind CLI timed out after {PAGEFIND_CLI_TIMEOUT:?}"))?
    .context("failed to execute pagefind CLI")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("pagefind CLI failed: {stderr}");
    }

    // Deploy using rename-swap for crash safety:
    // 1. Rename old index out of the way (if it exists)
    // 2. Rename new index into place
    // 3. Remove old backup
    // This ensures the index is never fully absent if a crash occurs
    // between steps.
    let target = static_dir.join("pagefind");
    let backup = static_dir.join("pagefind.old");

    // Remove any stale backup from a previous interrupted deploy
    if backup.exists() {
        tokio::fs::remove_dir_all(&backup).await.ok();
    }

    if target.exists() {
        tokio::fs::rename(&target, &backup)
            .await
            .context("failed to back up old pagefind index")?;
    }

    if output_path.exists() {
        tokio::fs::rename(&output_path, &target)
            .await
            .context("failed to deploy new pagefind index")?;
    }

    // Remove backup (best-effort)
    if backup.exists() {
        tokio::fs::remove_dir_all(&backup).await.ok();
    }

    Ok(count)
}

/// Extract searchable text from item fields using `search_field_config`.
///
/// Collects text from all fields configured for the item's bundle.
/// Falls back to `field_body` if no fields are configured.
fn extract_searchable_text(
    fields: &serde_json::Value,
    item_type: &str,
    configs: &HashMap<String, Vec<String>>,
) -> String {
    let mut parts = Vec::new();

    if let Some(field_names) = configs.get(item_type) {
        for name in field_names {
            if let Some(text) = extract_field_text(fields, name) {
                parts.push(text);
            }
        }
    }

    // Fall back to field_body if no search_field_config entries matched
    if parts.is_empty()
        && let Some(body) = extract_field_text(fields, "field_body")
    {
        parts.push(body);
    }

    parts.join(" ")
}

/// Extract text from a single JSONB field.
///
/// Handles both `{field_name: {value: "..."}}` (structured) and
/// `{field_name: "..."}` (plain string) formats.
fn extract_field_text(fields: &serde_json::Value, field_name: &str) -> Option<String> {
    fields.get(field_name).and_then(|f| {
        let text = f
            .get("value")
            .and_then(|v| v.as_str())
            .or_else(|| f.as_str())?;
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    })
}

/// Find the pagefind binary in PATH.
///
/// Only returns candidates that are both regular files and executable
/// (on Unix).
fn which_pagefind() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join("pagefind");
            if candidate.is_file() && is_executable(&candidate) {
                Some(candidate)
            } else {
                None
            }
        })
    })
}

/// Check if a file has executable permission.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// On non-Unix platforms, assume any file is executable.
#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}
