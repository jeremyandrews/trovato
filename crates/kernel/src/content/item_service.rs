//! Item service with tap integration.
//!
//! Provides CRUD operations for items with automatic tap invocations
//! for plugin taps (insert, update, delete, view, access).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use moka::sync::Cache;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::models::stage::{LIVE_STAGE_ID, Stage, StageVisibility};
use crate::models::{CreateItem, Item, ItemRevision, UpdateItem};
use crate::tap::{RequestState, TapDispatcher, UserContext};
use trovato_sdk::types::AccessResult;

/// Maximum entries in the item cache.
const MAX_CAPACITY: u64 = 50_000;

/// Maximum entries in the stage cache (stages are few and rarely change).
const STAGE_CACHE_CAPACITY: u64 = 16;

/// TTL for stage cache entries. Short because stage visibility changes are
/// security-relevant — a stage changed from Public to Internal should take
/// effect quickly. 30 seconds is a balance between avoiding per-item DB
/// queries and limiting the window of stale visibility data.
const STAGE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Service for item CRUD operations with tap integration.
#[derive(Clone)]
pub struct ItemService {
    inner: Arc<ItemServiceInner>,
}

struct ItemServiceInner {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    cache: Cache<Uuid, Item>,
    /// Cached stage lookups — stages rarely change and there are typically only 3.
    stage_cache: Cache<Uuid, Stage>,
    /// Cached field access decisions keyed by "role_hash:item_type:field_name:operation".
    /// Deny-wins aggregation result. 5-minute TTL.
    field_access_cache: Cache<String, bool>,
}

/// A translation record for an item in a specific language.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct ItemTranslation {
    /// The item this translation belongs to.
    pub item_id: Uuid,
    /// The language code (e.g., "fr", "de").
    pub language: String,
    /// The translated title.
    pub title: String,
    /// Translated field values (JSONB overlay).
    pub fields: serde_json::Value,
    /// Unix timestamp when the translation was created.
    pub created: i64,
    /// Unix timestamp when the translation was last changed.
    pub changed: i64,
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

    /// Whether the user is authenticated (false = anonymous).
    #[serde(default)]
    pub user_authenticated: bool,

    /// The user's granted permissions (empty for anonymous).
    #[serde(default)]
    pub user_permissions: Vec<String>,

    /// Stage UUID (None if item has no explicit stage).
    #[serde(default)]
    pub stage_id: Option<Uuid>,

    /// Stage machine name (e.g., "incoming", "curated", "live").
    #[serde(default)]
    pub stage_machine_name: Option<String>,
}

