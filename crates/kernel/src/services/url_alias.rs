//! URL alias writes.
//!
//! Every alias a person or a route writes goes through here: the alias admin
//! screens and the alias field on the front-end item form. What a write must
//! also do lives on the model it wraps (the navigation cache is invalidated by
//! [`UrlAlias`] itself) and the kernel defines no alias taps, so this is a thin
//! layer, there so that the next side effect has one place to go. Pathauto
//! (`services::pathauto`) and an item's delete (`Item::delete_in`) write
//! aliases as part of an item write, inside `ItemService`.

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::Item;
use crate::models::url_alias::{CreateUrlAlias, UpdateUrlAlias, UrlAlias};
use crate::services::audit::AuditTrail;

/// A path as aliases store it: with its leading slash.
pub fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    }
}

/// Service for URL alias writes.
#[derive(Clone)]
pub struct UrlAliasService {
    pool: PgPool,
    audit: AuditTrail,
}

impl UrlAliasService {
    /// Create the service.
    pub fn new(pool: PgPool, audit: AuditTrail) -> Self {
        Self { pool, audit }
    }

    /// Record one alias write in the audit log.
    async fn audit(&self, action: &str, alias: &UrlAlias) {
        self.audit
            .record(
                action,
                "url_alias",
                alias.id,
                None,
                serde_json::json!({ "source": alias.source, "alias": alias.alias }),
            )
            .await;
    }

    /// Create an alias. Both paths are stored with their leading slash.
    pub async fn create(&self, mut input: CreateUrlAlias) -> Result<UrlAlias> {
        input.source = normalize_path(&input.source);
        input.alias = normalize_path(&input.alias);
        let alias = UrlAlias::create(&self.pool, input).await?;
        self.audit("create", &alias).await;
        crate::search::index_queue::enqueue_alias_source(&self.pool, &alias.source).await;
        Ok(alias)
    }

    /// Update an alias. `None` when there is no such alias.
    pub async fn update(&self, id: Uuid, mut input: UpdateUrlAlias) -> Result<Option<UrlAlias>> {
        input.source = input.source.as_deref().map(normalize_path);
        input.alias = input.alias.as_deref().map(normalize_path);
        // The source it pointed at before, which may stop being aliased.
        let before = UrlAlias::find_by_id(&self.pool, id).await.ok().flatten();
        let alias = UrlAlias::update(&self.pool, id, input).await?;
        if let Some(alias) = &alias {
            self.audit("update", alias).await;
            crate::search::index_queue::enqueue_alias_source(&self.pool, &alias.source).await;
            if let Some(before) = before.filter(|b| b.source != alias.source) {
                crate::search::index_queue::enqueue_alias_source(&self.pool, &before.source).await;
            }
        }
        Ok(alias)
    }

    /// Delete an alias. `false` when there is no such alias.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let before = UrlAlias::find_by_id(&self.pool, id).await.ok().flatten();
        let deleted = UrlAlias::delete(&self.pool, id).await?;
        if deleted {
            if let Some(before) = &before {
                crate::search::index_queue::enqueue_alias_source(&self.pool, &before.source).await;
            }
            self.audit
                .record("delete", "url_alias", id, None, serde_json::json!({}))
                .await;
        }
        Ok(deleted)
    }

    /// Set `item`'s alias to `alias`, on the item's stage and in its language,
    /// replacing the one it has there.
    pub async fn set_for_item(&self, item: &Item, alias: &str) -> Result<UrlAlias> {
        let alias = UrlAlias::upsert_for_source(
            &self.pool,
            &format!("/item/{}", item.id),
            &normalize_path(alias),
            item.stage_id,
            &item.language,
        )
        .await
        .context("failed to set the item's alias")?;
        self.audit("update", &alias).await;
        crate::search::index_queue::enqueue_alias_source(&self.pool, &alias.source).await;
        Ok(alias)
    }

    /// Remove every alias of `item`. Returns how many there were.
    pub async fn clear_for_item(&self, item: &Item) -> Result<u64> {
        let source = format!("/item/{}", item.id);
        let removed = UrlAlias::delete_by_source(&self.pool, &source).await?;
        if removed > 0 {
            crate::search::index_queue::enqueue_alias_source(&self.pool, &source).await;
            self.audit
                .record(
                    "delete",
                    "url_alias",
                    &source,
                    None,
                    serde_json::json!({ "source": source, "count": removed }),
                )
                .await;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_path;

    #[test]
    fn a_path_gets_one_leading_slash() {
        assert_eq!(normalize_path("about"), "/about");
        assert_eq!(normalize_path("/about"), "/about");
        assert_eq!(normalize_path("  about "), "/about");
    }
}
