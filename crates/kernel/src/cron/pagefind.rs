//! Pagefind search index: what goes in it, and the cron step that keeps it
//! current.
//!
//! The `pagefind_index_status` table (created by the `trovato_search` plugin)
//! says the site wants the index, and carries an operator's full rebuild
//! request ([`request_rebuild`]). Each cron run applies the change queue
//! (`search_index_queue`, which every writer of searchable content fills)
//! through the incremental updater in [`super::search_update`], and takes a
//! full build only when one is requested or an update's preconditions fail.
//! This module decides which items are searchable and renders each as the HTML
//! fragment Pagefind reads ([`indexable_documents`], [`render_fragment`]); the
//! index itself is built natively, through the page ledger, by
//! [`super::search_index`], and published into `PAGEFIND_OUTPUT_DIR`
//! (`./static/pagefind/` by default), which the kernel serves at
//! `/static/pagefind/`. No `pagefind` binary is involved.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::content::ItemService;
use crate::content::page_meta::plain_text;
use crate::models::Item;
use crate::models::Role;
use crate::models::role::well_known::ANONYMOUS_ROLE_ID;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::helpers::html_escape;
use crate::search::SearchSwitches;
use crate::tap::UserContext;

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
    /// The Pagefind language key its page is indexed under: the item's
    /// language, lowercased, or the site's default language when the item has
    /// none (empty or `und`). Pagefind stems by it, and builds one index per
    /// language. Empty leaves the page with no `lang`, which Pagefind files
    /// under `unknown`.
    pub language: String,
    /// The URL the result card links to: the alias when there is one.
    pub url: String,
    /// The indexed fields that survived the seam, as plain text, heaviest
    /// first, each with the Pagefind weight its `search_field_config` letter
    /// maps to (D for a text field nobody configured).
    pub fields: Vec<IndexedField>,
    /// The body of every comment an anonymous visitor sees on the item, as
    /// plain text, in thread order. Indexed with the item: a match in a comment
    /// is a result for the item, never for the comment.
    pub comments: Vec<String>,
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

/// A content type's declared fields, and its whole `settings` (for its
/// search switches), as `item_type` stores them.
#[derive(sqlx::FromRow)]
struct TypeFieldsRow {
    #[sqlx(rename = "type")]
    item_type: String,
    fields: Option<serde_json::Value>,
    settings: Option<serde_json::Value>,
}

/// One publicly visible comment's subject and body, for the item it is on.
#[derive(sqlx::FromRow)]
struct CommentRow {
    item_id: Uuid,
    subject: Option<String>,
    body: String,
}

/// Every comment an anonymous visitor sees on the given items, in one query.
///
/// The rule the item page's thread follows ([`crate::models::Comment::list_for_item`]):
/// published, and under a published parent all the way up, so a published
/// reply to a comment a moderator hid is hidden with it. Ordered as the thread
/// is, which keeps the fragment, and so the index, stable between builds.
const VISIBLE_COMMENTS_SQL: &str = r#"
    WITH RECURSIVE visible AS (
        SELECT id, item_id, subject, body, ARRAY[created] AS sort_path
        FROM comment
        WHERE item_id = ANY($1) AND parent_id IS NULL AND status = $2
        UNION ALL
        SELECT c.id, c.item_id, c.subject, c.body, v.sort_path || c.created
        FROM comment c
        JOIN visible v ON c.parent_id = v.id
        WHERE c.status = $2
    )
    SELECT item_id, subject, body FROM visible ORDER BY item_id, sort_path
"#;

/// Ask for a **full** rebuild of the static index at the next cron run.
///
/// Ordinary writes do not call this: they queue the items they changed
/// ([`crate::search::index_queue`]) and cron rewrites only those items' part
/// of the index. This is for an operator, or a bulk change that would queue
/// most of the site anyway.
///
/// The flag lives in `trovato_search`'s own table, which does not exist on a
/// site that never installed the plugin, so the statement checks for the table
/// itself rather than failing. Best effort: a failure is logged.
pub async fn request_rebuild(pool: &PgPool) {
    let result = sqlx::query(
        "DO $$ BEGIN \
           IF to_regclass('pagefind_index_status') IS NOT NULL THEN \
             UPDATE pagefind_index_status SET rebuild_requested = true \
             WHERE id = 1 AND rebuild_requested = false; \
           END IF; \
         END $$",
    )
    .execute(pool)
    .await;
    if let Err(e) = result {
        warn!(error = %e, "failed to request a pagefind rebuild");
    }
}

