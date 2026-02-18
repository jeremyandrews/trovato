//! Stage publishing framework with ordered phases and conflict detection.
//!
//! This module provides the infrastructure for atomic, ordered stage publishing.
//! When a stage is published, changes are applied in a specific order to ensure
//! dependencies are satisfied:
//!
//! 1. **Config Types/Fields**: Item type definitions (nothing depends on these)
//! 2. **Categories**: Categories referenced by content items
//! 3. **Items**: Content items (depend on types and categories)
//! 4. **Dependents**: Menus, aliases, etc. (reference content)
//!
//! All phases execute within a single database transaction. If any phase fails,
//! the entire transaction rolls back.
//!
//! ## Conflict Detection
//!
//! Before publishing, the system detects potential conflicts:
//! - **Cross-stage conflicts**: Multiple stages have changes to the same entity
//! - **Live-modified conflicts**: Live version was changed after staging began
//!
//! Conflicts are reported but don't block publish (warn-only mode).
//! Users can choose to Skip, Overwrite, or Cancel per conflict.

use anyhow::{Context, Result};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::cache::CacheLayer;
use crate::models::stage::{CreateStage, Stage};

/// Identifies which publish phase is executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishPhase {
    /// Phase 1: Content types and field definitions.
    ConfigTypes,
    /// Phase 2: Categories (vocabularies and terms).
    Categories,
    /// Phase 3: Content items.
    Items,
    /// Phase 4: Dependents (menus, aliases, etc.).
    Dependents,
}

impl std::fmt::Display for PublishPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublishPhase::ConfigTypes => write!(f, "config_types"),
            PublishPhase::Categories => write!(f, "categories"),
            PublishPhase::Items => write!(f, "items"),
            PublishPhase::Dependents => write!(f, "dependents"),
        }
    }
}

/// Type of conflict detected during publish preparation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictType {
    /// Another stage has changes to the same entity.
    CrossStage {
        /// The other stage(s) that have changes.
        other_stages: Vec<String>,
    },
    /// The live version was modified after this entity was staged.
    LiveModified {
        /// When the entity was staged (Unix timestamp).
        staged_at: i64,
        /// When the live version was last modified (Unix timestamp).
        live_changed: i64,
    },
}

impl std::fmt::Display for ConflictType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConflictType::CrossStage { other_stages } => {
                write!(f, "also modified in: {}", other_stages.join(", "))
            }
            ConflictType::LiveModified {
                staged_at,
                live_changed,
            } => {
                write!(
                    f,
                    "live was modified (at {}) after staging (at {})",
                    live_changed, staged_at
                )
            }
        }
    }
}

/// Information about a single conflict.
#[derive(Debug, Clone)]
pub struct ConflictInfo {
    /// Entity type (e.g., "item", "item_type", "category").
    pub entity_type: String,
    /// Entity ID.
    pub entity_id: String,
    /// Human-readable label for the entity.
    pub label: Option<String>,
    /// Type of conflict detected.
    pub conflict_type: ConflictType,
}

impl ConflictInfo {
    /// Create a new conflict info.
    pub fn new(
        entity_type: impl Into<String>,
        entity_id: impl Into<String>,
        conflict_type: ConflictType,
    ) -> Self {
        Self {
            entity_type: entity_type.into(),
            entity_id: entity_id.into(),
            label: None,
            conflict_type,
        }
    }

    /// Set a human-readable label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// Resolution choice for a single conflicted entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Overwrite - publish anyway (Last Publish Wins).
    Overwrite,
    /// Skip - don't publish this entity, continue with others.
    Skip,
}

/// How to resolve conflicts during publish.
#[derive(Debug, Clone, Default)]
pub enum ConflictResolution {
    /// Abort entire publish operation.
    #[default]
    Cancel,
    /// Skip all conflicting entities, publish the rest.
    SkipAll,
    /// Overwrite all conflicts (Last Publish Wins).
    OverwriteAll,
    /// Per-entity decisions (key: "entity_type:entity_id").
    PerEntity(HashMap<String, Resolution>),
}

impl ConflictResolution {
    /// Get the resolution for a specific entity.
    pub fn resolution_for(&self, entity_type: &str, entity_id: &str) -> Option<Resolution> {
        match self {
            ConflictResolution::Cancel => None,
            ConflictResolution::SkipAll => Some(Resolution::Skip),
            ConflictResolution::OverwriteAll => Some(Resolution::Overwrite),
            ConflictResolution::PerEntity(map) => {
                let key = format!("{}:{}", entity_type, entity_id);
                map.get(&key).copied()
            }
        }
    }
}

