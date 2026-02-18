//! Stage model and CRUD operations.
//!
//! Stages represent publishing workflow states with hierarchy support.
//! Each stage may have an upstream parent, forming a chain that terminates
//! at the root "live" stage.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashSet;

/// Stage record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Stage {
    /// Stage identifier (e.g., "live", "draft", "review").
    pub id: String,

    /// Human-readable label.
    pub label: String,

    /// Optional upstream stage ID forming a hierarchy chain.
    pub upstream_id: Option<String>,

    /// Stage status (e.g., "open", "locked").
    pub status: String,

    /// Unix timestamp when this stage was created.
    pub created: i64,

    /// Unix timestamp when this stage was last changed.
    pub changed: i64,
}

/// Input for creating a new stage.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateStage {
    pub id: String,
    pub label: String,
    pub upstream_id: Option<String>,
}

impl Stage {
    /// Create a new stage.
    pub async fn create(pool: &PgPool, input: CreateStage) -> Result<Self> {
        let now = chrono::Utc::now().timestamp();

        let stage = sqlx::query_as::<_, Stage>(
            r#"
            INSERT INTO stage (id, label, upstream_id, status, created, changed)
            VALUES ($1, $2, $3, 'open', $4, $5)
            RETURNING id, label, upstream_id, status, created, changed
            "#,
        )
        .bind(&input.id)
        .bind(&input.label)
        .bind(&input.upstream_id)
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create stage")?;

        Ok(stage)
    }

    /// Find a stage by ID.
    pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<Self>> {
        let stage = sqlx::query_as::<_, Stage>(
            "SELECT id, label, upstream_id, status, created, changed FROM stage WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch stage by id")?;

        Ok(stage)
    }

    /// List all stages.
    pub async fn list_all(pool: &PgPool) -> Result<Vec<Self>> {
        let stages = sqlx::query_as::<_, Stage>(
            "SELECT id, label, upstream_id, status, created, changed FROM stage ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .context("failed to list stages")?;

        Ok(stages)
    }

    /// Walk the upstream_id chain from the given stage to the root ("live").
    ///
    /// Returns a list of stage IDs starting with the given `stage_id` and
    /// following `upstream_id` references up to the root. Uses a visited set
    /// AND a maximum of 10 iterations to prevent infinite loops from cycles.
    pub async fn get_ancestry(pool: &PgPool, stage_id: &str) -> Result<Vec<String>> {
        let mut ancestry = Vec::new();
        let mut visited = HashSet::new();
        let mut current_id = stage_id.to_string();

        for _ in 0..10 {
            if !visited.insert(current_id.clone()) {
                // Cycle detected — stop walking
                tracing::warn!(
                    stage_id = %current_id,
                    "cycle detected in stage ancestry chain"
                );
                break;
            }

            let stage = Self::find_by_id(pool, &current_id)
                .await?
                .with_context(|| {
                    format!("stage '{}' not found during ancestry walk", current_id)
                })?;

            ancestry.push(stage.id.clone());

            match stage.upstream_id {
                Some(ref upstream) if !upstream.is_empty() => {
                    current_id = upstream.clone();
                }
                _ => break,
            }
        }

        Ok(ancestry)
    }

    /// Update a stage's label.
    pub async fn update_label(pool: &PgPool, id: &str, label: &str) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();

        let result = sqlx::query("UPDATE stage SET label = $1, changed = $2 WHERE id = $3")
            .bind(label)
            .bind(now)
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update stage label")?;

        Ok(result.rows_affected() > 0)
    }

    /// Update a stage's status.
    pub async fn update_status(pool: &PgPool, id: &str, status: &str) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();

        let result = sqlx::query("UPDATE stage SET status = $1, changed = $2 WHERE id = $3")
            .bind(status)
            .bind(now)
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update stage status")?;

        Ok(result.rows_affected() > 0)
    }

    /// Delete a stage. The "live" stage cannot be deleted.
    pub async fn delete(pool: &PgPool, id: &str) -> Result<bool> {
        if id == "live" {
            anyhow::bail!("cannot delete the 'live' stage");
        }

        let result = sqlx::query("DELETE FROM stage WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete stage")?;

        Ok(result.rows_affected() > 0)
    }
}
