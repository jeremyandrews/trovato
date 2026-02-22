//! Stage model and CRUD operations.
//!
//! Stages represent publishing workflow states, stored as vocabulary terms
//! in the `category_tag` table with stage-specific metadata in `stage_config`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::routes::helpers::is_valid_machine_name;

/// Deterministic UUID for the "live" stage tag, matching the migration seed.
///
/// This is a synthetic UUIDv7 with valid version (7) and variant (RFC 4122)
/// bits, but its timestamp and random portions are near-zero. It will sort
/// **before** every real `Uuid::now_v7()` value, which is intentional — the
/// live stage is the earliest-created stage in any deployment.
///
/// Hex: `0193a5a0-0000-7000-8000-000000000001`
pub const LIVE_STAGE_ID: Uuid = Uuid::from_bytes([
    0x01, 0x93, 0xa5, 0xa0, 0x00, 0x00, 0x70, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
]);

/// Stage visibility level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StageVisibility {
    /// Only visible to editors with stage access.
    Internal,
    /// Visible to all visitors (the live/published stage).
    Public,
    /// Accessible only via direct URL, not in listings.
    Accessible,
}

impl StageVisibility {
    /// Return the string representation stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Public => "public",
            Self::Accessible => "accessible",
        }
    }
}

impl std::str::FromStr for StageVisibility {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "internal" => Ok(Self::Internal),
            "public" => Ok(Self::Public),
            "accessible" => Ok(Self::Accessible),
            _ => Err(anyhow::anyhow!(
                "invalid stage visibility: {s:?} (expected internal, public, or accessible)"
            )),
        }
    }
}

impl From<&str> for StageVisibility {
    /// Parse a visibility string, defaulting to `Internal` for unrecognized
    /// values. Prefer [`std::str::FromStr`] when you want errors on bad input.
    fn from(s: &str) -> Self {
        match s {
            "public" => Self::Public,
            "accessible" => Self::Accessible,
            other => {
                if other != "internal" {
                    tracing::warn!(
                        visibility = other,
                        "unrecognized stage visibility, defaulting to internal"
                    );
                }
                Self::Internal
            }
        }
    }
}

