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
use crate::gather::types::{GatherQuery, QueryDefinition, QueryDisplay};
use crate::models::stage::LIVE_STAGE_ID;
use crate::models::{
    Category, CreateCategory, CreateLanguage, ItemType, Language, Tag, UpdateCategory, UpdateTag,
    UrlAlias,
};

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
            .map_err(|e| anyhow::anyhow!("invalid search field config ID '{id}': {e}"))?;

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
            .map_err(|e| anyhow::anyhow!("invalid search field config ID '{id}': {e}"))?;

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

    // ---- Language helpers ----

    async fn load_language(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let lang = Language::find_by_id(&self.pool, id).await?;
        Ok(lang.map(ConfigEntity::Language))
    }

    async fn save_language(&self, lang: &Language) -> Result<()> {
        let input = CreateLanguage {
            id: lang.id.clone(),
            label: lang.label.clone(),
            weight: Some(lang.weight),
            is_default: Some(lang.is_default),
            direction: Some(lang.direction.clone()),
        };

        Language::upsert(&self.pool, input).await?;
        Ok(())
    }

    async fn delete_language(&self, id: &str) -> Result<bool> {
        Language::delete(&self.pool, id).await
    }

    async fn list_languages(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let langs = Language::list_all(&self.pool).await?;
        let mut entities: Vec<ConfigEntity> =
            langs.into_iter().map(ConfigEntity::Language).collect();

        // Apply field/value filtering (e.g., field="is_default", value="true")
        if let Some(f) = filter {
            if let (Some(field), Some(value)) = (f.field.as_deref(), f.value.as_deref()) {
                let is_known_field = matches!(field, "is_default" | "direction" | "id");
                if !is_known_field {
                    tracing::warn!(
                        field = %field,
                        value = %value,
                        "list_languages: unknown filter field, returning all results"
                    );
                }
                entities.retain(|e| {
                    if let ConfigEntity::Language(lang) = e {
                        match field {
                            "is_default" => lang.is_default.to_string() == value,
                            "direction" => lang.direction == value,
                            "id" => lang.id == value,
                            _ => true, // unknown field: don't filter (warned above)
                        }
                    } else {
                        true
                    }
                });
            }

            if let Some(offset) = f.offset {
                entities = entities.into_iter().skip(offset).collect();
            }
            if let Some(limit) = f.limit {
                entities.truncate(limit);
            }
        }

        Ok(entities)
    }

    // ---- GatherQuery helpers ----

    async fn load_gather_query(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let row = sqlx::query_as::<_, GatherQueryRow>(
            "SELECT query_id, label, description, definition, display, plugin, created, changed \
             FROM gather_query WHERE query_id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch gather query")?;

        row.map(|r| r.into_config_entity()).transpose()
    }

    async fn save_gather_query(&self, query: &GatherQuery) -> Result<()> {
        let now = Utc::now().timestamp();
        let definition_json =
            serde_json::to_value(&query.definition).context("failed to serialize definition")?;
        let display_json =
            serde_json::to_value(&query.display).context("failed to serialize display")?;

        sqlx::query(
            r#"
            INSERT INTO gather_query (query_id, label, description, definition, display, plugin, created, changed)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (query_id) DO UPDATE SET
                label = EXCLUDED.label,
                description = EXCLUDED.description,
                definition = EXCLUDED.definition,
                display = EXCLUDED.display,
                plugin = EXCLUDED.plugin,
                changed = EXCLUDED.changed
            "#,
        )
        .bind(&query.query_id)
        .bind(&query.label)
        .bind(&query.description)
        .bind(&definition_json)
        .bind(&display_json)
        .bind(&query.plugin)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to save gather query")?;

        Ok(())
    }

    async fn delete_gather_query(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM gather_query WHERE query_id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("failed to delete gather query")?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_gather_queries(
        &self,
        filter: Option<&ConfigFilter>,
    ) -> Result<Vec<ConfigEntity>> {
        let rows = if let Some(f) = filter
            && f.field.as_deref() == Some("plugin")
            && let Some(ref plugin) = f.value
        {
            sqlx::query_as::<_, GatherQueryRow>(
                "SELECT query_id, label, description, definition, display, plugin, created, changed \
                 FROM gather_query WHERE plugin = $1 ORDER BY query_id",
            )
            .bind(plugin)
            .fetch_all(&self.pool)
            .await
            .context("failed to list gather queries by plugin")?
        } else {
            sqlx::query_as::<_, GatherQueryRow>(
                "SELECT query_id, label, description, definition, display, plugin, created, changed \
                 FROM gather_query ORDER BY query_id",
            )
            .fetch_all(&self.pool)
            .await
            .context("failed to list all gather queries")?
        };

        let mut entities: Vec<ConfigEntity> = rows
            .into_iter()
            .filter_map(|r| r.into_config_entity().ok())
            .collect();

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

    // ---- UrlAlias helpers ----

    async fn load_url_alias(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id
            .parse::<Uuid>()
            .map_err(|e| anyhow::anyhow!("invalid url_alias ID '{id}': {e}"))?;

        let alias = sqlx::query_as::<_, UrlAlias>(
            "SELECT id, source, alias, language, stage_id, created \
             FROM url_alias WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await
        .context("failed to fetch url alias")?;

        Ok(alias.map(ConfigEntity::UrlAlias))
    }

    async fn save_url_alias(&self, alias: &UrlAlias) -> Result<()> {
        let now = Utc::now().timestamp();

        sqlx::query(
            r#"
            INSERT INTO url_alias (id, source, alias, language, stage_id, created)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (alias, language, stage_id) DO UPDATE SET
                source = EXCLUDED.source
            "#,
        )
        .bind(alias.id)
        .bind(&alias.source)
        .bind(&alias.alias)
        .bind(&alias.language)
        .bind(alias.stage_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("failed to save url alias")?;

        Ok(())
    }

    async fn delete_url_alias(&self, id: &str) -> Result<bool> {
        let uuid = id
            .parse::<Uuid>()
            .map_err(|e| anyhow::anyhow!("invalid url_alias ID '{id}': {e}"))?;

        let result = sqlx::query("DELETE FROM url_alias WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete url alias")?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_url_aliases(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let aliases = sqlx::query_as::<_, UrlAlias>(
            "SELECT id, source, alias, language, stage_id, created \
             FROM url_alias WHERE stage_id = $1 ORDER BY alias",
        )
        .bind(LIVE_STAGE_ID)
        .fetch_all(&self.pool)
        .await
        .context("failed to list url aliases")?;

        let mut entities: Vec<ConfigEntity> =
            aliases.into_iter().map(ConfigEntity::UrlAlias).collect();

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
            entity_types::LANGUAGE => self.load_language(id).await,
            entity_types::GATHER_QUERY => self.load_gather_query(id).await,
            entity_types::URL_ALIAS => self.load_url_alias(id).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {entity_type}")),
        }
    }

    async fn save(&self, entity: &ConfigEntity) -> Result<()> {
        match entity {
            ConfigEntity::ItemType(t) => self.save_item_type(t).await,
            ConfigEntity::SearchFieldConfig(f) => self.save_search_field_config(f).await,
            ConfigEntity::Category(c) => self.save_category(c).await,
            ConfigEntity::Tag(t) => self.save_tag(t).await,
            ConfigEntity::Variable { key, value } => self.save_variable(key, value).await,
            ConfigEntity::Language(l) => self.save_language(l).await,
            ConfigEntity::GatherQuery(q) => self.save_gather_query(q).await,
            ConfigEntity::UrlAlias(a) => self.save_url_alias(a).await,
        }
    }

    async fn delete(&self, entity_type: &str, id: &str) -> Result<bool> {
        match entity_type {
            entity_types::ITEM_TYPE => self.delete_item_type(id).await,
            entity_types::SEARCH_FIELD_CONFIG => self.delete_search_field_config(id).await,
            entity_types::CATEGORY => self.delete_category(id).await,
            entity_types::TAG => self.delete_tag(id).await,
            entity_types::VARIABLE => self.delete_variable(id).await,
            entity_types::LANGUAGE => self.delete_language(id).await,
            entity_types::GATHER_QUERY => self.delete_gather_query(id).await,
            entity_types::URL_ALIAS => self.delete_url_alias(id).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {entity_type}")),
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
            entity_types::LANGUAGE => self.list_languages(filter).await,
            entity_types::GATHER_QUERY => self.list_gather_queries(filter).await,
            entity_types::URL_ALIAS => self.list_url_aliases(filter).await,
            _ => Err(anyhow::anyhow!("unknown entity type: {entity_type}")),
        }
    }
}

/// Row type for variable queries.
#[derive(sqlx::FromRow)]
struct VariableRow {
    key: String,
    value: serde_json::Value,
}

/// Row type for gather_query queries.
#[derive(sqlx::FromRow)]
struct GatherQueryRow {
    query_id: String,
    label: String,
    description: Option<String>,
    definition: serde_json::Value,
    display: serde_json::Value,
    plugin: String,
    created: i64,
    changed: i64,
}

impl GatherQueryRow {
    /// Convert a database row into a [`ConfigEntity::GatherQuery`].
    fn into_config_entity(self) -> Result<ConfigEntity> {
        let definition: QueryDefinition = serde_json::from_value(self.definition).context(
            format!("failed to parse definition for '{}'", self.query_id),
        )?;
        let display: QueryDisplay = serde_json::from_value(self.display)
            .context(format!("failed to parse display for '{}'", self.query_id))?;

        Ok(ConfigEntity::GatherQuery(Box::new(GatherQuery {
            query_id: self.query_id,
            label: self.label,
            description: self.description,
            definition,
            display,
            plugin: self.plugin,
            created: self.created,
            changed: self.changed,
        })))
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