/// The kernel's documents for the search index: [`indexable_documents_for`],
/// as the anonymous visitor.
pub struct ItemDocuments<'a> {
    /// The database.
    pub pool: &'a PgPool,
    /// The item service, for the access seam.
    pub items: &'a ItemService,
}

#[async_trait::async_trait]
impl super::search_update::DocumentSource for ItemDocuments<'_> {
    async fn documents(&self, ids: Option<&[Uuid]>) -> Result<Vec<IndexDocument>> {
        indexable_documents_for(self.pool, self.items, ids).await
    }
}

/// Bring the static search index up to date, if the `trovato_search` plugin
/// is installed: a full build when one was requested (or nothing is live
/// yet), otherwise the queued items' updates, then compaction past
/// `compaction_percent` tombstones ([`super::search_update::run_index`]).
///
/// Returns `Ok(true)` if anything was published or drained, `Ok(false)` if
/// there was nothing to do (or the plugin's table does not exist), and `Err`
/// on failure, which is also recorded in `pagefind_index_status.last_error`.
/// `output_dir` is `PAGEFIND_OUTPUT_DIR`.
pub async fn maybe_rebuild_index(
    pool: &PgPool,
    output_dir: &Path,
    items: &ItemService,
    compaction_percent: f64,
) -> Result<bool> {
    let source = ItemDocuments { pool, items };
    maybe_rebuild_index_with(pool, output_dir, &source, compaction_percent).await
}

