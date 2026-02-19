//! Category models: categories and tags with hierarchical category system.
//!
//! Categories provide flexible classification with:
//! - Category: Named collections of tags (e.g., "Topics", "Regions")
//! - Tag: Individual category entries with labels and descriptions
//! - Hierarchy: DAG structure supporting multiple parents per tag

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// A category (collection of tags).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Category {
    /// Machine name identifier.
    pub id: String,

    /// Human-readable label.
    pub label: String,

    /// Optional description.
    pub description: Option<String>,

    /// Hierarchy mode: 0=flat, 1=single parent, 2=multiple parents (DAG).
    pub hierarchy: i16,

    /// Sort weight.
    pub weight: i16,
}

/// A tag within a category.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tag {
    /// Unique identifier (UUIDv7).
    pub id: Uuid,

    /// Category this tag belongs to.
    pub category_id: String,

    /// Human-readable label.
    pub label: String,

    /// Optional description.
    pub description: Option<String>,

    /// Sort weight within its level.
    pub weight: i16,

    /// Unix timestamp when created.
    pub created: i64,

    /// Unix timestamp when last changed.
    pub changed: i64,
}

/// Tag hierarchy record (junction table for DAG).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TagHierarchy {
    /// Tag ID.
    pub tag_id: Uuid,

    /// Parent tag ID (NULL for root tags).
    pub parent_id: Option<Uuid>,
}

/// Tag with depth information (for tree queries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagWithDepth {
    pub tag: Tag,
    pub depth: i32,
}

/// Tree node for hierarchical display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagTreeNode {
    pub tag: Tag,
    pub depth: i32,
    pub children: Vec<TagTreeNode>,
}

/// Input for creating a category.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCategory {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub hierarchy: Option<i16>,
    pub weight: Option<i16>,
}

/// Input for updating a category.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateCategory {
    pub label: Option<String>,
    pub description: Option<String>,
    pub hierarchy: Option<i16>,
    pub weight: Option<i16>,
}

/// Input for creating a tag.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTag {
    pub category_id: String,
    pub label: String,
    pub description: Option<String>,
    pub weight: Option<i16>,
    pub parent_ids: Option<Vec<Uuid>>,
}

/// Input for updating a tag.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTag {
    pub label: Option<String>,
    pub description: Option<String>,
    pub weight: Option<i16>,
}

impl Category {
    /// Find a category by ID.
    pub async fn find_by_id(pool: &PgPool, id: &str) -> Result<Option<Self>> {
        let category = sqlx::query_as::<_, Self>(
            "SELECT id, label, description, hierarchy, weight FROM category WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch category")?;

        Ok(category)
    }

    /// List all categories ordered by weight.
    pub async fn list(pool: &PgPool) -> Result<Vec<Self>> {
        let categories = sqlx::query_as::<_, Self>(
            "SELECT id, label, description, hierarchy, weight FROM category ORDER BY weight, label",
        )
        .fetch_all(pool)
        .await
        .context("failed to list categories")?;

        Ok(categories)
    }

    /// Create a new category.
    pub async fn create(pool: &PgPool, input: CreateCategory) -> Result<Self> {
        sqlx::query(
            r#"
            INSERT INTO category (id, label, description, hierarchy, weight)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(&input.id)
        .bind(&input.label)
        .bind(&input.description)
        .bind(input.hierarchy.unwrap_or(0))
        .bind(input.weight.unwrap_or(0))
        .execute(pool)
        .await
        .context("failed to create category")?;

        Self::find_by_id(pool, &input.id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to fetch created category"))
    }

    /// Update a category.
    pub async fn update(pool: &PgPool, id: &str, input: UpdateCategory) -> Result<Option<Self>> {
        let Some(current) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        let label = input.label.unwrap_or(current.label);
        let description = input.description.or(current.description);
        let hierarchy = input.hierarchy.unwrap_or(current.hierarchy);
        let weight = input.weight.unwrap_or(current.weight);

        sqlx::query(
            r#"
            UPDATE category
            SET label = $1, description = $2, hierarchy = $3, weight = $4
            WHERE id = $5
            "#,
        )
        .bind(&label)
        .bind(&description)
        .bind(hierarchy)
        .bind(weight)
        .bind(id)
        .execute(pool)
        .await
        .context("failed to update category")?;

        Self::find_by_id(pool, id).await
    }

    /// Delete a category (cascades to tags).
    pub async fn delete(pool: &PgPool, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM category WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete category")?;

        Ok(result.rows_affected() > 0)
    }

    /// Check if a category exists.
    pub async fn exists(pool: &PgPool, id: &str) -> Result<bool> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM category WHERE id = $1)")
                .bind(id)
                .fetch_one(pool)
                .await
                .context("failed to check category existence")?;

        Ok(exists)
    }
}

