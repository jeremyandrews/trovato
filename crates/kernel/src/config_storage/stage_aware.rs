//! Stage-aware implementation of ConfigStorage.
//!
//! This decorator wraps DirectConfigStorage to add stage context.
//! It implements the core staging behavior for config entities:
//!
//! - **Load**: Check stage first (config_stage_association + config_revision),
//!   then fall back to live (DirectConfigStorage)
//! - **Save**: Write to config_revision, update config_stage_association
//! - **Delete**: Record in stage_deletion (don't actually delete until publish)
//! - **List**: Merge stage changes with live, respecting deletions

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

use super::{ConfigEntity, ConfigFilter, ConfigStorage, DirectConfigStorage};

/// Stage-aware config storage decorator.
///
/// Wraps DirectConfigStorage to provide stage-specific config views.
/// Changes made through this storage are isolated to the stage until published.
///
/// Supports hierarchy: when `ancestry` contains intermediate stages
/// (e.g., `["review", "draft"]`), the storage walks the chain before
/// falling back to live (DirectConfigStorage).
#[derive(Clone)]
pub struct StageAwareConfigStorage {
    /// The underlying live storage.
    direct: Arc<DirectConfigStorage>,
    /// Database pool for stage-specific queries.
    pool: PgPool,
    /// The stage ID this storage operates in (writes go here).
    stage_id: String,
    /// Ancestor stage IDs ordered from nearest to farthest (excluding self).
    /// Empty for a stage that is a direct child of live.
    ancestors: Vec<String>,
}

impl StageAwareConfigStorage {
    /// Create a new stage-aware config storage.
    ///
    /// # Arguments
    /// * `direct` - The live storage to fall back to
    /// * `pool` - Database pool for stage queries
    /// * `stage_id` - The stage this storage operates in
    pub fn new(direct: Arc<DirectConfigStorage>, pool: PgPool, stage_id: String) -> Self {
        Self {
            direct,
            pool,
            stage_id,
            ancestors: Vec::new(),
        }
    }

    /// Create a new stage-aware config storage with hierarchy.
    ///
    /// `ancestry` should be the full chain from self → ... → root,
    /// e.g., `["review", "draft", "live"]`. The first element is this
    /// storage's stage_id; remaining non-"live" entries become ancestors.
    pub fn new_with_ancestry(
        direct: Arc<DirectConfigStorage>,
        pool: PgPool,
        ancestry: Vec<String>,
    ) -> Self {
        let stage_id = ancestry
            .first()
            .cloned()
            .unwrap_or_else(|| "live".to_string());
        let ancestors: Vec<String> = ancestry
            .into_iter()
            .skip(1) // skip self
            .filter(|s| s != "live") // live is handled by DirectConfigStorage
            .collect();
        Self {
            direct,
            pool,
            stage_id,
            ancestors,
        }
    }

    /// Get the stage ID this storage operates in.
    pub fn stage_id(&self) -> &str {
        &self.stage_id
    }

    /// Check if an entity is marked as deleted in this stage.
    async fn is_deleted(&self, entity_type: &str, entity_id: &str) -> Result<bool> {
        let deleted: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM stage_deletion
                WHERE stage_id = $1 AND entity_type = $2 AND entity_id = $3
            )
            "#,
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .bind(entity_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to check deletion status")?;

        Ok(deleted)
    }