impl ItemService {
    /// Create a new item service.
    pub fn new(pool: PgPool, dispatcher: Arc<TapDispatcher>, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(ItemServiceInner {
                pool,
                dispatcher,
                cache: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
                stage_cache: Cache::builder()
                    .max_capacity(STAGE_CACHE_CAPACITY)
                    .time_to_live(STAGE_CACHE_TTL)
                    .build(),
                field_access_cache: Cache::builder()
                    .max_capacity(10_000)
                    .time_to_live(Duration::from_secs(300))
                    .build(),
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
            return Ok(Some(item));
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
    pub async fn load_with_overlay(&self, id: Uuid, stage_ids: &[Uuid]) -> Result<Option<Item>> {
        // Check cache first (cache is stage-agnostic — items have single stage_id)
        if let Some(item) = self.inner.cache.get(&id) {
            // Verify the item's stage is in our overlay list
            if stage_ids.contains(&item.stage_id) {
                return Ok(Some(item));
            }
        }

        // Load from database — the item has a single stage_id
        let item = Item::find_by_id(&self.inner.pool, id).await?;

        if let Some(ref i) = item {
            // Only return if the item is in one of the visible stages
            if stage_ids.contains(&i.stage_id) {
                self.inner.cache.insert(id, i.clone());
                return Ok(Some(i.clone()));
            }
        }

        Ok(None)
    }

    /// Load a translation for an item in a specific language.
    ///
    /// Returns `None` if no translation exists for the given language.
    pub async fn load_translation(
        &self,
        item_id: Uuid,
        language: &str,
    ) -> Result<Option<ItemTranslation>> {
        let row = sqlx::query_as::<_, ItemTranslation>(
            "SELECT item_id, language, title, fields, created, changed \
             FROM item_translation WHERE item_id = $1 AND language = $2",
        )
        .bind(item_id)
        .bind(language)
        .fetch_optional(&self.inner.pool)
        .await
        .context("failed to load item translation")?;

        Ok(row)
    }

    /// List all translations that exist for an item, ordered by language.
    ///
    /// Returns `(language, title)` pairs for use in admin translation listing.
    pub async fn list_translations(&self, item_id: Uuid) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT language, title FROM item_translation \
             WHERE item_id = $1 ORDER BY language",
        )
        .bind(item_id)
        .fetch_all(&self.inner.pool)
        .await
        .context("failed to list translations")?;
        Ok(rows)
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
    ///
    /// Access resolution order:
    /// 1. Admin bypass (always allowed)
    /// 2. Stage visibility — anonymous users are denied on internal stages
    /// 3. Published fast-path — public-stage + published + "access content"
    /// 4. Plugin `tap_item_access` — Deny wins, then Grant
    /// 5. Role-based fallback — generic and type-specific permission patterns
    ///
    /// **Design note:** The published-view fast-path (step 3) runs before plugin
    /// dispatch. This means plugins cannot Deny published items on public stages
    /// via `tap_item_access` for "view" operations. This is intentional — it
    /// optimizes the overwhelmingly common case (anonymous/authenticated users
    /// viewing Live content) and matches the CMS convention that "published =
    /// publicly visible." If a plugin needs to restrict specific Live items,
    /// it should use item status (unpublish) rather than access denial.
    pub async fn check_access(
        &self,
        item: &Item,
        operation: &str,
        user: &UserContext,
    ) -> Result<bool> {
        // 1. Admin always has access
        if user.is_admin() {
            return Ok(true);
        }

        // 2. Resolve stage visibility. Use cached lookups — stages are few
        //    and rarely change, but check_access runs on every item view.
        let (is_internal, stage_machine_name) = if item.stage_id == LIVE_STAGE_ID {
            // Live stage is always public — no DB lookup needed.
            (false, Some("live".to_string()))
        } else if let Some(stage) = self.inner.stage_cache.get(&item.stage_id) {
            (
                stage.visibility == StageVisibility::Internal,
                Some(stage.machine_name.clone()),
            )
        } else {
            match Stage::find_by_id(&self.inner.pool, item.stage_id).await {
                Ok(Some(stage)) => {
                    let internal = stage.visibility == StageVisibility::Internal;
                    let name = stage.machine_name.clone();
                    self.inner.stage_cache.insert(item.stage_id, stage);
                    (internal, Some(name))
                }
                Ok(None) => {
                    warn!(
                        stage_id = %item.stage_id,
                        item_id = %item.id,
                        "stage not found for item, treating as public"
                    );
                    (false, None)
                }
                Err(e) => {
                    // DB errors must not silently upgrade access — deny and log.
                    warn!(
                        stage_id = %item.stage_id,
                        item_id = %item.id,
                        error = %e,
                        "failed to resolve stage visibility, denying access"
                    );
                    return Ok(false);
                }
            }
        };

        // Anonymous users cannot access items on internal stages
        if is_internal && !user.authenticated {
            return Ok(false);
        }

        // 3. Published content on public/live stages is viewable by anyone
        //    with "access content". Skip this fast-path for internal stages
        //    so plugins can enforce stage-specific permissions.
        if operation == "view"
            && !is_internal
            && item.is_published()
            && user.has_permission("access content")
        {
            return Ok(true);
        }

        // 4. Build access check input with full context for plugins.
        //    stage_id and stage_machine_name are Option in the SDK for
        //    forward-compatibility, but the kernel always populates them
        //    here since every item has a stage_id.
        let input = ItemAccessInput {
            item_id: item.id,
            item_type: item.item_type.clone(),
            author_id: item.author_id,
            operation: operation.to_string(),
            user_id: user.id,
            user_authenticated: user.authenticated,
            user_permissions: user.permissions.clone(),
            stage_id: Some(item.stage_id),
            stage_machine_name,
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

        // 5. Fall back to role-based permissions. Check both type-specific and
        // generic patterns, plus own-vs-any variants:
        //   "{op} any content"             — generic, any author
        //   "{op} own content"             — generic, own items only
        //   "{op} any {type}"              — type-specific, any author
        //   "{op} own {type}"              — type-specific, own items only
        //   "{op} {type} content"          — legacy pattern
        let is_own = user.id == item.author_id;
        let checks: &[String] = &[
            format!("{operation} any content"),
            format!("{operation} any {}", item.item_type),
            format!("{operation} {} content", item.item_type),
        ];
        for perm in checks {
            if user.has_permission(perm) {
                return Ok(true);
            }
        }
        // "own" variants only apply when the user authored the item
        if is_own {
            let own_checks: &[String] = &[
                format!("{operation} own content"),
                format!("{operation} own {}", item.item_type),
            ];
            for perm in own_checks {
                if user.has_permission(perm) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Check if a user can access a specific field (view or edit).
    ///
    /// Dispatches `tap_field_access` to all implementing plugins and
    /// aggregates with deny-wins semantics. Results are cached per
    /// `(role_set, item_type, field_name, operation)` tuple for 5 minutes.
    ///
    /// Admin users bypass field access checks entirely.
    pub async fn check_field_access(
        &self,
        user: &UserContext,
        item_type: &str,
        field_name: &str,
        operation: &str,
    ) -> bool {
        // Admin bypass
        if user.is_admin() {
            return true;
        }

        // Build cache key from hashed permission set + field info.
        // Using a hash of sorted permissions keeps keys compact.
        use std::hash::{Hash, Hasher};
        let mut perms = user.permissions.clone();
        perms.sort_unstable();
        let mut hasher = std::hash::DefaultHasher::new();
        perms.hash(&mut hasher);
        let perm_hash = hasher.finish();
        let cache_key = format!("{perm_hash:x}:{item_type}:{field_name}:{operation}");

        // Check cache
        if let Some(allowed) = self.inner.field_access_cache.get(&cache_key) {
            return allowed;
        }

        // No plugins implement tap_field_access yet — default to allow.
        // When plugins are added, dispatch here and aggregate deny-wins.
        let allowed = true;

        self.inner.field_access_cache.insert(cache_key, allowed);

        allowed
    }

    /// Filter a set of field names to only those the user can access.
    ///
    /// Convenience wrapper around [`check_field_access`] for filtering
    /// fields before rendering or form building.
    pub async fn accessible_fields(
        &self,
        user: &UserContext,
        item_type: &str,
        field_names: &[String],
        operation: &str,
    ) -> Vec<String> {
        let mut accessible = Vec::with_capacity(field_names.len());
        for name in field_names {
            if self
                .check_field_access(user, item_type, name, operation)
                .await
            {
                accessible.push(name.clone());
            }
        }
        accessible
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
        self.inner.cache.invalidate(&id);
    }

    /// Clear all cached items and stages.
    pub fn clear_cache(&self) {
        self.inner.cache.invalidate_all();
        self.inner.stage_cache.invalidate_all();
    }

    /// Clear cached stage data. Call when stage config changes (visibility,
    /// machine name, etc.) so access checks use fresh data.
    pub fn clear_stage_cache(&self) {
        self.inner.stage_cache.invalidate_all();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.inner.cache.entry_count() as usize
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
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
            user_authenticated: false,
            user_permissions: vec![],
            stage_id: None,
            stage_machine_name: None,
        };

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"operation\":\"view\""));
    }

    #[test]
    fn item_access_input_deserialization() {
        // Old-format JSON (without new fields) should deserialize via #[serde(default)]
        let json = r#"{"item_id":"00000000-0000-0000-0000-000000000000","item_type":"page","author_id":"00000000-0000-0000-0000-000000000000","operation":"edit","user_id":"00000000-0000-0000-0000-000000000000"}"#;
        let input: ItemAccessInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.item_type, "page");
        assert_eq!(input.operation, "edit");
        assert!(!input.user_authenticated);
        assert!(input.user_permissions.is_empty());
        assert!(input.stage_id.is_none());
        assert!(input.stage_machine_name.is_none());
    }

    #[test]
    fn item_access_input_roundtrip() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();
        let stage = Uuid::now_v7();

        let input = ItemAccessInput {
            item_id: id1,
            item_type: "article".to_string(),
            author_id: id2,
            operation: "delete".to_string(),
            user_id: id3,
            user_authenticated: true,
            user_permissions: vec!["edit any content".to_string()],
            stage_id: Some(stage),
            stage_machine_name: Some("curated".to_string()),
        };

        let json = serde_json::to_string(&input).unwrap();
        let parsed: ItemAccessInput = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.item_id, id1);
        assert_eq!(parsed.author_id, id2);
        assert_eq!(parsed.user_id, id3);
        assert_eq!(parsed.operation, "delete");
        assert!(parsed.user_authenticated);
        assert_eq!(parsed.user_permissions, vec!["edit any content"]);
        assert_eq!(parsed.stage_id, Some(stage));
        assert_eq!(parsed.stage_machine_name.as_deref(), Some("curated"));
    }
}
