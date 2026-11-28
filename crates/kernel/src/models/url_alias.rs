//! URL Alias model for human-readable URLs.
//!
//! Maps alias paths (e.g., /about-us) to source paths (e.g., /item/{uuid}).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

use super::stage::LIVE_STAGE_ID;

/// URL Alias record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UrlAlias {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Source path (e.g., "/item/550e8400-e29b-41d4-a716-446655440000").
    pub source: String,

    /// Alias path (e.g., "/about-us").
    pub alias: String,

    /// Language code (default: "en").
    pub language: String,

    /// Stage UUID referencing category_tag(id) in the "stages" category.
    pub stage_id: Uuid,

    /// Unix timestamp when created.
    #[serde(default = "crate::models::unix_now")]
    pub created: i64,

    /// Unix timestamp when last changed: set on insert, moved on every
    /// update. Config fixtures predating this column have no key for it and
    /// fall back to "now", the same way a missing `created` does.
    #[serde(default = "crate::models::unix_now")]
    pub changed: i64,
}

/// What [`UrlAlias::resolve_path`] found for a requested path.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AliasMatch {
    #[sqlx(flatten)]
    pub alias: UrlAlias,
    /// Whether the path matched the alias exactly (as requested or
    /// percent-decoded), not only ignoring case.
    pub exact: bool,
    /// The same source's alias in the requested language, if it has one.
    pub own_alias: Option<String>,
}

/// Input for creating a URL alias.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateUrlAlias {
    pub source: String,
    pub alias: String,
    pub language: Option<String>,
    pub stage_id: Option<Uuid>,
}

/// Input for updating a URL alias.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateUrlAlias {
    pub source: Option<String>,
    pub alias: Option<String>,
    pub language: Option<String>,
    pub stage_id: Option<Uuid>,
}

impl UrlAlias {
    /// Create a new URL alias.
    pub async fn create(pool: &PgPool, input: CreateUrlAlias) -> Result<Self> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let language = input.language.unwrap_or_else(|| "en".to_string());
        let stage_id = input.stage_id.unwrap_or(LIVE_STAGE_ID);

        let alias = sqlx::query_as::<_, UrlAlias>(
            r#"
            INSERT INTO url_alias (id, source, alias, language, stage_id, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $6)
            RETURNING id, source, alias, language, stage_id, created, changed
            "#,
        )
        .bind(id)
        .bind(&input.source)
        .bind(&input.alias)
        .bind(&language)
        .bind(stage_id)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create url alias")?;
        crate::menu::invalidate_config_menus();

