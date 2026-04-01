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
    ConfigEntity, ConfigFilter, ConfigItem, ConfigStorage, SearchFieldConfig, entity_types,
    parse_tag_id,
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
                    slug: tag.slug.clone(),
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
                INSERT INTO category_tag (id, category_id, label, description, slug, weight, created, changed)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
            )
            .bind(tag.id)
            .bind(&tag.category_id)
            .bind(&tag.label)
            .bind(&tag.description)
            .bind(&tag.slug)
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

    // ---- Item (content) helpers ----

    /// Load an item by UUID string.
    async fn load_item(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id.parse::<Uuid>().context("invalid item UUID")?;

        let row = sqlx::query_as::<_, crate::models::Item>(
            "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
             promote, sticky, fields, stage_id, language, item_group_id, retention_days \
             FROM item WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load item")?;

        Ok(row.map(|r| {
            ConfigEntity::Item(ConfigItem {
                id: r.id,
                item_type: r.item_type,
                title: r.title,
                language: r.language,
                status: r.status,
                fields: r.fields.clone(),
                created: r.created,
                changed: r.changed,
            })
        }))
    }

    /// Save (upsert) an item from config.
    async fn save_item(&self, item: &ConfigItem) -> Result<()> {
        let now = if item.created > 0 {
            item.created
        } else {
            chrono::Utc::now().timestamp()
        };
        let changed = if item.changed > 0 { item.changed } else { now };

        sqlx::query(
            r#"
            INSERT INTO item (id, type, title, author_id, status, fields, created, changed,
                              promote, sticky, stage_id, language, item_group_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, 0, $9, $10, $11)
            ON CONFLICT (id) DO UPDATE SET
                title = EXCLUDED.title,
                fields = EXCLUDED.fields,
                status = EXCLUDED.status,
                language = EXCLUDED.language,
                changed = EXCLUDED.changed
            "#,
        )
        .bind(item.id)
        .bind(&item.item_type)
        .bind(&item.title)
        .bind(Uuid::nil()) // author_id — anonymous for config-imported items
        .bind(item.status)
        .bind(&item.fields)
        .bind(now)
        .bind(changed)
        .bind(crate::models::stage::LIVE_STAGE_ID)
        .bind(&item.language)
        .bind(Uuid::now_v7()) // item_group_id
        .execute(&self.pool)
        .await
        .context("failed to save item")?;

        Ok(())
    }

    /// Delete an item by UUID string.
    async fn delete_item(&self, id: &str) -> Result<bool> {
        let uuid = id.parse::<Uuid>().context("invalid item UUID")?;
        let result = sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete item")?;
        Ok(result.rows_affected() > 0)
    }

    /// List all items (optionally filtered by type).
    async fn list_items(&self, filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let item_type_filter = filter
            .and_then(|f| f.field.as_deref())
            .filter(|f| *f == "type")
            .and(filter.and_then(|f| f.value.as_deref()));

        let rows: Vec<crate::models::Item> = if let Some(item_type) = item_type_filter {
            sqlx::query_as(
                "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
                 promote, sticky, fields, stage_id, language, item_group_id, retention_days \
                 FROM item WHERE type = $1 ORDER BY created",
            )
            .bind(item_type)
            .fetch_all(&self.pool)
            .await
            .context("failed to list items")?
        } else {
            sqlx::query_as(
                "SELECT id, current_revision_id, type, title, author_id, status, created, changed, \
                 promote, sticky, fields, stage_id, language, item_group_id, retention_days \
                 FROM item ORDER BY created",
            )
            .fetch_all(&self.pool)
            .await
            .context("failed to list items")?
        };

        Ok(rows
            .into_iter()
            .map(|r| {
                ConfigEntity::Item(ConfigItem {
                    id: r.id,
                    item_type: r.item_type,
                    title: r.title,
                    language: r.language,
                    status: r.status,
                    fields: r.fields.clone(),
                    created: r.created,
                    changed: r.changed,
                })
            })
            .collect())
    }

    // ---- Role helpers ----

    async fn load_role(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id.parse::<Uuid>().context("invalid role UUID")?;
        let row = sqlx::query_as::<_, crate::models::Role>(
            "SELECT id, name, created FROM roles WHERE id = $1",
        )
        .bind(uuid)
        .fetch_optional(&self.pool)
        .await
        .context("failed to load role")?;
        Ok(row.map(ConfigEntity::Role))
    }

    async fn save_role(&self, role: &crate::models::Role) -> Result<()> {
        sqlx::query(
            "INSERT INTO roles (id, name, created) VALUES ($1, $2, $3) \
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
        )
        .bind(role.id)
        .bind(&role.name)
        .bind(role.created)
        .execute(&self.pool)
        .await
        .context("failed to save role")?;
        Ok(())
    }

    async fn delete_role(&self, id: &str) -> Result<bool> {
        let uuid = id.parse::<Uuid>().context("invalid role UUID")?;
        let r = sqlx::query("DELETE FROM roles WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete role")?;
        Ok(r.rows_affected() > 0)
    }

    async fn list_roles(&self, _filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let rows: Vec<crate::models::Role> =
            sqlx::query_as("SELECT id, name, created FROM roles ORDER BY name")
                .fetch_all(&self.pool)
                .await
                .context("failed to list roles")?;
        Ok(rows.into_iter().map(ConfigEntity::Role).collect())
    }

    // ---- Stage helpers ----

    async fn load_stage(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id.parse::<Uuid>().context("invalid stage UUID")?;
        let row = crate::models::Stage::find_by_id(&self.pool, uuid).await?;
        Ok(row.map(ConfigEntity::Stage))
    }

    async fn save_stage(&self, stage: &crate::models::Stage) -> Result<()> {
        // Stages are stored across category_tag + stage_config tables.
        let input = crate::models::CreateStage {
            label: stage.label.clone(),
            machine_name: stage.machine_name.clone(),
            description: stage.description.clone(),
            visibility: Some(stage.visibility.to_string()),
            is_default: Some(stage.is_default),
            weight: Some(stage.weight),
        };
        // Try update first, create if not exists
        if crate::models::Stage::find_by_id(&self.pool, stage.id)
            .await?
            .is_some()
        {
            sqlx::query(
                "UPDATE stage_config SET machine_name = $1, visibility = $2, is_default = $3 \
                 WHERE stage_id = $4",
            )
            .bind(&stage.machine_name)
            .bind(stage.visibility.to_string())
            .bind(stage.is_default)
            .bind(stage.id)
            .execute(&self.pool)
            .await
            .context("failed to update stage")?;
        } else {
            crate::models::Stage::create(&self.pool, input).await?;
        }
        Ok(())
    }

    async fn delete_stage(&self, id: &str) -> Result<bool> {
        let uuid = id.parse::<Uuid>().context("invalid stage UUID")?;
        let r = sqlx::query("DELETE FROM stage_config WHERE stage_id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete stage config")?;
        Ok(r.rows_affected() > 0)
    }

    async fn list_stages(&self, _filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let rows = crate::models::Stage::list_all(&self.pool).await?;
        Ok(rows.into_iter().map(ConfigEntity::Stage).collect())
    }

    // ---- Tile helpers ----

    async fn load_tile(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id.parse::<Uuid>().context("invalid tile UUID")?;
        let row =
            sqlx::query_as::<_, crate::models::tile::Tile>("SELECT * FROM tile WHERE id = $1")
                .bind(uuid)
                .fetch_optional(&self.pool)
                .await
                .context("failed to load tile")?;
        Ok(row.map(ConfigEntity::Tile))
    }

    async fn save_tile(&self, tile: &crate::models::tile::Tile) -> Result<()> {
        sqlx::query(
            "INSERT INTO tile (id, machine_name, label, region, tile_type, config, visibility, \
             weight, status, plugin, created, changed) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (id) DO UPDATE SET \
             label = EXCLUDED.label, region = EXCLUDED.region, config = EXCLUDED.config, \
             visibility = EXCLUDED.visibility, weight = EXCLUDED.weight, \
             status = EXCLUDED.status, changed = EXCLUDED.changed",
        )
        .bind(tile.id)
        .bind(&tile.machine_name)
        .bind(&tile.label)
        .bind(&tile.region)
        .bind(&tile.tile_type)
        .bind(&tile.config)
        .bind(&tile.visibility)
        .bind(tile.weight)
        .bind(tile.status)
        .bind(&tile.plugin)
        .bind(tile.created)
        .bind(tile.changed)
        .execute(&self.pool)
        .await
        .context("failed to save tile")?;
        Ok(())
    }

    async fn delete_tile(&self, id: &str) -> Result<bool> {
        let uuid = id.parse::<Uuid>().context("invalid tile UUID")?;
        let r = sqlx::query("DELETE FROM tile WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete tile")?;
        Ok(r.rows_affected() > 0)
    }

    async fn list_tiles(&self, _filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let rows: Vec<crate::models::tile::Tile> =
            sqlx::query_as("SELECT * FROM tile ORDER BY region, weight")
                .fetch_all(&self.pool)
                .await
                .context("failed to list tiles")?;
        Ok(rows.into_iter().map(ConfigEntity::Tile).collect())
    }

    // ---- MenuLink helpers ----

    async fn load_menu_link(&self, id: &str) -> Result<Option<ConfigEntity>> {
        let uuid = id.parse::<Uuid>().context("invalid menu_link UUID")?;
        let row =
            sqlx::query_as::<_, crate::models::MenuLink>("SELECT * FROM menu_link WHERE id = $1")
                .bind(uuid)
                .fetch_optional(&self.pool)
                .await
                .context("failed to load menu link")?;
        Ok(row.map(ConfigEntity::MenuLink))
    }

    async fn save_menu_link(&self, link: &crate::models::MenuLink) -> Result<()> {
        sqlx::query(
            "INSERT INTO menu_link (id, menu_name, path, title, weight, created, changed) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (id) DO UPDATE SET \
             menu_name = EXCLUDED.menu_name, path = EXCLUDED.path, \
             title = EXCLUDED.title, weight = EXCLUDED.weight, changed = EXCLUDED.changed",
        )
        .bind(link.id)
        .bind(&link.menu_name)
        .bind(&link.path)
        .bind(&link.title)
        .bind(link.weight)
        .bind(link.created)
        .bind(link.changed)
        .execute(&self.pool)
        .await
        .context("failed to save menu link")?;
        Ok(())
    }

    async fn delete_menu_link(&self, id: &str) -> Result<bool> {
        let uuid = id.parse::<Uuid>().context("invalid menu_link UUID")?;
        let r = sqlx::query("DELETE FROM menu_link WHERE id = $1")
            .bind(uuid)
            .execute(&self.pool)
            .await
            .context("failed to delete menu link")?;
        Ok(r.rows_affected() > 0)
    }

    async fn list_menu_links(&self, _filter: Option<&ConfigFilter>) -> Result<Vec<ConfigEntity>> {
        let rows: Vec<crate::models::MenuLink> =
            sqlx::query_as("SELECT * FROM menu_link ORDER BY menu_name, weight")
                .fetch_all(&self.pool)
                .await
                .context("failed to list menu links")?;
        Ok(rows.into_iter().map(ConfigEntity::MenuLink).collect())
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
            entity_types::ITEM => self.load_item(id).await,
            entity_types::ROLE => self.load_role(id).await,
            entity_types::STAGE => self.load_stage(id).await,
            entity_types::TILE => self.load_tile(id).await,
            entity_types::MENU_LINK => self.load_menu_link(id).await,
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
            ConfigEntity::Item(i) => self.save_item(i).await,
            ConfigEntity::Role(r) => self.save_role(r).await,
            ConfigEntity::Stage(s) => self.save_stage(s).await,
            ConfigEntity::Tile(t) => self.save_tile(t).await,
            ConfigEntity::MenuLink(m) => self.save_menu_link(m).await,
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
            entity_types::ITEM => self.delete_item(id).await,
            entity_types::ROLE => self.delete_role(id).await,
            entity_types::STAGE => self.delete_stage(id).await,
            entity_types::TILE => self.delete_tile(id).await,
            entity_types::MENU_LINK => self.delete_menu_link(id).await,
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
            entity_types::ITEM => self.list_items(filter).await,
            entity_types::ROLE => self.list_roles(filter).await,
            entity_types::STAGE => self.list_stages(filter).await,
            entity_types::TILE => self.list_tiles(filter).await,
            entity_types::MENU_LINK => self.list_menu_links(filter).await,
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
