//! Direct database implementation of ConfigStorage.
//!
//! This is the v1.0 implementation - no stage awareness, just a clean interface.
//! Post-MVP, a `StageAwareConfigStorage` decorator can wrap this to add stage context.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use super::{
    ConfigEntity, ConfigFilter, ConfigStorage, SearchFieldConfig, entity_types, parse_tag_id,
};
use crate::models::{Category, CreateCategory, ItemType, Tag, UpdateCategory, UpdateTag};

/// Direct database implementation of ConfigStorage.
///
/// This implementation executes SQL directly against the database.
/// It provides the baseline behavior that stage-aware decorators will wrap.
#[derive(Clone)]
pub struct DirectConfigStorage {
    pool: PgPool,
}

impl DirectConfigStorage {
    /// Create a new DirectConfigStorage with a database connection pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ---- ItemType helpers ----

    async fn load_item_type(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let item_type = ItemType::find_by_type(&self.pool, id).await?;
        Ok(item_type.map(ConfigEntity::ItemType))
    }

    async fn save_item_type(&self, item_type: &ItemType) -> Result<()> {
        let input = crate::models::item_type::CreateItemType {
            type_name: item_type.type_name.clone(),
            label: item_type.label.clone(),
            description: item_type.description.clone(),
            has_title: Some(item_type.has_title),
            title_label: item_type.title_label.clone(),
            plugin: item_type.plugin.clone(),
            settings: Some(item_type.settings.clone()),
        };

        ItemType::upsert(&self.pool, input).await?;
        Ok(())
    }

    async fn delete_item_type(&self, id: &str) -> Result<bool> {
        ItemType::delete(&self.pool, id).await
    }

    async fn list_item_types(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let types = if let Some(f) = filter {
            if f.field.as_deref() == Some("plugin") {
                if let Some(ref plugin) = f.value {
                    ItemType::list_by_plugin(&self.pool, plugin).await?
                } else {
                    ItemType::list(&self.pool).await?
                }
            } else {
                ItemType::list(&self.pool).await?
            }
        } else {
            ItemType::list(&self.pool).await?
        };

        let mut entities: Vec<ConfigEntity> =
            types.into_iter().map(ConfigEntity::ItemType).collect();

        // Apply limit/offset if specified
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }

    // ---- SearchFieldConfig helpers ----

    async fn load_search_field_config(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id
            .parse::<Uuid>()
            .map_err(|e| anyhow::anyhow!("invalid search field config ID '{}': {}", id, e))?;

        let config = sqlx::query_as::<_, SearchFieldConfig>(
            "SELECT id, bundle, field_name, weight FROM search_field_config WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch search field config")?;

        Ok(config.map(ConfigEntity::SearchFieldConfig))
    }

