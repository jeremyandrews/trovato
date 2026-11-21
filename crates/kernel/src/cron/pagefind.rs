//! Pagefind search index builder.
//!
//! Checks the `pagefind_index_status` signal table (created by the
//! `trovato_search` plugin) and rebuilds the client-side search index
//! when requested. Exports published live-stage items as HTML fragments,
//! runs the Pagefind CLI, and atomically deploys the index to
//! `PAGEFIND_OUTPUT_DIR` (`./static/pagefind/` by default), which the kernel
//! serves at `/static/pagefind/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::content::ItemService;
use crate::models::Item;
use crate::models::Role;
use crate::models::role::well_known::ANONYMOUS_ROLE_ID;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::helpers::html_escape;
use crate::tap::UserContext;

/// Maximum time allowed for the pagefind CLI to run (2 minutes).
const PAGEFIND_CLI_TIMEOUT: Duration = Duration::from_secs(120);

/// One item's contribution to the static index: the text and the meta values,
/// already decided.
///
/// Exposed (`#[doc(hidden)]`, re-exported from `cron`) so an integration test
/// can assert **which items and which text** without a Pagefind CLI on the
/// path. The module itself stays private; this type and
/// [`indexable_documents`] are the whole seam.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct IndexDocument {
    /// The item this document describes.
    pub id: Uuid,
    /// Its content type.
    pub item_type: String,
    /// Its title, which is a column and not a field.
    pub title: String,
    /// The URL the result card links to: the alias when there is one.
    pub url: String,
    /// The indexed fields that survived the seam, heaviest first, each with
    /// the Pagefind weight its `search_field_config` letter maps to.
    pub fields: Vec<IndexedField>,
    /// The `description` meta value, likewise.
    pub description: String,
    /// The `location` meta value, likewise.
    pub location: String,
    /// The `event_dates` meta value, likewise.
    pub event_dates: String,
    /// The human-readable created date.
    pub created_display: String,
}

/// One field's text in the static index, at its weight.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedField {
    /// The field's machine name.
    pub name: String,
    /// Its `data-pagefind-weight` ([`pagefind_weight`] of its letter).
    pub weight: f32,
    /// Its text.
    pub text: String,
}

/// The `data-pagefind-weight` for a `search_field_config` letter.
///
/// The ratios of PostgreSQL's default `ts_rank` weights (A 1.0, B 0.4, C 0.2,
/// D 0.1), scaled so A is Pagefind's maximum of 10.0. The title is A, as in the
/// tsvector trigger, so both backends rank a title match above a field match by
/// the same factor. An unknown letter is D.
pub fn pagefind_weight(letter: char) -> f32 {
    match letter.to_ascii_uppercase() {
        'A' => 10.0,
        'B' => 4.0,
        'C' => 2.0,
        _ => 1.0,
    }
}

/// Row type for URL alias lookup.
#[derive(sqlx::FromRow)]
struct AliasRow {
    source: String,
    alias: String,
}

/// Row type for search field configuration.
#[derive(sqlx::FromRow)]
struct FieldConfigRow {
    bundle: String,
    field_name: String,
    weight: String,
}

