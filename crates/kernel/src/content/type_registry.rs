//! Content type registry.
//!
//! Manages content type definitions collected from plugins via tap_item_info
//! and synced to the database.

use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::models::{CreateItemType, ItemType};
use crate::tap::TapDispatcher;
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition};

/// Registry of content types.
///
/// Content types are collected from plugins at startup and cached
/// in memory for fast access.
#[derive(Clone)]
pub struct ContentTypeRegistry {
    inner: Arc<ContentTypeRegistryInner>,
}

struct ContentTypeRegistryInner {
    pool: PgPool,
    types: DashMap<String, ContentTypeDefinition>,
}

impl ContentTypeRegistry {
    /// Create a new content type registry.
    pub fn new(pool: PgPool) -> Self {
        Self {
            inner: Arc::new(ContentTypeRegistryInner {
                pool,
                types: DashMap::new(),
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
                        if let Err(e) = self.register_type(&def).await {
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
                fields: self.parse_fields_from_settings(&db_type.settings),
            };
            self.inner.types.insert(db_type.type_name, def);
        }

        info!(count = synced_count, "content types synced from plugins");
        Ok(())
    }

    /// Register a content type definition.
    async fn register_type(&self, def: &ContentTypeDefinition) -> Result<()> {
        // Upsert to database
        let input = CreateItemType {
            type_name: def.machine_name.clone(),
            label: def.label.clone(),
            description: Some(def.description.clone()),
            has_title: Some(true),
            title_label: Some("Title".to_string()),
            plugin: "plugin".to_string(), // TODO: Get actual plugin name
            settings: Some(serde_json::to_value(&def.fields).context("serialize fields")?),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache
        self.inner
            .types
            .insert(def.machine_name.clone(), def.clone());

        info!(type_name = %def.machine_name, "registered content type");
        Ok(())
    }

    /// Parse field definitions from ItemType settings JSON.
    fn parse_fields_from_settings(&self, settings: &serde_json::Value) -> Vec<FieldDefinition> {
        settings
            .get("fields")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Get a content type by machine name.
    pub fn get(&self, type_name: &str) -> Option<ContentTypeDefinition> {
        self.inner.types.get(type_name).map(|r| r.clone())
    }

    /// Get a content type by machine name (async version for API compatibility).
    pub async fn get_async(&self, type_name: &str) -> Option<ContentTypeDefinition> {
        self.get(type_name)
    }

    /// List all content types.
    pub fn list(&self) -> Vec<ContentTypeDefinition> {
        self.inner.types.iter().map(|r| r.value().clone()).collect()
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
        let input = CreateItemType {
            type_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
            has_title: Some(true),
            title_label: Some("Title".to_string()),
            plugin: "core".to_string(),
            settings: Some(settings.clone()),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache
        let def = ContentTypeDefinition {
            machine_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.unwrap_or("").to_string(),
            fields: vec![],
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

        let input = CreateItemType {
            type_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
            has_title: Some(true),
            title_label: settings
                .get("title_label")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or(Some("Title".to_string())),
            plugin: "core".to_string(),
            settings: Some(settings.clone()),
        };

        ItemType::upsert(&self.inner.pool, input).await?;

        // Update cache
        let def = ContentTypeDefinition {
            machine_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.unwrap_or("").to_string(),
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

        let mut def = self.get(type_name).context("content type not found")?;

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

    /// List content type names.
    pub fn type_names(&self) -> Vec<String> {
        self.inner.types.iter().map(|r| r.key().clone()).collect()
    }

    /// Check if a content type exists.
    pub fn exists(&self, type_name: &str) -> bool {
        self.inner.types.contains_key(type_name)
    }

    /// Get the number of registered content types.
    pub fn len(&self) -> usize {
        self.inner.types.len()
    }

    /// Check if registry is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.types.is_empty()
    }

    /// Invalidate cached content type.
    pub fn invalidate(&self, type_name: &str) {
        self.inner.types.remove(type_name);
    }

    /// Clear all cached content types.
    pub fn clear(&self) {
        self.inner.types.clear();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn content_type_registry_placeholder() {
        // Full tests require database connection
        // See tests/item_test.rs for ContentTypeDefinition tests
        assert!(true);
    }
}