    /// Get a staged revision for an entity.
    async fn get_staged_revision(
        &self,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Option<ConfigRevisionRow>> {
        let revision = sqlx::query_as::<_, ConfigRevisionRow>(
            r#"
            SELECT cr.id, cr.entity_type, cr.entity_id, cr.data, cr.created, cr.author_id
            FROM config_revision cr
            INNER JOIN config_stage_association csa
                ON cr.id = csa.target_revision_id
            WHERE csa.stage_id = $1
                AND csa.entity_type = $2
                AND csa.entity_id = $3
            "#,
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .bind(entity_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch staged revision")?;

        Ok(revision)
    }

    /// Get a staged revision for an entity in a specific (ancestor) stage.
    async fn get_staged_revision_in(
        &self,
        entity_type: &str,
        entity_id: &str,
        stage_id: &str,
    ) -> Result<Option<ConfigRevisionRow>> {
        let revision = sqlx::query_as::<_, ConfigRevisionRow>(
            r#"
            SELECT cr.id, cr.entity_type, cr.entity_id, cr.data, cr.created, cr.author_id
            FROM config_revision cr
            INNER JOIN config_stage_association csa
                ON cr.id = csa.target_revision_id
            WHERE csa.stage_id = $1
                AND csa.entity_type = $2
                AND csa.entity_id = $3
            "#,
        )
        .bind(stage_id)
        .bind(entity_type)
        .bind(entity_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch ancestor staged revision")?;

        Ok(revision)
    }

    /// Create a new revision for an entity and associate it with this stage.
    async fn create_staged_revision(
        &self,
        entity: &ConfigEntity,
        author_id: Option<Uuid>,
    ) -> Result<()> {
        let entity_type = entity.entity_type();
        let entity_id = entity.id();
        let data = serde_json::to_value(entity).context("failed to serialize entity")?;
        let revision_id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start transaction")?;

        // Insert the revision
        sqlx::query(
            r#"
            INSERT INTO config_revision (id, entity_type, entity_id, data, created, author_id)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(revision_id)
        .bind(entity_type)
        .bind(&entity_id)
        .bind(&data)
        .bind(now)
        .bind(author_id)
        .execute(&mut *tx)
        .await
        .context("failed to insert config revision")?;

        // Update or insert the stage association
        sqlx::query(
            r#"
            INSERT INTO config_stage_association (stage_id, entity_type, entity_id, target_revision_id)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (stage_id, entity_type, entity_id) DO UPDATE
                SET target_revision_id = EXCLUDED.target_revision_id
            "#,
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .bind(&entity_id)
        .bind(revision_id)
        .execute(&mut *tx)
        .await
        .context("failed to update stage association")?;

        // If the entity was previously marked for deletion, remove that mark
        sqlx::query(
            "DELETE FROM stage_deletion WHERE stage_id = $1 AND entity_type = $2 AND entity_id = $3",
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .bind(&entity_id)
        .execute(&mut *tx)
        .await
        .context("failed to clear deletion mark")?;

        tx.commit().await.context("failed to commit transaction")?;

        debug!(
            stage_id = %self.stage_id,
            entity_type = %entity_type,
            entity_id = %entity_id,
            revision_id = %revision_id,
            "created staged revision"
        );

        Ok(())
    }

    /// Mark an entity as deleted in this stage.
    async fn mark_deleted(&self, entity_type: &str, entity_id: &str) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start transaction")?;

        // Remove any staged revisions for this entity
        sqlx::query(
            "DELETE FROM config_stage_association WHERE stage_id = $1 AND entity_type = $2 AND entity_id = $3",
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .bind(entity_id)
        .execute(&mut *tx)
        .await
        .context("failed to remove stage association")?;

        // Check if entity exists in live
        let exists_in_live = self.direct.exists(entity_type, entity_id).await?;

        if exists_in_live {
            // Mark for deletion (will be applied on publish)
            sqlx::query(
                r#"
                INSERT INTO stage_deletion (stage_id, entity_type, entity_id, deleted_at)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (stage_id, entity_type, entity_id) DO NOTHING
                "#,
            )
            .bind(&self.stage_id)
            .bind(entity_type)
            .bind(entity_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .context("failed to mark entity for deletion")?;
        }

        tx.commit().await.context("failed to commit transaction")?;

        debug!(
            stage_id = %self.stage_id,
            entity_type = %entity_type,
            entity_id = %entity_id,
            exists_in_live = %exists_in_live,
            "marked entity for deletion"
        );

        Ok(true)
    }

    /// Get all entity IDs that are deleted in this stage.
    async fn get_deleted_ids(&self, entity_type: &str) -> Result<HashSet<String>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT entity_id FROM stage_deletion WHERE stage_id = $1 AND entity_type = $2",
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch deleted IDs")?;

        Ok(ids.into_iter().collect())
    }

    /// Get all staged entities of a type.
    async fn get_staged_entities(&self, entity_type: &str) -> Result<Vec<ConfigEntity>> {
        let revisions = sqlx::query_as::<_, ConfigRevisionRow>(
            r#"
            SELECT cr.id, cr.entity_type, cr.entity_id, cr.data, cr.created, cr.author_id
            FROM config_revision cr
            INNER JOIN config_stage_association csa
                ON cr.id = csa.target_revision_id
            WHERE csa.stage_id = $1 AND csa.entity_type = $2
            "#,
        )
        .bind(&self.stage_id)
        .bind(entity_type)
        .fetch_all(&self.pool)
        .await
        .context("failed to fetch staged entities")?;

        let mut entities = Vec::new();
        for rev in revisions {
            if let Ok(entity) = serde_json::from_value::<ConfigEntity>(rev.data) {
                entities.push(entity);
            }
        }

        Ok(entities)
    }
}

#[async_trait]
impl ConfigStorage for StageAwareConfigStorage {
    async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>> {
        // 1. Check if entity is deleted in this stage
        if self.is_deleted(entity_type, id).await? {
            debug!(
                stage_id = %self.stage_id,
                entity_type = %entity_type,
                id = %id,
                "entity deleted in stage"
            );
            return Ok(None);
        }

        // 2. Check for staged revision in this stage
        if let Some(revision) = self.get_staged_revision(entity_type, id).await? {
            debug!(
                stage_id = %self.stage_id,
                entity_type = %entity_type,
                id = %id,
                "loaded from staged revision"
            );
            let entity = serde_json::from_value(revision.data)
                .context("failed to deserialize staged revision")?;
            return Ok(Some(entity));
        }

        // 3. Walk ancestor stages (hierarchy overlay)
        for ancestor in &self.ancestors {
            if let Some(revision) = self
                .get_staged_revision_in(entity_type, id, ancestor)
                .await?
            {
                debug!(
                    stage_id = %self.stage_id,
                    ancestor = %ancestor,
                    entity_type = %entity_type,
                    id = %id,
                    "loaded from ancestor staged revision"
                );
                let entity = serde_json::from_value(revision.data)
                    .context("failed to deserialize ancestor staged revision")?;
                return Ok(Some(entity));
            }
        }

        // 4. Fall back to live
        debug!(
            stage_id = %self.stage_id,
            entity_type = %entity_type,
            id = %id,
            "falling back to live"
        );
        self.direct.load(entity_type, id).await
    }

    async fn save(&self, entity: &ConfigEntity) -> Result<()> {
        // Create a staged revision (don't touch live)
        self.create_staged_revision(entity, None).await
    }

    async fn delete(&self, entity_type: &str, id: &str) -> Result<bool> {
        // Mark for deletion (don't actually delete until publish)
        self.mark_deleted(entity_type, id).await
    }

    async fn list(
        &self,
        entity_type: &str,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>> {
        // Get deleted IDs to exclude
        let deleted_ids = self.get_deleted_ids(entity_type).await?;

        // Get staged entities
        let staged = self.get_staged_entities(entity_type).await?;
        let staged_ids: HashSet<String> = staged.iter().map(|e| e.id()).collect();

        // Get live entities
        let live = self.direct.list(entity_type, filter).await?;

        // Merge: staged overrides live, exclude deleted
        let mut result: Vec<ConfigEntity> = Vec::new();

        // Add all staged entities (they override live)
        for entity in staged {
            if !deleted_ids.contains(&entity.id()) {
                result.push(entity);
            }
        }

        // Add live entities that aren't staged or deleted
        for entity in live {
            let id = entity.id();
            if !staged_ids.contains(&id) && !deleted_ids.contains(&id) {
                result.push(entity);
            }
        }

        // Apply filter if provided (limit/offset)
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                result = result.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                result.truncate(limit);
            }
        }

        Ok(result)
    }

    async fn exists(&self, entity_type: &str, id: &str) -> Result<bool> {
        // Deleted in stage means doesn't exist
        if self.is_deleted(entity_type, id).await? {
            return Ok(false);
        }

        // Check staged in this stage
        if self.get_staged_revision(entity_type, id).await?.is_some() {
            return Ok(true);
        }

        // Check ancestor stages
        for ancestor in &self.ancestors {
            if self
                .get_staged_revision_in(entity_type, id, ancestor)
                .await?
                .is_some()
            {
                return Ok(true);
            }
        }

        // Fall back to live
        self.direct.exists(entity_type, id).await
    }
}

impl std::fmt::Debug for StageAwareConfigStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageAwareConfigStorage")
            .field("stage_id", &self.stage_id)
            .finish()
    }
}

/// Row type for config revision queries.
#[derive(sqlx::FromRow)]
struct ConfigRevisionRow {
    #[allow(dead_code)]
    id: Uuid,
    #[allow(dead_code)]
    entity_type: String,
    #[allow(dead_code)]
    entity_id: String,
    data: serde_json::Value,
    #[allow(dead_code)]
    created: i64,
    #[allow(dead_code)]
    author_id: Option<Uuid>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn stage_aware_config_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<StageAwareConfigStorage>();
    }
}
