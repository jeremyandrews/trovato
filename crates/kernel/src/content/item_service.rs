//! Item service with tap integration.
//!
//! Provides CRUD operations for items with automatic tap invocations
//! for plugin taps (insert, update, delete, view, access).

use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use crate::models::{CreateItem, Item, ItemRevision, UpdateItem};
use crate::tap::{RequestState, TapDispatcher, UserContext};
use trovato_sdk::types::AccessResult;

/// Service for item CRUD operations with tap integration.
#[derive(Clone)]
pub struct ItemService {
    inner: Arc<ItemServiceInner>,
}

struct ItemServiceInner {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    cache: DashMap<Uuid, Item>,
}

/// Input for checking item access.
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs` for
/// plugin-side deserialization. The kernel serializes this; plugins deserialize
/// it. If you change fields here, update the SDK copy to match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemAccessInput {
    pub item_id: Uuid,
    pub item_type: String,
    pub author_id: Uuid,
    pub operation: String,
    pub user_id: Uuid,
}

impl ItemService {
    /// Create a new item service.
    pub fn new(pool: PgPool, dispatcher: Arc<TapDispatcher>) -> Self {
        Self {
            inner: Arc::new(ItemServiceInner {
                pool,
                dispatcher,
                cache: DashMap::new(),
            }),
        }
    }

    /// Create a new item with tap_item_insert invocation.
    pub async fn create(&self, input: CreateItem, user: &UserContext) -> Result<Item> {
        // Create the item in the database
        let item = Item::create(&self.inner.pool, input).await?;

        // Invoke tap_item_insert for post-insert taps
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = RequestState::without_services(user.clone());

        let _results = self
            .inner
            .dispatcher
            .dispatch("tap_item_insert", &item_json, state)
            .await;

        // Tap errors are logged by the dispatcher

        info!(item_id = %item.id, item_type = %item.item_type, "item created");
        Ok(item)
    }

    /// Load an item by ID.
    pub async fn load(&self, id: Uuid) -> Result<Option<Item>> {
        // Check cache first
        if let Some(item) = self.inner.cache.get(&id) {
            return Ok(Some(item.clone()));
        }

        // Load from database
        let item = Item::find_by_id(&self.inner.pool, id).await?;

        // Cache if found
        if let Some(ref i) = item {
            self.inner.cache.insert(id, i.clone());
        }

        Ok(item)
    }

    /// Load an item by ID with stage hierarchy overlay.
    ///
    /// Tries to find the item in the nearest stage in the ancestry chain.
    /// For example, with `stage_ids = ["review", "draft", "live"]`, returns
    /// the first match found when checking stage_id = review, then draft, then live.
    ///
    /// Falls back to `load()` if the item exists but isn't in any of the given stages
    /// (e.g., it was loaded by a direct UUID link).
    pub async fn load_with_overlay(&self, id: Uuid, stage_ids: &[String]) -> Result<Option<Item>> {
        // Check cache first (cache is stage-agnostic — items have single stage_id)
        if let Some(item) = self.inner.cache.get(&id) {
            // Verify the item's stage is in our overlay list
            if stage_ids.iter().any(|s| s == &item.stage_id) {
                return Ok(Some(item.clone()));
            }
        }

        // Load from database — the item has a single stage_id
        let item = Item::find_by_id(&self.inner.pool, id).await?;

        if let Some(ref i) = item {
            // Only return if the item is in one of the visible stages
            if stage_ids.iter().any(|s| s == &i.stage_id) {
                self.inner.cache.insert(id, i.clone());
                return Ok(Some(i.clone()));
            }
        }

        Ok(None)
    }

    /// Load an item and invoke tap_item_view for rendering.
    pub async fn load_for_view(
        &self,
        id: Uuid,
        user: &UserContext,
    ) -> Result<Option<(Item, Vec<String>)>> {
        let Some(item) = self.load(id).await? else {
            return Ok(None);
        };

        // Check access
        if !self.check_access(&item, "view", user).await? {
            return Ok(None); // Return None for access denied (shows as 404)
        }

        // Invoke tap_item_view for rendering transformations
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = RequestState::without_services(user.clone());

        let results = self
            .inner
            .dispatcher
            .dispatch("tap_item_view", &item_json, state)
            .await;

        // Collect render outputs
        let render_outputs: Vec<String> = results.into_iter().map(|r| r.output).collect();

        Ok(Some((item, render_outputs)))
    }

    /// Update an item with tap_item_update invocation.
    pub async fn update(
        &self,
        id: Uuid,
        input: UpdateItem,
        user: &UserContext,
    ) -> Result<Option<Item>> {
        // Load existing item
        let Some(existing) = self.load(id).await? else {
            return Ok(None);
        };

        // Check access
        if !self.check_access(&existing, "edit", user).await? {
            anyhow::bail!("access denied");
        }

        // Update the item
        let item = Item::update(&self.inner.pool, id, user.id, input).await?;

        if let Some(ref i) = item {
            // Invoke tap_item_update
            let item_json = serde_json::to_string(i).context("serialize item")?;
            let state = RequestState::without_services(user.clone());

            let _results = self
                .inner
                .dispatcher
                .dispatch("tap_item_update", &item_json, state)
                .await;

            // Tap errors are logged by the dispatcher

            // Invalidate cache
            self.invalidate(id);

            info!(item_id = %id, "item updated");
        }

        Ok(item)
    }

    /// Delete an item with tap_item_delete invocation.
    pub async fn delete(&self, id: Uuid, user: &UserContext) -> Result<bool> {
        // Load item
        let Some(item) = self.load(id).await? else {
            return Ok(false);
        };

        // Check access
        if !self.check_access(&item, "delete", user).await? {
            anyhow::bail!("access denied");
        }

        // Invoke tap_item_delete (can abort deletion)
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = RequestState::without_services(user.clone());

        let _results = self
            .inner
            .dispatcher
            .dispatch("tap_item_delete", &item_json, state)
            .await;

        // Tap errors are logged by the dispatcher

        // Delete from database
        let deleted = Item::delete(&self.inner.pool, id).await?;

        if deleted {
            // Invalidate cache
            self.invalidate(id);
            info!(item_id = %id, "item deleted");
        }

        Ok(deleted)
    }

    /// Check if a user has access to perform an operation on an item.
    pub async fn check_access(
        &self,
        item: &Item,
        operation: &str,
        user: &UserContext,
    ) -> Result<bool> {
        // Admin always has access
        if user.is_admin() {
            return Ok(true);
        }

        // Published content is viewable by anyone with "access content" permission
        // This is the standard CMS pattern - published = publicly visible
        if operation == "view" && item.is_published() && user.has_permission("access content") {
            return Ok(true);
        }

        // Build access check input
        let input = ItemAccessInput {
            item_id: item.id,
            item_type: item.item_type.clone(),
            author_id: item.author_id,
            operation: operation.to_string(),
            user_id: user.id,
        };

        let input_json = serde_json::to_string(&input).context("serialize access input")?;
        let state = RequestState::without_services(user.clone());

        // Invoke tap_item_access
        let results = self
            .inner
            .dispatcher
            .dispatch("tap_item_access", &input_json, state)
            .await;

        // Aggregate results: Deny wins, then Grant, else Neutral
        let mut has_grant = false;

        for result in results {
            if let Ok(access) = serde_json::from_str::<AccessResult>(&result.output) {
                match access {
                    AccessResult::Deny => return Ok(false),
                    AccessResult::Grant => has_grant = true,
                    AccessResult::Neutral => {}
                }
            }
        }

        // If any plugin granted, allow
        if has_grant {
            return Ok(true);
        }

        // Fall back to role-based permission
        let permission = format!("{} {} content", operation, item.item_type);
        Ok(user.has_permission(&permission))
    }

    /// List items by type.
    pub async fn list_by_type(&self, item_type: &str) -> Result<Vec<Item>> {
        Item::list_by_type(&self.inner.pool, item_type).await
    }

    /// List published items.
    pub async fn list_published(&self, limit: i64, offset: i64) -> Result<Vec<Item>> {
        Item::list_published(&self.inner.pool, limit, offset).await
    }

    /// List items with filtering and return total count for pagination.
    pub async fn list_filtered(
        &self,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Item>, i64)> {
        let items = Item::list_filtered(
            &self.inner.pool,
            item_type,
            status,
            author_id,
            limit,
            offset,
        )
        .await?;
        let total = Item::count_filtered(&self.inner.pool, item_type, status, author_id).await?;
        Ok((items, total))
    }

    /// Get revisions for an item.
    pub async fn get_revisions(&self, item_id: Uuid) -> Result<Vec<ItemRevision>> {
        Item::get_revisions(&self.inner.pool, item_id).await
    }

    /// Revert an item to a previous revision.
    pub async fn revert_to_revision(
        &self,
        item_id: Uuid,
        revision_id: Uuid,
        user: &UserContext,
    ) -> Result<Item> {
        // Load item to check access
        let item = self
            .load(item_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("item not found"))?;

        if !self.check_access(&item, "edit", user).await? {
            anyhow::bail!("access denied");
        }

        let updated =
            Item::revert_to_revision(&self.inner.pool, item_id, revision_id, user.id).await?;

        // Invalidate cache
        self.invalidate(item_id);

        // Invoke tap_item_update for the revert
        let item_json = serde_json::to_string(&updated).context("serialize item")?;
        let state = RequestState::without_services(user.clone());

        let _ = self
            .inner
            .dispatcher
            .dispatch("tap_item_update", &item_json, state)
            .await;

        info!(item_id = %item_id, revision_id = %revision_id, "item reverted");
        Ok(updated)
    }

    /// Invalidate cached item.
    pub fn invalidate(&self, id: Uuid) {
        self.inner.cache.remove(&id);
    }

    /// Clear all cached items.
    pub fn clear_cache(&self) {
        self.inner.cache.clear();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.inner.cache.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_access_input_serialization() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".to_string(),
            author_id: Uuid::nil(),
            operation: "view".to_string(),
            user_id: Uuid::nil(),
        };

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"operation\":\"view\""));
    }

    #[test]
    fn item_access_input_deserialization() {
        let json = r#"{"item_id":"00000000-0000-0000-0000-000000000000","item_type":"page","author_id":"00000000-0000-0000-0000-000000000000","operation":"edit","user_id":"00000000-0000-0000-0000-000000000000"}"#;
        let input: ItemAccessInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.item_type, "page");
        assert_eq!(input.operation, "edit");
    }

    #[test]
    fn item_access_input_roundtrip() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();

        let input = ItemAccessInput {
            item_id: id1,
            item_type: "article".to_string(),
            author_id: id2,
            operation: "delete".to_string(),
            user_id: id3,
        };

        let json = serde_json::to_string(&input).unwrap();
        let parsed: ItemAccessInput = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.item_id, id1);
        assert_eq!(parsed.author_id, id2);
        assert_eq!(parsed.user_id, id3);
        assert_eq!(parsed.operation, "delete");
    }
}
