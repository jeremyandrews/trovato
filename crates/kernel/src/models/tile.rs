//! Tile (block) model for placing content in page regions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Tile record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Tile {
    pub id: Uuid,
    pub machine_name: String,
    pub label: String,
    pub region: String,
    pub tile_type: String,
    pub config: serde_json::Value,
    pub visibility: serde_json::Value,
    pub weight: i32,
    pub status: i32,
    pub plugin: String,
    pub stage_id: String,
    pub created: i64,
    pub changed: i64,
}

/// Input for creating a tile.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTile {
    pub machine_name: String,
    pub label: String,
    pub region: Option<String>,
    pub tile_type: Option<String>,
    pub config: Option<serde_json::Value>,
    pub visibility: Option<serde_json::Value>,
    pub weight: Option<i32>,
    pub status: Option<i32>,
    pub plugin: Option<String>,
    pub stage_id: Option<String>,
}

/// Input for updating a tile.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTile {
    pub label: Option<String>,
    pub region: Option<String>,
    pub tile_type: Option<String>,
    pub config: Option<serde_json::Value>,
    pub visibility: Option<serde_json::Value>,
    pub weight: Option<i32>,
    pub status: Option<i32>,
    pub plugin: Option<String>,
    pub stage_id: Option<String>,
}

impl Tile {
    /// Create a new tile.
    pub async fn create(pool: &PgPool, input: CreateTile) -> Result<Self> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();
        let region = input.region.unwrap_or_else(|| "sidebar".to_string());
        let tile_type = input.tile_type.unwrap_or_else(|| "custom_html".to_string());
        let config = input.config.unwrap_or_else(|| serde_json::json!({}));
        let visibility = input.visibility.unwrap_or_else(|| serde_json::json!({}));
        let weight = input.weight.unwrap_or(0);
        let status = input.status.unwrap_or(1);
        let plugin = input.plugin.unwrap_or_else(|| "core".to_string());
        let stage_id = input.stage_id.unwrap_or_else(|| "live".to_string());

        let tile = sqlx::query_as::<_, Tile>(
            r#"
            INSERT INTO tile (id, machine_name, label, region, tile_type, config, visibility, weight, status, plugin, stage_id, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(&input.machine_name)
        .bind(&input.label)
        .bind(&region)
        .bind(&tile_type)
        .bind(&config)
        .bind(&visibility)
        .bind(weight)
        .bind(status)
        .bind(&plugin)
        .bind(&stage_id)
        .bind(now)
        .bind(now)
        .fetch_one(pool)
        .await
        .context("failed to create tile")?;

        Ok(tile)
    }

    /// Find a tile by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let tile = sqlx::query_as::<_, Tile>("SELECT * FROM tile WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch tile by id")?;

        Ok(tile)
    }

    /// List all tiles, ordered by region then weight.
    pub async fn list_all(pool: &PgPool) -> Result<Vec<Self>> {
        let tiles = sqlx::query_as::<_, Tile>(
            "SELECT * FROM tile ORDER BY region ASC, weight ASC, label ASC",
        )
        .fetch_all(pool)
        .await
        .context("failed to list tiles")?;

        Ok(tiles)
    }

    /// List active tiles for a region and stage, ordered by weight.
    pub async fn list_by_region(pool: &PgPool, region: &str, stage_id: &str) -> Result<Vec<Self>> {
        let tiles = sqlx::query_as::<_, Tile>(
            r#"
            SELECT * FROM tile
            WHERE region = $1 AND stage_id = $2 AND status = 1
            ORDER BY weight ASC, label ASC
            "#,
        )
        .bind(region)
        .bind(stage_id)
        .fetch_all(pool)
        .await
        .context("failed to list tiles by region")?;

        Ok(tiles)
    }

    /// Update a tile.
    ///
    /// Uses COALESCE in a single query to apply partial updates atomically,
    /// avoiding a separate SELECT + UPDATE race condition.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateTile) -> Result<Option<Self>> {
        let now = chrono::Utc::now().timestamp();

        let updated = sqlx::query_as::<_, Tile>(
            r#"
            UPDATE tile
            SET label      = COALESCE($1, label),
                region     = COALESCE($2, region),
                tile_type  = COALESCE($3, tile_type),
                config     = COALESCE($4, config),
                visibility = COALESCE($5, visibility),
                weight     = COALESCE($6, weight),
                status     = COALESCE($7, status),
                plugin     = COALESCE($8, plugin),
                stage_id   = COALESCE($9, stage_id),
                changed    = $10
            WHERE id = $11
            RETURNING *
            "#,
        )
        .bind(input.label)
        .bind(input.region)
        .bind(input.tile_type)
        .bind(input.config)
        .bind(input.visibility)
        .bind(input.weight)
        .bind(input.status)
        .bind(input.plugin)
        .bind(input.stage_id)
        .bind(now)
        .bind(id)
        .fetch_optional(pool)
        .await
        .context("failed to update tile")?;