/// Result of a stage publish operation.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// Whether the publish succeeded.
    pub success: bool,
    /// The stage that was published.
    pub stage_id: String,
    /// Number of items published (moved to live).
    pub items_published: i64,
    /// Number of items deleted (from deletion records).
    pub items_deleted: i64,
    /// Number of config entities published.
    pub config_published: i64,
    /// Number of dependent entities published (aliases, menu links).
    pub dependents_published: i64,
    /// Conflicts detected (if any).
    pub conflicts: Vec<ConflictInfo>,
    /// If failed, which phase failed.
    pub failed_phase: Option<PublishPhase>,
    /// Error message if failed.
    pub error_message: Option<String>,
}

impl PublishResult {
    /// Create a successful result.
    pub fn success(stage_id: String, items_published: i64, items_deleted: i64) -> Self {
        Self {
            success: true,
            stage_id,
            items_published,
            items_deleted,
            config_published: 0,
            dependents_published: 0,
            conflicts: Vec::new(),
            failed_phase: None,
            error_message: None,
        }
    }

    /// Create a successful result with conflicts.
    pub fn success_with_conflicts(
        stage_id: String,
        items_published: i64,
        items_deleted: i64,
        conflicts: Vec<ConflictInfo>,
    ) -> Self {
        Self {
            success: true,
            stage_id,
            items_published,
            items_deleted,
            config_published: 0,
            dependents_published: 0,
            conflicts,
            failed_phase: None,
            error_message: None,
        }
    }

    /// Create a cancelled result due to conflicts.
    pub fn cancelled(stage_id: String, conflicts: Vec<ConflictInfo>) -> Self {
        Self {
            success: false,
            stage_id,
            items_published: 0,
            items_deleted: 0,
            config_published: 0,
            dependents_published: 0,
            conflicts,
            failed_phase: None,
            error_message: Some("Publish cancelled due to conflicts".to_string()),
        }
    }

    /// Create a failed result.
    pub fn failure(stage_id: String, phase: PublishPhase, error: String) -> Self {
        Self {
            success: false,
            stage_id,
            items_published: 0,
            items_deleted: 0,
            config_published: 0,
            dependents_published: 0,
            conflicts: Vec::new(),
            failed_phase: Some(phase),
            error_message: Some(error),
        }
    }

    /// Check if there were any conflicts.
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Phase callbacks for custom publish logic (post-MVP).
///
/// Each phase receives a mutable transaction reference and can perform
/// database operations. If any callback returns an error, the entire
/// transaction is rolled back.
///
/// In v1.0, this struct is not used - StageService::publish() handles
/// phases directly. Post-MVP, this will enable custom phase logic.
#[allow(dead_code)]
pub struct PublishPhases<'a> {
    /// Phase 1: Publish config types (no-op in v1.0).
    pub config_types: Box<dyn FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a>,
    /// Phase 2: Publish categories (no-op in v1.0).
    pub categories: Box<dyn FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a>,
    /// Phase 3: Publish items (active in v1.0).
    pub items: Box<dyn FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a>,
    /// Phase 4: Publish dependents (no-op in v1.0).
    pub dependents: Box<dyn FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a>,
}

impl<'a> PublishPhases<'a> {
    /// Create default phases with no-op placeholders.
    ///
    /// The items phase must be provided separately via `with_items()`.
    pub fn new() -> Self {
        Self {
            config_types: Box::new(|_tx| {
                debug!("config_types phase: no-op (v1.0)");
                Ok(())
            }),
            categories: Box::new(|_tx| {
                debug!("categories phase: no-op (v1.0)");
                Ok(())
            }),
            items: Box::new(|_tx| {
                debug!("items phase: default no-op");
                Ok(())
            }),
            dependents: Box::new(|_tx| {
                debug!("dependents phase: no-op (v1.0)");
                Ok(())
            }),
        }
    }

    /// Set the items phase callback.
    pub fn with_items<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a,
    {
        self.items = Box::new(f);
        self
    }

    /// Set the config_types phase callback (post-MVP).
    #[allow(dead_code)]
    pub fn with_config_types<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a,
    {
        self.config_types = Box::new(f);
        self
    }

    /// Set the categories phase callback (post-MVP).
    #[allow(dead_code)]
    pub fn with_categories<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a,
    {
        self.categories = Box::new(f);
        self
    }

