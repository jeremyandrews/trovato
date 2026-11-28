//! Tiles, with the ids and dates the file gives.
//!
//! Checked as the tile form checks one: a machine name of the form's shape
//! and free on the live stage, a type the kernel renders, a custom HTML body
//! in a text format the site has (as for an administrator), and a visibility
//! whose `audience` is one the form offers. Tiles are on the live stage,
//! where the form makes them.

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

use super::record::TileRecord;
use super::{Outcome, Run, require_flag, require_id, require_order};
use crate::models::stage::LIVE_STAGE_ID;
use crate::models::tile::Tile;
use crate::routes::helpers::is_valid_machine_name;
use crate::routes::tile_admin::AUDIENCE_VALUES;

/// The tile types `services::tile` renders.
const TILE_TYPES: &[&str] = &[
    "custom_html",
    "menu",
    "gather_query",
    "language_switcher",
    "chat",
    "user_login",
];

/// A stored tile, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredTile {
    machine_name: String,
    label: String,
    region: String,
    tile_type: String,
    config: Value,
    visibility: Value,
    weight: i32,
    status: i32,
    created: i64,
    changed: i64,
}

/// Import one `tile` record.
pub(crate) async fn import_tile(run: &mut Run<'_>, r: TileRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    require_order(r.created, r.changed)?;
    if !is_valid_machine_name(&r.machine_name) {
        anyhow::bail!(
            "machine_name '{}' must start with a lowercase letter and hold only lowercase \
             letters, digits and underscores",
            r.machine_name
        );
    }
    if r.label.trim().is_empty() || r.region.trim().is_empty() {
        anyhow::bail!("label and region are required");
    }
    let tile_type = r
        .tile_type
        .clone()
        .unwrap_or_else(|| "custom_html".to_string());
    if !TILE_TYPES.contains(&tile_type.as_str()) {
        anyhow::bail!(
            "tile_type '{tile_type}' is not one of {}",
            TILE_TYPES.join(", ")
        );
    }
    let config = r
        .config
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !config.is_object() {
        anyhow::bail!("config must be a JSON object");
    }
    if tile_type == "custom_html" {
        if config.get("body").is_some_and(|b| !b.is_string()) {
            anyhow::bail!("config.body must be a string");
        }
        let format = config
            .get("format")
            .and_then(Value::as_str)
            .unwrap_or("filtered_html");
        if !run.formats.iter().any(|f| f == format) {
            anyhow::bail!("text format '{format}' is not one the site has");
        }
    }
    let visibility = r
        .visibility
        .clone()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !visibility.is_object() {
        anyhow::bail!("visibility must be a JSON object");
    }
    if let Some(audience) = visibility.get("audience")
        && !audience
            .as_str()
            .is_some_and(|a| AUDIENCE_VALUES.contains(&a))
    {
        anyhow::bail!(
            "visibility.audience must be one of {}",
            AUDIENCE_VALUES.join(", ")
        );
    }
    let status = r.status.unwrap_or(1);
    require_flag(status, "status")?;

    let wanted = StoredTile {
        machine_name: r.machine_name.clone(),
        label: r.label.clone(),
        region: r.region.clone(),
        tile_type,
        config,
        visibility,
        weight: r.weight.unwrap_or(0),
        status: i32::from(status),
        created: r.created,
        changed: r.changed,
    };
    let stored: Option<StoredTile> = sqlx::query_as(
        "SELECT machine_name, label, region, tile_type, config, visibility, weight, status, \
         created, changed FROM tile WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the tile")?;
    if stored.as_ref() == Some(&wanted) {
        return Ok(Outcome::Unchanged);
    }
    let holder: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tile WHERE machine_name = $1 AND stage_id = $2 AND id <> $3",
    )
    .bind(&wanted.machine_name)
    .bind(LIVE_STAGE_ID)
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the machine name")?;
    if let Some(holder) = holder {
        anyhow::bail!(
            "machine_name '{}' is already tile {holder}",
            wanted.machine_name
        );
    }

    if stored.is_some() {
        sqlx::query(
            "UPDATE tile SET machine_name = $2, label = $3, region = $4, tile_type = $5, \
             config = $6, visibility = $7, weight = $8, status = $9, created = $10, \
             changed = $11 WHERE id = $1",
        )
        .bind(r.id)
        .bind(&wanted.machine_name)
        .bind(&wanted.label)
        .bind(&wanted.region)
        .bind(&wanted.tile_type)
        .bind(&wanted.config)
        .bind(&wanted.visibility)
        .bind(wanted.weight)
        .bind(wanted.status)
        .bind(wanted.created)
        .bind(wanted.changed)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the tile")?;
        return Ok(Outcome::Updated);
    }
    sqlx::query(
        "INSERT INTO tile (id, machine_name, label, region, tile_type, config, visibility, \
         weight, status, plugin, stage_id, created, changed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'core', $10, $11, $12)",
    )
    .bind(r.id)
    .bind(&wanted.machine_name)
    .bind(&wanted.label)
    .bind(&wanted.region)
    .bind(&wanted.tile_type)
    .bind(&wanted.config)
    .bind(&wanted.visibility)
    .bind(wanted.weight)
    .bind(wanted.status)
    .bind(LIVE_STAGE_ID)
    .bind(wanted.created)
    .bind(wanted.changed)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the tile")?;
    Ok(Outcome::Created)
}

/// Import one `delete` record naming a tile: remove it, through
/// [`Tile::delete_in`] — the same path `TileService::delete` (the admin
/// delete link) takes. An id that is not a stored tile is unchanged, so a
/// rerun is idempotent.
pub(crate) async fn delete_tile(run: &mut Run<'_>, id: Uuid) -> Result<Outcome> {
    require_id(id, "id")?;
    let stored: Option<i32> = sqlx::query_scalar("SELECT 1 FROM tile WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up the tile")?;
    if stored.is_none() {
        return Ok(Outcome::Unchanged);
    }
    Tile::delete_in(&mut run.tx, id).await?;
    Ok(Outcome::Deleted)
}