    async fn save_search_field_config(&self, config: &SearchFieldConfig) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO search_field_config (id, bundle, field_name, weight)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (id) DO UPDATE SET
                bundle = EXCLUDED.bundle,
                field_name = EXCLUDED.field_name,
                weight = EXCLUDED.weight
            "#,
        )
        .bind(config.id)
        .bind(&config.bundle)
        .bind(&config.field_name)
        .bind(&config.weight)
        .execute(&self.pool)
        .await
        .context("failed to save search field config")?;

        Ok(())
    }

    async fn delete_search_field_config(&self, id: &str) -> Result<bool> {
        let uuid = id
            .parse::<Uuid>()
            .map_err(|e| anyhow::anyhow!("invalid search field config ID '{}': {}", id, e))?;

        let result = sqlx::query("DELETE FROM search_field_config WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete search field config")?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_search_field_configs(
        &self,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>> {
        let configs = if let Some(f) = filter {
            if f.field.as_deref() == Some("bundle") {
                if let Some(ref bundle) = f.value {
                    sqlx::query_as::<_, SearchFieldConfig>(
                        "SELECT id, bundle, field_name, weight FROM search_field_config WHERE bundle = $1 ORDER BY field_name",
                    )
                    .bind(bundle)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to list search field configs by bundle")?
                } else {
                    self.fetch_all_search_field_configs().await?
                }
            } else {
                self.fetch_all_search_field_configs().await?
            }
        } else {
            self.fetch_all_search_field_configs().await?
        };

        let mut entities: Vec<ConfigEntity> = configs
            .into_iter()
            .map(ConfigEntity::SearchFieldConfig)
            .collect();

        // Apply limit/offset
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }

    async fn fetch_all_search_field_configs(&self) -> Result<Vec<SearchFieldConfig>> {
        sqlx::query_as::<_, SearchFieldConfig>(
            "SELECT id, bundle, field_name, weight FROM search_field_config ORDER BY bundle, field_name",
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list all search field configs")
    }

    // ---- Category helpers ----

    async fn load_category(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let category = Category::find_by_id(&self.pool, id).await?;
        Ok(category.map(ConfigEntity::Category))
    }

    async fn save_category(&self, category: &Category) -> Result<()> {
        // Check if exists
        let exists = Category::exists(&self.pool, &category.id).await?;

        if exists {
            Category::update(
                &self.pool,
                &category.id,
                UpdateCategory {
                    label: Some(category.label.clone()),
                    description: category.description.clone(),
                    hierarchy: Some(category.hierarchy),
                    weight: Some(category.weight),
                },
            )
            .await?;
        } else {
            Category::create(
                &self.pool,
                CreateCategory {
                    id: category.id.clone(),
                    label: category.label.clone(),
                    description: category.description.clone(),
                    hierarchy: Some(category.hierarchy),
                    weight: Some(category.weight),
                },
            )
            .await?;
        }

        Ok(())
    }

    async fn delete_category(&self, id: &str) -> Result<bool> {
        Category::delete(&self.pool, id).await
    }

    async fn list_categories(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let categories = Category::list(&self.pool).await?;
        let mut entities: Vec<ConfigEntity> =
            categories.into_iter().map(ConfigEntity::Category).collect();

        // Apply limit/offset
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }

    // ---- Tag helpers ----

    async fn load_tag(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = parse_tag_id(id)?;
        let tag = Tag::find_by_id(&self.pool, uuid).await?;
        Ok(tag.map(ConfigEntity::Tag))
    }

    async fn save_tag(&self, tag: &Tag) -> Result<()> {
        // Check if exists
        let exists = Tag::find_by_id(&self.pool, tag.id).await?.is_some();

        if exists {
            Tag::update(
                &self.pool,
                tag.id,
                UpdateTag {
                    label: Some(tag.label.clone()),
                    description: tag.description.clone(),
                    weight: Some(tag.weight),
                },
            )
            .await?;
        } else {
            // Insert directly with the provided ID to preserve it
            // (Tag::create() generates a new UUID, which breaks round-trip)
            let now = Utc::now().timestamp();

            let mut tx = self
                .pool
                .begin()
                .await
                .context("failed to start transaction")?;

            sqlx::query(
                r#"
                INSERT INTO category_tag (id, category_id, label, description, weight, created, changed)
                VALUES ($1, $2, $3, $4, $5, $6, $7)
                "#,
            )
            .bind(tag.id)
            .bind(&tag.category_id)
            .bind(&tag.label)
            .bind(&tag.description)
            .bind(tag.weight)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await
            .context("failed to insert tag")?;

            // Insert as root tag (no parent hierarchy)
            sqlx::query("INSERT INTO category_tag_hierarchy (tag_id, parent_id) VALUES ($1, NULL)")
                .bind(tag.id)
                .execute(&mut *tx)
                .await
                .context("failed to insert root hierarchy")?;

            tx.commit().await.context("failed to commit transaction")?;
        }

        Ok(())
    }

    async fn delete_tag(&self, id: &str) -> Result<bool> {
        let uuid = parse_tag_id(id)?;
        Tag::delete(&self.pool, uuid).await
    }

    async fn list_tags(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let tags = if let Some(f) = filter {
            if f.field.as_deref() == Some("category_id") {
                if let Some(ref category_id) = f.value {
                    Tag::list_by_category(&self.pool, category_id).await?
                } else {
                    self.fetch_all_tags().await?
                }
            } else {
                self.fetch_all_tags().await?
            }
        } else {
            self.fetch_all_tags().await?
        };

        let mut entities: Vec<ConfigEntity> = tags.into_iter().map(ConfigEntity::Tag).collect();

        // Apply limit/offset
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }

    async fn fetch_all_tags(&self) -> Result<Vec<Tag>> {
        sqlx::query_as::<_, Tag>(
            "SELECT id, category_id, label, description, weight, created, changed FROM category_tag ORDER BY category_id, weight, label"
        )
        .fetch_all(&self.pool)
        .await
        .context("failed to list all tags")
    }

    // ---- Variable helpers ----

    async fn load_variable(&self, key: &str) -> Result<Option<ConfigEntity>> {
        let value = sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT value FROM site_config WHERE key = $1",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch variable")?;

        Ok(value.map(|v| ConfigEntity::Variable {
            key: key.to_string(),
            value: v,
        }))
    }

    async fn save_variable(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO site_config (key, value, updated)
            VALUES ($1, $2, NOW())
            ON CONFLICT (key) DO UPDATE SET value = $2, updated = NOW()
            "#,
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .context("failed to save variable")?;

        Ok(())
    }

    async fn delete_variable(&self, key: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM site_config WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .context("failed to delete variable")?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_variables(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let rows =
            sqlx::query_as::<_, VariableRow>("SELECT key, value FROM site_config ORDER BY key")
                .fetch_all(&self.pool)
                .await
                .context("failed to list variables")?;

        let mut entities: Vec<ConfigEntity> = rows
            .into_iter()
            .map(|r| ConfigEntity::Variable {
                key: r.key,
                value: r.value,
            })
            .collect();

        // Apply limit/offset
        if let Some(f) = filter {
            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }
}

#[async_trait]
impl ConfigStorage for DirectConfigStorage {
    async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>> {
        match entity_type {
            entity_types::ITEM_TYPE => self.load_item_type(id).await,
            entity_types::SEARCH_FIELD_CONFIG => self.load_search_field_config(id).await,
            entity_types::CATEGORY => self.load_category(id).await,
            entity_types::TAG => self.load_tag(id).await,
            entity_types::VARIABLE => self.load_variable(id).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {}", entity_type)),
        }
    }

    async fn save(&self, entity: &ConfigEntity) -> Result<()> {
        match entity {
            ConfigEntity::ItemType(t) => self.save_item_type(t).await,
            ConfigEntity::SearchFieldConfig(f) => self.save_search_field_config(f).await,
            ConfigEntity::Category(c) => self.save_category(c).await,
            ConfigEntity::Tag(t) => self.save_tag(t).await,
            ConfigEntity::Variable { key, value } => self.save_variable(key, value).await,
        }
    }

    async fn delete(&self, entity_type: &str, id: &str) -> Result<bool> {
        match entity_type {
            entity_types::ITEM_TYPE => self.delete_item_type(id).await,
            entity_types::SEARCH_FIELD_CONFIG => self.delete_search_field_config(id).await,
            entity_types::CATEGORY => self.delete_category(id).await,
            entity_types::TAG => self.delete_tag(id).await,
            entity_types::VARIABLE => self.delete_variable(id).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {}", entity_type)),
        }
    }

    async fn list(
        &self,
        entity_type: &str,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>> {
        match entity_type {
            entity_types::ITEM_TYPE => self.list_item_types(filter).await,
            entity_types::SEARCH_FIELD_CONFIG => self.list_search_field_configs(filter).await,
            entity_types::CATEGORY => self.list_categories(filter).await,
            entity_types::TAG => self.list_tags(filter).await,
            entity_types::VARIABLE => self.list_variables(filter).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {}", entity_type)),
        }
    }
}

/// Row type for variable queries.
#[derive(sqlx::FromRow)]
struct VariableRow {
    key: String,
    value: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_config_storage_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        // This test ensures DirectConfigStorage implements Send + Sync
        // which is required for async operations
        assert_send_sync::<DirectConfigStorage>();
    }
}