impl Tag {
    /// Find a tag by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let tag = sqlx::query_as::<_, Self>(
            "SELECT id, category_id, label, description, weight, created, changed FROM category_tag WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to fetch tag")?;

        Ok(tag)
    }

    /// List all tags in a category ordered by weight.
    pub async fn list_by_category(pool: &PgPool, category_id: &str) -> Result<Vec<Self>> {
        let tags = sqlx::query_as::<_, Self>(
            "SELECT id, category_id, label, description, weight, created, changed FROM category_tag WHERE category_id = $1 ORDER BY weight, label",
        )
        .bind(category_id)
        .fetch_all(pool)
        .await
        .context("failed to list tags")?;

        Ok(tags)
    }

    /// Create a new tag.
    pub async fn create(pool: &PgPool, input: CreateTag) -> Result<Self> {
        let now = chrono::Utc::now().timestamp();
        let id = Uuid::now_v7();

        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Insert tag
        sqlx::query(
            r#"
            INSERT INTO category_tag (id, category_id, label, description, weight, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(id)
        .bind(&input.category_id)
        .bind(&input.label)
        .bind(&input.description)
        .bind(input.weight.unwrap_or(0))
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await
        .context("failed to insert tag")?;

        // Insert hierarchy entries
        if let Some(ref parent_ids) = input.parent_ids {
            if parent_ids.is_empty() {
                // Root tag - insert with NULL parent
                sqlx::query(
                    "INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL)",
                )
                .bind(id)
                .execute(&mut *tx)
                .await
                .context("failed to insert root hierarchy")?;
            } else {
                // Insert parent relationships
                for parent_id in parent_ids {
                    sqlx::query(
                        "INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, $2)",
                    )
                    .bind(id)
                    .bind(parent_id)
                    .execute(&mut *tx)
                    .await
                    .context("failed to insert hierarchy")?;
                }
            }
        } else {
            // No parents specified - make it a root tag
            sqlx::query("INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL)")
                .bind(id)
                .execute(&mut *tx)
                .await
                .context("failed to insert root hierarchy")?;
        }

        tx.commit().await.context("failed to commit transaction")?;

        Self::find_by_id(pool, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to fetch created tag"))
    }

    /// Update a tag.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateTag) -> Result<Option<Self>> {
        let now = chrono::Utc::now().timestamp();

        let Some(current) = Self::find_by_id(pool, id).await? else {
            return Ok(None);
        };

        let label = input.label.unwrap_or(current.label);
        let description = input.description.or(current.description);
        let weight = input.weight.unwrap_or(current.weight);

        sqlx::query(
            r#"
            UPDATE category_tag
            SET label = $1, description = $2, weight = $3, changed = $4
            WHERE id = $5
            "#,
        )
        .bind(&label)
        .bind(&description)
        .bind(weight)
        .bind(now)
        .bind(id)
        .execute(pool)
        .await
        .context("failed to update tag")?;

        Self::find_by_id(pool, id).await
    }

    /// Delete a tag (cascades hierarchy).
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM category_tag WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete tag")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get direct parents of a tag.
    pub async fn get_parents(pool: &PgPool, id: Uuid) -> Result<Vec<Self>> {
        let parents = sqlx::query_as::<_, Self>(
            r#"
            SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed
            FROM category_tag t
            INNER JOIN category_tag_hierarchy h ON t.id = h.parent_id
            WHERE h.tag_id = $1
            ORDER BY t.weight, t.label
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("failed to fetch parents")?;

        Ok(parents)
    }

    /// Get direct children of a tag.
    pub async fn get_children(pool: &PgPool, id: Uuid) -> Result<Vec<Self>> {
        let children = sqlx::query_as::<_, Self>(
            r#"
            SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed
            FROM category_tag t
            INNER JOIN category_tag_hierarchy h ON t.id = h.tag_id
            WHERE h.parent_id = $1
            ORDER BY t.weight, t.label
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("failed to fetch children")?;

        Ok(children)
    }

    /// Get root tags in a category (tags with no parents).
    pub async fn get_roots(pool: &PgPool, category_id: &str) -> Result<Vec<Self>> {
        let roots = sqlx::query_as::<_, Self>(
            r#"
            SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed
            FROM category_tag t
            INNER JOIN category_tag_hierarchy h ON t.id = h.tag_id
            WHERE t.category_id = $1 AND h.parent_id IS NULL
            ORDER BY t.weight, t.label
            "#,
        )
        .bind(category_id)
        .fetch_all(pool)
        .await
        .context("failed to fetch root tags")?;

        Ok(roots)
    }

    /// Get all ancestors of a tag using recursive CTE.
    pub async fn get_ancestors(pool: &PgPool, id: Uuid) -> Result<Vec<TagWithDepth>> {
        #[derive(sqlx::FromRow)]
        struct AncestorRow {
            id: Uuid,
            category_id: String,
            label: String,
            description: Option<String>,
            weight: i16,
            created: i64,
            changed: i64,
            depth: i32,
        }

        let rows = sqlx::query_as::<_, AncestorRow>(
            r#"
            WITH RECURSIVE ancestors AS (
                -- Base case: direct parents
                SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed, 1 as depth
                FROM category_tag t
                INNER JOIN category_tag_hierarchy h ON t.id = h.parent_id
                WHERE h.tag_id = $1 AND h.parent_id IS NOT NULL

                UNION ALL

                -- Recursive case: parents of parents
                SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed, a.depth + 1
                FROM category_tag t
                INNER JOIN category_tag_hierarchy h ON t.id = h.parent_id
                INNER JOIN ancestors a ON h.tag_id = a.id
                WHERE h.parent_id IS NOT NULL
            )
            SELECT DISTINCT id, category_id, label, description, weight, created, changed, depth
            FROM ancestors
            ORDER BY depth DESC
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("failed to fetch ancestors")?;

        Ok(rows
            .into_iter()
            .map(|r| TagWithDepth {
                tag: Tag {
                    id: r.id,
                    category_id: r.category_id,
                    label: r.label,
                    description: r.description,
                    weight: r.weight,
                    created: r.created,
                    changed: r.changed,
                },
                depth: r.depth,
            })
            .collect())
    }

    /// Get all descendants of a tag using recursive CTE.
    pub async fn get_descendants(pool: &PgPool, id: Uuid) -> Result<Vec<TagWithDepth>> {
        #[derive(sqlx::FromRow)]
        struct DescendantRow {
            id: Uuid,
            category_id: String,
            label: String,
            description: Option<String>,
            weight: i16,
            created: i64,
            changed: i64,
            depth: i32,
        }

        let rows = sqlx::query_as::<_, DescendantRow>(
            r#"
            WITH RECURSIVE descendants AS (
                -- Base case: direct children
                SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed, 1 as depth
                FROM category_tag t
                INNER JOIN category_tag_hierarchy h ON t.id = h.tag_id
                WHERE h.parent_id = $1

                UNION ALL

                -- Recursive case: children of children
                SELECT t.id, t.category_id, t.label, t.description, t.weight, t.created, t.changed, d.depth + 1
                FROM category_tag t
                INNER JOIN category_tag_hierarchy h ON t.id = h.tag_id
                INNER JOIN descendants d ON h.parent_id = d.id
            )
            SELECT DISTINCT id, category_id, label, description, weight, created, changed, depth
            FROM descendants
            ORDER BY depth, weight, label
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("failed to fetch descendants")?;

        Ok(rows
            .into_iter()
            .map(|r| TagWithDepth {
                tag: Tag {
                    id: r.id,
                    category_id: r.category_id,
                    label: r.label,
                    description: r.description,
                    weight: r.weight,
                    created: r.created,
                    changed: r.changed,
                },
                depth: r.depth,
            })
            .collect())
    }

    /// Get tag IDs of a tag and all its descendants (for category filtering).
    pub async fn get_tag_and_descendant_ids(pool: &PgPool, id: Uuid) -> Result<Vec<Uuid>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            r#"
            WITH RECURSIVE descendants AS (
                -- Base case: the tag itself
                SELECT $1::uuid as id

                UNION ALL

                -- Recursive case: children
                SELECT h.tag_id
                FROM category_tag_hierarchy h
                INNER JOIN descendants d ON h.parent_id = d.id
            )
            SELECT id FROM descendants
            "#,
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .context("failed to fetch tag and descendant ids")?;

        Ok(ids)
    }

    /// Set the parents of a tag (replaces existing parents).
    pub async fn set_parents(pool: &PgPool, id: Uuid, parent_ids: &[Uuid]) -> Result<()> {
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Remove existing hierarchy entries
        sqlx::query("DELETE FROM category_tag_hierarchy WHERE tag_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to delete existing hierarchy")?;

        // Insert new hierarchy entries
        if parent_ids.is_empty() {
            // Root tag
            sqlx::query("INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL)")
                .bind(id)
                .execute(&mut *tx)
                .await
                .context("failed to insert root hierarchy")?;
        } else {
            for parent_id in parent_ids {
                sqlx::query(
                    "INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, $2)",
                )
                .bind(id)
                .bind(parent_id)
                .execute(&mut *tx)
                .await
                .context("failed to insert hierarchy")?;
            }
        }

        // Update changed timestamp
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE category_tag SET changed = $1 WHERE id = $2")
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to update tag timestamp")?;

        tx.commit().await.context("failed to commit transaction")?;

        Ok(())
    }

    /// Add a parent to a tag.
    pub async fn add_parent(pool: &PgPool, id: Uuid, parent_id: Uuid) -> Result<()> {
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Remove NULL parent if this was a root tag
        sqlx::query("DELETE FROM category_tag_hierarchy WHERE tag_id = $1 AND parent_id IS NULL")
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to remove root status")?;

        // Insert new parent
        sqlx::query(
            "INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(parent_id)
        .execute(&mut *tx)
        .await
        .context("failed to add parent")?;

        // Update changed timestamp
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE category_tag SET changed = $1 WHERE id = $2")
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to update tag timestamp")?;

        tx.commit().await.context("failed to commit transaction")?;

        Ok(())
    }

    /// Remove a parent from a tag.
    pub async fn remove_parent(pool: &PgPool, id: Uuid, parent_id: Uuid) -> Result<()> {
        let mut tx = pool.begin().await.context("failed to start transaction")?;

        // Remove the parent
        sqlx::query("DELETE FROM category_tag_hierarchy WHERE tag_id = $1 AND parent_id = $2")
            .bind(id)
            .bind(parent_id)
            .execute(&mut *tx)
            .await
            .context("failed to remove parent")?;

        // Check if tag has any remaining parents
        let has_parents: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM category_tag_hierarchy WHERE tag_id = $1 AND parent_id IS NOT NULL)",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .context("failed to check for remaining parents")?;

        // If no parents remain, make it a root tag
        if !has_parents {
            sqlx::query(
                "INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to make tag root")?;
        }

        // Update changed timestamp
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE category_tag SET changed = $1 WHERE id = $2")
            .bind(now)
            .bind(id)
            .execute(&mut *tx)
            .await
            .context("failed to update tag timestamp")?;

        tx.commit().await.context("failed to commit transaction")?;

        Ok(())
    }

    /// Count tags in a category.
    pub async fn count_by_category(pool: &PgPool, category_id: &str) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM category_tag WHERE category_id = $1")
                .bind(category_id)
                .fetch_one(pool)
                .await
                .context("failed to count tags")?;

        Ok(count)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn category_hierarchy_modes() {
        let flat = Category {
            id: "tags".to_string(),
            label: "Tags".to_string(),
            description: None,
            hierarchy: 0,
            weight: 0,
        };
        assert_eq!(flat.hierarchy, 0);

        let dag = Category {
            id: "topics".to_string(),
            label: "Topics".to_string(),
            description: Some("Hierarchical topics".to_string()),
            hierarchy: 2,
            weight: 0,
        };
        assert_eq!(dag.hierarchy, 2);
    }

    #[test]
    fn tag_serialization() {
        let tag = Tag {
            id: Uuid::nil(),
            category_id: "tags".to_string(),
            label: "Rust".to_string(),
            description: Some("Rust programming language".to_string()),
            weight: 0,
            created: 1000,
            changed: 1000,
        };

        let json = serde_json::to_string(&tag).unwrap();
        assert!(json.contains("Rust"));

        let parsed: Tag = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.label, "Rust");
    }

    #[test]
    fn tag_with_depth() {
        let tag = Tag {
            id: Uuid::nil(),
            category_id: "topics".to_string(),
            label: "Programming".to_string(),
            description: None,
            weight: 0,
            created: 1000,
            changed: 1000,
        };

        let with_depth = TagWithDepth { tag, depth: 2 };
        assert_eq!(with_depth.depth, 2);
        assert_eq!(with_depth.tag.label, "Programming");
    }

    #[test]
    fn create_category_input() {
        let input = CreateCategory {
            id: "tags".to_string(),
            label: "Tags".to_string(),
            description: Some("Content tags".to_string()),
            hierarchy: Some(0),
            weight: None,
        };

        assert_eq!(input.id, "tags");
        assert_eq!(input.hierarchy, Some(0));
    }

    #[test]
    fn create_tag_input() {
        let input = CreateTag {
            category_id: "topics".to_string(),
            label: "Technology".to_string(),
            description: None,
            weight: Some(10),
            parent_ids: Some(vec![Uuid::nil()]),
        };

        assert_eq!(input.category_id, "topics");
        assert_eq!(input.parent_ids.as_ref().unwrap().len(), 1);
    }
}
