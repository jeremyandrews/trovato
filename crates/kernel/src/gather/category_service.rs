//! Category service with caching.
//!
//! Provides high-level operations on categories and tags with DashMap caching
//! for fast lookups.

use crate::models::{
    Category, CreateCategory, CreateTag, Tag, TagWithDepth, UpdateCategory, UpdateTag,
};
use anyhow::Result;
use dashmap::DashMap;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// Service for managing categories and tags with caching.
pub struct CategoryService {
    pool: PgPool,
    /// Cache: category id -> tags in that category
    tag_cache: DashMap<String, Vec<Tag>>,
    /// Cache: category id -> category
    category_cache: DashMap<String, Category>,
}

impl CategoryService {
    /// Create a new CategoryService.
    pub fn new(pool: PgPool) -> Arc<Self> {
        Arc::new(Self {
            pool,
            tag_cache: DashMap::new(),
            category_cache: DashMap::new(),
        })
    }

    // -------------------------------------------------------------------------
    // Category operations
    // -------------------------------------------------------------------------

    /// Get a category by ID, with caching.
    pub async fn get_category(&self, id: &str) -> Result<Option<Category>> {
        // Check cache first
        if let Some(category) = self.category_cache.get(id) {
            return Ok(Some(category.clone()));
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
        }
        Ok(category)
    }

    /// Delete a category.
    pub async fn delete_category(&self, id: &str) -> Result<bool> {
        let deleted = Category::delete(&self.pool, id).await?;
        if deleted {
            self.category_cache.remove(id);
            self.tag_cache.remove(id);
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
        if let Some(tags) = self.tag_cache.get(category_id) {
            return Ok(tags.clone());
        }

        // Fetch from database
        let tags = Tag::list_by_category(&self.pool, category_id).await?;

        // Cache
        self.tag_cache.insert(category_id.to_string(), tags.clone());

        Ok(tags)
    }

    /// Create a new tag.
    pub async fn create_tag(&self, input: CreateTag) -> Result<Tag> {
        let category_id = input.category_id.clone();
        let tag = Tag::create(&self.pool, input).await?;
        self.invalidate_cache(&category_id);
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

        Ok(tag)
    }

    /// Delete a tag.
    pub async fn delete_tag(&self, id: Uuid) -> Result<bool> {
        // Get tag to know which category cache to invalidate
        let tag = Tag::find_by_id(&self.pool, id).await?;
        let deleted = Tag::delete(&self.pool, id).await?;

        if let Some(t) = tag {
            self.invalidate_cache(&t.category_id);
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

    /// Invalidate cache for a category.
    pub fn invalidate_cache(&self, category_id: &str) {
        self.tag_cache.remove(category_id);
    }

    /// Clear all caches.
    pub fn clear_cache(&self) {
        self.tag_cache.clear();
        self.category_cache.clear();
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn category_service_cache_types() {
        // Just verify the types compile correctly
        let _cache: DashMap<String, Vec<Tag>> = DashMap::new();
        let _category_cache: DashMap<String, Category> = DashMap::new();
    }
}