    /// Set the dependents phase callback (post-MVP).
    #[allow(dead_code)]
    pub fn with_dependents<F>(mut self, f: F) -> Self
    where
        F: FnMut(&mut Transaction<'_, Postgres>) -> Result<()> + Send + 'a,
    {
        self.dependents = Box::new(f);
        self
    }
}

impl Default for PublishPhases<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Stage service for managing content stages.
#[derive(Clone)]
pub struct StageService {
    pool: PgPool,
    cache: CacheLayer,
}

impl StageService {
    /// Create a new stage service.
    pub fn new(pool: PgPool, cache: CacheLayer) -> Self {
        Self { pool, cache }
    }

    /// Publish a stage to live using default phases.
    ///
    /// This is the primary entry point for stage publishing.
    /// It uses the default items phase which moves staged items to live.
    pub async fn publish(&self, stage_id: &str) -> Result<PublishResult> {
        if stage_id == "live" {
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                "Cannot publish 'live' stage to itself".to_string(),
            ));
        }

        info!(stage_id = %stage_id, "starting stage publish (default phases)");

        // Start transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start transaction")?;

        // Count items BEFORE publishing (so we have accurate counts)
        let items_to_publish: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
                .bind(stage_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap_or(0);

        let items_to_delete: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stage_deletion WHERE stage_id = $1 AND entity_type = 'item'",
        )
        .bind(stage_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);

        // Phase 1: Config types (no-op in v1.0)
        debug!("executing phase 1: config_types (no-op)");

        // Phase 2: Categories (no-op in v1.0)
        debug!("executing phase 2: categories (no-op)");