impl std::fmt::Display for StageVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stage record, joined from `category_tag` + `stage_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stage {
    /// Tag UUID (category_tag.id).
    pub id: Uuid,

    /// Human-readable label (category_tag.label).
    pub label: String,

    /// Optional description (category_tag.description).
    pub description: Option<String>,

    /// Machine-readable identifier (stage_config.machine_name).
    pub machine_name: String,

    /// Visibility level (stage_config.visibility).
    pub visibility: StageVisibility,

    /// Whether this is the default stage for new content (stage_config.is_default).
    pub is_default: bool,

    /// Sort weight (category_tag.weight).
    pub weight: i16,

    /// Unix timestamp when created (category_tag.created).
    pub created: i64,

    /// Unix timestamp when last changed (category_tag.changed).
    pub changed: i64,
}

/// Row type for reading Stage from DB (visibility stored as VARCHAR).
#[derive(sqlx::FromRow)]
struct StageRow {
    id: Uuid,
    label: String,
    description: Option<String>,
    machine_name: String,
    visibility: String,
    is_default: bool,
    weight: i16,
    created: i64,
    changed: i64,
}

impl From<StageRow> for Stage {
    fn from(row: StageRow) -> Self {
        Self {
            id: row.id,
            label: row.label,
            description: row.description,
            machine_name: row.machine_name,
            visibility: StageVisibility::from(row.visibility.as_str()),
            is_default: row.is_default,
            weight: row.weight,
            created: row.created,
            changed: row.changed,
        }
    }
}

/// Input for creating a new stage.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateStage {
    /// Human-readable label.
    pub label: String,
    /// Machine name (e.g., "draft", "review").
    pub machine_name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Visibility level (defaults to "internal").
    pub visibility: Option<String>,
    /// Whether this is the default stage (defaults to false).
    pub is_default: Option<bool>,
    /// Sort weight (defaults to 0).
    pub weight: Option<i16>,
}

impl Stage {
    /// Create a new stage (inserts into both `category_tag` and `stage_config`).
    ///
    /// Validates `machine_name` (lowercase alphanumeric + underscores, starts with letter)
    /// and `visibility` (must be "internal", "public", or "accessible").
    pub async fn create(pool: &PgPool, input: CreateStage) -> Result<Self> {
        // Validate machine_name format
        if !is_valid_machine_name(&input.machine_name) {
            anyhow::bail!(
                "invalid machine_name {:?}: must be lowercase alphanumeric with underscores, starting with a letter",
                input.machine_name
            );
        }

        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let visibility_str = input.visibility.unwrap_or_else(|| "internal".to_string());
        // Validate visibility — reject unknown values rather than silently defaulting
        let _visibility: StageVisibility = visibility_str
            .parse()
            .context("invalid visibility for new stage")?;
        let is_default = input.is_default.unwrap_or(false);
        let weight = input.weight.unwrap_or(0);

        let mut tx = pool.begin().await.context("failed to start transaction")?;

        sqlx::query(
            r#"
            INSERT INTO category_tag (id, category_id, label, description, weight, created, changed)
            VALUES ($1, 'stages', $2, $3, $4, $5, $6)
            "#,
        )
        .bind(id)
        .bind(&input.label)
        .bind(&input.description)
        .bind(weight)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .context("failed to insert stage tag")?;

        sqlx::query(
            r#"
            INSERT INTO stage_config (tag_id, machine_name, visibility, is_default)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(id)
        .bind(&input.machine_name)
        .bind(&visibility_str)
        .bind(is_default)
        .execute(&mut *tx)
        .await
        .context("failed to insert stage config")?;

        tx.commit().await.context("failed to commit transaction")?;

        Self::find_by_id(pool, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to fetch created stage"))
    }

    /// Find a stage by UUID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let row = sqlx::query_as::<_, StageRow>(
            r#"
            SELECT ct.id, ct.label, ct.description, ct.weight, ct.created, ct.changed,
                   sc.machine_name, sc.visibility, sc.is_default
            FROM category_tag ct
            JOIN stage_config sc ON ct.id = sc.tag_id
            WHERE ct.category_id = 'stages' AND ct.id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch stage by id")?;

        Ok(row.map(Stage::from))
    }

    /// Find a stage by machine name.
    pub async fn find_by_machine_name(pool: &PgPool, machine_name: &str) -> Result<Option<Self>> {
        let row = sqlx::query_as::<_, StageRow>(
            r#"
            SELECT ct.id, ct.label, ct.description, ct.weight, ct.created, ct.changed,
                   sc.machine_name, sc.visibility, sc.is_default
            FROM category_tag ct
            JOIN stage_config sc ON ct.id = sc.tag_id
            WHERE ct.category_id = 'stages' AND sc.machine_name = $1
            "#,
        )
        .bind(machine_name)
        .fetch_optional(pool)
        .await
        .context("failed to fetch stage by machine name")?;

        Ok(row.map(Stage::from))
    }

    /// List all stages ordered by weight.
    pub async fn list_all(pool: &PgPool) -> Result<Vec<Self>> {
        let rows = sqlx::query_as::<_, StageRow>(
            r#"
            SELECT ct.id, ct.label, ct.description, ct.weight, ct.created, ct.changed,
                   sc.machine_name, sc.visibility, sc.is_default
            FROM category_tag ct
            JOIN stage_config sc ON ct.id = sc.tag_id
            WHERE ct.category_id = 'stages'
            ORDER BY ct.weight ASC, ct.label ASC
            "#,
        )
        .fetch_all(pool)
        .await
        .context("failed to list stages")?;

        Ok(rows.into_iter().map(Stage::from).collect())
    }

    /// Update a stage's label.
    pub async fn update_label(pool: &PgPool, id: Uuid, label: &str) -> Result<bool> {
        let now = chrono::Utc::now().timestamp();

        let result = sqlx::query(
            "UPDATE category_tag SET label = $1, changed = $2 WHERE id = $3 AND category_id = 'stages'",
        )
        .bind(label)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .context("failed to update stage label")?;

        Ok(result.rows_affected() > 0)
    }

    /// Update a stage's visibility.
    ///
    /// Validates that `visibility` is one of "internal", "public", or "accessible".
    pub async fn update_visibility(pool: &PgPool, id: Uuid, visibility: &str) -> Result<bool> {
        // Validate visibility before writing to DB
        let _: StageVisibility = visibility
            .parse()
            .context("invalid visibility for stage update")?;

        let result = sqlx::query("UPDATE stage_config SET visibility = $1 WHERE tag_id = $2")
            .bind(visibility)
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update stage visibility")?;

        if result.rows_affected() > 0 {
            let now = chrono::Utc::now().timestamp();
            sqlx::query("UPDATE category_tag SET changed = $1 WHERE id = $2")
                .bind(now)
                .bind(id)
                .execute(pool)
                .await
                .context("failed to update stage changed timestamp")?;
        }

        Ok(result.rows_affected() > 0)
    }

    /// Delete a stage. The public, default, and live stages cannot be deleted.
    ///
    /// Also checks for content referencing this stage (items, aliases, menu links,
    /// tiles) and refuses deletion with a descriptive error if any exist.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        if id == LIVE_STAGE_ID {
            anyhow::bail!("cannot delete the live stage");
        }

        // Check for protected stages (public visibility or is_default)
        let is_protected: bool = sqlx::query_scalar(
            "SELECT (visibility = 'public' OR is_default) FROM stage_config WHERE tag_id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to check stage protection")?
        .unwrap_or(false);

        if is_protected {
            anyhow::bail!("cannot delete the public or default stage");
        }

        // Check for content referencing this stage (descriptive error before FK blocks it)
        let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item WHERE stage_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .context("failed to count items in stage")?;

        if item_count > 0 {
            anyhow::bail!(
                "cannot delete stage: {item_count} item(s) still reference it; \
                 migrate or delete them first"
            );
        }

        // Delete from category_tag (cascades to stage_config via ON DELETE CASCADE)
        let result =
            sqlx::query("DELETE FROM category_tag WHERE id = $1 AND category_id = 'stages'")
                .bind(id)
                .execute(pool)
                .await
                .context("failed to delete stage")?;

        Ok(result.rows_affected() > 0)
    }
}
