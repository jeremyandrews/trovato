//! Full-text search service.
//!
//! Uses PostgreSQL tsvector columns with GIN indexes for efficient
//! full-text search across content items.

/// The longest search or autocomplete query any API accepts, in bytes: the
/// JSON API and the MCP tools refuse a longer one alike.
pub const MAX_QUERY_LENGTH: usize = 1000;

pub mod analytics;
pub mod index_queue;
pub mod prompts;
pub mod sentiment;
pub mod settings;
pub(crate) mod snippet;

pub use settings::{PriorityPage, SearchSettings};

use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgPool};
use tracing::debug;
use uuid::Uuid;

use crate::cache::CacheLayer;
use analytics::SearchAnalytics;

/// Reduce a query to the form it is compared and counted in: trimmed,
/// internal whitespace collapsed, lowercased.
///
/// The one definition: the expansion cache keys on it and search analytics
/// stores it (after removing identifying tokens).
pub fn normalize_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A content type's two search switches, stored under `settings.search` in
/// its `item_type` row (so config export and import carry them with the type).
///
/// Both default to on: a type with no `search` key, which is every type on a
/// site that never opened the screen, is searched with its comments. Only an
/// explicit `false` turns one off. Both backends honour them: the Pagefind
/// export leaves the type's items (or their comments) out, and the
/// `item_search_vector` SQL function reads the same two keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSwitches {
    /// Whether the type's items are in search at all.
    pub include: bool,
    /// Whether the type's items are found by their published comments.
    pub comments: bool,
}

impl Default for SearchSwitches {
    fn default() -> Self {
        Self {
            include: true,
            comments: true,
        }
    }
}

impl SearchSwitches {
    /// The `settings` key the switches live under.
    pub const SETTINGS_KEY: &'static str = "search";

    /// Read the switches out of a type's `settings`. Anything that is not an
    /// explicit `false` reads as on.
    pub fn from_settings(settings: &serde_json::Value) -> Self {
        let switch = |name: &str| {
            settings
                .get(Self::SETTINGS_KEY)
                .and_then(|s| s.get(name))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
        };
        Self {
            include: switch("include"),
            comments: switch("comments"),
        }
    }
}

/// Whether a field type carries text that search indexes: `Text` (string) and
/// `TextLong` (long text, with or without a summary). Files, references,
/// numbers, dates, booleans and structured fields do not.
///
/// Both backends index every field of these types: the Pagefind export and
/// the `item_search_vector` SQL function, which reads the same two types out
/// of `item_type.settings`.
pub fn is_text_field(field_type: &trovato_sdk::types::FieldType) -> bool {
    use trovato_sdk::types::FieldType;
    matches!(field_type, FieldType::Text { .. } | FieldType::TextLong)
}

/// Search result with ranking information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Item ID.
    pub id: Uuid,
    /// Item type/bundle.
    pub item_type: String,
    /// Item title.
    pub title: String,
    /// Relevance rank (higher is better).
    pub rank: f32,
    /// Snippet with highlighted matches.
    pub snippet: Option<String>,
}

/// Collection of search results with pagination.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResults {
    /// Search query used.
    pub query: String,
    /// Matching results.
    pub results: Vec<SearchResult>,
    /// Total count of matches.
    pub total: i64,
    /// Current page offset.
    pub offset: i64,
    /// Page size limit.
    pub limit: i64,
    /// The query held no word that can be searched for: only words too common
    /// to index (`a`, `it's`) or punctuation (`&`). Nothing was searched, and
    /// the results page says why rather than only "no results".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_terms: bool,
}

/// Search service for full-text content search.
#[derive(Clone)]
pub struct SearchService {
    pool: PgPool,
    /// Where the search settings are read from, so a search costs no extra
    /// database round trip for them.
    cache: CacheLayer,
    /// The in memory query counter every recorded search adds to.
    analytics: Arc<SearchAnalytics>,
}

impl SearchService {
    /// Create a new search service, with an empty analytics aggregator.
    pub fn new(pool: PgPool, cache: CacheLayer) -> Self {
        let analytics = Arc::new(SearchAnalytics::new(pool.clone()));
        Self {
            pool,
            cache,
            analytics,
        }
    }

    /// The current search settings, read through the cache.
    pub async fn settings(&self) -> SearchSettings {
        SearchSettings::load(&self.pool, &self.cache).await
    }