/// [`maybe_rebuild_index`] over any document source.
///
/// A full rebuild request is cleared before the run, so a request made during
/// it is kept, and set again when the run did not do the build it asked for:
/// the run failed, or another publisher got in first. A request is then never
/// lost to an error; the next cron run tries again.
#[doc(hidden)]
pub async fn maybe_rebuild_index_with<S: super::search_update::DocumentSource>(
    pool: &PgPool,
    output_dir: &Path,
    source: &S,
    compaction_percent: f64,
) -> Result<bool> {
    // The plugin's table says whether the index is wanted at all, and carries
    // an operator's full rebuild request.
    let requested: Option<bool> =
        sqlx::query_scalar("SELECT rebuild_requested FROM pagefind_index_status WHERE id = 1")
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    let Some(requested) = requested else {
        return Ok(false);
    };
    if requested {
        sqlx::query("UPDATE pagefind_index_status SET rebuild_requested = false WHERE id = 1")
            .execute(pool)
            .await
            .context("failed to clear pagefind rebuild signal")?;
    }
    let query_timestamp = chrono::Utc::now().timestamp();
    let options = super::search_update::RunOptions {
        full_build: requested.then(|| "a full rebuild was requested".to_string()),
        compaction_percent,
        ..super::search_update::RunOptions::default()
    };
    let result = super::search_update::run_index(pool, output_dir, source, &options).await;
    // A requested build is the run's first full build; without one, the
    // request stands for the next run.
    let built = result
        .as_ref()
        .is_ok_and(|report| !report.full_builds.is_empty());
    if requested && !built {
        sqlx::query("UPDATE pagefind_index_status SET rebuild_requested = true WHERE id = 1")
            .execute(pool)
            .await
            .context("failed to keep the pagefind rebuild request")?;
        info!("the requested search index rebuild did not run; it is requested again");
    }
    match result {
        Ok(report) => {
            if report.did_work() {
                sqlx::query(
                    "UPDATE pagefind_index_status SET last_indexed_at = $1, last_error = NULL \
                     WHERE id = 1",
                )
                .bind(query_timestamp)
                .execute(pool)
                .await
                .context("failed to update pagefind last_indexed_at")?;
            }
            Ok(report.did_work())
        }
        Err(e) => {
            // A publish that was not recorded is mended by the next run's entry
            // check; anything else is recorded and retried next run, the queue
            // intact.
            let error_msg = format!("{e:#}");
            sqlx::query("UPDATE pagefind_index_status SET last_error = $1 WHERE id = 1")
                .bind(&error_msg)
                .execute(pool)
                .await
                .ok();
            warn!(error = %e, "search index update failed");
            Err(e)
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
///
/// Comments follow the same rule from the other side: only the comments on an
/// item that survived the seam, and of those only the ones the item page shows
/// a visitor ([`VISIBLE_COMMENTS_SQL`]).
///
/// Six queries whatever the number of items: the candidates, the anonymous
/// role's permissions, the search field configuration, the declared fields of
/// every type, the visible comments of every visible item, and the URL aliases.
/// Live published items are decided by the access fast path, which reads
/// nothing.
#[doc(hidden)]
pub async fn indexable_documents(pool: &PgPool, items: &ItemService) -> Result<Vec<IndexDocument>> {
    indexable_documents_for(pool, items, None).await
}

/// [`indexable_documents`] restricted to `ids` (every searchable item for
/// `None`): the same seam, the same rules, for the items one incremental
/// update applies. An id that is not searchable is simply absent.
#[doc(hidden)]
pub async fn indexable_documents_for(
    pool: &PgPool,
    items: &ItemService,
    ids: Option<&[Uuid]>,
) -> Result<Vec<IndexDocument>> {
    // Every type's declared fields and search switches, once per build: the
    // fields so every text field is indexed and not only the configured ones,
    // the switches so a type switched out of search, or its comments, is left
    // out (`SearchSwitches`, both on unless set to false).
    let type_rows = sqlx::query_as::<_, TypeFieldsRow>(
        "SELECT type, settings -> 'fields' AS fields, settings FROM item_type",
    )
    .fetch_all(pool)
    .await
    .context("failed to read content types for pagefind index")?;
    let switches: HashMap<String, SearchSwitches> = type_rows
        .iter()
        .map(|row| {
            let switches = row
                .settings
                .as_ref()
                .map(SearchSwitches::from_settings)
                .unwrap_or_default();
            (row.item_type.clone(), switches)
        })
        .collect();
    let switches_for = |item_type: &str| switches.get(item_type).copied().unwrap_or_default();

    // The coarse candidate set: published, live stage. Exactly what the seam is
    // then asked about, rather than what it is trusted to have already been.
    let candidates = sqlx::query_as::<_, Item>(
        r#"
        SELECT id, current_revision_id, type, title, author_id, status, created, changed,
               promote, sticky, fields, stage_id, language, item_group_id, retention_days
        FROM item
        WHERE status = 1 AND stage_id = $1 AND ($2::uuid[] IS NULL OR id = ANY($2))
        "#,
    )
    .bind(LIVE_STAGE_ID)
    .bind(ids)
    .fetch_all(pool)
    .await
    .context("failed to query items for pagefind index")?;
    let candidates: Vec<Item> = candidates
        .into_iter()
        .filter(|item| switches_for(&item.item_type).include)
        .collect();

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

    let mut text_fields: HashMap<String, Vec<SearchableField>> = HashMap::new();
    for row in type_rows {
        let names = text_fields.entry(row.item_type).or_default();
        if let Some(serde_json::Value::Array(defs)) = row.fields {
            for def in defs {
                if let Ok(def) = serde_json::from_value::<trovato_sdk::types::FieldDefinition>(def)
                    && crate::search::is_text_field(&def.field_type)
                {
                    names.push(SearchableField {
                        name: def.field_name,
                        kind: SearchFieldKind::of(&def.field_type, &def.settings),
                    });
                }
            }
        }
    }

    // Every visible comment on every visible item whose type has comments in
    // search, in one query.
    let visible_ids: Vec<Uuid> = visible
        .iter()
        .filter(|item| switches_for(&item.item_type).comments)
        .map(|item| item.id)
        .collect();
    let comment_rows = sqlx::query_as::<_, CommentRow>(VISIBLE_COMMENTS_SQL)
        .bind(&visible_ids)
        .bind(crate::models::CommentStatus::Published.as_i16())
        .fetch_all(pool)
        .await
        .context("failed to query comments for pagefind index")?;
    let mut comments_by_item: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in comment_rows {
        // The subject folded in alongside the body, at the body's own weight:
        // a subject is not ranked differently from the text it labels, and a
        // comment with none contributes exactly what it always did.
        let mut text = row
            .subject
            .as_deref()
            .map(plain_text)
            .filter(|s| !s.is_empty())
            .unwrap_or_default();
        let body_text = plain_text(&row.body);
        if !body_text.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&body_text);
        }
        if !text.is_empty() {
            comments_by_item.entry(row.item_id).or_default().push(text);
        }
    }

    // The site's default language, for an item that has none of its own: the
    // page then stems as the site's language, and no `unknown` index appears
    // beside the real ones (Pagefind would merge it into the most common
    // language anyway). English when no language is marked default, which is
    // also what `base.html` renders for `<html lang>` then.
    let default_language: String =
        sqlx::query_scalar("SELECT id FROM language WHERE is_default = true LIMIT 1")
            .fetch_optional(pool)
            .await
            .context("failed to read the default language for pagefind index")?
            .map(|id: String| pagefind_language(&id))
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "en".to_string());

    // Batch-load URL aliases so result cards link to friendly paths: each
    // item's canonical Live alias, the most recent, as the item page picks it.
    // One row per source, so a full build and an update restricted to a few
    // items always choose the same alias.
    let sources: Option<Vec<String>> =
        ids.map(|ids| ids.iter().map(|id| format!("/item/{id}")).collect());
    let alias_rows = sqlx::query_as::<_, AliasRow>(
        "SELECT DISTINCT ON (source) source, alias FROM url_alias \
         WHERE source LIKE '/item/%' AND stage_id = $1 \
           AND ($2::text[] IS NULL OR source = ANY($2)) \
         ORDER BY source, created DESC, id DESC",
    )
    .bind(LIVE_STAGE_ID)
    .bind(sources)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let alias_map: HashMap<String, String> = alias_rows
        .into_iter()
        .map(|r| (r.source, r.alias))
        .collect();

    let mut documents = Vec::with_capacity(visible.len());
    for item in &visible {
        let fields = searchable_fields(
            &item.fields,
            text_fields
                .get(&item.item_type)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            config_map
                .get(&item.item_type)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        );

        // Use friendly URL alias if available, otherwise /item/{id}
        let source_path = format!("/item/{}", item.id);
        let url = alias_map.get(&source_path).cloned().unwrap_or(source_path);

        // Extract structured metadata from fields for richer result cards
        // The long text field under either of its two names (BL-21), so a
        // `page` item's result card is not left with no description.
        let description = extract_field_text(&item.fields, "field_description")
            .or_else(|| crate::content::body_field::body_text(&item.fields))
            .unwrap_or_default();
        // Plain text, then truncated, for a clean description meta.
        let description_clean = plain_text(&description);

        documents.push(IndexDocument {
            id: item.id,
            item_type: item.item_type.clone(),
            title: plain_text(&item.title),
            language: Some(pagefind_language(&item.language))
                .filter(|lang| !lang.is_empty() && lang != "und")
                .unwrap_or_else(|| default_language.clone()),
            url,
            fields,
            comments: comments_by_item.remove(&item.id).unwrap_or_default(),
            description: truncate_meta(&description_clean, 200),
            location: build_location_meta(&item.fields),
            event_dates: build_date_range_meta(&item.fields),
            created_display: format_unix_date(item.created),
        });
    }

    Ok(documents)
}

