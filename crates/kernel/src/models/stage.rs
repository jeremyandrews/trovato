//! Stage model and CRUD operations.
//!
//! Stages represent publishing workflow states, stored as category tags
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

/// What still references a stage, table by table.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct StageReferences {
    /// Content items.
    pub items: i64,
    /// URL aliases.
    pub url_aliases: i64,
    /// Menu links.
    pub menu_links: i64,
    /// Tiles.
    pub tiles: i64,
}

impl StageReferences {
    /// Total across every table.
    pub fn total(&self) -> i64 {
        self.items + self.url_aliases + self.menu_links + self.tiles
    }

    /// A human-readable list of the non-zero counts.
    ///
    /// "3 items and 1 menu link" rather than "4 things", because the operator has
    /// to go and find them.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (count, singular, plural) in [
            (self.items, "item", "items"),
            (self.url_aliases, "URL alias", "URL aliases"),
            (self.menu_links, "menu link", "menu links"),
            (self.tiles, "tile", "tiles"),
        ] {
            if count > 0 {
                let noun = if count == 1 { singular } else { plural };
                parts.push(format!("{count} {noun}"));
            }
        }
        match parts.len() {
            0 => "nothing".to_string(),
            1 => parts.remove(0),
            _ => {
                let last = parts.pop().unwrap_or_default();
                format!("{} and {last}", parts.join(", "))
            }
        }
    }
}

/// Fields an update may change. `None` leaves a field as it is.
///
/// `machine_name` is included because a stage's machine name is what config files
/// and workflow definitions refer to it by, so an editable label with a frozen
/// machine name would be a form that cannot fix a typo made at creation.
#[derive(Debug, Clone, Default)]
pub struct UpdateStage {
    /// Human-readable label.
    pub label: Option<String>,
    /// Machine name.
    pub machine_name: Option<String>,
    /// Description. `Some(None)` clears it.
    pub description: Option<Option<String>>,
    /// Visibility level.
    pub visibility: Option<String>,
    /// Whether this becomes the default stage. `Some(false)` is refused when this
    /// is the only default, because content has to land somewhere.
    pub is_default: Option<bool>,
    /// Sort weight.
    pub weight: Option<i16>,
}

impl Stage {
    /// Create a new stage (inserts into both `category_tag` and `stage_config`).
    ///
    /// Validates `machine_name` (lowercase alphanumeric + underscores, starts with letter)
    /// and `visibility` (must be "internal", "public", or "accessible").
    pub async fn create(pool: &PgPool, input: CreateStage) -> Result<Self> {
        Self::create_with_id(pool, Uuid::now_v7(), input).await
    }

    /// Create a new stage under a caller-supplied UUID.
    ///
    /// Config import needs this: a `stage.{uuid}.yml` file declares the stage's
    /// identity, and generating a fresh UUID instead would make the file's `id`
    /// a lie, break re-import (the second run would not find the stage and would
    /// try to create it again, colliding on `machine_name`), and make export not
    /// round-trip.
    pub async fn create_with_id(pool: &PgPool, id: Uuid, input: CreateStage) -> Result<Self> {
        // Validate machine_name format
        if !is_valid_machine_name(&input.machine_name) {
            anyhow::bail!(
                "invalid machine_name {:?}: must be lowercase alphanumeric with underscores, starting with a letter",
                input.machine_name
            );
        }

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

    /// Apply an update across both of a stage's tables.
    ///
    /// One transaction, because a stage is two rows and a half-applied edit is a
    /// stage whose label and visibility disagree about which stage it is.
    ///
    /// Three rules the form does not get to bypass, enforced here so the CLI and
    /// any future caller get them too:
    ///
    /// - **The Live stage stays public.** `stage_config` has a partial unique index
    ///   on `visibility = 'public'`, so exactly one stage can be public, and the
    ///   render layer resolves published content through it. Demoting it would take
    ///   the site's published content off the site.
    /// - **Exactly one default.** Setting `is_default` clears it everywhere else in
    ///   the same transaction, which is also what the partial unique index on
    ///   `is_default = true` requires. Clearing the last one is refused: new
    ///   content has to land somewhere.
    /// - **A machine name is a machine name.** Same rule as creation.
    ///
    /// # Errors
    ///
    /// Returns an error when a rule above is broken or the write fails. Returns
    /// `Ok(None)` when no such stage exists.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateStage) -> Result<Option<Self>> {
        let Some(existing) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        if let Some(machine_name) = input.machine_name.as_deref()
            && !is_valid_machine_name(machine_name)
        {
            anyhow::bail!(
                "invalid machine_name {machine_name:?}: must be lowercase alphanumeric with \
                 underscores, starting with a letter"
            );
        }

        let visibility = match input.visibility.as_deref() {
            Some(raw) => {
                let parsed: StageVisibility =
                    raw.parse().context("invalid visibility for stage update")?;
                if id == LIVE_STAGE_ID && parsed != StageVisibility::Public {
                    anyhow::bail!(
                        "the live stage must stay public: published content is resolved through it"
                    );
                }
                parsed
            }
            None => existing.visibility,
        };

        let is_default = input.is_default.unwrap_or(existing.is_default);
        if existing.is_default && !is_default {
            anyhow::bail!(
                "cannot clear the default stage: new content has to land somewhere, so make \
                 another stage the default instead"
            );
        }

        let label = input.label.unwrap_or(existing.label);
        let machine_name = input.machine_name.unwrap_or(existing.machine_name);
        let description = input.description.unwrap_or(existing.description);
        let weight = input.weight.unwrap_or(existing.weight);
        let now = chrono::Utc::now().timestamp();

        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Clear any other default first: the partial unique index would otherwise
        // reject the write, and "there is exactly one default" is the invariant
        // this is maintaining rather than a side effect.
        if is_default {
            sqlx::query("UPDATE stage_config SET is_default = false WHERE tag_id <> $1")
                .bind(id)
                .execute(&mut *tx)
                .await
                .context("failed to clear the previous default stage")?;
        }

        sqlx::query(
            "UPDATE category_tag SET label = $1, description = $2, weight = $3, changed = $4 \
             WHERE id = $5 AND category_id = 'stages'",
        )
        .bind(&label)
        .bind(&description)
        .bind(weight)
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("failed to update stage tag")?;

        sqlx::query(
            "UPDATE stage_config SET machine_name = $1, visibility = $2, is_default = $3 \
             WHERE tag_id = $4",
        )
        .bind(&machine_name)
        .bind(visibility.as_str())
        .bind(is_default)
        .bind(id)
        .execute(&mut *tx)
        .await
        .context("failed to update stage config")?;

        tx.commit().await.context("failed to commit transaction")?;

        Self::find_by_id(pool, id).await
    }