        Ok(alias)
    }

    /// Find a URL alias by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let alias = sqlx::query_as::<_, UrlAlias>(
            "SELECT id, source, alias, language, stage_id, created, changed \
             FROM url_alias WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch url alias by id")?;

        Ok(alias)
    }

    /// Find a URL alias by alias path (for route resolution).
    /// Uses the live stage and language = 'en' by default.
    pub async fn find_by_alias(pool: &PgPool, alias_path: &str) -> Result<Option<Self>> {
        Self::find_by_alias_with_context(pool, alias_path, LIVE_STAGE_ID, "en").await
    }

    /// Find a URL alias by alias path with specific stage and language.
    /// Find an alias by path, stage, and language.
    ///
    /// Tries the requested language first, then `'en'`, then any language. An
    /// alias is written in its item's language, and the address has to resolve
    /// for a reader in any language, as it did when every alias was `en`;
    /// where one path has a row per language, the reader's own wins.
    pub async fn find_by_alias_with_context(
        pool: &PgPool,
        alias_path: &str,
        stage_id: Uuid,
        language: &str,
    ) -> Result<Option<Self>> {
        let alias = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created, changed
            FROM url_alias
            WHERE alias = $1 AND stage_id = $2
            ORDER BY CASE WHEN language = $3 THEN 0 WHEN language = 'en' THEN 1 ELSE 2 END,
                     created DESC, id DESC
            LIMIT 1
            "#,
        )
        .bind(alias_path)
        .bind(stage_id)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to fetch url alias by alias path")?;

        Ok(alias)
    }

    /// Resolve a requested path to an alias, in one indexed query (B161).
    ///
    /// `candidates` are the forms the path may be stored in, each matched
    /// exactly: the path as requested (percent-encoded) and percent-decoded, so
    /// `/es/recipes/quiche-mediterr%C3%A1neo-profundo` finds the alias stored
    /// as `quiche-mediterráneo-profundo` and an alias stored encoded still
    /// resolves as it did. When none matches exactly, a match ignoring case
    /// (`lower(alias)`, its own index) is returned with `exact` false, for the
    /// caller to redirect to the stored spelling.
    ///
    /// Rows are preferred as [`Self::find_by_alias_with_context`] prefers them
    /// (exact first, then `language`, then `en`, then any). `own_alias` is the
    /// same source's alias in `language` on the same stage, if it has one: the
    /// address to send a request to when it named the source by another
    /// language's alias.
    pub async fn resolve_path(
        pool: &PgPool,
        candidates: &[&str],
        stage_id: Uuid,
        language: &str,
    ) -> Result<Option<AliasMatch>> {
        let Some(lowered) = candidates.last() else {
            return Ok(None);
        };
        let found = sqlx::query_as::<_, AliasMatch>(
            r#"
            SELECT a.id, a.source, a.alias, a.language, a.stage_id, a.created, a.changed,
                   (a.alias = ANY($1)) AS exact,
                   (SELECT o.alias FROM url_alias o
                     WHERE o.source = a.source AND o.stage_id = a.stage_id AND o.language = $3
                     ORDER BY o.created DESC, o.id DESC LIMIT 1) AS own_alias
            FROM url_alias a
            WHERE (a.alias = ANY($1) OR lower(a.alias) = lower($2)) AND a.stage_id = $4
            ORDER BY (a.alias = ANY($1)) DESC,
                     CASE WHEN a.language = $3 THEN 0 WHEN a.language = 'en' THEN 1 ELSE 2 END,
                     a.created DESC, a.id DESC
            LIMIT 1
            "#,
        )
        .bind(candidates)
        .bind(*lowered)
        .bind(language)
        .bind(stage_id)
        .fetch_optional(pool)
        .await
        .context("failed to resolve a path to an alias")?;
        Ok(found)
    }

    /// Find all URL aliases for a given source path.
    pub async fn find_by_source(pool: &PgPool, source: &str) -> Result<Vec<Self>> {
        Self::find_by_source_with_context(pool, source, LIVE_STAGE_ID, "en").await
    }

    /// Every alias of a source path, on every stage and in every language,
    /// most recent first.
    pub async fn find_all_by_source(pool: &PgPool, source: &str) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created, changed
            FROM url_alias
            WHERE source = $1
            ORDER BY created DESC, id DESC
            "#,
        )
        .bind(source)
        .fetch_all(pool)
        .await
        .context("failed to fetch url aliases by source")?;

        Ok(aliases)
    }

    /// Find all URL aliases for a given source path with specific stage and language.
    pub async fn find_by_source_with_context(
        pool: &PgPool,
        source: &str,
        stage_id: Uuid,
        language: &str,
    ) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created, changed
            FROM url_alias
            WHERE source = $1 AND stage_id = $2 AND language = $3
            ORDER BY created DESC, id DESC
            "#,
        )
        .bind(source)
        .bind(stage_id)
        .bind(language)
        .fetch_all(pool)
        .await
        .context("failed to fetch url aliases by source")?;

        Ok(aliases)
    }

    /// Get the canonical (most recent) alias for a source path.
    /// Returns the alias path if found, otherwise None.
    pub async fn get_canonical_alias(pool: &PgPool, source: &str) -> Result<Option<String>> {
        Self::get_canonical_alias_with_context(pool, source, LIVE_STAGE_ID, "en").await
    }

    /// Get the canonical (most recent) alias for a source path on a stage,
    /// preferring one in `language` and otherwise the most recent in any:
    /// an item's alias is written in the item's language, and a reader in
    /// another one is still linked to it rather than to `/item/{id}`.
    pub async fn get_canonical_alias_with_context(
        pool: &PgPool,
        source: &str,
        stage_id: Uuid,
        language: &str,
    ) -> Result<Option<String>> {
        let alias: Option<String> = sqlx::query_scalar(
            r#"
            SELECT alias
            FROM url_alias
            WHERE source = $1 AND stage_id = $2
            ORDER BY (language = $3) DESC, created DESC, id DESC
            LIMIT 1
            "#,
        )
        .bind(source)
        .bind(stage_id)
        .bind(language)
        .fetch_optional(pool)
        .await
        .context("failed to get canonical alias")?;

        Ok(alias)
    }

    /// The canonical alias for a source path, with the row's `changed`.
    ///
    /// Only [`crate::gather::GatherService::outbound_alias_for_gather`] calls
    /// this: a feed's channel link needs the alias's own change time to fold
    /// into `Last-Modified`, which [`Self::get_canonical_alias`] (used much
    /// more widely, including on every item page) has no reason to carry.
    /// Kept as its own query rather than added to that one so that every other
    /// caller's statement, and its performance-test fingerprint, is untouched.
    pub async fn get_canonical_alias_and_changed(
        pool: &PgPool,
        source: &str,
    ) -> Result<Option<(String, i64)>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"
            SELECT alias, changed
            FROM url_alias
            WHERE source = $1 AND stage_id = $2
            ORDER BY (language = $3) DESC, created DESC, id DESC
            LIMIT 1
            "#,
        )
        .bind(source)
        .bind(LIVE_STAGE_ID)
        .bind("en")
        .fetch_optional(pool)
        .await
        .context("failed to get canonical alias and changed")?;

        Ok(row)
    }

    /// Get the canonical alias for many source paths in one query.
    ///
    /// Returns a map from source path to alias, holding only the sources that
    /// have one. "Canonical" means the same most-recent row
    /// [`Self::get_canonical_alias_with_context`] picks: `DISTINCT ON (source)`
    /// with the same ordering, so a page of rows and a single lookup cannot
    /// disagree about which alias is canonical.
    ///
    /// Exists for callers rendering a list of items — a feed of twenty entries
    /// is otherwise twenty round trips.
    pub async fn canonical_aliases_for(
        pool: &PgPool,
        sources: &[String],
        stage_id: Uuid,
        language: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        if sources.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (source) source, alias
            FROM url_alias
            WHERE source = ANY($1) AND stage_id = $2
            ORDER BY source, (language = $3) DESC, created DESC, id DESC
            "#,
        )
        .bind(sources)
        .bind(stage_id)
        .bind(language)
        .fetch_all(pool)
        .await
        .context("failed to get canonical aliases")?;

        Ok(rows.into_iter().collect())
    }

    /// The canonical alias for many sources, across a stage overlay.
    ///
    /// [`Self::canonical_aliases_for`] for a caller holding several stages at
    /// once — a gather executed with a stage overlay draws its rows from all of
    /// them, and resolving their addresses against only one would give a staged
    /// row the live site's address or none at all.
    ///
    /// "Canonical" is the same most-recent row every other lookup here picks, so
    /// a stage overlay does not change which alias wins for a source that has one
    /// in only one stage.
    pub async fn canonical_aliases_for_stages(
        pool: &PgPool,
        sources: &[String],
        stage_ids: &[Uuid],
        language: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        if sources.is_empty() || stage_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (source) source, alias
            FROM url_alias
            WHERE source = ANY($1) AND stage_id = ANY($2)
            ORDER BY source, (language = $3) DESC, created DESC, id DESC
            "#,
        )
        .bind(sources)
        .bind(stage_ids)
        .bind(language)
        .fetch_all(pool)
        .await
        .context("failed to get canonical aliases for stages")?;

        Ok(rows.into_iter().collect())
    }

    /// Resolve many aliases back to their source paths in one query.
    ///
    /// The reverse of [`Self::canonical_aliases_for`], and it exists for the same
    /// reason: menu building holds a page of link paths at once, and asking what
    /// each one points at cannot be a query per link on every page render.
    ///
    /// Returns a map from alias to source, holding only the aliases that resolve.
    /// `DISTINCT ON (alias)` with the same most-recent ordering the forward
    /// lookup uses, so the two cannot disagree about which row is canonical.
    pub async fn sources_for_aliases(
        pool: &PgPool,
        aliases: &[String],
        stage_ids: &[Uuid],
        language: &str,
    ) -> Result<std::collections::HashMap<String, String>> {
        if aliases.is_empty() || stage_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        let rows: Vec<(String, String)> = sqlx::query_as(
            r#"
            SELECT DISTINCT ON (alias) alias, source
            FROM url_alias
            WHERE alias = ANY($1) AND stage_id = ANY($2)
            ORDER BY alias, (language = $3) DESC, created DESC, id DESC
            "#,
        )
        .bind(aliases)
        .bind(stage_ids)
        .bind(language)
        .fetch_all(pool)
        .await
        .context("failed to resolve aliases to sources")?;

        Ok(rows.into_iter().collect())
    }

    /// Get the canonical URL for an item.
    /// Returns the alias path if found, otherwise `/item/{id}`.
    pub async fn get_canonical_url(pool: &PgPool, item_id: Uuid) -> Result<String> {
        let source = format!("/item/{item_id}");
        if let Some(alias) = Self::get_canonical_alias(pool, &source).await? {
            Ok(alias)
        } else {
            Ok(source)
        }
    }

    /// Update a URL alias.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateUrlAlias) -> Result<Option<Self>> {
        let Some(existing) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        let source = input.source.unwrap_or(existing.source);
        let alias = input.alias.unwrap_or(existing.alias);
        let language = input.language.unwrap_or(existing.language);
        let stage_id = input.stage_id.unwrap_or(existing.stage_id);
        let now = chrono::Utc::now().timestamp();

        let updated = sqlx::query_as::<_, UrlAlias>(
            r#"
            UPDATE url_alias
            SET source = $1, alias = $2, language = $3, stage_id = $4, changed = $5
            WHERE id = $6
            RETURNING id, source, alias, language, stage_id, created, changed
            "#,
        )
        .bind(&source)
        .bind(&alias)
        .bind(&language)
        .bind(stage_id)
        .bind(now)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to update url alias")?;
        crate::menu::invalidate_config_menus();

        Ok(updated)
    }

    /// Delete a URL alias.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM url_alias WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete url alias")?;
        crate::menu::invalidate_config_menus();

        Ok(result.rows_affected() > 0)
    }

    /// [`Self::delete`] inside the caller's transaction. The caller
    /// invalidates the navigation cache once the transaction commits.
    pub async fn delete_in(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM url_alias WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .context("failed to delete url alias")?;
        Ok(result.rows_affected() > 0)
    }

    /// Delete all aliases for a source path.
    pub async fn delete_by_source(pool: &PgPool, source: &str) -> Result<u64> {
        let result = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(source)
            .execute(pool)
            .await
            .context("failed to delete url aliases by source")?;
        crate::menu::invalidate_config_menus();

        Ok(result.rows_affected())
    }

    /// Delete every alias of a source path inside the caller's transaction.
    ///
    /// The caller invalidates the navigation cache once the transaction
    /// commits.
    pub async fn delete_by_source_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
    ) -> Result<u64> {
        let result = sqlx::query("DELETE FROM url_alias WHERE source = $1")
            .bind(source)
            .execute(&mut **tx)
            .await
            .context("failed to delete url aliases by source")?;
        Ok(result.rows_affected())
    }

    /// List all URL aliases with pagination.
    pub async fn list_all(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created, changed
            FROM url_alias
            ORDER BY created DESC, id DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
        .context("failed to list all url aliases")?;

        Ok(aliases)
    }

    /// Count all URL aliases.
    pub async fn count_all(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM url_alias")
            .fetch_one(pool)
            .await
            .context("failed to count all url aliases")?;

        Ok(count)
    }

    /// One page of the admin alias listing, and how many aliases match.
    ///
    /// `like` is an escaped `ILIKE` pattern matched against the alias and its
    /// source, bound as a parameter. `order_by` must come from the listing's
    /// own allowed columns (`routes::admin_list::Sort::order_by`), never
    /// from a request.
    pub async fn admin_page(
        pool: &PgPool,
        like: Option<&str>,
        order_by: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Self>, i64)> {
        let condition = if like.is_some() {
            " WHERE (alias ILIKE $1 OR source ILIKE $1)"
        } else {
            ""
        };
        let n = usize::from(like.is_some());
        // Ties: newest first, then the id; a sort by `created` needs only
        // the id after it.
        let ties = if order_by.starts_with("created ") {
            "id DESC"
        } else {
            "created DESC, id DESC"
        };
        let list_sql = format!(
            "SELECT id, source, alias, language, stage_id, created, changed \
             FROM url_alias{condition} ORDER BY {order_by}, {ties} \
             LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let count_sql = format!("SELECT COUNT(*) FROM url_alias{condition}");

        let mut list = sqlx::query_as::<_, UrlAlias>(AssertSqlSafe(list_sql));
        let mut count = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
        if let Some(like) = like {
            list = list.bind(like);
            count = count.bind(like);
        }
        let aliases = list
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
            .context("failed to list the admin alias page")?;
        let total = count
            .fetch_one(pool)
            .await
            .context("failed to count the admin alias page")?;
        Ok((aliases, total))
    }

    /// Find aliases that conflict with a given alias path across stages.
    ///
    /// Returns aliases in other stages (excluding `excluding_stage` and the live
    /// stage) that share the same alias path and language. Used by publish
    /// conflict detection.
    pub async fn find_conflicting_aliases(
        pool: &PgPool,
        alias_path: &str,
        language: &str,
        excluding_stage: Uuid,
    ) -> Result<Vec<Self>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            r#"
            SELECT id, source, alias, language, stage_id, created, changed
            FROM url_alias
            WHERE alias = $1 AND language = $2 AND stage_id != $3 AND stage_id != $4
            ORDER BY created DESC
            "#,
        )
        .bind(alias_path)
        .bind(language)
        .bind(excluding_stage)
        .bind(LIVE_STAGE_ID)
        .fetch_all(pool)
        .await
        .context("failed to find conflicting aliases")?;

        Ok(aliases)
    }

    /// Create or update an alias for a source path.
    /// If an alias already exists for this source (same stage/language), update it.
    /// Otherwise, create a new alias.
    pub async fn upsert_for_source(
        pool: &PgPool,
        source: &str,
        alias_path: &str,
        stage_id: Uuid,
        language: &str,
    ) -> Result<Self> {
        // Check if there's an existing alias for this source
        let existing = Self::find_by_source_with_context(pool, source, stage_id, language).await?;

        if let Some(first) = existing.into_iter().next() {
            // Update existing alias
            let updated = Self::update(
                pool,
                first.id,
                UpdateUrlAlias {
                    source: None,
                    alias: Some(alias_path.to_string()),
                    language: None,
                    stage_id: None,
                },
            )
            .await?;
            Ok(updated.ok_or_else(|| {
                anyhow::anyhow!("url_alias update returned None for existing record")
            })?)
        } else {
            // Create new alias
            Self::create(
                pool,
                CreateUrlAlias {
                    source: source.to_string(),
                    alias: alias_path.to_string(),
                    language: Some(language.to_string()),
                    stage_id: Some(stage_id),
                },
            )
            .await
        }
    }
}