/// The Pagefind language key for a language code: trimmed and lowercased, as
/// Pagefind lowercases `<html lang>`.
fn pagefind_language(code: &str) -> String {
    code.trim().to_lowercase()
}

/// One item's HTML fragment, as the search index tokenizer reads it.
///
/// Pagefind indexes only inside `data-pagefind-body` when a page has one, so
/// everything searchable goes in there: the title, at weight A and still the
/// result's `title` meta, then each field in its own element at its own weight,
/// heaviest first, then each visible comment. The result URL stays the item's: a
/// match in a comment is a result for the item. The meta values sit outside the region; they are shown on
/// the result card but not searched.
///
/// `<html lang>` carries the document's language, so Pagefind stems the page
/// and files it under that language's index. A document with no language gets
/// no `lang`, as every page did before the native build.
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
    // Comments last, each in its own element at Pagefind's default weight.
    for comment in &document.comments {
        body.push_str(&format!("\n<p>{}</p>", html_escape(comment)));
    }

    let lang = if document.language.is_empty() {
        String::new()
    } else {
        format!(" lang=\"{}\"", html_escape(&document.language))
    };
    format!(
        "<html{lang}><head><title>{title}</title></head>\n\
         <body>\n\
         <div data-pagefind-body>\n{body}\n</div>\n\
         {meta_tags}\n\
         </body></html>",
        title = html_escape(&document.title),
    )
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

/// What text a field type contributes to search: a `Text`/`TextLong` value
/// (and its summary), a Link's `title` (never its `uri`), or a List's label
/// for the key it stores (looked up in its own `allowed_values`). A
/// Telephone's number is never indexed.
#[derive(Debug, Clone)]
enum SearchFieldKind {
    Text,
    Link,
    List(Vec<(String, String)>),
}

impl SearchFieldKind {
    fn of(field_type: &trovato_sdk::types::FieldType, settings: &serde_json::Value) -> Self {
        use trovato_sdk::types::FieldType;
        match field_type {
            FieldType::Link => Self::Link,
            FieldType::List => Self::List(crate::content::field_value::list_options(settings)),
            _ => Self::Text,
        }
    }
}