/// Check if the `trovato_search` plugin has requested a rebuild and
/// perform it if so.
///
/// Returns `Ok(true)` if an index was built, `Ok(false)` if no rebuild
/// was needed (or the signal table doesn't exist), and `Err` on failure.
/// `output_dir` is the directory the generated index is written to
/// (`PAGEFIND_OUTPUT_DIR`), supplied by the caller rather than read back out of
/// the environment here.
pub async fn maybe_rebuild_index(
    pool: &PgPool,
    output_dir: &Path,
    items: &ItemService,
) -> Result<bool> {
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
    match build_index(pool, output_dir, items).await {
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
///
/// `output_dir` is `PAGEFIND_OUTPUT_DIR`, set by
/// `CronService::apply_runtime_config`: the index needs exactly one
/// destination.
async fn build_index(pool: &PgPool, output_dir: &Path, items: &ItemService) -> Result<usize> {
    let paths = SwapPaths::for_output(output_dir);
    tokio::fs::create_dir_all(&paths.temp)
        .await
        .context("failed to create pagefind temp directory")?;

    // Ensure cleanup on both success and failure
    let result = build_index_inner(pool, &paths, items).await;

    // Clean up temp directory
    if let Err(e) = tokio::fs::remove_dir_all(&paths.temp).await {
        debug!(error = %e, path = %paths.temp.display(), "failed to remove pagefind temp dir");
    }

    result
}

/// Where a build is made, and where the old index goes while the new one is
/// renamed into place.
///
/// Both are siblings of the output directory, in the same parent, so the swap
/// is a rename within one filesystem (on several servers, the volume they
/// share). Their names carry a fresh id rather than the process id, which is 1
/// in every container.
#[derive(Debug)]
struct SwapPaths {
    /// The directory the index is served from.
    output: PathBuf,
    /// The directory the build is made in.
    temp: PathBuf,
    /// Where the old index goes during the swap.
    backup: PathBuf,
}

impl SwapPaths {
    fn for_output(output: &Path) -> Self {
        let parent = match output.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let name = output
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "pagefind".to_string());
        let id = Uuid::now_v7().simple();
        Self {
            output: output.to_path_buf(),
            temp: parent.join(format!(".{name}_build_{id}")),
            backup: parent.join(format!(".{name}_old_{id}")),
        }
    }
}

/// The items this index may carry, and the text each contributes.
///
/// The index is a **static file any visitor downloads**, so it is built as the
/// anonymous visitor: [`UserContext::anonymous`] carrying the permissions the
/// anonymous role actually holds, routed through
/// [`ItemService::filter_page_for_view`]. Both tiers matter here. The item tier
/// is why a site whose anonymous role does not hold `access content` no longer
/// publishes its content in a world-readable index; the field tier is why a
/// field `tap_field_access` denies an anonymous visitor is not indexed,
/// searchable, or quoted back in an excerpt.
///
/// Every value below — the body text and all four meta values — is built from
/// the **filtered** fields, so there is no second path to the field the first
/// one dropped. `title` is a column rather than a field and is not governed.
#[doc(hidden)]
pub async fn indexable_documents(pool: &PgPool, items: &ItemService) -> Result<Vec<IndexDocument>> {
    // The coarse candidate set: published, live stage. Exactly what the seam is
    // then asked about, rather than what it is trusted to have already been.
    let candidates = sqlx::query_as::<_, Item>(
        r#"
        SELECT id, current_revision_id, type, title, author_id, status, created, changed,
               promote, sticky, fields, stage_id, language, item_group_id, retention_days
        FROM item
        WHERE status = 1 AND stage_id = $1
        "#,
    )
    .bind(LIVE_STAGE_ID)
    .fetch_all(pool)
    .await
    .context("failed to query items for pagefind index")?;

    // The anonymous visitor, with the anonymous role's real permission set — not
    // a bare `UserContext::anonymous()`, whose empty set would deny everything
    // and quietly publish an empty index.
    let anon_permissions = Role::get_permissions(pool, ANONYMOUS_ROLE_ID)
        .await
        .context("failed to read the anonymous role's permissions")?;
    let mut anonymous = UserContext::anonymous();
    anonymous.permissions = anon_permissions;

    let page_size = candidates.len();
    let visible = items
        .filter_page_for_view(candidates, &anonymous, "view", page_size)
        .await;

    // Every search field config with its weight, once per build, so the export
    // indexes the fields the trigger indexes at the weights it gives them.
    let field_configs = sqlx::query_as::<_, FieldConfigRow>(
        "SELECT bundle, field_name, weight FROM search_field_config",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut config_map: HashMap<String, Vec<(String, f32)>> = HashMap::new();
    for row in field_configs {
        let letter = row.weight.chars().next().unwrap_or('D');
        config_map
            .entry(row.bundle)
            .or_default()
            .push((row.field_name, pagefind_weight(letter)));
    }

    // Batch-load URL aliases for all items so we can use friendly paths.
    let alias_rows = sqlx::query_as::<_, AliasRow>(
        "SELECT source, alias FROM url_alias WHERE source LIKE '/item/%'",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let alias_map: HashMap<String, String> = alias_rows
        .into_iter()
        .map(|r| (r.source, r.alias))
        .collect();

    let mut documents = Vec::with_capacity(visible.len());
    for item in &visible {
        let fields = searchable_fields(&item.fields, &item.item_type, &config_map);

        // Use friendly URL alias if available, otherwise /item/{id}
        let source_path = format!("/item/{}", item.id);
        let url = alias_map.get(&source_path).cloned().unwrap_or(source_path);

        // Extract structured metadata from fields for richer result cards
        // The long text field under either of its two names (BL-21), so a
        // `page` item's result card is not left with no description.
        let description = extract_field_text(&item.fields, "field_description")
            .or_else(|| crate::content::body_field::body_text(&item.fields))
            .unwrap_or_default();
        // Strip HTML and truncate for a clean description meta
        let description_clean = ammonia::clean(&description);

        documents.push(IndexDocument {
            id: item.id,
            item_type: item.item_type.clone(),
            title: item.title.clone(),
            url,
            fields,
            description: truncate_meta(&description_clean, 200),
            location: build_location_meta(&item.fields),
            event_dates: build_date_range_meta(&item.fields),
            created_display: format_unix_date(item.created),
        });
    }

    Ok(documents)
}

/// One item's HTML fragment, as the Pagefind CLI indexes it.
///
/// Pagefind indexes only inside `data-pagefind-body` when a page has one, so
/// everything searchable goes in there: the title, at weight A and still the
/// result's `title` meta, then each field in its own element at its own weight,
/// heaviest first. The meta values sit outside the region; they are shown on
/// the result card but not searched.
///
/// Exposed (`#[doc(hidden)]`, re-exported from `cron`) beside
/// [`indexable_documents`] so a test can read the fragment without the CLI.
#[doc(hidden)]
pub fn render_fragment(document: &IndexDocument) -> String {
    let mut meta_tags = format!(
        "<meta data-pagefind-meta=\"type:{}\" />\n\
         <meta data-pagefind-meta=\"date:{}\" />",
        html_escape(&document.item_type),
        html_escape(&document.created_display),
    );

    if !document.description.is_empty() {
        meta_tags.push_str(&format!(
            "\n<meta data-pagefind-meta=\"description:{}\" />",
            html_escape(&document.description)
        ));
    }
    if !document.location.is_empty() {
        meta_tags.push_str(&format!(
            "\n<meta data-pagefind-meta=\"location:{}\" />",
            html_escape(&document.location)
        ));
    }
    if !document.event_dates.is_empty() {
        meta_tags.push_str(&format!(
            "\n<meta data-pagefind-meta=\"event_dates:{}\" />",
            html_escape(&document.event_dates)
        ));
    }

    // Store the item URL as a meta tag. Pagefind treats `url` as
    // a special key that overrides the auto-detected file URL.
    meta_tags.push_str(&format!(
        "\n<meta data-pagefind-meta=\"url:{}\" />",
        html_escape(&document.url)
    ));

    let mut body = format!(
        "<h1 data-pagefind-meta=\"title\" data-pagefind-weight=\"{:.1}\">{}</h1>",
        pagefind_weight('A'),
        html_escape(&document.title),
    );
    for field in &document.fields {
        body.push_str(&format!(
            "\n<p data-pagefind-weight=\"{:.1}\">{}</p>",
            field.weight,
            html_escape(&field.text),
        ));
    }

    format!(
        "<html><head><title>{title}</title></head>\n\
         <body>\n\
         <div data-pagefind-body>\n{body}\n</div>\n\
         {meta_tags}\n\
         </body></html>",
        title = html_escape(&document.title),
    )
}

/// Inner build logic, separated for cleanup guarantee.
async fn build_index_inner(pool: &PgPool, paths: &SwapPaths, items: &ItemService) -> Result<usize> {
    let temp_dir = paths.temp.as_path();
    let documents = indexable_documents(pool, items).await?;

    let count = documents.len();
    debug!(count = count, "exporting items for pagefind");

    // Write each item as an HTML fragment with rich metadata
    for document in &documents {
        let html = render_fragment(document);

        let file_path = temp_dir.join(format!("{}.html", document.id));
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
    let target = &paths.output;
    let backup = &paths.backup;

    if target.exists() {
        tokio::fs::rename(target, backup)
            .await
            .context("failed to back up old pagefind index")?;
    }

    if output_path.exists() {
        tokio::fs::rename(&output_path, target)
            .await
            .context("failed to deploy new pagefind index")?;
    }

    // Remove backup (best-effort)
    if backup.exists() {
        tokio::fs::remove_dir_all(backup).await.ok();
    }

    Ok(count)
}

/// Build a location string from city/country fields.
fn build_location_meta(fields: &serde_json::Value) -> String {
    let city = extract_field_text(fields, "field_city").unwrap_or_default();
    let country = extract_field_text(fields, "field_country").unwrap_or_default();
    match (city.is_empty(), country.is_empty()) {
        (true, true) => String::new(),
        (false, true) => city,
        (true, false) => country,
        (false, false) => format!("{city}, {country}"),
    }
}

/// Build a date range string from date start/end fields.
fn build_date_range_meta(fields: &serde_json::Value) -> String {
    let start = extract_field_text(fields, "field_date_start").unwrap_or_default();
    let end = extract_field_text(fields, "field_date_end").unwrap_or_default();
    match (start.is_empty(), end.is_empty()) {
        (true, true) => String::new(),
        (false, true) => start,
        (true, false) => end,
        (false, false) => format!("{start} – {end}"),
    }
}

/// Format a Unix timestamp as a human-readable date (YYYY-MM-DD).
fn format_unix_date(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

/// Truncate a string to at most `max_len` bytes on a char boundary.
fn truncate_meta(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut end = max_len;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The configured searchable fields an item has text in, heaviest first, then
/// by field name.
///
/// When none of the item's configured fields has text, the long text field
/// (under either of its two names, BL-21) is indexed at weight D, so an item of
/// a type with no search configuration is still findable by its body.
fn searchable_fields(
    fields: &serde_json::Value,
    item_type: &str,
    configs: &HashMap<String, Vec<(String, f32)>>,
) -> Vec<IndexedField> {
    let mut indexed: Vec<IndexedField> = configs
        .get(item_type)
        .into_iter()
        .flatten()
        .filter_map(|(name, weight)| {
            extract_field_text(fields, name).map(|text| IndexedField {
                name: name.clone(),
                weight: *weight,
                text,
            })
        })
        .collect();

    if indexed.is_empty()
        && let Some(body) = crate::content::body_field::body_text(fields)
    {
        indexed.push(IndexedField {
            name: "body".to_string(),
            weight: pagefind_weight('D'),
            text: body,
        });
    }

    indexed.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    indexed
}

/// Extract text from a single JSONB field.
///
/// Handles both `{field_name: {value: "..."}}` (structured) and
/// `{field_name: "..."}` (plain string) formats.
fn extract_field_text(fields: &serde_json::Value, field_name: &str) -> Option<String> {
    crate::content::body_field::field_text(fields, field_name)
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

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_build_and_the_backup_sit_beside_the_output() {
        let paths = SwapPaths::for_output(Path::new("/shared/pagefind"));
        assert_eq!(paths.output, PathBuf::from("/shared/pagefind"));
        assert_eq!(paths.temp.parent(), Some(Path::new("/shared")));
        assert_eq!(paths.backup.parent(), Some(Path::new("/shared")));
        let temp = paths.temp.file_name().unwrap().to_string_lossy();
        let backup = paths.backup.file_name().unwrap().to_string_lossy();
        assert!(temp.starts_with(".pagefind_build_"), "{temp}");
        assert!(backup.starts_with(".pagefind_old_"), "{backup}");
    }

    #[test]
    fn each_build_gets_its_own_names() {
        let a = SwapPaths::for_output(Path::new("./static/pagefind"));
        let b = SwapPaths::for_output(Path::new("./static/pagefind"));
        assert_ne!(a.temp, b.temp);
        assert_ne!(a.backup, b.backup);
    }

    #[test]
    fn a_bare_name_uses_the_current_directory() {
        let paths = SwapPaths::for_output(Path::new("index"));
        assert_eq!(paths.temp.parent(), Some(Path::new(".")));
        assert!(
            paths
                .temp
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".index_build_")
        );
    }
}