        // Phase 3: Items (active in v1.0)
        debug!("executing phase 3: items");
        if let Err(e) = publish_items_default(&mut tx, stage_id).await {
            warn!(error = %e, "items phase failed");
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                e.to_string(),
            ));
        }

        // Phase 4: Dependents (aliases, menu links)
        debug!("executing phase 4: dependents");
        let dependents_published = match publish_dependents_default(&mut tx, stage_id).await {
            Ok(count) => count,
            Err(e) => {
                warn!(error = %e, "dependents phase failed");
                return Ok(PublishResult::failure(
                    stage_id.to_string(),
                    PublishPhase::Dependents,
                    e.to_string(),
                ));
            }
        };

        // Commit transaction
        tx.commit().await.context("failed to commit transaction")?;

        info!(
            stage_id = %stage_id,
            items_published = %items_to_publish,
            items_deleted = %items_to_delete,
            dependents_published = %dependents_published,
            "stage publish completed"
        );

        // Cache invalidation AFTER transaction commits
        self.cache.invalidate_stage(stage_id).await;

        let mut result =
            PublishResult::success(stage_id.to_string(), items_to_publish, items_to_delete);
        result.dependents_published = dependents_published;
        Ok(result)
    }

    // Note: publish_with_phases() removed for v1.0.
    // The callback-based approach will be added post-MVP when config staging needs it.
    // For v1.0, use publish() which handles items phase directly.

    // ── Stage hierarchy management ──

    /// Create a new stage with an optional upstream parent.
    pub async fn create_stage(
        &self,
        id: &str,
        label: &str,
        upstream_id: Option<&str>,
    ) -> Result<Stage> {
        Stage::create(
            &self.pool,
            CreateStage {
                id: id.to_string(),
                label: label.to_string(),
                upstream_id: upstream_id.map(|s| s.to_string()),
            },
        )
        .await
    }

    /// Get a stage by ID.
    pub async fn get_stage(&self, id: &str) -> Result<Option<Stage>> {
        Stage::find_by_id(&self.pool, id).await
    }

    /// List all stages.
    pub async fn list_stages(&self) -> Result<Vec<Stage>> {
        Stage::list_all(&self.pool).await
    }

    /// Get the ancestry chain for a stage (self → parent → ... → root).
    pub async fn get_ancestry(&self, stage_id: &str) -> Result<Vec<String>> {
        Stage::get_ancestry(&self.pool, stage_id).await
    }

    /// Resolve the upstream target for a stage.
    ///
    /// Returns the upstream_id if set, otherwise `"live"`.
    async fn resolve_upstream(&self, stage_id: &str) -> Result<String> {
        let stage = Stage::find_by_id(&self.pool, stage_id)
            .await?
            .with_context(|| format!("stage '{}' not found", stage_id))?;

        Ok(stage
            .upstream_id
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "live".to_string()))
    }

    /// Publish a stage to its upstream parent (or live if no parent).
    ///
    /// This is the hierarchy-aware entry point: instead of always targeting
    /// "live", it moves content to whichever stage is the direct upstream.
    pub async fn publish_to_upstream(&self, stage_id: &str) -> Result<PublishResult> {
        if stage_id == "live" {
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                "Cannot publish 'live' stage".to_string(),
            ));
        }

        let target = self.resolve_upstream(stage_id).await?;

        info!(stage_id = %stage_id, target = %target, "publishing to upstream");

        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start transaction")?;

        // Count items before publishing
        let items_to_publish: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
                .bind(stage_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap_or(0);

        let items_to_delete: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stage_deletion WHERE stage_id = $1 AND entity_type = 'item'",
        )
        .bind(stage_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);

        // Phase 1-2: Config types / Categories (no-op in v1.0)
        debug!("executing phases 1-2: config_types + categories (no-op)");

        // Phase 3: Items — move to target stage instead of "live"
        debug!("executing phase 3: items -> {}", target);
        if let Err(e) = publish_items_to_target(&mut tx, stage_id, &target).await {
            warn!(error = %e, "items phase failed");
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                e.to_string(),
            ));
        }

        // Phase 4: Dependents — move aliases/menus to target stage
        debug!("executing phase 4: dependents -> {}", target);
        let dependents_published =
            match publish_dependents_to_target(&mut tx, stage_id, &target).await {
                Ok(count) => count,
                Err(e) => {
                    warn!(error = %e, "dependents phase failed");
                    return Ok(PublishResult::failure(
                        stage_id.to_string(),
                        PublishPhase::Dependents,
                        e.to_string(),
                    ));
                }
            };

        tx.commit().await.context("failed to commit transaction")?;

        info!(
            stage_id = %stage_id,
            target = %target,
            items_published = %items_to_publish,
            items_deleted = %items_to_delete,
            dependents_published = %dependents_published,
            "stage publish to upstream completed"
        );

        self.cache.invalidate_stage(stage_id).await;

        let mut result =
            PublishResult::success(stage_id.to_string(), items_to_publish, items_to_delete);
        result.dependents_published = dependents_published;
        Ok(result)
    }

    /// Check if a stage has any pending changes.
    pub async fn has_changes(&self, stage_id: &str) -> Result<bool> {
        if stage_id == "live" {
            return Ok(false);
        }

        // Check for staged items
        let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
            .bind(stage_id)
            .fetch_one(&self.pool)
            .await
            .context("failed to count staged items")?;

        if item_count > 0 {
            return Ok(true);
        }

        // Check for staged config
        let config_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM config_stage_association WHERE stage_id = $1")
                .bind(stage_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to count staged config")?;

        if config_count > 0 {
            return Ok(true);
        }

        // Check for pending deletions
        let deletion_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM stage_deletion WHERE stage_id = $1")
                .bind(stage_id)
                .fetch_one(&self.pool)
                .await
                .context("failed to count deletions")?;

        Ok(deletion_count > 0)
    }

    /// Detect conflicts before publishing a stage.
    ///
    /// Returns a list of conflicts found. Empty list means no conflicts.
    pub async fn detect_conflicts(&self, stage_id: &str) -> Result<Vec<ConflictInfo>> {
        if stage_id == "live" {
            return Ok(Vec::new());
        }

        let mut conflicts = Vec::new();

        // Detect cross-stage config conflicts
        // Find config entities in this stage that are also modified in other stages
        let cross_stage_config = self.detect_cross_stage_config_conflicts(stage_id).await?;
        conflicts.extend(cross_stage_config);

        // Detect live-modified config conflicts
        // Find config entities where live version was modified after staging
        let live_modified_config = self.detect_live_modified_config_conflicts(stage_id).await?;
        conflicts.extend(live_modified_config);

        // Detect cross-stage alias conflicts
        let alias_conflicts = self.detect_cross_stage_alias_conflicts(stage_id).await?;
        conflicts.extend(alias_conflicts);

        Ok(conflicts)
    }

    /// Detect config entities modified in multiple stages.
    async fn detect_cross_stage_config_conflicts(
        &self,
        stage_id: &str,
    ) -> Result<Vec<ConflictInfo>> {
        // Find config entities in this stage that are also in other stages
        let rows = sqlx::query(
            r#"
            SELECT
                a.entity_type,
                a.entity_id,
                array_agg(DISTINCT b.stage_id) as other_stages
            FROM config_stage_association a
            JOIN config_stage_association b
                ON a.entity_type = b.entity_type
                AND a.entity_id = b.entity_id
                AND b.stage_id != $1
                AND b.stage_id != 'live'
            WHERE a.stage_id = $1
            GROUP BY a.entity_type, a.entity_id
            "#,
        )
        .bind(stage_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to detect cross-stage config conflicts")?;

        let mut conflicts = Vec::new();
        for row in rows {
            let entity_type: String = row.get("entity_type");
            let entity_id: String = row.get("entity_id");
            let other_stages: Vec<String> = row.get("other_stages");

            conflicts.push(ConflictInfo::new(
                &entity_type,
                &entity_id,
                ConflictType::CrossStage { other_stages },
            ));
        }

        Ok(conflicts)
    }

    /// Detect config entities where live was modified after staging.
    async fn detect_live_modified_config_conflicts(
        &self,
        stage_id: &str,
    ) -> Result<Vec<ConflictInfo>> {
        // For config entities, compare the staged revision's created timestamp
        // against the most recent live revision for the same entity
        let rows = sqlx::query(
            r#"
            SELECT
                staged.entity_type,
                staged.entity_id,
                staged_rev.created as staged_at,
                live_rev.created as live_changed
            FROM config_stage_association staged
            JOIN config_revision staged_rev ON staged.target_revision_id = staged_rev.id
            JOIN LATERAL (
                -- Find most recent live revision for this entity
                SELECT cr.created
                FROM config_revision cr
                JOIN config_stage_association live_assoc
                    ON live_assoc.target_revision_id = cr.id
                    AND live_assoc.stage_id = 'live'
                WHERE cr.entity_type = staged.entity_type
                    AND cr.entity_id = staged.entity_id
                ORDER BY cr.created DESC
                LIMIT 1
            ) live_rev ON true
            WHERE staged.stage_id = $1
                AND live_rev.created > staged_rev.created
            "#,
        )
        .bind(stage_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to detect live-modified config conflicts")?;

        let mut conflicts = Vec::new();
        for row in rows {
            let entity_type: String = row.get("entity_type");
            let entity_id: String = row.get("entity_id");
            let staged_at: i64 = row.get("staged_at");
            let live_changed: i64 = row.get("live_changed");

            conflicts.push(ConflictInfo::new(
                &entity_type,
                &entity_id,
                ConflictType::LiveModified {
                    staged_at,
                    live_changed,
                },
            ));
        }

        Ok(conflicts)
    }

    /// Detect URL aliases in this stage that conflict with aliases in other stages.
    async fn detect_cross_stage_alias_conflicts(
        &self,
        stage_id: &str,
    ) -> Result<Vec<ConflictInfo>> {
        let rows = sqlx::query(
            r#"
            SELECT
                a.alias,
                a.language,
                array_agg(DISTINCT b.stage_id) as other_stages
            FROM url_alias a
            JOIN url_alias b
                ON a.alias = b.alias
                AND a.language = b.language
                AND b.stage_id != $1
                AND b.stage_id != 'live'
            WHERE a.stage_id = $1
            GROUP BY a.alias, a.language
            "#,
        )
        .bind(stage_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to detect cross-stage alias conflicts")?;

        let mut conflicts = Vec::new();
        for row in rows {
            let alias: String = row.get("alias");
            let other_stages: Vec<String> = row.get("other_stages");

            conflicts.push(
                ConflictInfo::new(
                    "url_alias",
                    &alias,
                    ConflictType::CrossStage { other_stages },
                )
                .with_label(format!("URL alias: {}", alias)),
            );
        }

        Ok(conflicts)
    }

    /// Publish a stage with explicit conflict resolution.
    ///
    /// If conflicts are detected and resolution is Cancel, the publish is aborted.
    /// Otherwise, entities are published according to the resolution strategy.
    pub async fn publish_with_resolution(
        &self,
        stage_id: &str,
        resolution: ConflictResolution,
    ) -> Result<PublishResult> {
        if stage_id == "live" {
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                "Cannot publish 'live' stage to itself".to_string(),
            ));
        }

        // Detect conflicts first
        let conflicts = self.detect_conflicts(stage_id).await?;

        if !conflicts.is_empty() {
            info!(
                stage_id = %stage_id,
                conflict_count = %conflicts.len(),
                "detected conflicts during publish"
            );

            // If resolution is Cancel and there are conflicts, abort
            if matches!(resolution, ConflictResolution::Cancel) {
                return Ok(PublishResult::cancelled(stage_id.to_string(), conflicts));
            }
        }

        // POST-MVP: Collect entity IDs to skip based on resolution.
        // Currently, only items are published (config phases are no-op).
        // When config publishing is implemented, this will filter which
        // config entities to skip during the ConfigTypes/Categories phases.
        let _skip_entities: Vec<(String, String)> = conflicts
            .iter()
            .filter(|c| {
                resolution.resolution_for(&c.entity_type, &c.entity_id) == Some(Resolution::Skip)
            })
            .map(|c| (c.entity_type.clone(), c.entity_id.clone()))
            .collect();

        info!(
            stage_id = %stage_id,
            conflicts = %conflicts.len(),
            "starting stage publish with conflict resolution"
        );

        // Start transaction
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start transaction")?;

        // Count items BEFORE publishing (so we have accurate counts)
        let items_to_publish: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
                .bind(stage_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap_or(0);

        let items_to_delete: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM stage_deletion WHERE stage_id = $1 AND entity_type = 'item'",
        )
        .bind(stage_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(0);

        // Phase 1: Config types (no-op in v1.0)
        debug!("executing phase 1: config_types (no-op)");

        // Phase 2: Categories (no-op in v1.0)
        debug!("executing phase 2: categories (no-op)");

        // Phase 3: Items
        debug!("executing phase 3: items");
        if let Err(e) = publish_items_default(&mut tx, stage_id).await {
            warn!(error = %e, "items phase failed");
            return Ok(PublishResult::failure(
                stage_id.to_string(),
                PublishPhase::Items,
                e.to_string(),
            ));
        }

        // Phase 4: Dependents (aliases, menu links)
        debug!("executing phase 4: dependents");
        let dependents_published = match publish_dependents_default(&mut tx, stage_id).await {
            Ok(count) => count,
            Err(e) => {
                warn!(error = %e, "dependents phase failed");
                return Ok(PublishResult::failure(
                    stage_id.to_string(),
                    PublishPhase::Dependents,
                    e.to_string(),
                ));
            }
        };

        // Commit transaction
        tx.commit().await.context("failed to commit transaction")?;

        info!(
            stage_id = %stage_id,
            items_published = %items_to_publish,
            items_deleted = %items_to_delete,
            dependents_published = %dependents_published,
            conflicts = %conflicts.len(),
            "stage publish completed"
        );

        // Cache invalidation AFTER transaction commits
        self.cache.invalidate_stage(stage_id).await;

        let mut result = PublishResult::success_with_conflicts(
            stage_id.to_string(),
            items_to_publish,
            items_to_delete,
            conflicts,
        );
        result.dependents_published = dependents_published;
        Ok(result)
    }
}

/// Default items publish phase: moves staged items to live and processes deletions.
async fn publish_items_default(tx: &mut Transaction<'_, Postgres>, stage_id: &str) -> Result<()> {
    // Move staged items to live
    let updated =
        sqlx::query("UPDATE item SET stage_id = 'live', changed = $1 WHERE stage_id = $2")
            .bind(chrono::Utc::now().timestamp())
            .bind(stage_id)
            .execute(&mut **tx)
            .await
            .context("failed to move staged items to live")?;

    debug!(rows = %updated.rows_affected(), "moved items to live");

    // Process deletions: delete items that were marked for deletion in this stage
    let deleted = sqlx::query(
        r#"
        DELETE FROM item
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'item'
        )
        "#,
    )
    .bind(stage_id)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged items")?;

    debug!(rows = %deleted.rows_affected(), "deleted items from deletion records");

    // Clean up deletion records for this stage
    sqlx::query("DELETE FROM stage_deletion WHERE stage_id = $1")
        .bind(stage_id)
        .execute(&mut **tx)
        .await
        .context("failed to clean up deletion records")?;

    Ok(())
}

/// Default dependents publish phase: moves staged aliases and menu links to live,
/// processes dependent deletions.
async fn publish_dependents_default(
    tx: &mut Transaction<'_, Postgres>,
    stage_id: &str,
) -> Result<i64> {
    let mut total: i64 = 0;

    // Move staged url_alias records to live
    let aliases_updated = sqlx::query("UPDATE url_alias SET stage_id = 'live' WHERE stage_id = $1")
        .bind(stage_id)
        .execute(&mut **tx)
        .await
        .context("failed to move staged aliases to live")?;
    total += aliases_updated.rows_affected() as i64;
    debug!(rows = %aliases_updated.rows_affected(), "moved url_alias to live");

    // Move staged menu_link records to live
    let menus_updated = sqlx::query("UPDATE menu_link SET stage_id = 'live' WHERE stage_id = $1")
        .bind(stage_id)
        .execute(&mut **tx)
        .await
        .context("failed to move staged menu links to live")?;
    total += menus_updated.rows_affected() as i64;
    debug!(rows = %menus_updated.rows_affected(), "moved menu_link to live");

    // Process dependent deletions (url_alias, menu_link)
    let alias_deletions = sqlx::query(
        r#"
        DELETE FROM url_alias
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'url_alias'
        )
        "#,
    )
    .bind(stage_id)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged url_alias deletions")?;
    debug!(rows = %alias_deletions.rows_affected(), "deleted url_alias from deletion records");

    let menu_deletions = sqlx::query(
        r#"
        DELETE FROM menu_link
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'menu_link'
        )
        "#,
    )
    .bind(stage_id)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged menu_link deletions")?;
    debug!(rows = %menu_deletions.rows_affected(), "deleted menu_link from deletion records");

    // Clean up dependent deletion records
    sqlx::query(
        "DELETE FROM stage_deletion WHERE stage_id = $1 AND entity_type IN ('url_alias', 'menu_link')",
    )
    .bind(stage_id)
    .execute(&mut **tx)
    .await
    .context("failed to clean up dependent deletion records")?;

    Ok(total)
}