    /// How much content references this stage, table by table.
    ///
    /// Returned rather than summed so a refusal can say where the content is. Every
    /// one of these columns is a `RESTRICT` foreign key, so without this the
    /// refusal is a raw constraint error naming a constraint instead of a count.
    pub async fn reference_counts(pool: &PgPool, id: Uuid) -> Result<StageReferences> {
        let counts: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM item WHERE stage_id = $1), \
                    (SELECT COUNT(*) FROM url_alias WHERE stage_id = $1), \
                    (SELECT COUNT(*) FROM menu_link WHERE stage_id = $1), \
                    (SELECT COUNT(*) FROM tile WHERE stage_id = $1)",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .context("failed to count what references a stage")?;

        Ok(StageReferences {
            items: counts.0,
            url_aliases: counts.1,
            menu_links: counts.2,
            tiles: counts.3,
        })
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

        // Check for content referencing this stage, before the foreign key does it
        // with a message naming a constraint. Every one of these four columns is a
        // RESTRICT reference; counting only items, as this used to, meant a stage
        // holding a menu link or a tile was refused by Postgres instead.
        let references = Self::reference_counts(pool, id).await?;
        if references.total() > 0 {
            anyhow::bail!(
                "cannot delete stage: {} still reference it; move or delete them first",
                references.describe()
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn refs(items: i64, url_aliases: i64, menu_links: i64, tiles: i64) -> StageReferences {
        StageReferences {
            items,
            url_aliases,
            menu_links,
            tiles,
        }
    }

    /// The refusal message an operator reads names where the content is, and
    /// pluralizes, because "1 items" reads as a bug in the thing refusing.
    #[test]
    fn references_describe_only_the_non_zero_counts() {
        assert_eq!(refs(0, 0, 0, 0).describe(), "nothing");
        assert_eq!(refs(1, 0, 0, 0).describe(), "1 item");
        assert_eq!(refs(3, 0, 0, 0).describe(), "3 items");
        assert_eq!(refs(0, 0, 1, 0).describe(), "1 menu link");
        assert_eq!(refs(3, 0, 1, 0).describe(), "3 items and 1 menu link");
        assert_eq!(
            refs(2, 1, 1, 4).describe(),
            "2 items, 1 URL alias, 1 menu link and 4 tiles"
        );
    }

    /// The total is what decides whether a delete is refused at all, so it counts
    /// every table rather than the one `Stage::delete` used to look at.
    #[test]
    fn references_total_covers_every_table() {
        assert_eq!(refs(0, 0, 0, 0).total(), 0);
        assert_eq!(refs(1, 2, 3, 4).total(), 10);
        assert_eq!(
            refs(0, 0, 1, 0).total(),
            1,
            "a menu link alone must block a delete: it is a RESTRICT reference too"
        );
    }

    #[test]
    fn visibility_round_trips_through_its_string_form() {
        for visibility in [
            StageVisibility::Internal,
            StageVisibility::Public,
            StageVisibility::Accessible,
        ] {
            assert_eq!(
                visibility.as_str().parse::<StageVisibility>().unwrap(),
                visibility
            );
        }
        assert!("nonsense".parse::<StageVisibility>().is_err());
    }
}