/// One field name this type declares text for, and what kind of text it is.
#[derive(Debug, Clone)]
struct SearchableField {
    name: String,
    kind: SearchFieldKind,
}

/// Every text field an item has text in, as plain text, heaviest first, then by
/// field name.
///
/// Which fields: every text field the type declares (`text_fields`), every
/// field configured in `search_field_config` (`configs`), and the long text
/// field under either of its names (BL-21), so a type that declares no fields
/// is still findable by its body. Each at its configured weight, D when it has
/// none: the configuration says how much a field counts, never whether it is
/// searched at all. `fields` is the item's fields after the anonymous field
/// access pass, so a field the seam dropped is not there to be read.
fn searchable_fields(
    fields: &serde_json::Value,
    text_fields: &[SearchableField],
    configs: &[(String, f32)],
) -> Vec<IndexedField> {
    let mut names: Vec<(&str, &SearchFieldKind)> = text_fields
        .iter()
        .map(|f| (f.name.as_str(), &f.kind))
        .collect();
    for (name, _) in configs {
        if !names.iter().any(|(n, _)| *n == name) {
            names.push((name.as_str(), &SearchFieldKind::Text));
        }
    }
    for name in crate::content::body_field::BODY_FIELD_NAMES {
        if !names.iter().any(|(n, _)| *n == name) {
            names.push((name, &SearchFieldKind::Text));
        }
    }
    names.sort_unstable_by_key(|(n, _)| *n);
    names.dedup_by_key(|(n, _)| *n);

    let mut indexed: Vec<IndexedField> = names
        .into_iter()
        .filter_map(|(name, kind)| {
            let text = field_plain_text(fields, name, kind)?;
            let weight = configs
                .iter()
                .find(|(configured, _)| configured == name)
                .map(|(_, weight)| *weight)
                .unwrap_or_else(|| pagefind_weight('D'));
            Some(IndexedField {
                name: name.to_string(),
                weight,
                text,
            })
        })
        .collect();

    indexed.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    indexed
}

/// One field's searchable text, as plain text, read the way its
/// [`SearchFieldKind`] says to: a `Text`/`TextLong` value (and its summary), a
/// Link's `title`, or a List's label for the key it stores. A field holding
/// more than one value (an array) contributes every value. `None` when
/// nothing is left.
fn field_plain_text(
    fields: &serde_json::Value,
    name: &str,
    kind: &SearchFieldKind,
) -> Option<String> {
    fn collect_text<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::String(s) => out.push(s),
            serde_json::Value::Object(obj) => {
                for key in ["value", "summary"] {
                    if let Some(serde_json::Value::String(s)) = obj.get(key) {
                        out.push(s);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for v in values {
                    collect_text(v, out);
                }
            }
            _ => {}
        }
    }
    fn collect_link<'a>(value: &'a serde_json::Value, out: &mut Vec<&'a str>) {
        match value {
            serde_json::Value::Object(obj) => {
                if let Some(serde_json::Value::String(s)) = obj.get("title") {
                    out.push(s);
                }
            }
            serde_json::Value::Array(values) => {
                for v in values {
                    collect_link(v, out);
                }
            }
            _ => {}
        }
    }
    fn collect_list(
        value: &serde_json::Value,
        options: &[(String, String)],
        out: &mut Vec<String>,
    ) {
        match value {
            serde_json::Value::String(key) => {
                if let Some((_, label)) = options.iter().find(|(k, _)| k == key) {
                    out.push(label.clone());
                }
            }
            serde_json::Value::Array(values) => {
                for v in values {
                    collect_list(v, options, out);
                }
            }
            _ => {}
        }
    }

    let value = fields.get(name)?;
    let joined = match kind {
        SearchFieldKind::Text => {
            let mut parts = Vec::new();
            collect_text(value, &mut parts);
            parts.join(" ")
        }
        SearchFieldKind::Link => {
            let mut parts = Vec::new();
            collect_link(value, &mut parts);
            parts.join(" ")
        }
        SearchFieldKind::List(options) => {
            let mut parts = Vec::new();
            collect_list(value, options, &mut parts);
            parts.join(" ")
        }
    };
    let text = plain_text(&joined);
    (!text.is_empty()).then_some(text)
}

/// Extract text from a single JSONB field.
///
/// Handles both `{field_name: {value: "..."}}` (structured) and
/// `{field_name: "..."}` (plain string) formats.
fn extract_field_text(fields: &serde_json::Value, field_name: &str) -> Option<String> {
    crate::content::body_field::field_text(fields, field_name)
}
