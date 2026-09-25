//! Content type registry.
//!
//! Manages content type definitions collected from plugins via tap_item_info
//! and synced to the database. Uses a TTL-based Moka cache with a background
//! reload task so that external database changes (CLI config import, second
//! server instance) become visible within a bounded window.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use moka::sync::Cache;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::models::{CreateItemType, ItemType};
use crate::tap::TapDispatcher;
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition};

/// Maximum entries in the content type cache.
const MAX_CAPACITY: u64 = 500;

/// Registry of content types.
///
/// Content types are collected from plugins at startup and cached
/// in memory for fast access. A background task periodically reloads
/// all types from the database to pick up external changes.
#[derive(Clone)]
pub struct ContentTypeRegistry {
    inner: Arc<ContentTypeRegistryInner>,
}

struct ContentTypeRegistryInner {
    pool: PgPool,
    types: Cache<String, ContentTypeDefinition>,
}

/// Name the JSON shape of a value, for diagnostics.
fn json_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Resolve the title label, normalizing empty strings to None and
/// falling back to "Title" if no value is provided.
fn resolve_title_label(primary: Option<&str>, fallback: Option<&str>) -> Option<String> {
    let normalize = |s: &str| {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    primary
        .and_then(normalize)
        .or_else(|| fallback.and_then(normalize))
        .or_else(|| Some("Title".to_string()))
}

impl ContentTypeRegistry {
    /// Create a new content type registry.
    pub fn new(pool: PgPool, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(ContentTypeRegistryInner {
                pool,
                types: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
            }),
        }
    }

    /// Sync content types from plugins via tap_item_info.
    ///
    /// This calls tap_item_info on all plugins, collects the returned
    /// ContentTypeDefinitions, and upserts them into the database.
    pub async fn sync_from_plugins(&self, dispatcher: &TapDispatcher) -> Result<()> {
        use crate::tap::{RequestState, UserContext};

        info!("syncing content types from plugins");

        // Create a minimal request state for tap invocation
        let state = RequestState::without_services(UserContext::anonymous());

        // Invoke tap_item_info on all plugins
        let results = dispatcher.dispatch("tap_item_info", "{}", state).await;

        let mut synced_count = 0;

        for result in results {
            // Parse the JSON as Vec<ContentTypeDefinition>
            match serde_json::from_str::<Vec<ContentTypeDefinition>>(&result.output) {
                Ok(definitions) => {
                    for def in definitions {
                        if let Err(e) = self.register_type(&def, &result.plugin_name).await {
                            warn!(
                                plugin = %result.plugin_name,
                                type_name = %def.machine_name,
                                error = %e,
                                "failed to register content type"
                            );
                        } else {
                            synced_count += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        plugin = %result.plugin_name,
                        error = %e,
                        "failed to parse tap_item_info response"
                    );
                }
            }
        }

        // Also load types from database (including 'core' types)
        let db_types = ItemType::list(&self.inner.pool).await?;
        for db_type in db_types {
            // Convert ItemType to ContentTypeDefinition for cache
            let def = ContentTypeDefinition {
                machine_name: db_type.type_name.clone(),
                label: db_type.label.clone(),
                description: db_type.description.clone().unwrap_or_default(),
                title_label: db_type.title_label.clone(),
                fields: self.parse_fields_from_settings(&db_type.type_name, &db_type.settings),
            };
            self.inner.types.insert(db_type.type_name, def);
        }

        info!(count = synced_count, "content types synced from plugins");
        Ok(())
    }

    /// Register a content type definition from a plugin.
    async fn register_type(&self, def: &ContentTypeDefinition, plugin_name: &str) -> Result<()> {
        // Upsert to database
        let input = CreateItemType {
            type_name: def.machine_name.clone(),
            label: def.label.clone(),
            description: Some(def.description.clone()),
            has_title: Some(true),
            title_label: resolve_title_label(def.title_label.as_deref(), None),
            plugin: plugin_name.to_string(),
            // The object shape `{"fields": [...]}` is the one canonical layout
            // for this column, and the one `parse_fields_from_settings` reads.
            // Writing the bare array here is what made every plugin-declared
            // type come back from the database with zero fields.
            settings: Some(serde_json::json!({
                "fields": serde_json::to_value(&def.fields).context("serialize fields")?,
            })),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache
        self.inner
            .types
            .insert(def.machine_name.clone(), def.clone());

        info!(type_name = %def.machine_name, "registered content type");
        Ok(())
    }

    /// Parse field definitions from the canonical `{"fields": [...]}` settings
    /// object.
    ///
    /// Returning an empty `Vec` for a shape this cannot read is what let a
    /// writer/reader disagreement go unnoticed: a content type with no fields
    /// and a content type whose fields could not be parsed looked identical,
    /// in the admin form and in required-field validation alike. An
    /// unreadable shape is now logged rather than passed off as "no fields".
    fn parse_fields_from_settings(
        &self,
        type_name: &str,
        settings: &serde_json::Value,
    ) -> Vec<FieldDefinition> {
        let Some(obj) = settings.as_object() else {
            warn!(
                type_name = %type_name,
                shape = %json_shape(settings),
                "item_type.settings is not an object; reading it as having no fields"
            );
            return Vec::new();
        };

        // No "fields" key is legitimate: a type that declares none.
        let Some(raw) = obj.get("fields") else {
            return Vec::new();
        };

        match serde_json::from_value::<Vec<FieldDefinition>>(raw.clone()) {
            Ok(fields) => fields,
            Err(e) => {
                warn!(
                    type_name = %type_name,
                    error = %e,
                    "failed to parse item_type.settings fields; reading it as having no fields"
                );
                Vec::new()
            }
        }
    }

    /// Reload all content types from the database into cache.
    ///
    /// Called periodically by the background reload task to keep the
    /// cache fresh so that external database changes become visible.
    pub async fn reload_from_db(&self) -> Result<()> {
        let db_types = ItemType::list(&self.inner.pool).await?;
        for db_type in db_types {
            let def = ContentTypeDefinition {
                machine_name: db_type.type_name.clone(),
                label: db_type.label.clone(),
                description: db_type.description.clone().unwrap_or_default(),
                title_label: db_type.title_label.clone(),
                fields: self.parse_fields_from_settings(&db_type.type_name, &db_type.settings),
            };
            self.inner.types.insert(db_type.type_name, def);
        }
        Ok(())
    }

    /// Get a content type by machine name (sync, cache-only).
    pub fn get(&self, type_name: &str) -> Option<ContentTypeDefinition> {
        self.inner.types.get(type_name)
    }

    /// Get a content type by machine name with DB fallback on cache miss.
    pub async fn get_or_load(&self, type_name: &str) -> Result<Option<ContentTypeDefinition>> {
        if let Some(def) = self.inner.types.get(type_name) {
            return Ok(Some(def));
        }

        // Cache miss — load from database
        let db_type = ItemType::find_by_type(&self.inner.pool, type_name).await?;
        if let Some(db_type) = db_type {
            let def = ContentTypeDefinition {
                machine_name: db_type.type_name.clone(),
                label: db_type.label.clone(),
                description: db_type.description.clone().unwrap_or_default(),
                title_label: db_type.title_label.clone(),
                fields: self.parse_fields_from_settings(&db_type.type_name, &db_type.settings),
            };
            self.inner.types.insert(type_name.to_string(), def.clone());
            Ok(Some(def))
        } else {
            Ok(None)
        }
    }

    /// Get a content type by machine name (async version for API compatibility).
    pub async fn get_async(&self, type_name: &str) -> Option<ContentTypeDefinition> {
        self.get(type_name)
    }

    /// List all content types.
    pub fn list(&self) -> Vec<ContentTypeDefinition> {
        self.inner.types.iter().map(|(_k, v)| v).collect()
    }

    /// List all content types (async version for API compatibility).
    pub async fn list_all(&self) -> Vec<ContentTypeDefinition> {
        self.list()
    }

    /// Create a new content type.
    pub async fn create(
        &self,
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> Result<()> {
        let title_label =
            resolve_title_label(settings.get("title_label").and_then(|v| v.as_str()), None);

        let input = CreateItemType {
            type_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
            has_title: Some(true),
            title_label: title_label.clone(),
            plugin: "core".to_string(),
            settings: Some(settings.clone()),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache (parse fields from settings if present)
        let fields = self.parse_fields_from_settings(machine_name, &settings);
        let def = ContentTypeDefinition {
            machine_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.unwrap_or("").to_string(),
            title_label,
            fields,
        };
        self.inner.types.insert(machine_name.to_string(), def);

        info!(type_name = %machine_name, "content type created");
        Ok(())
    }

    /// Update an existing content type.
    pub async fn update(
        &self,
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> Result<()> {
        // Get existing type to preserve fields
        let existing = self.get(machine_name);

        let title_label = resolve_title_label(
            settings.get("title_label").and_then(|v| v.as_str()),
            existing.as_ref().and_then(|e| e.title_label.as_deref()),
        );

        let input = CreateItemType {
            type_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
            has_title: Some(true),
            title_label: title_label.clone(),
            plugin: "core".to_string(),
            settings: Some(settings.clone()),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache
        let def = ContentTypeDefinition {
            machine_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.unwrap_or("").to_string(),
            title_label,
            fields: existing.map(|e| e.fields).unwrap_or_default(),
        };
        self.inner.types.insert(machine_name.to_string(), def);

        info!(type_name = %machine_name, "content type updated");
        Ok(())
    }

    /// Add a field to a content type.
    pub async fn add_field(
        &self,
        type_name: &str,
        field_name: &str,
        field_label: &str,
        field_type: &str,
    ) -> Result<()> {
        use trovato_sdk::types::FieldType;

        let mut def = self
            .get_or_load(type_name)
            .await?
            .context("content type not found")?;

        // Parse field type
        let ft = match field_type {
            "text" => FieldType::Text { max_length: None },
            "text_long" => FieldType::TextLong,
            "integer" => FieldType::Integer,
            "float" => FieldType::Float,
            "boolean" => FieldType::Boolean,
            "date" => FieldType::Date,
            "email" => FieldType::Email,
            "record_reference" => FieldType::RecordReference(String::new()),
            "compound" => FieldType::Compound {
                allowed_types: vec![],
                min_items: None,
                max_items: None,
            },
            "blocks" => FieldType::Blocks,
            _ => FieldType::Text { max_length: None },
        };

        // Create field definition
        let field = FieldDefinition {
            field_name: field_name.to_string(),
            field_type: ft,
            label: field_label.to_string(),
            required: false,
            cardinality: 1,
            settings: serde_json::Value::Object(serde_json::Map::new()),
            personal_data: false,
        };

        // Add to existing fields
        def.fields.push(field);

        // Update database
        let settings = serde_json::json!({
            "fields": def.fields,
        });

        sqlx::query("UPDATE item_type SET settings = $1 WHERE type = $2")
            .bind(&settings)
            .bind(type_name)
            .execute(&self.inner.pool)
            .await
            .context("failed to update item_type")?;

        // Update cache
        self.inner.types.insert(type_name.to_string(), def);

        info!(type_name = %type_name, field = %field_name, "field added");
        Ok(())
    }

    /// Persist the field list for a content type, merging into existing
    /// settings so that non-field keys (`title_label`, `published_default`,
    /// etc.) are preserved.
    async fn persist_fields(&self, type_name: &str, def: &ContentTypeDefinition) -> Result<()> {
        // Load current settings from DB to preserve non-field keys
        let current: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = $1")
                .bind(type_name)
                .fetch_optional(&self.inner.pool)
                .await
                .context("failed to read item_type settings")?;

        // Anything that is not an object cannot carry a "fields" key, and
        // silently leaving it alone is how a field edit used to report success
        // while writing nothing. Replace it with the canonical object shape,
        // keeping any other settings keys an object already holds.
        let mut obj = match current {
            Some(serde_json::Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        obj.insert(
            "fields".to_string(),
            serde_json::to_value(&def.fields).context("serialize fields")?,
        );
        let settings = serde_json::Value::Object(obj);

        sqlx::query("UPDATE item_type SET settings = $1 WHERE type = $2")
            .bind(&settings)
            .bind(type_name)
            .execute(&self.inner.pool)
            .await
            .context("failed to update item_type")?;

        self.inner.types.insert(type_name.to_string(), def.clone());
        Ok(())
    }

    /// Update properties of an existing field (label, required, cardinality).
    pub async fn update_field(
        &self,
        type_name: &str,
        field_name: &str,
        label: &str,
        required: bool,
        cardinality: i32,
    ) -> Result<()> {
        let mut def = self
            .get_or_load(type_name)
            .await?
            .context("content type not found")?;

        let field = def
            .fields
            .iter_mut()
            .find(|f| f.field_name == field_name)
            .context("field not found")?;

        field.label = label.to_string();
        field.required = required;
        field.cardinality = cardinality;

        self.persist_fields(type_name, &def).await?;
        info!(type_name = %type_name, field = %field_name, "field updated");
        Ok(())
    }

    /// Remove a field from a content type.
    pub async fn delete_field(&self, type_name: &str, field_name: &str) -> Result<()> {
        let mut def = self
            .get_or_load(type_name)
            .await?
            .context("content type not found")?;

        let before = def.fields.len();
        def.fields.retain(|f| f.field_name != field_name);
        if def.fields.len() == before {
            anyhow::bail!("field '{field_name}' not found on type '{type_name}'");
        }

        self.persist_fields(type_name, &def).await?;
        info!(type_name = %type_name, field = %field_name, "field deleted");
        Ok(())
    }

    /// List content type names.
    pub fn type_names(&self) -> Vec<String> {
        self.inner
            .types
            .iter()
            .map(|(k, _v)| (*k).clone())
            .collect()
    }

    /// Check if a content type exists.
    pub fn exists(&self, type_name: &str) -> bool {
        self.inner.types.get(type_name).is_some()
    }

    /// Get the number of registered content types.
    pub fn len(&self) -> usize {
        self.inner.types.entry_count() as usize
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.types.entry_count() == 0
    }

    /// Invalidate cached content type.
    pub fn invalidate(&self, type_name: &str) {
        self.inner.types.invalidate(type_name);
    }

    /// Clear all cached content types.
    pub fn clear(&self) {
        self.inner.types.invalidate_all();
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    #[test]
    fn content_type_registry_placeholder() {
        // Full tests require database connection.
        // See tests/item_test.rs for ContentTypeDefinition tests.
    }
}
