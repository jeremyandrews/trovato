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
use sqlx::{AssertSqlSafe, PgPool};
use tracing::{info, warn};

use crate::metrics::{CacheName, Metrics};
use crate::models::{CreateItemType, ItemType};
use crate::tap::TapDispatcher;
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition, FieldType};

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

    /// Serializes this server's writes with its reloads. A write holds it from
    /// its database write until its definition is cached; a reload holds it
    /// from reading the rows until it has applied them. Without it a reload
    /// could read the rows just before a write, then put the old definition
    /// back over the new one, or drop a type created meanwhile.
    writes: tokio::sync::Mutex<()>,

    /// Counts `types` hits and misses.
    metrics: Arc<Metrics>,
}

/// Why a field could not be added to a content type.
///
/// The first two are things a person does at the admin screen, so both admin
/// routes turn them into a message that names what was wrong. Everything else
/// is a load or database failure and stays opaque to the user.
#[derive(Debug, thiserror::Error)]
pub enum AddFieldError {
    /// The content type already has a field with this machine name.
    ///
    /// The `Display` text is what the admin screens show, so the wording lives
    /// here rather than being written out again at each route.
    #[error("A field with machine name {field_name} already exists on this type.")]
    DuplicateFieldName {
        /// The machine name that was already taken.
        field_name: String,
    },

    /// The `field_type` string is not one the kernel knows.
    ///
    /// This used to fall through to `Text`, so a typo or a stale option in the
    /// template silently stored a field of the wrong type.
    #[error("Unknown field type: {field_type}")]
    UnknownFieldType {
        /// The unrecognised type string, as submitted.
        field_type: String,
    },

    /// A category reference field was asked for without naming its category.
    #[error("Category is required for a category reference field.")]
    CategoryRequired,

    /// A category reference field named a category that does not exist.
    #[error("Unknown category: {category}")]
    UnknownCategory {
        /// The category machine name, as submitted.
        category: String,
    },

    /// Loading the content type, or writing the new field list, failed.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Why a content type could not be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateContentTypeError {
    /// A row with this machine name already exists in the database.
    ///
    /// The `Display` text is what the add form shows, so the wording lives
    /// here and nowhere else.
    #[error("A content type with machine name '{machine_name}' already exists.")]
    AlreadyExists {
        /// The machine name that was already taken.
        machine_name: String,
    },