/// Hierarchy-aware items publish: moves items from `source` to `target` stage.
async fn publish_items_to_target(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    target: &str,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();

    // Move staged items to target stage
    let updated = sqlx::query("UPDATE item SET stage_id = $1, changed = $2 WHERE stage_id = $3")
        .bind(target)
        .bind(now)
        .bind(source)
        .execute(&mut **tx)
        .await
        .context("failed to move staged items to target")?;

    debug!(rows = %updated.rows_affected(), target = %target, "moved items to target stage");

    // Process deletions: delete items marked for deletion in this stage
    // (Deletions always delete the actual item regardless of target.)
    let deleted = sqlx::query(
        r#"
        DELETE FROM item
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'item'
        )
        "#,
    )
    .bind(source)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged items")?;

    debug!(rows = %deleted.rows_affected(), "deleted items from deletion records");

    // Clean up item deletion records for this stage only
    // (dependent entity types like url_alias, menu_link are cleaned up in phase 4)
    sqlx::query("DELETE FROM stage_deletion WHERE stage_id = $1 AND entity_type = 'item'")
        .bind(source)
        .execute(&mut **tx)
        .await
        .context("failed to clean up deletion records")?;

    Ok(())
}

/// Hierarchy-aware dependents publish: moves aliases/menus from `source` to `target` stage.
async fn publish_dependents_to_target(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    target: &str,
) -> Result<i64> {
    let mut total: i64 = 0;

    // Move staged url_alias records to target stage
    let aliases_updated = sqlx::query("UPDATE url_alias SET stage_id = $1 WHERE stage_id = $2")
        .bind(target)
        .bind(source)
        .execute(&mut **tx)
        .await
        .context("failed to move staged aliases to target")?;
    total += aliases_updated.rows_affected() as i64;
    debug!(rows = %aliases_updated.rows_affected(), target = %target, "moved url_alias to target stage");

    // Move staged menu_link records to target stage
    let menus_updated = sqlx::query("UPDATE menu_link SET stage_id = $1 WHERE stage_id = $2")
        .bind(target)
        .bind(source)
        .execute(&mut **tx)
        .await
        .context("failed to move staged menu links to target")?;
    total += menus_updated.rows_affected() as i64;
    debug!(rows = %menus_updated.rows_affected(), target = %target, "moved menu_link to target stage");

    // Process dependent deletions (same as default — deletions are absolute)
    let alias_deletions = sqlx::query(
        r#"
        DELETE FROM url_alias
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'url_alias'
        )
        "#,
    )
    .bind(source)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged url_alias deletions")?;
    debug!(rows = %alias_deletions.rows_affected(), "deleted url_alias from deletion records");

    let menu_deletions = sqlx::query(
        r#"
        DELETE FROM menu_link
        WHERE id IN (
            SELECT entity_id::uuid FROM stage_deletion
            WHERE stage_id = $1 AND entity_type = 'menu_link'
        )
        "#,
    )
    .bind(source)
    .execute(&mut **tx)
    .await
    .context("failed to delete staged menu_link deletions")?;
    debug!(rows = %menu_deletions.rows_affected(), "deleted menu_link from deletion records");

    // Clean up dependent deletion records
    sqlx::query(
        "DELETE FROM stage_deletion WHERE stage_id = $1 AND entity_type IN ('url_alias', 'menu_link')",
    )
    .bind(source)
    .execute(&mut **tx)
    .await
    .context("failed to clean up dependent deletion records")?;

    Ok(total)
}