        Ok(updated)
    }

    /// Delete a tile.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM tile WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete tile")?;

        Ok(result.rows_affected() > 0)
    }

    /// Check if this tile is visible for a given request path and user roles.
    ///
    /// Visibility rules in JSON:
    /// - `{ "paths": ["/admin/*", "/user/*"] }` — show only on matching paths
    /// - `{ "paths_exclude": ["/admin/*"] }` — hide on matching paths
    /// - `{ "roles": ["authenticated user", "administrator"] }` — show only to users with one of these roles
    /// - `{}` — always visible
    pub fn is_visible(&self, path: &str, user_roles: &[String]) -> bool {
        // Role check
        if let Some(roles) = self.visibility.get("roles").and_then(|v| v.as_array()) {
            let role_match = roles.iter().any(|r| {
                let required = r.as_str().unwrap_or("");
                user_roles.iter().any(|ur| ur == required)
            });
            if !role_match {
                return false;
            }
        }

        // Path check
        if let Some(paths) = self.visibility.get("paths").and_then(|v| v.as_array()) {
            return paths.iter().any(|p| {
                let pattern = p.as_str().unwrap_or("");
                path_matches(pattern, path)
            });
        }

        if let Some(paths) = self
            .visibility
            .get("paths_exclude")
            .and_then(|v| v.as_array())
        {
            return !paths.iter().any(|p| {
                let pattern = p.as_str().unwrap_or("");
                path_matches(pattern, path)
            });
        }

        true
    }
}

/// Simple glob-style path matching (supports trailing `*` only).
fn path_matches(pattern: &str, path: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        path.starts_with(prefix)
    } else {
        path == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tile(visibility: serde_json::Value) -> Tile {
        Tile {
            id: Uuid::nil(),
            machine_name: "test".into(),
            label: "Test".into(),
            region: "sidebar".into(),
            tile_type: "custom_html".into(),
            config: serde_json::json!({}),
            visibility,
            weight: 0,
            status: 1,
            plugin: "core".into(),
            stage_id: "live".into(),
            created: 0,
            changed: 0,
        }
    }

    #[test]
    fn empty_visibility_always_visible() {
        let tile = make_tile(serde_json::json!({}));
        assert!(tile.is_visible("/", &[]));
        assert!(tile.is_visible("/admin", &[]));
    }

    #[test]
    fn paths_include_filter() {
        let tile = make_tile(serde_json::json!({ "paths": ["/admin/*"] }));
        assert!(tile.is_visible("/admin/people", &[]));
        assert!(!tile.is_visible("/user/login", &[]));
    }

    #[test]
    fn paths_exclude_filter() {
        let tile = make_tile(serde_json::json!({ "paths_exclude": ["/admin/*"] }));
        assert!(!tile.is_visible("/admin/people", &[]));
        assert!(tile.is_visible("/user/login", &[]));
    }

    #[test]
    fn exact_path_match() {
        let tile = make_tile(serde_json::json!({ "paths": ["/about"] }));
        assert!(tile.is_visible("/about", &[]));
        assert!(!tile.is_visible("/about/team", &[]));
    }

    #[test]
    fn role_visibility_filter() {
        let tile = make_tile(serde_json::json!({ "roles": ["administrator"] }));
        let admin_roles = vec![
            "authenticated user".to_string(),
            "administrator".to_string(),
        ];
        let user_roles = vec!["authenticated user".to_string()];
        assert!(tile.is_visible("/", &admin_roles));
        assert!(!tile.is_visible("/", &user_roles));
        assert!(!tile.is_visible("/", &[]));
    }

    #[test]
    fn role_and_path_combined() {
        let tile = make_tile(serde_json::json!({
            "roles": ["authenticated user"],
            "paths": ["/dashboard*"]
        }));
        let roles = vec!["authenticated user".to_string()];
        assert!(tile.is_visible("/dashboard", &roles));
        assert!(!tile.is_visible("/about", &roles));
        assert!(!tile.is_visible("/dashboard", &[]));
    }

    #[test]
    fn path_matches_glob() {
        assert!(path_matches("/admin/*", "/admin/people"));
        assert!(path_matches("/admin/*", "/admin/"));
        assert!(!path_matches("/admin/*", "/user/login"));
        assert!(path_matches("/about", "/about"));
        assert!(!path_matches("/about", "/about/team"));
    }
}
