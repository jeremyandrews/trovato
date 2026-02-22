//! Full-text search service.
//!
//! Uses PostgreSQL tsvector columns with GIN indexes for efficient
//! full-text search across content items.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

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
}

/// Search service for full-text content search.
#[derive(Clone)]
pub struct SearchService {
    pool: PgPool,
}

impl SearchService {
    /// Create a new search service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Search for items matching the query.
    ///
    /// Uses PostgreSQL full-text search with ts_rank for relevance scoring.
    /// Results are filtered to only include items whose `stage_id` is in
    /// `stage_ids`. If `user_id` is provided, also includes the user's
    /// draft items (still stage-filtered).
    pub async fn search(
        &self,
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
            });
        }

        // Convert query to tsquery format
        // Split on whitespace and join with & for AND search
        let ts_query = query_clean
            .split_whitespace()
            .map(|w| format!("{w}:*")) // Prefix matching
            .collect::<Vec<_>>()
            .join(" & ");

        debug!(query = %query_clean, ts_query = %ts_query, "executing search");

        // Get total count
        let total: i64 = if let Some(uid) = user_id {
            sqlx::query_scalar(
                r#"
                SELECT COUNT(*)
                FROM item
                WHERE search_vector @@ to_tsquery('english', $1)
                  AND (status = 1 OR author_id = $2)
                  AND stage_id = ANY($3)
                "#,
            )
            .bind(&ts_query)
            .bind(uid)
            .bind(stage_ids)
            .fetch_one(&self.pool)
            .await
            .context("failed to count search results")?
        } else {
            sqlx::query_scalar(
                r#"
                SELECT COUNT(*)
                FROM item
                WHERE search_vector @@ to_tsquery('english', $1)
                  AND status = 1
                  AND stage_id = ANY($2)
                "#,
            )
            .bind(&ts_query)
            .bind(stage_ids)
            .fetch_one(&self.pool)
            .await
            .context("failed to count search results")?
        };

        // Get ranked results
        // Headline source: title + body text for richer snippets
        let results = if let Some(uid) = user_id {
            sqlx::query_as::<_, SearchResultRow>(
                r#"
                SELECT
                    id,
                    type,
                    title,
                    ts_rank(search_vector, to_tsquery('english', $1)) as rank,
                    ts_headline(
                        'english',
                        COALESCE(title, '') || ' ' || COALESCE(
                            fields->'field_body'->>'value',
                            fields->>'field_body',
                            ''
                        ),
                        to_tsquery('english', $1),
                        'StartSel=<mark>, StopSel=</mark>, MaxWords=35, MinWords=15'
                    ) as snippet
                FROM item
                WHERE search_vector @@ to_tsquery('english', $1)
                  AND (status = 1 OR author_id = $2)
                  AND stage_id = ANY($3)
                ORDER BY rank DESC, created DESC
                LIMIT $4 OFFSET $5
                "#,
            )
            .bind(&ts_query)
            .bind(uid)
            .bind(stage_ids)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .context("failed to execute search query")?
        } else {
            sqlx::query_as::<_, SearchResultRow>(
                r#"
                SELECT
                    id,
                    type,
                    title,
                    ts_rank(search_vector, to_tsquery('english', $1)) as rank,
                    ts_headline(
                        'english',
                        COALESCE(title, '') || ' ' || COALESCE(
                            fields->'field_body'->>'value',
                            fields->>'field_body',
                            ''
                        ),
                        to_tsquery('english', $1),
                        'StartSel=<mark>, StopSel=</mark>, MaxWords=35, MinWords=15'
                    ) as snippet
                FROM item
                WHERE search_vector @@ to_tsquery('english', $1)
                  AND status = 1
                  AND stage_id = ANY($2)
                ORDER BY rank DESC, created DESC
                LIMIT $3 OFFSET $4
                "#,
            )
            .bind(&ts_query)
            .bind(stage_ids)
            .bind(limit)
            .bind(offset)
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
    pub async fn reindex_item(&self, item_id: Uuid) -> Result<()> {
        // Touch the item to trigger the search_vector update trigger
        sqlx::query(
            r#"
            UPDATE item
            SET changed = $2
            WHERE id = $1
            "#,
        )
        .bind(item_id)
        .bind(chrono::Utc::now().timestamp())
        .execute(&self.pool)
        .await
        .context("failed to reindex item")?;

        Ok(())
    }

    /// Reindex all items of a specific type.
    pub async fn reindex_bundle(&self, bundle: &str) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE item
            SET changed = $2
            WHERE type = $1
            "#,
        )
        .bind(bundle)
        .bind(chrono::Utc::now().timestamp())
        .execute(&self.pool)
        .await
        .context("failed to reindex bundle")?;

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
            snippet: row.snippet,
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