    /// The search analytics aggregator.
    pub fn analytics(&self) -> &Arc<SearchAnalytics> {
        &self.analytics
    }

    /// Search as a person searching: [`search_unrecorded`](Self::search_unrecorded),
    /// then the query and its result total counted in search analytics.
    ///
    /// Counted once per search, on its first page: asking for page two of the
    /// same query is not another search. Counting is in memory and never waits
    /// on the database.
    pub async fn search(
        &self,
        query: &str,
        stage_ids: &[Uuid],
        user_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<SearchResults> {
        let settings = self.settings().await;
        let results = self
            .search_with(&settings, query, stage_ids, user_id, limit, offset)
            .await?;
        if offset == 0 {
            self.analytics
                .record(&settings, query, u64::try_from(results.total).unwrap_or(0));
        }
        Ok(results)
    }

    /// Search without counting it: for callers that are not a person typing a
    /// query (retrieval for the AI chat, the MCP tool, tests).
    pub async fn search_unrecorded(
        &self,
        query: &str,
        stage_ids: &[Uuid],
        user_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<SearchResults> {
        let settings = self.settings().await;
        self.search_with(&settings, query, stage_ids, user_id, limit, offset)
            .await
    }

    /// Search for items matching the query.
    ///
    /// Uses PostgreSQL full-text search with ts_rank for relevance scoring,
    /// multiplied by the item type's boost from [`SearchSettings::type_boosts`]
    /// (1.0 for a type not listed). The boosts are bound as two parallel arrays
    /// and joined in the same query, never one query per type.
    ///
    /// Results are filtered to only include items whose `stage_id` is in
    /// `stage_ids`. If `user_id` is provided, also includes the user's
    /// draft items (still stage-filtered).
    async fn search_with(
        &self,
        settings: &SearchSettings,
        query: &str,
        stage_ids: &[Uuid],
        user_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<SearchResults> {
        let query_clean = query.trim();
        if query_clean.is_empty() {
            return Ok(SearchResults {
                query: query.to_string(),
                results: vec![],
                total: 0,
                offset,
                limit,
                no_terms: false,
            });
        }

        // The visitor's words go to Postgres as they were typed, parsed by
        // `websearch_to_tsquery`: every word must match, "a quoted phrase"
        // matches as a phrase, `or` between words matches either and a leading
        // `-` excludes a word, which is the syntax Drupal's search taught
        // visitors. It never fails to parse: `&`, `<b>`, quotes and
        // apostrophes are read as punctuation, where the hand-built
        // `to_tsquery` text this replaced (each word with `:*` appended) was
        // tsquery syntax, so `noank & mystic` answered 500 (B160).
        //
        // Markup in the query is read as the indexed text is: the document
        // parser skips a tag, but `websearch_to_tsquery` would read `<b>` as
        // the word `b` and require it next to the one after, so the query's
        // text is taken first, with the extractor the snippet uses.
        let query_text = crate::content::page_meta::text_of(query_clean);
        let ts_query = query_text.as_str();

        debug!(query = %query_clean, ts_query = %ts_query, "executing search");

        let (boost_types, boost_values) = settings.type_boost_arrays();

        // Get total count
        let (total, terms): (i64, i32) = if let Some(uid) = user_id {
            sqlx::query_as(
                r#"
                SELECT COUNT(*), numnode(websearch_to_tsquery('english', $1))
                FROM item
                WHERE search_vector @@ websearch_to_tsquery('english', $1)
                  AND (status = 1 OR author_id = $2)
                  AND stage_id = ANY($3)
                "#,
            )
            .bind(ts_query)
            .bind(uid)
            .bind(stage_ids)
            .fetch_one(&self.pool)
            .await
            .context("failed to count search results")?
        } else {
            sqlx::query_as(
                r#"
                SELECT COUNT(*), numnode(websearch_to_tsquery('english', $1))
                FROM item
                WHERE search_vector @@ websearch_to_tsquery('english', $1)
                  AND status = 1
                  AND stage_id = ANY($2)
                "#,
            )
            .bind(ts_query)
            .bind(stage_ids)
            .fetch_one(&self.pool)
            .await
            .context("failed to count search results")?
        };

        // Get ranked results. The snippet is `ts_headline` over the title and
        // body with tags stripped, highlighted with fixed markers that
        // [`snippet::render`] escapes and turns into `<mark>` (B123).
        let results = if let Some(uid) = user_id {
            sqlx::query_as::<_, SearchResultRow>(
                // Built from compile-time constants only: no input in the text.
                AssertSqlSafe(format!(
                    r#"
                SELECT
                    id,
                    type,
                    title,
                    ts_rank(search_vector, websearch_to_tsquery('english', $1))
                        * COALESCE(boost.factor, 1.0::real) AS rank,
                    ts_headline(
                        'english',
                        -- B123: tags stripped from the source before
                        -- highlighting; see crate::search::snippet.
                        {source},
                        websearch_to_tsquery('english', $1),
                        '{options}'
                    ) as snippet
                FROM item
                LEFT JOIN unnest($6::text[], $7::real[]) AS boost(item_type, factor)
                    ON boost.item_type = item.type
                WHERE search_vector @@ websearch_to_tsquery('english', $1)
                  AND (status = 1 OR author_id = $2)
                  AND stage_id = ANY($3)
                ORDER BY rank DESC, created DESC
                LIMIT $4 OFFSET $5
                "#,
                    source = snippet::SOURCE_SQL,
                    options = snippet::HEADLINE_OPTIONS,
                )),
            )
            .bind(ts_query)
            .bind(uid)
            .bind(stage_ids)
            .bind(limit)
            .bind(offset)
            .bind(&boost_types)
            .bind(&boost_values)
            .fetch_all(&self.pool)
            .await
            .context("failed to execute search query")?
        } else {
            sqlx::query_as::<_, SearchResultRow>(
                // Built from compile-time constants only: no input in the text.
                AssertSqlSafe(format!(
                    r#"
                SELECT
                    id,
                    type,
                    title,
                    ts_rank(search_vector, websearch_to_tsquery('english', $1))
                        * COALESCE(boost.factor, 1.0::real) AS rank,
                    ts_headline(
                        'english',
                        -- B123: tags stripped from the source before
                        -- highlighting; see crate::search::snippet.
                        {source},
                        websearch_to_tsquery('english', $1),
                        '{options}'
                    ) as snippet
                FROM item
                LEFT JOIN unnest($5::text[], $6::real[]) AS boost(item_type, factor)
                    ON boost.item_type = item.type
                WHERE search_vector @@ websearch_to_tsquery('english', $1)
                  AND status = 1
                  AND stage_id = ANY($2)
                ORDER BY rank DESC, created DESC
                LIMIT $3 OFFSET $4
                "#,
                    source = snippet::SOURCE_SQL,
                    options = snippet::HEADLINE_OPTIONS,
                )),
            )
            .bind(ts_query)
            .bind(stage_ids)
            .bind(limit)
            .bind(offset)
            .bind(&boost_types)
            .bind(&boost_values)
            .fetch_all(&self.pool)
            .await
            .context("failed to execute search query")?
        };

        debug!(
            query = %query_clean,
            total = %total,
            returned = %results.len(),
            "search completed"
        );

        Ok(SearchResults {
            query: query.to_string(),
            results: results.into_iter().map(|r| r.into()).collect(),
            total,
            offset,
            limit,
            no_terms: terms == 0,
        })
    }

    /// Configure search indexing for a field.
    ///
    /// Sets the weight (A-D) for a specific field on a content type.
    pub async fn configure_field(
        &self,
        bundle: &str,
        field_name: &str,
        weight: char,
    ) -> Result<()> {
        if !['A', 'B', 'C', 'D'].contains(&weight) {
            anyhow::bail!("weight must be A, B, C, or D");
        }

        sqlx::query(
            r#"
            INSERT INTO search_field_config (id, bundle, field_name, weight)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (bundle, field_name)
            DO UPDATE SET weight = $4
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(bundle)
        .bind(field_name)
        .bind(weight.to_string())
        .execute(&self.pool)
        .await
        .context("failed to configure search field")?;
        // A weight is in every page of the type, as data-pagefind-weight.
        crate::search::index_queue::enqueue_type(&self.pool, bundle, "field_config").await?;

        debug!(bundle = %bundle, field = %field_name, weight = %weight, "search field configured");
        Ok(())
    }

    /// Remove search indexing configuration for a field.
    pub async fn remove_field_config(&self, bundle: &str, field_name: &str) -> Result<bool> {
        let result = sqlx::query(
            r#"
            DELETE FROM search_field_config
            WHERE bundle = $1 AND field_name = $2
            "#,
        )
        .bind(bundle)
        .bind(field_name)
        .execute(&self.pool)
        .await
        .context("failed to remove search field config")?;
        if result.rows_affected() > 0 {
            crate::search::index_queue::enqueue_type(&self.pool, bundle, "field_config").await?;
        }

        Ok(result.rows_affected() > 0)
    }

    /// List all search field configurations for a bundle.
    pub async fn list_field_configs(&self, bundle: &str) -> Result<Vec<FieldConfig>> {
        let configs = sqlx::query_as::<_, FieldConfigRow>(
            r#"
            SELECT field_name, weight
            FROM search_field_config
            WHERE bundle = $1
            ORDER BY field_name
            "#,
        )
        .bind(bundle)
        .fetch_all(&self.pool)
        .await
        .context("failed to list search field configs")?;

        Ok(configs.into_iter().map(|r| r.into()).collect())
    }

    /// Reindex a single item (useful after bulk field config changes).
    ///
    /// Recomputes the vector with `item_search_vector`, the function the
    /// `trg_item_search` trigger calls. Touching `changed`, which this used to
    /// do, never fired that trigger (it runs on `title`, `fields` and `type`
    /// only), so a reindex left the vector as it was.
    pub async fn reindex_item(&self, item_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE item
            SET search_vector = item_search_vector(id, type, title, fields)
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .execute(&self.pool)
        .await
        .context("failed to reindex item")?;
        crate::cache::generation::bump(crate::cache::generation::CacheKind::Items);

        Ok(())
    }

    /// Reindex all items of a specific type, as [`Self::reindex_item`] does one.
    pub async fn reindex_bundle(&self, bundle: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE item
            SET search_vector = item_search_vector(id, type, title, fields)
            WHERE type = $1
            "#,
        )
        .bind(bundle)
        .execute(&self.pool)
        .await
        .context("failed to reindex bundle")?;
        crate::cache::generation::bump(crate::cache::generation::CacheKind::Items);
        // And the static index: every page of the type is rebuilt from the item.
        crate::search::index_queue::enqueue_type(&self.pool, bundle, "reindex").await?;

        debug!(bundle = %bundle, count = %result.rows_affected(), "bundle reindexed");
        Ok(result.rows_affected())
    }
}

/// Internal row type for search results.
#[derive(sqlx::FromRow)]
struct SearchResultRow {
    id: Uuid,
    #[sqlx(rename = "type")]
    item_type: String,
    title: String,
    rank: f32,
    snippet: Option<String>,
}

impl From<SearchResultRow> for SearchResult {
    fn from(row: SearchResultRow) -> Self {
        Self {
            id: row.id,
            item_type: row.item_type,
            title: row.title,
            rank: row.rank,
            // The one place a snippet is built: see [`snippet`].
            snippet: row.snippet.as_deref().map(snippet::render),
        }
    }
}

/// Search field configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldConfig {
    pub field_name: String,
    pub weight: char,
}

#[derive(sqlx::FromRow)]
struct FieldConfigRow {
    field_name: String,
    weight: String,
}

impl From<FieldConfigRow> for FieldConfig {
    fn from(row: FieldConfigRow) -> Self {
        Self {
            field_name: row.field_name,
            weight: row.weight.chars().next().unwrap_or('C'),
        }
    }
}

impl std::fmt::Debug for SearchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchService").finish()
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_search_results_serde() {
        let results = SearchResults {
            query: "test".to_string(),
            results: vec![SearchResult {
                id: Uuid::now_v7(),
                item_type: "page".to_string(),
                title: "Test Page".to_string(),
                rank: 0.5,
                snippet: Some("<mark>Test</mark> content".to_string()),
            }],
            total: 1,
            offset: 0,
            limit: 10,
            no_terms: false,
        };

        let json = serde_json::to_string(&results).unwrap();
        let parsed: SearchResults = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.query, "test");
        assert_eq!(parsed.total, 1);
    }

    #[test]
    fn test_field_config() {
        let config = FieldConfig {
            field_name: "body".to_string(),
            weight: 'B',
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("body"));
        assert!(json.contains("B"));
    }
}