    /// Writing the row failed.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// The columns every statement here reads back, in `ItemType`'s shape.
const RETURNING_ITEM_TYPE: &str =
    "RETURNING type AS type_name, label, description, has_title, title_label, plugin, settings";

/// The one rule for writing into `item_type.settings`: the top-level keys of
/// the bound patch replace those keys, and every other key the row holds stays.
///
/// A column that is not an object (a bare array from an old writer, or null)
/// cannot hold keys to keep, so it is read as `{}`.
///
/// It is a SQL expression rather than Rust so that the read and the write are
/// one statement: a writer that read the column, merged in memory and wrote it
/// back could lose a key another writer stored in between. Three writers use
/// it: [`ContentTypeRegistry::update`], which owns every key but `fields`;
/// `persist_fields`, which owns `fields` alone; and plugin registration
/// (`register_type`), which writes only `fields`, merged by
/// [`merge_declared_fields_sql`].
fn merge_settings_sql(patch_param: &str) -> String {
    format!(
        "(CASE WHEN jsonb_typeof(item_type.settings) = 'object' \
         THEN item_type.settings ELSE '{{}}'::jsonb END) || {patch_param}::jsonb"
    )
}

/// The one rule for merging a plugin's declared fields into the field list a
/// row already holds: the plugin wins on the names it declares and is silent
/// about every other field.
///
/// `declared_param` is a bound `{"fields": [...]}` object. The result is a
/// JSON array that keeps the row's field order; each existing field whose
/// `field_name` the plugin declares is replaced whole by the declared one,
/// each existing field it does not declare (one an admin added, or one a newer
/// plugin version stopped declaring) stays unchanged, and declared fields the
/// row lacks are appended in declared order. One key of an existing field
/// survives its replacement: `settings.filterable`, the site's "Filterable and
/// sortable" choice (B207), which is the site's to make, not the plugin's. A
/// column that is not an object, or a `fields` that is not an array, is read as
/// no fields, so a fresh or unreadable row gets the declared list as it is.
///
/// SQL for the same reason as [`merge_settings_sql`]: it reads the row as it
/// stands when the statement writes it.
fn merge_declared_fields_sql(declared_param: &str) -> String {
    let existing = "(CASE WHEN jsonb_typeof(item_type.settings) = 'object' \
         AND jsonb_typeof(item_type.settings -> 'fields') = 'array' \
         THEN item_type.settings -> 'fields' ELSE '[]'::jsonb END)";
    let declared = format!(
        "(CASE WHEN jsonb_typeof({declared_param}::jsonb -> 'fields') = 'array' \
         THEN {declared_param}::jsonb -> 'fields' ELSE '[]'::jsonb END)"
    );
    format!(
        "COALESCE((SELECT jsonb_agg(m.field ORDER BY m.grp, m.ord) FROM ( \
           SELECT COALESCE( \
                    (SELECT d.field || jsonb_build_object('settings', \
                              (CASE WHEN jsonb_typeof(d.field -> 'settings') = 'object' \
                               THEN d.field -> 'settings' ELSE '{{}}'::jsonb END) \
                              || jsonb_strip_nulls(jsonb_build_object('filterable', \
                                   e.field -> 'settings' -> 'filterable'))) \
                     FROM jsonb_array_elements({declared}) \
                       WITH ORDINALITY AS d(field, ord) \
                     WHERE d.field ->> 'field_name' = e.field ->> 'field_name' \
                     ORDER BY d.ord LIMIT 1), \
                    e.field) AS field, \
                  0 AS grp, e.ord \
           FROM jsonb_array_elements({existing}) WITH ORDINALITY AS e(field, ord) \
           UNION ALL \
           SELECT d.field, 1 AS grp, d.ord \
           FROM jsonb_array_elements({declared}) WITH ORDINALITY AS d(field, ord) \
           WHERE NOT EXISTS ( \
             SELECT 1 FROM jsonb_array_elements({existing}) AS x(field) \
             WHERE x.field ->> 'field_name' = d.field ->> 'field_name') \
         ) AS m), '[]'::jsonb)"
    )
}

/// Map a `field_type` form value to the field type it names.
///
/// `None` for anything unrecognised: the caller refuses rather than guessing.
/// The strings are the `value` attributes of the Type select in
/// `templates/admin/field-list.html`.
fn parse_field_type(field_type: &str) -> Option<trovato_sdk::types::FieldType> {
    Some(match field_type {
        "text" => FieldType::Text { max_length: None },
        "text_long" => FieldType::TextLong,
        "integer" => FieldType::Integer,
        "float" => FieldType::Float,
        "boolean" => FieldType::Boolean,
        "date" => FieldType::Date,
        "email" => FieldType::Email,
        "record_reference" => FieldType::RecordReference(String::new()),
        // The category is filled in by `add_field_in_category`, which refuses
        // the field without one.
        "category_reference" => FieldType::CategoryReference(String::new()),
        "compound" => FieldType::Compound {
            allowed_types: vec![],
            min_items: None,
            max_items: None,
        },
        "blocks" => FieldType::Blocks,
        _ => return None,
    })
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
    primary
        .and_then(normalize_label)
        .or_else(|| fallback.and_then(normalize_label))
        .or_else(|| Some("Title".to_string()))
}

/// Trim a label, reading an empty one as absent.
fn normalize_label(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

impl ContentTypeRegistry {
    /// Create a new content type registry.
    pub fn new(pool: PgPool, ttl: Duration, metrics: Arc<Metrics>) -> Self {
        Self {
            inner: Arc::new(ContentTypeRegistryInner {
                pool,
                types: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
                writes: tokio::sync::Mutex::new(()),
                metrics,
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
            self.cache_row(&db_type);
        }

        info!(count = synced_count, "content types synced from plugins");
        Ok(())
    }

    /// Register a content type definition from a plugin.
    ///
    /// Runs on every boot, so it must leave what an admin saved alone. The
    /// declaration is authoritative for what it declares and silent about the
    /// rest: label and description are written from it, `title_label` only
    /// when it declares one, and of `settings` only `fields`, merged by
    /// [`merge_declared_fields_sql`] into the row's list through
    /// [`merge_settings_sql`]. Every other settings key, and every field the
    /// plugin does not declare, stays. This used to write
    /// `{"fields": <declared>}` over the whole column through a whole-row
    /// upsert, so each boot dropped `published_default`, `revision_default`,
    /// every other saved key and every admin-added field, and reset the title
    /// label to "Title".
    async fn register_type(&self, def: &ContentTypeDefinition, plugin_name: &str) -> Result<()> {
        let input = CreateItemType {
            type_name: def.machine_name.clone(),
            label: def.label.clone(),
            description: Some(def.description.clone()),
            has_title: Some(true),
            // `None` keeps the row's label; a new row gets "Title".
            title_label: def.title_label.as_deref().and_then(normalize_label),
            plugin: plugin_name.to_string(),
            // The object shape `{"fields": [...]}` is the one canonical layout
            // for this column, and the one `parse_fields_from_settings` reads.
            // A new row is written with it as is; an existing row merges it.
            settings: Some(serde_json::json!({
                "fields": serde_json::to_value(&def.fields).context("serialize fields")?,
            })),
        };

        let patch = format!(
            "(jsonb_build_object('fields', {}))",
            merge_declared_fields_sql("$7")
        );
        let row =
            ItemType::upsert_plugin_declared(&self.inner.pool, input, &merge_settings_sql(&patch))
                .await?;

        // Cache the row as the database now holds it, not the declaration: the
        // row also carries the fields and title label an admin set.
        self.cache_row(&row);

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
        let _reload = self.inner.writes.lock().await;
        let db_types = ItemType::list(&self.inner.pool).await?;
        let stored: std::collections::HashSet<String> =
            db_types.iter().map(|t| t.type_name.clone()).collect();
        for db_type in db_types {
            self.cache_row(&db_type);
        }
        // A type deleted elsewhere (an import) leaves this server too. Every type,
        // a plugin's included, is a row, so the rows are the whole list.
        for name in self.type_names() {
            if !stored.contains(&name) {
                self.inner.types.invalidate(&name);
            }
        }
        Ok(())
    }

    /// Get a content type by machine name (sync, cache-only).
    pub fn get(&self, type_name: &str) -> Option<ContentTypeDefinition> {
        self.inner
            .metrics
            .cache_lookup(CacheName::ContentType, self.inner.types.get(type_name))
    }

    /// Get a content type by machine name with DB fallback on cache miss.
    pub async fn get_or_load(&self, type_name: &str) -> Result<Option<ContentTypeDefinition>> {
        if let Some(def) = self.get(type_name) {
            return Ok(Some(def));
        }

        // Cache miss — load from database
        let db_type = ItemType::find_by_type(&self.inner.pool, type_name).await?;
        if let Some(db_type) = db_type {
            let def = self.definition_from_row(&db_type);
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

    /// Build the cached definition from a row as the database holds it.
    fn definition_from_row(&self, row: &ItemType) -> ContentTypeDefinition {
        ContentTypeDefinition {
            machine_name: row.type_name.clone(),
            label: row.label.clone(),
            description: row.description.clone().unwrap_or_default(),
            title_label: row.title_label.clone(),
            fields: self.parse_fields_from_settings(&row.type_name, &row.settings),
        }
    }

    /// Cache the definition a write just returned.
    fn cache_row(&self, row: &ItemType) {
        self.inner
            .types
            .insert(row.type_name.clone(), self.definition_from_row(row));
    }

    /// Cache the definition a write just returned, and tell every other
    /// server to read the types again. Call it after the write commits.
    fn cache_written(&self, row: &ItemType) {
        self.cache_row(row);
        crate::cache::generation::bump(crate::cache::generation::CacheKind::ContentTypes);
        crate::edge_cache::purge([crate::edge_cache::tags::SITE]);
    }

    /// The row input `create` and `replace` both write.
    fn core_type_input(
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> CreateItemType {
        CreateItemType {
            type_name: machine_name.to_string(),
            label: label.to_string(),
            description: description.map(|s| s.to_string()),
            has_title: Some(true),
            title_label: resolve_title_label(
                settings.get("title_label").and_then(|v| v.as_str()),
                None,
            ),
            plugin: "core".to_string(),
            settings: Some(settings),
        }
    }

    /// Create a new content type.
    ///
    /// Refuses a machine name that already has a row, with
    /// [`CreateContentTypeError::AlreadyExists`]. The database decides, so a
    /// row this process has not cached yet (written by a second server, or by
    /// a config import before the next reload) is refused rather than
    /// overwritten. To overwrite on purpose, use [`Self::replace`].
    pub async fn create(
        &self,
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> Result<(), CreateContentTypeError> {
        let input = Self::core_type_input(machine_name, label, description, settings);

        // Held from the write to the cache update; see `writes`.
        let _write = self.inner.writes.lock().await;
        let Some(row) = ItemType::insert_if_absent(&self.inner.pool, input).await? else {
            return Err(CreateContentTypeError::AlreadyExists {
                machine_name: machine_name.to_string(),
            });
        };

        self.cache_written(&row);
        info!(type_name = %machine_name, "content type created");
        Ok(())
    }

    /// Write a whole content type definition, creating the row or
    /// overwriting every column of an existing one, `settings` included.
    ///
    /// A fixture and import tool: whatever fields and settings the row held
    /// before are gone afterwards, replaced by `settings` as given. Tests use
    /// it to put a shared type into a known shape at the start of each run.
    /// Admin forms must not: adding a type goes through [`Self::create`], and
    /// editing one through [`Self::update`].
    pub async fn replace(
        &self,
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> Result<()> {
        let input = Self::core_type_input(machine_name, label, description, settings);
        // Held from the write to the cache update; see `writes`.
        let _write = self.inner.writes.lock().await;
        let row = ItemType::upsert(&self.inner.pool, input).await?;

        self.cache_written(&row);
        info!(type_name = %machine_name, "content type replaced");
        Ok(())
    }

    /// Update an existing content type's label, description and settings.
    ///
    /// `settings` is merged into the row's current settings key by key, by
    /// the rule in `merge_settings_sql`; any key it does not name stays as
    /// it is. A `fields` key in `settings` is ignored: the field list belongs
    /// to `add_field`, `update_field` and `delete_field`. This used to write
    /// `settings` over the whole column, so saving the edit form deleted every
    /// field the type declared, and the cache, rebuilt from the previous cache
    /// entry, hid the loss until the next reload from the database.
    pub async fn update(
        &self,
        machine_name: &str,
        label: &str,
        description: Option<&str>,
        settings: serde_json::Value,
    ) -> Result<()> {
        let serde_json::Value::Object(mut patch) = settings else {
            anyhow::bail!(
                "settings for content type '{machine_name}' must be an object, got {}",
                json_shape(&settings)
            );
        };
        patch.remove("fields");

        let title_label = patch
            .get("title_label")
            .and_then(|v| v.as_str())
            .and_then(normalize_label);

        // One statement: the merge reads the column as it stands when the row
        // is written, so a field added concurrently is not lost between a read
        // here and a write.
        let sql = format!(
            "UPDATE item_type SET \
                label = $2, \
                description = $3, \
                has_title = true, \
                title_label = COALESCE($4, item_type.title_label, 'Title'), \
                settings = {merge} \
             WHERE type = $1 \
             {RETURNING_ITEM_TYPE}",
            merge = merge_settings_sql("$5"),
        );
        // Held from the write to the cache update; see `writes`.
        let _write = self.inner.writes.lock().await;
        let row = sqlx::query_as::<_, ItemType>(AssertSqlSafe(sql))
            .bind(machine_name)
            .bind(label)
            .bind(description)
            .bind(title_label)
            .bind(serde_json::Value::Object(patch))
            .fetch_optional(&self.inner.pool)
            .await
            .context("failed to update item_type")?
            .with_context(|| format!("content type '{machine_name}' not found"))?;

        self.cache_written(&row);
        info!(type_name = %machine_name, "content type updated");
        Ok(())
    }

    /// Add a field to a content type.
    ///
    /// This is the single place the "no two fields with one machine name" rule
    /// lives. Both admin routes call it and map [`AddFieldError`] to a message;
    /// anything else that adds a field inherits the same refusal rather than
    /// repeating the check.
    pub async fn add_field(
        &self,
        type_name: &str,
        field_name: &str,
        field_label: &str,
        field_type: &str,
    ) -> Result<(), AddFieldError> {
        self.add_field_in_category(type_name, field_name, field_label, field_type, None)
            .await
    }

    /// [`Self::add_field`], with the category a `category_reference` field
    /// holds tags of. That kind needs one, and it must exist
    /// ([`AddFieldError::CategoryRequired`], [`AddFieldError::UnknownCategory`]);
    /// every other kind ignores it.
    pub async fn add_field_in_category(
        &self,
        type_name: &str,
        field_name: &str,
        field_label: &str,
        field_type: &str,
        category: Option<&str>,
    ) -> Result<(), AddFieldError> {
        let mut def = self
            .get_or_load(type_name)
            .await?
            .context("content type not found")?;

        // Compared with `==`, which is how `update_field`, `delete_field` and
        // every other field lookup in the kernel compares a machine name.
        // `is_valid_machine_name` confines admin-supplied names to lowercase
        // ASCII before they get here, so there is no case to fold.
        if def.fields.iter().any(|f| f.field_name == field_name) {
            return Err(AddFieldError::DuplicateFieldName {
                field_name: field_name.to_string(),
            });
        }

        let field_type =
            parse_field_type(field_type).ok_or_else(|| AddFieldError::UnknownFieldType {
                field_type: field_type.to_string(),
            })?;
        let field_type = match field_type {
            FieldType::CategoryReference(_) => {
                let category = category
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
                    .ok_or(AddFieldError::CategoryRequired)?;
                FieldType::CategoryReference(self.existing_category(category).await?)
            }
            other => other,
        };

        def.fields.push(FieldDefinition {
            field_name: field_name.to_string(),
            field_type,
            label: field_label.to_string(),
            required: false,
            cardinality: 1,
            settings: serde_json::Value::Object(serde_json::Map::new()),
            personal_data: false,
        });

        // Through `persist_fields`, which merges into the existing settings.
        // This used to build `{"fields": ...}` and write it over the whole
        // column, so adding a field dropped `title_label`, `published_default`
        // and every other settings key the type carried.
        self.persist_fields(type_name, &def).await?;

        info!(type_name = %type_name, field = %field_name, "field added");
        Ok(())
    }

    /// Persist the field list for a content type, merging it into the existing
    /// settings so that non-field keys (`title_label`, `published_default`,
    /// etc.) are preserved.
    ///
    /// Through [`merge_settings_sql`], the same rule `update` writes by. A
    /// column that is not an object becomes the canonical object shape: it
    /// cannot carry a "fields" key, and leaving it alone is how a field edit
    /// used to report success while writing nothing.
    async fn persist_fields(&self, type_name: &str, def: &ContentTypeDefinition) -> Result<()> {
        let patch = serde_json::json!({
            "fields": serde_json::to_value(&def.fields).context("serialize fields")?,
        });

        let sql = format!(
            "UPDATE item_type SET settings = {merge} WHERE type = $1 {RETURNING_ITEM_TYPE}",
            merge = merge_settings_sql("$2"),
        );
        // Held from the write to the cache update; see `writes`.
        let _write = self.inner.writes.lock().await;
        let row = sqlx::query_as::<_, ItemType>(AssertSqlSafe(sql))
            .bind(type_name)
            .bind(&patch)
            .fetch_optional(&self.inner.pool)
            .await
            .context("failed to update item_type")?
            .with_context(|| format!("content type '{type_name}' not found"))?;

        self.cache_written(&row);
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

    /// Set one key of a field's `settings` (B207: `filterable`), keeping the
    /// rest.
    pub async fn set_field_setting(
        &self,
        type_name: &str,
        field_name: &str,
        key: &str,
        value: serde_json::Value,
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
        if !field.settings.is_object() {
            field.settings = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(settings) = field.settings.as_object_mut() {
            settings.insert(key.to_string(), value);
        }
        self.persist_fields(type_name, &def).await?;
        info!(type_name = %type_name, field = %field_name, key, "field setting updated");
        Ok(())
    }

    /// Point a `CategoryReference` field at another category, which must
    /// exist. Values already stored are not rewritten; the next write of the
    /// field is checked against the new category.
    pub async fn set_field_category(
        &self,
        type_name: &str,
        field_name: &str,
        category: &str,
    ) -> Result<(), AddFieldError> {
        let category = self.existing_category(category.trim()).await?;
        let mut def = self
            .get_or_load(type_name)
            .await?
            .context("content type not found")?;
        let field = def
            .fields
            .iter_mut()
            .find(|f| f.field_name == field_name)
            .context("field not found")?;
        let FieldType::CategoryReference(current) = &mut field.field_type else {
            return Err(anyhow::anyhow!("field '{field_name}' is not a category reference").into());
        };
        if *current == category {
            return Ok(());
        }
        *current = category;
        self.persist_fields(type_name, &def).await?;
        info!(type_name = %type_name, field = %field_name, "field category changed");
        Ok(())
    }

    /// `category` if a category with that machine name exists.
    async fn existing_category(&self, category: &str) -> Result<String, AddFieldError> {
        if category.is_empty() {
            return Err(AddFieldError::CategoryRequired);
        }
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM category WHERE id = $1)")
                .bind(category)
                .fetch_one(&self.inner.pool)
                .await
                .context("failed to look the category up")?;
        if exists {
            Ok(category.to_string())
        } else {
            Err(AddFieldError::UnknownCategory {
                category: category.to_string(),
            })
        }
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
        self.get(type_name).is_some()
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

    /// Clear all cached content types. `get` answers `None` until the next
    /// [`Self::reload_from_db`], which is what a bump from another server runs.
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