impl std::fmt::Debug for StageService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StageService").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_publish_phase_display() {
        assert_eq!(PublishPhase::ConfigTypes.to_string(), "config_types");
        assert_eq!(PublishPhase::Categories.to_string(), "categories");
        assert_eq!(PublishPhase::Items.to_string(), "items");
        assert_eq!(PublishPhase::Dependents.to_string(), "dependents");
    }

    #[test]
    fn test_publish_result_success() {
        let result = PublishResult::success("preview".to_string(), 5, 2);
        assert!(result.success);
        assert_eq!(result.items_published, 5);
        assert_eq!(result.items_deleted, 2);
        assert!(result.failed_phase.is_none());
        assert!(!result.has_conflicts());
    }

    #[test]
    fn test_publish_result_failure() {
        let result = PublishResult::failure(
            "preview".to_string(),
            PublishPhase::Items,
            "test error".to_string(),
        );
        assert!(!result.success);
        assert_eq!(result.failed_phase, Some(PublishPhase::Items));
        assert_eq!(result.error_message, Some("test error".to_string()));
    }

    #[test]
    fn test_publish_result_with_conflicts() {
        let conflicts = vec![ConflictInfo::new(
            "item_type",
            "blog",
            ConflictType::CrossStage {
                other_stages: vec!["stage-b".to_string()],
            },
        )];
        let result = PublishResult::success_with_conflicts("preview".to_string(), 3, 1, conflicts);
        assert!(result.success);
        assert!(result.has_conflicts());
        assert_eq!(result.conflicts.len(), 1);
    }

    #[test]
    fn test_publish_result_cancelled() {
        let conflicts = vec![ConflictInfo::new(
            "category",
            "tags",
            ConflictType::LiveModified {
                staged_at: 1000,
                live_changed: 2000,
            },
        )];
        let result = PublishResult::cancelled("preview".to_string(), conflicts);
        assert!(!result.success);
        assert!(result.has_conflicts());
        assert!(result.error_message.is_some());
    }

    #[test]
    fn test_conflict_type_display() {
        let cross = ConflictType::CrossStage {
            other_stages: vec!["a".to_string(), "b".to_string()],
        };
        assert_eq!(cross.to_string(), "also modified in: a, b");

        let live = ConflictType::LiveModified {
            staged_at: 1000,
            live_changed: 2000,
        };
        assert_eq!(
            live.to_string(),
            "live was modified (at 2000) after staging (at 1000)"
        );
    }

    #[test]
    fn test_conflict_resolution() {
        // Cancel returns None for all
        let cancel = ConflictResolution::Cancel;
        assert_eq!(cancel.resolution_for("item", "123"), None);

        // SkipAll returns Skip
        let skip_all = ConflictResolution::SkipAll;
        assert_eq!(
            skip_all.resolution_for("item", "123"),
            Some(Resolution::Skip)
        );

        // OverwriteAll returns Overwrite
        let overwrite_all = ConflictResolution::OverwriteAll;
        assert_eq!(
            overwrite_all.resolution_for("item", "123"),
            Some(Resolution::Overwrite)
        );

        // PerEntity returns specific resolution
        let mut map = std::collections::HashMap::new();
        map.insert("item:123".to_string(), Resolution::Skip);
        map.insert("config:abc".to_string(), Resolution::Overwrite);
        let per_entity = ConflictResolution::PerEntity(map);
        assert_eq!(
            per_entity.resolution_for("item", "123"),
            Some(Resolution::Skip)
        );
        assert_eq!(
            per_entity.resolution_for("config", "abc"),
            Some(Resolution::Overwrite)
        );
        assert_eq!(per_entity.resolution_for("other", "xyz"), None);
    }

    #[test]
    fn test_publish_phases_default() {
        let _phases = PublishPhases::new();
        // Just verify it builds without panic (post-MVP feature)
    }

    #[test]
    fn test_publish_result_with_dependents() {
        let mut result = PublishResult::success("child".to_string(), 3, 1);
        result.dependents_published = 5;
        assert!(result.success);
        assert_eq!(result.items_published, 3);
        assert_eq!(result.items_deleted, 1);
        assert_eq!(result.dependents_published, 5);
    }

    #[test]
    fn test_publish_result_hierarchy_target_stage() {
        // Verify result captures the source stage_id correctly
        let result = PublishResult::success("review".to_string(), 10, 0);
        assert_eq!(result.stage_id, "review");
        assert!(result.success);
        assert!(!result.has_conflicts());
    }

    #[test]
    fn test_publish_all_phases_enum() {
        // Verify all phases have Display implementations
        let phases = vec![
            PublishPhase::ConfigTypes,
            PublishPhase::Categories,
            PublishPhase::Items,
            PublishPhase::Dependents,
        ];
        for phase in phases {
            let s = phase.to_string();
            assert!(
                !s.is_empty(),
                "phase {:?} should have non-empty display",
                phase
            );
        }
    }
}
