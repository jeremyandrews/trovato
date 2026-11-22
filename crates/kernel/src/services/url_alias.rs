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
}

impl UrlAliasService {
    /// Create the service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create an alias. Both paths are stored with their leading slash.
    pub async fn create(&self, mut input: CreateUrlAlias) -> Result<UrlAlias> {
        input.source = normalize_path(&input.source);
        input.alias = normalize_path(&input.alias);
        UrlAlias::create(&self.pool, input).await
    }

    /// Update an alias. `None` when there is no such alias.
    pub async fn update(&self, id: Uuid, mut input: UpdateUrlAlias) -> Result<Option<UrlAlias>> {
        input.source = input.source.as_deref().map(normalize_path);
        input.alias = input.alias.as_deref().map(normalize_path);
        UrlAlias::update(&self.pool, id, input).await
    }

    /// Delete an alias. `false` when there is no such alias.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        UrlAlias::delete(&self.pool, id).await
    }

    /// Set `item`'s alias to `alias`, on the item's stage and in its language,
    /// replacing the one it has there.
    pub async fn set_for_item(&self, item: &Item, alias: &str) -> Result<UrlAlias> {
        UrlAlias::upsert_for_source(
            &self.pool,
            &format!("/item/{}", item.id),
            &normalize_path(alias),
            item.stage_id,
            &item.language,
        )
        .await
        .context("failed to set the item's alias")
    }

    /// Remove every alias of `item`. Returns how many there were.
    pub async fn clear_for_item(&self, item: &Item) -> Result<u64> {
        UrlAlias::delete_by_source(&self.pool, &format!("/item/{}", item.id)).await
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
