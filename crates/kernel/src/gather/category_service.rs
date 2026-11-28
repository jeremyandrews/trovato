//! Category service with caching.
//!
//! Provides high-level operations on categories and tags with TTL-based
//! Moka caching for fast lookups.

use crate::cache::generation::{CacheKind, bump};
use crate::metrics::{CacheName, Metrics};
use crate::models::{
    Category, CreateCategory, CreateTag, Tag, TagWithDepth, UpdateCategory, UpdateTag,
};
use anyhow::Result;
use moka::sync::Cache;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Maximum entries in each category cache.
const MAX_CAPACITY: u64 = 1_000;

/// Service for managing categories and tags with caching.
pub struct CategoryService {
    pool: PgPool,
    /// Cache: category id -> tags in that category
    tag_cache: Cache<String, Vec<Tag>>,
    /// Cache: category id -> category
    category_cache: Cache<String, Category>,
    /// Counts hits and misses in both caches.
    metrics: Arc<Metrics>,
    /// Set once by `AppState`; a service built without it records nothing.
    audit: std::sync::OnceLock<crate::services::audit::AuditTrail>,
}

impl CategoryService {
    /// Create a new CategoryService.
    pub fn new(pool: PgPool, ttl: Duration, metrics: Arc<Metrics>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            tag_cache: Cache::builder()
                .max_capacity(MAX_CAPACITY)
                .time_to_live(ttl)
                .build(),
            category_cache: Cache::builder()
                .max_capacity(MAX_CAPACITY)
                .time_to_live(ttl)
                .build(),
            metrics,
            audit: std::sync::OnceLock::new(),
        })
    }

    /// Record category and tag writes through `audit`. Set once, at startup.
    pub fn set_audit_trail(&self, audit: crate::services::audit::AuditTrail) {
        let _ = self.audit.set(audit);
    }

    /// Record one category or tag write in the audit log.
    async fn audit(
        &self,
        action: &str,
        entity_type: &str,
        id: impl std::fmt::Display,
        details: serde_json::Value,
    ) {
        if let Some(audit) = self.audit.get() {
            audit.record(action, entity_type, id, None, details).await;
        }
    }

    // -------------------------------------------------------------------------
    // Category operations
    // -------------------------------------------------------------------------

    /// Get a category by ID, with caching.
    pub async fn get_category(&self, id: &str) -> Result<Option<Category>> {
        // Check cache first
        if let Some(category) = self
            .metrics
            .cache_lookup(CacheName::Category, self.category_cache.get(id))
        {
            return Ok(Some(category));
        }

        // Fetch from database
        let category = Category::find_by_id(&self.pool, id).await?;

        // Cache if found
        if let Some(ref c) = category {
            self.category_cache.insert(id.to_string(), c.clone());
        }

        Ok(category)
    }

    /// List all categories.
    pub async fn list_categories(&self) -> Result<Vec<Category>> {
        Category::list(&self.pool).await
    }

    /// Create a new category.
    pub async fn create_category(&self, input: CreateCategory) -> Result<Category> {
        let category = Category::create(&self.pool, input).await?;
        self.category_cache
            .insert(category.id.clone(), category.clone());
        bump(CacheKind::Categories);
        crate::edge_cache::purge([crate::edge_cache::tags::SITE]);
        self.audit(
            "create",
            "category",
            &category.id,
            serde_json::json!({ "label": category.label }),
        )
        .await;
        Ok(category)
    }

    /// Update a category.
    pub async fn update_category(
        &self,
        id: &str,
        input: UpdateCategory,
    ) -> Result<Option<Category>> {
        let category = Category::update(&self.pool, id, input).await?;
        if let Some(ref c) = category {
            self.category_cache.insert(id.to_string(), c.clone());
            bump(CacheKind::Categories);
            crate::edge_cache::purge([crate::edge_cache::tags::SITE]);
            self.audit(
                "update",
                "category",
                id,
                serde_json::json!({ "label": c.label }),
            )
            .await;
        }
        Ok(category)
    }

    /// Delete a category.
    pub async fn delete_category(&self, id: &str) -> Result<bool> {
        let deleted = Category::delete(&self.pool, id).await?;
        if deleted {
            self.category_cache.invalidate(id);
            self.tag_cache.invalidate(id);
            bump(CacheKind::Categories);
            crate::edge_cache::purge([crate::edge_cache::tags::SITE]);
            self.audit("delete", "category", id, serde_json::json!({}))
                .await;
        }
        Ok(deleted)
    }

    // -------------------------------------------------------------------------
    // Tag operations
    // -------------------------------------------------------------------------

    /// Get a tag by ID.
    pub async fn get_tag(&self, id: Uuid) -> Result<Option<Tag>> {
        Tag::find_by_id(&self.pool, id).await
    }

    /// List tags in a category, with caching.
    pub async fn list_tags(&self, category_id: &str) -> Result<Vec<Tag>> {
        // Check cache first
        if let Some(tags) = self
            .metrics
            .cache_lookup(CacheName::CategoryTags, self.tag_cache.get(category_id))
        {
            return Ok(tags);
        }

        // Fetch from database
        let tags = Tag::list_by_category(&self.pool, category_id).await?;

        // Cache
        self.tag_cache.insert(category_id.to_string(), tags.clone());

        Ok(tags)
    }

    /// The terms of `vocabularies` whose name starts with `prefix`, ignoring
    /// case, ordered by name, at most `limit`: what a term reference field's
    /// autocomplete offers. Reads each vocabulary's cached term list.
    pub async fn search_tags(
        &self,
        vocabularies: &[String],
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<Tag>> {
        let prefix = prefix.trim().to_lowercase();
        if prefix.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for vocabulary in vocabularies {
            found.extend(
                self.list_tags(vocabulary)
                    .await?
                    .into_iter()
                    .filter(|t| t.label.to_lowercase().starts_with(&prefix)),
            );
        }
        found.sort_by(|a, b| {
            a.label
                .to_lowercase()
                .cmp(&b.label.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
        found.truncate(limit);
        Ok(found)
    }

    /// List all tags in a category with hierarchy depth for display.
    ///
    /// Returns tags ordered root-first (depth 0) then children, suitable for
    /// populating a `<select>` with indented options.
    pub async fn list_tags_with_depth(&self, category_id: &str) -> Result<Vec<TagWithDepth>> {
        Tag::list_with_depth(&self.pool, category_id).await
    }

    /// Create a new tag.
    pub async fn create_tag(&self, input: CreateTag) -> Result<Tag> {
        let category_id = input.category_id.clone();
        let tag = Tag::create(&self.pool, input).await?;
        self.invalidate_cache(&category_id);
        self.audit(
            "create",
            "tag",
            tag.id,
            serde_json::json!({ "category": category_id, "label": tag.label }),
        )
        .await;
        Ok(tag)
    }

    /// Update a tag.
    pub async fn update_tag(&self, id: Uuid, input: UpdateTag) -> Result<Option<Tag>> {
        // Get current tag to know which category cache to invalidate
        let current = Tag::find_by_id(&self.pool, id).await?;
        let tag = Tag::update(&self.pool, id, input).await?;

        if let Some(ref c) = current {
            self.invalidate_cache(&c.category_id);
        }
        if let Some(ref t) = tag {
            self.audit(
                "update",
                "tag",
                id,
                serde_json::json!({ "category": t.category_id, "label": t.label }),
            )
            .await;
        }

        Ok(tag)
    }

    /// Delete a tag.
    pub async fn delete_tag(&self, id: Uuid) -> Result<bool> {
        // Get tag to know which category cache to invalidate
        let tag = Tag::find_by_id(&self.pool, id).await?;
        let deleted = Tag::delete(&self.pool, id).await?;

        if let Some(t) = tag {
            self.invalidate_cache(&t.category_id);
            if deleted {
                self.audit(
                    "delete",
                    "tag",
                    id,
                    serde_json::json!({ "category": t.category_id, "label": t.label }),
                )
                .await;
            }
        }

        Ok(deleted)
    }

    // -------------------------------------------------------------------------
    // Hierarchy operations
    // -------------------------------------------------------------------------

    /// Get root tags in a category.
    pub async fn get_root_tags(&self, category_id: &str) -> Result<Vec<Tag>> {
        Tag::get_roots(&self.pool, category_id).await
    }

    /// Get direct parents of a tag.
    pub async fn get_parents(&self, id: Uuid) -> Result<Vec<Tag>> {
        Tag::get_parents(&self.pool, id).await
    }

    /// Get direct children of a tag.
    pub async fn get_children(&self, id: Uuid) -> Result<Vec<Tag>> {
        Tag::get_children(&self.pool, id).await
    }

    /// Get all ancestors of a tag (for breadcrumbs).
    pub async fn get_ancestors(&self, id: Uuid) -> Result<Vec<TagWithDepth>> {
        Tag::get_ancestors(&self.pool, id).await
    }

    /// Get all descendants of a tag.
    pub async fn get_descendants(&self, id: Uuid) -> Result<Vec<TagWithDepth>> {
        Tag::get_descendants(&self.pool, id).await
    }

    /// Get breadcrumb path from root to tag.
    /// Returns ancestors ordered from root to immediate parent.
    pub async fn get_breadcrumb(&self, id: Uuid) -> Result<Vec<Tag>> {
        let ancestors = self.get_ancestors(id).await?;
        // Ancestors are returned deepest-first, reverse for breadcrumb order
        Ok(ancestors.into_iter().rev().map(|a| a.tag).collect())
    }

    /// Get a tag and all its descendant IDs (for category filtering).
    pub async fn get_tag_with_descendants(&self, id: Uuid) -> Result<Vec<Uuid>> {
        Tag::get_tag_and_descendant_ids(&self.pool, id).await
    }

    /// Set the parents of a tag.
    pub async fn set_parents(&self, id: Uuid, parent_ids: &[Uuid]) -> Result<()> {
        // Get tag to know which category cache to invalidate
        let tag = Tag::find_by_id(&self.pool, id).await?;
        Tag::set_parents(&self.pool, id, parent_ids).await?;

        if let Some(t) = tag {
            self.invalidate_cache(&t.category_id);
        }

        Ok(())
    }

    /// Add a parent to a tag.
    pub async fn add_parent(&self, id: Uuid, parent_id: Uuid) -> Result<()> {
        let tag = Tag::find_by_id(&self.pool, id).await?;
        Tag::add_parent(&self.pool, id, parent_id).await?;

        if let Some(t) = tag {
            self.invalidate_cache(&t.category_id);
        }

        Ok(())
    }

    /// Remove a parent from a tag.
    pub async fn remove_parent(&self, id: Uuid, parent_id: Uuid) -> Result<()> {
        let tag = Tag::find_by_id(&self.pool, id).await?;
        Tag::remove_parent(&self.pool, id, parent_id).await?;

        if let Some(t) = tag {
            self.invalidate_cache(&t.category_id);
        }

        Ok(())
    }

    // -------------------------------------------------------------------------
    // Cache management
    // -------------------------------------------------------------------------

    /// Invalidate a category's tag list, here and on every other server (which
    /// clear all their category caches). Call it after the write commits.
    pub fn invalidate_cache(&self, category_id: &str) {
        self.tag_cache.invalidate(category_id);
        bump(CacheKind::Categories);
        crate::edge_cache::purge([crate::edge_cache::tags::SITE]);
    }

    /// Clear all caches, here and on every other server.
    pub fn clear_cache(&self) {
        self.clear_local();
        bump(CacheKind::Categories);
    }

    /// Clear this server's caches only: what another server's bump does here.
    pub fn clear_local(&self) {
        self.tag_cache.invalidate_all();
        self.category_cache.invalidate_all();
    }

    // -------------------------------------------------------------------------
    // Statistics
    // -------------------------------------------------------------------------

    /// Count tags in a category.
    pub async fn count_tags(&self, category_id: &str) -> Result<i64> {
        Tag::count_by_category(&self.pool, category_id).await
    }

    /// Check if a category exists.
    pub async fn category_exists(&self, id: &str) -> Result<bool> {
        Category::exists(&self.pool, id).await
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn category_service_cache_types() {
        // Just verify the Moka cache types compile correctly
        let _cache: Cache<String, Vec<Tag>> = Cache::builder().max_capacity(10).build();
        let _category_cache: Cache<String, Category> = Cache::builder().max_capacity(10).build();
    }
}
