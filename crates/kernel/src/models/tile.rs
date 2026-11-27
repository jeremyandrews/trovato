//! Tile (block) model for placing content in page regions.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::stage::LIVE_STAGE_ID;

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
    /// Stage UUID referencing category_tag(id) in the "stages" category.
    pub stage_id: Uuid,
    #[serde(default = "crate::models::unix_now")]
    pub created: i64,
    #[serde(default = "crate::models::unix_now")]
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
    pub stage_id: Option<Uuid>,
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
    pub stage_id: Option<Uuid>,
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
        let stage_id = input.stage_id.unwrap_or(LIVE_STAGE_ID);

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
        .bind(stage_id)
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

    /// Find a tile by machine name.
    pub async fn find_by_machine_name(pool: &PgPool, machine_name: &str) -> Result<Option<Self>> {
        let tile = sqlx::query_as::<_, Tile>("SELECT * FROM tile WHERE machine_name = $1")
            .bind(machine_name)
            .fetch_optional(pool)
            .await
            .context("failed to fetch tile by machine_name")?;

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
    pub async fn list_by_region(pool: &PgPool, region: &str, stage_id: Uuid) -> Result<Vec<Self>> {
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

    /// Check if this tile is visible for a given request path, user roles,
    /// signed-in state and the type of the item the page shows, if any.
    ///
    /// Visibility rules in JSON:
    /// - `{ "paths": ["/admin/*", "/user/*"] }` — show only on matching paths
    /// - `{ "paths_exclude": ["/admin/*"] }` — hide on matching paths
    /// - `{ "roles": ["authenticated user", "administrator"] }` — show only to users with one of these roles
    /// - `{ "audience": "anonymous" }` — show only to signed-out visitors (the
    ///   login tile's case); `{ "audience": "authenticated" }` — show only to
    ///   signed-in users. Decided from the viewer's actual signed-in state,
    ///   not from `user_roles` — every viewer, signed in or not, carries the
    ///   `anonymous user` role there, so a role rule alone cannot mean
    ///   "anonymous only".
    /// - `{ "item_types": ["article", "recipe"] }` — show only on a page
    ///   showing an item of one of these types (B186). A page showing no item
    ///   — a gather listing, the front page, an admin screen — never matches:
    ///   `item_type` is `None` there, not a type the rule could name. An empty
    ///   list is no rule, as the tile form's blank field is.
    /// - `{}` — always visible
    pub fn is_visible(
        &self,
        path: &str,
        user_roles: &[String],
        authenticated: bool,
        item_type: Option<&str>,
    ) -> bool {
        // Audience check
        if let Some(audience) = self.visibility.get("audience").and_then(|v| v.as_str()) {
            match audience {
                "anonymous" if authenticated => return false,
                "authenticated" if !authenticated => return false,
                _ => {}
            }
        }

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

        // Item type check (B186). An empty list is no rule, the way the tile
        // form shows and saves it ("blank for every page"): otherwise a tile
        // imported with `[]` would be hidden everywhere until an unrelated
        // edit saved the blank field and made it visible everywhere.
        if let Some(item_types) = self
            .visibility
            .get("item_types")
            .and_then(|v| v.as_array())
            .filter(|types| !types.is_empty())
        {
            let type_match =
                item_type.is_some_and(|t| item_types.iter().any(|v| v.as_str() == Some(t)));
            if !type_match {
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

/// The rows a `gather_query` tile shows when its config names no count.
pub const GATHER_TILE_DEFAULT_ITEMS: u32 = 5;

/// The counts a `gather_query` tile's `items` may take.
const GATHER_TILE_ITEMS: std::ops::RangeInclusive<u32> = 1..=50;

/// Why an item count was refused, worded for whoever set it.
const GATHER_TILE_ITEMS_ERROR: &str =
    "The number of items must be a whole number between 1 and 50.";

/// The number of rows a `gather_query` tile shows: its config's `items`, or
/// [`GATHER_TILE_DEFAULT_ITEMS`] when the config has none.
///
/// # Errors
///
/// A message saying why, when `items` is not a whole number from 1 to 50.
pub fn gather_tile_items(config: &serde_json::Value) -> std::result::Result<u32, &'static str> {
    match config.get("items") {
        None | Some(serde_json::Value::Null) => Ok(GATHER_TILE_DEFAULT_ITEMS),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| GATHER_TILE_ITEMS.contains(n))
            .ok_or(GATHER_TILE_ITEMS_ERROR),
    }
}

/// The item count typed into the tile form; left blank, the default.
///
/// # Errors
///
/// A message saying why, when the text is not a whole number from 1 to 50.
pub fn parse_gather_tile_items(typed: &str) -> std::result::Result<u32, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(GATHER_TILE_DEFAULT_ITEMS);
    }
    typed
        .parse::<u32>()
        .ok()
        .filter(|n| GATHER_TILE_ITEMS.contains(n))
        .ok_or(GATHER_TILE_ITEMS_ERROR)
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
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
            stage_id: LIVE_STAGE_ID,
            created: 0,
            changed: 0,
        }
    }

    #[test]
    fn empty_visibility_always_visible() {
        let tile = make_tile(serde_json::json!({}));
        assert!(tile.is_visible("/", &[], false, None));
        assert!(tile.is_visible("/admin", &[], false, None));
        assert!(tile.is_visible("/", &[], true, None));
        assert!(tile.is_visible("/admin", &[], true, None));
    }

    #[test]
    fn paths_include_filter() {
        let tile = make_tile(serde_json::json!({ "paths": ["/admin/*"] }));
        assert!(tile.is_visible("/admin/people", &[], false, None));
        assert!(!tile.is_visible("/user/login", &[], false, None));
        assert!(tile.is_visible("/admin/people", &[], true, None));
        assert!(!tile.is_visible("/user/login", &[], true, None));
    }

    #[test]
    fn paths_exclude_filter() {
        let tile = make_tile(serde_json::json!({ "paths_exclude": ["/admin/*"] }));
        assert!(!tile.is_visible("/admin/people", &[], false, None));
        assert!(tile.is_visible("/user/login", &[], false, None));
        assert!(!tile.is_visible("/admin/people", &[], true, None));
        assert!(tile.is_visible("/user/login", &[], true, None));
    }

    #[test]
    fn exact_path_match() {
        let tile = make_tile(serde_json::json!({ "paths": ["/about"] }));
        assert!(tile.is_visible("/about", &[], false, None));
        assert!(!tile.is_visible("/about/team", &[], false, None));
    }

    #[test]
    fn role_visibility_filter() {
        let tile = make_tile(serde_json::json!({ "roles": ["administrator"] }));
        let admin_roles = vec![
            "authenticated user".to_string(),
            "administrator".to_string(),
        ];
        let user_roles = vec!["authenticated user".to_string()];
        assert!(tile.is_visible("/", &admin_roles, true, None));
        assert!(!tile.is_visible("/", &user_roles, true, None));
        assert!(!tile.is_visible("/", &[], false, None));
    }

    #[test]
    fn role_and_path_combined() {
        let tile = make_tile(serde_json::json!({
            "roles": ["authenticated user"],
            "paths": ["/dashboard*"]
        }));
        let roles = vec!["authenticated user".to_string()];
        assert!(tile.is_visible("/dashboard", &roles, true, None));
        assert!(!tile.is_visible("/about", &roles, true, None));
        assert!(!tile.is_visible("/dashboard", &[], false, None));
    }

    #[test]
    fn audience_anonymous_shows_only_to_signed_out_visitors() {
        let tile = make_tile(serde_json::json!({ "audience": "anonymous" }));
        assert!(tile.is_visible("/", &[], false, None));
        assert!(!tile.is_visible("/", &["anonymous user".to_string()], true, None));
    }

    #[test]
    fn audience_authenticated_shows_only_to_signed_in_users() {
        let tile = make_tile(serde_json::json!({ "audience": "authenticated" }));
        assert!(!tile.is_visible("/", &[], false, None));
        assert!(tile.is_visible("/", &["authenticated user".to_string()], true, None));
    }

    #[test]
    fn audience_ignores_the_anonymous_user_role_every_viewer_carries() {
        // Every viewer, signed in or not, can carry "anonymous user" in
        // `user_roles` (see routes::helpers::inject_site_context). `audience`
        // is decided from the `authenticated` flag, not that list, so a
        // signed-in viewer who still carries the role is still excluded.
        let tile = make_tile(serde_json::json!({ "audience": "anonymous" }));
        assert!(!tile.is_visible("/", &["anonymous user".to_string()], true, None));
    }

    #[test]
    fn audience_combines_with_paths_and_roles() {
        let tile = make_tile(serde_json::json!({
            "audience": "anonymous",
            "paths": ["/dashboard*"]
        }));
        assert!(tile.is_visible("/dashboard", &[], false, None));
        assert!(!tile.is_visible("/dashboard", &[], true, None));
        assert!(!tile.is_visible("/about", &[], false, None));
    }

    #[test]
    fn unknown_audience_value_is_ignored() {
        let tile = make_tile(serde_json::json!({ "audience": "everyone" }));
        assert!(tile.is_visible("/", &[], false, None));
        assert!(tile.is_visible("/", &[], true, None));
    }

    #[test]
    fn item_types_visibility_shows_only_on_matching_item_pages() {
        let tile = make_tile(serde_json::json!({ "item_types": ["article"] }));
        assert!(tile.is_visible("/item/1", &[], false, Some("article")));
        assert!(!tile.is_visible("/item/2", &[], false, Some("recipe")));
        // A page with no item in view never matches: there is no type to
        // compare the rule against.
        assert!(!tile.is_visible("/", &[], false, None));
    }

    #[test]
    fn empty_item_types_is_no_rule() {
        // The tile form shows `[]` as a blank field and saves a blank field
        // as no rule; the two must mean the same, or saving an unrelated
        // edit flips a tile from hidden everywhere to shown everywhere.
        let tile = make_tile(serde_json::json!({ "item_types": [] }));
        assert!(tile.is_visible("/", &[], false, None));
        assert!(tile.is_visible("/item/1", &[], false, Some("article")));
        let blank = make_tile(serde_json::json!({}));
        for (path, item_type) in [("/", None), ("/item/1", Some("article"))] {
            assert_eq!(
                tile.is_visible(path, &[], false, item_type),
                blank.is_visible(path, &[], false, item_type)
            );
        }
    }

    #[test]
    fn item_types_combines_with_roles() {
        let tile = make_tile(serde_json::json!({
            "item_types": ["article"],
            "roles": ["administrator"]
        }));
        let admin_roles = vec!["administrator".to_string()];
        assert!(tile.is_visible("/item/1", &admin_roles, true, Some("article")));
        assert!(!tile.is_visible("/item/1", &[], false, Some("article")));
        assert!(!tile.is_visible("/item/1", &admin_roles, true, Some("recipe")));
    }

    #[test]
    fn path_matches_glob() {
        assert!(path_matches("/admin/*", "/admin/people"));
        assert!(path_matches("/admin/*", "/admin/"));
        assert!(!path_matches("/admin/*", "/user/login"));
        assert!(path_matches("/about", "/about"));
        assert!(!path_matches("/about", "/about/team"));
    }

    #[test]
    fn gather_tile_items_reads_the_config_or_defaults_to_five() {
        assert_eq!(gather_tile_items(&serde_json::json!({})), Ok(5));
        assert_eq!(
            gather_tile_items(&serde_json::json!({ "items": null })),
            Ok(5)
        );
        assert_eq!(gather_tile_items(&serde_json::json!({ "items": 1 })), Ok(1));
        assert_eq!(
            gather_tile_items(&serde_json::json!({ "items": 50 })),
            Ok(50)
        );
        for bad in [
            serde_json::json!(0),
            serde_json::json!(51),
            serde_json::json!(-1),
            serde_json::json!(2.5),
            serde_json::json!("7"),
        ] {
            assert!(gather_tile_items(&serde_json::json!({ "items": bad })).is_err());
        }
    }

    #[test]
    fn parse_gather_tile_items_takes_blank_as_the_default() {
        assert_eq!(parse_gather_tile_items(""), Ok(5));
        assert_eq!(parse_gather_tile_items(" 12 "), Ok(12));
        for bad in ["0", "51", "-3", "five", "2.5"] {
            assert!(parse_gather_tile_items(bad).is_err(), "{bad}");
        }
    }
}
