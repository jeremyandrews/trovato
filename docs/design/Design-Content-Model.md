# Trovato Design: Content Model

*Sections 6-8 of the v2.1 Design Document*

---

## 6. Items and the Content Type System (CCK)

### The Core Problem

In Drupal 6, each CCK field created a separate database table, and loading an item meant joining all those tables together. An item type with 15 fields meant 15 JOINs. This was the single biggest performance bottleneck.

### The Solution: JSONB

We store custom fields in a single JSONB column. This eliminates JOINs entirely for field loading. PostgreSQL's GIN indexing on JSONB provides reasonable query performance for Gather-style filtering.

### Database Schema

```sql
CREATE TABLE item_type (
    type VARCHAR(32) PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    has_title BOOLEAN NOT NULL DEFAULT true,
    title_label VARCHAR(255) DEFAULT 'Title',
    plugin VARCHAR(64) NOT NULL
);

CREATE TABLE field_config (
    id UUID PRIMARY KEY,
    field_name VARCHAR(32) NOT NULL UNIQUE,
    field_type VARCHAR(32) NOT NULL,
    cardinality INTEGER NOT NULL DEFAULT 1,
    settings JSONB DEFAULT '{}'::jsonb
);

CREATE TABLE field_instance (
    id UUID PRIMARY KEY,
    field_name VARCHAR(32) NOT NULL
        REFERENCES field_config(field_name),
    bundle VARCHAR(32) NOT NULL
        REFERENCES item_type(type),
    label VARCHAR(255) NOT NULL,
    required BOOLEAN NOT NULL DEFAULT false,
    weight INTEGER NOT NULL DEFAULT 0,
    widget_settings JSONB DEFAULT '{}'::jsonb,
    display_settings JSONB DEFAULT '{}'::jsonb,
    UNIQUE (field_name, bundle)
);

CREATE TABLE item (
    id UUID PRIMARY KEY,
    current_revision_id UUID,
    type VARCHAR(32) NOT NULL REFERENCES item_type(type),
    title VARCHAR(255) NOT NULL,
    author_id UUID NOT NULL REFERENCES users(id),
    status INTEGER NOT NULL DEFAULT 1,
    created BIGINT NOT NULL,
    changed BIGINT NOT NULL,
    promote INTEGER NOT NULL DEFAULT 0,
    sticky INTEGER NOT NULL DEFAULT 0,
    fields JSONB DEFAULT '{}'::jsonb,
    search_vector tsvector,
    stage_id VARCHAR(64) NOT NULL DEFAULT 'live' REFERENCES stage(id)
);

CREATE INDEX idx_item_type ON item(type);
CREATE INDEX idx_item_author ON item(author_id);
CREATE INDEX idx_item_status ON item(status);
CREATE INDEX idx_item_created ON item(created);
CREATE INDEX idx_item_fields ON item USING GIN (fields);
CREATE INDEX idx_item_search ON item USING GIN (search_vector);
CREATE INDEX idx_item_stage ON item(stage_id);

CREATE TABLE system (
    name VARCHAR(255) NOT NULL,
    type VARCHAR(32) NOT NULL DEFAULT 'plugin',
    status INTEGER NOT NULL DEFAULT 0,
    weight INTEGER NOT NULL DEFAULT 0,
    info JSONB DEFAULT '{}'::jsonb,
    PRIMARY KEY (name, type)
);
```

### The Item Struct

This struct represents a *loaded* Item — the result of joining `item` with its current `item_revision`. It is not a 1:1 map of the `item` table; `revision_author_id`, `revision_log`, and `revision_created` come from the revision row. The `sqlx::FromRow` derive works because the load query always joins both tables.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Item {
    // From item table
    pub id: Uuid,
    pub current_revision_id: Uuid,
    pub r#type: String,
    pub title: String,
    pub author_id: Uuid,
    pub status: i32,
    pub created: i64,
    pub changed: i64,
    pub promote: i32,
    pub sticky: i32,
    pub fields: serde_json::Value,
    // From item_revision table (via JOIN on current_revision_id)
    pub revision_author_id: Uuid,
    pub revision_log: Option<String>,
    pub revision_created: i64,
}

impl Item {
    pub fn new(item_type: &str, title: &str, author_id: Uuid) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: Uuid::now_v7(),
            current_revision_id: Uuid::nil(),
            r#type: item_type.to_string(),
            title: title.to_string(), author_id,
            status: 1, created: now, changed: now,
            promote: 0, sticky: 0,
            fields: serde_json::json!({}),
            revision_author_id: author_id,
            revision_log: None,
            revision_created: now,
        }
    }

    pub fn get_field(
        &self, field_name: &str,
    ) -> Option<&serde_json::Value> {
        self.fields.get(field_name)
    }

    pub fn set_field(
        &mut self, field_name: &str, value: serde_json::Value,
    ) {
        self.fields.as_object_mut()
            .expect("fields must be a JSON object")
            .insert(field_name.to_string(), value);
    }
}
```

### Field Storage Format

Fields in the JSONB column follow a consistent structure:

```json
{
    "field_subtitle": { "value": "A great subtitle" },
    "field_rating": { "value": 4 },
    "field_tags": [
        {"target_id": "550e8400-e29b-41d4-a716-446655440005", "target_type": "category_term"},
        {"target_id": "550e8400-e29b-41d4-a716-446655440012", "target_type": "category_term"}
    ],
    "field_author_ref": {
        "target_id": "550e8400-e29b-41d4-a716-446655440042", "target_type": "user"
    },
    "field_body": {
        "value": "<p>The body text...</p>",
        "format": "filtered_html"
    },
    "field_image": {
        "file_id": "550e8400-e29b-41d4-a716-446655440099",
        "uri": "public://images/photo.jpg",
        "alt": "A photo of the office",
        "title": "Office photo",
        "width": 1200,
        "height": 800
    }
}
```

### Field Validation

When saving an item, the Kernel validates each field against its `field_config` definition. This validation logic resides in Rust, not WASM, for speed.

```rust
#[derive(Debug)]
pub enum FieldType {
    Text { max_length: Option<usize> },
    Integer { min: Option<i64>, max: Option<i64> },
    Decimal { precision: u32, scale: u32 },
    Boolean,
    RecordReference { target_type: String },
    Date,
    File { allowed_extensions: Vec<String>, max_size: u64 },
}

pub fn validate_field(
    field_name: &str, value: &serde_json::Value,
    field_type: &FieldType, cardinality: i32,
    required: bool,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if required
        && (value.is_null()
            || value == &serde_json::json!({}))
    {
        errors.push(format!("{field_name} is required"));
        return Err(errors);
    }

    let values: Vec<&serde_json::Value> = if cardinality != 1 {
        match value.as_array() {
            Some(arr) => {
                if cardinality > 0
                    && arr.len() as i32 > cardinality
                {
                    errors.push(format!(
                        "{field_name} allows at most {cardinality} values"
                    ));
                }
                arr.iter().collect()
            }
            None => {
                errors.push(format!(
                    "{field_name} must be an array for multi-value fields"
                ));
                return Err(errors);
            }
        }
    } else {
        vec![value]
    };

    for v in values {
        match field_type {
            FieldType::Text { max_length } => {
                if let Some(s) = v.get("value").and_then(|v| v.as_str()) {
                    if let Some(max) = max_length {
                        if s.len() > *max {
                            errors.push(format!(
                                "{field_name}: exceeds max length {max}"
                            ));
                        }
                    }
                } else {
                    errors.push(format!("{field_name}: expected text value"));
                }
            }
            FieldType::Integer { min, max } => {
                if let Some(n) = v.get("value").and_then(|v| v.as_i64()) {
                    if let Some(m) = min {
                        if n < *m {
                            errors.push(format!(
                                "{field_name}: below minimum {m}"
                            ));
                        }
                    }
                    if let Some(m) = max {
                        if n > *m {
                            errors.push(format!(
                                "{field_name}: above maximum {m}"
                            ));
                        }
                    }
                } else {
                    errors.push(format!("{field_name}: expected integer value"));
                }
            }
            FieldType::RecordReference { .. } => {
                if v.get("target_id").and_then(|v| v.as_str())
                    .and_then(|s| uuid::Uuid::parse_str(s).ok()).is_none()
                {
                    errors.push(format!("{field_name}: expected target_id as valid UUID"));
                }
            }
            FieldType::Boolean => {
                if v.get("value").and_then(|v| v.as_bool()).is_none() {
                    errors.push(format!("{field_name}: expected boolean value"));
                }
            }
            // TODO (Phase 3): Implement validation for remaining field types.
            // Decimal: check precision/scale, parse as numeric.
            // Date: validate ISO 8601 format or unix timestamp.
            // File: verify fid exists in file_managed, check allowed_extensions.
            // Email: validate RFC 5322 format.
            _ => {}
        }
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}
```

### Item CRUD with Tap Invocations

> **Note:** The code below illustrates the tap invocation flow using full JSON serialization for clarity. In production, presave/insert/update taps will use **handle-based data access** (see [[Projects/Trovato/Design-Plugin-SDK#6. WIT Interface|SDK Spec §6]]). The Kernel passes an `item-handle` to plugins; plugins read/write fields via host functions. The *flow* (validate → presave → save → postsave → cache invalidation) remains the same.

```rust
pub async fn item_save(
    state: &mut AppState, item: &mut Item,
) -> Result<(), ItemError> {
    let is_new = item.current_revision_id.is_nil();

    validate_item_fields(&state.db, item).await?;

    let tap = if is_new { "tap_item_presave_insert" } else { "tap_item_presave_update" };
    let item_json = serde_json::to_string(item)?;
    for result in state.plugin_registry.invoke_all(tap, &item_json) {
        if let Ok(modified) = result {
            *item = serde_json::from_str(&modified)?;
        }
    }

    item.changed = chrono::Utc::now().timestamp();
    if is_new {
        sqlx::query(
            "INSERT INTO item (id, type, title, author_id, status,
             created, changed, promote, sticky, fields)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)"
        )
        .bind(item.id).bind(&item.r#type).bind(&item.title)
        .bind(item.author_id).bind(item.status)
        .bind(item.created).bind(item.changed)
        .bind(item.promote).bind(item.sticky)
        .bind(&item.fields)
        .execute(&state.db).await?;
    } else {
        sqlx::query(
            "UPDATE item SET title=$1, status=$2,
             changed=$3, promote=$4, sticky=$5,
             fields=$6 WHERE id=$7"
        )
        .bind(&item.title).bind(item.status)
        .bind(item.changed).bind(item.promote)
        .bind(item.sticky).bind(&item.fields)
        .bind(item.id)
        .execute(&state.db).await?;
    }

    save_revision(&state.db, item).await?;

    let tap = if is_new { "tap_item_insert" } else { "tap_item_update" };
    state.plugin_registry.invoke_all(tap, &serde_json::to_string(item)?);

    // Invalidate cache tags
    invalidate_tag(&state.cache, &format!("item:{}", item.id)).await;
    invalidate_tag(&state.cache, &format!("item_list:{}", item.r#type)).await;
    invalidate_tag(&state.cache, "item_list:all").await;

    Ok(())
}
```

---

## 7. Stages & Content Revisions

### Why Stages Instead of Simple Revisions

Revisions alone give you version history. Stages give you content staging — "Draft", "Live", "Spring Campaign" can all exist simultaneously with different versions of the same item. This is far more powerful than a boolean published/unpublished flag.

More importantly, if you design the item table without stage support baked in, retrofitting it later means a data migration on your most critical table. Get the schema right from the start even if you defer the UI.

### Schema

```sql
CREATE TABLE stage (
    id VARCHAR(64) PRIMARY KEY,
    label VARCHAR(255) NOT NULL,
    owner_id UUID REFERENCES users(id),
    created BIGINT NOT NULL,
    upstream_id VARCHAR(64) REFERENCES stage(id) DEFAULT 'live'
);

INSERT INTO stage (id, label, created) VALUES ('live', 'Live', 0);

CREATE TABLE item_revision (
    id UUID PRIMARY KEY,
    item_id UUID NOT NULL
        REFERENCES item(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES users(id),
    title VARCHAR(255) NOT NULL,
    status INTEGER NOT NULL DEFAULT 1,
    created BIGINT NOT NULL,
    log TEXT,
    fields JSONB DEFAULT '{}'::jsonb
);

CREATE INDEX idx_revision_item ON item_revision(item_id);
CREATE INDEX idx_revision_item_id ON item_revision(item_id, id DESC);

ALTER TABLE item ADD CONSTRAINT fk_item_revision
    FOREIGN KEY (current_revision_id) REFERENCES item_revision(id);

CREATE TABLE stage_association (
    stage_id VARCHAR(64) NOT NULL REFERENCES stage(id),
    item_id UUID NOT NULL,
    target_revision_id UUID NOT NULL REFERENCES item_revision(id),
    PRIMARY KEY (stage_id, item_id)
);

CREATE TABLE stage_deletion (
    stage_id VARCHAR(64) NOT NULL REFERENCES stage(id),
    entity_type VARCHAR(32) NOT NULL,
    entity_id UUID NOT NULL,
    deleted_at BIGINT NOT NULL,
    PRIMARY KEY (stage_id, entity_type, entity_id)
);
```

### How It Works

The stage system supports three operations: **creating** new content in a stage, **modifying** existing live content in a stage, and **deleting** live content in a stage. All three are reversible until publish.

**Creating new content in a stage:**
The item is inserted into the `item` table with `stage_id` set to the active stage (e.g., `'spring_campaign'`). The item only appears when viewing in that stage. An initial revision is created in `item_revision`. No `stage_association` entry is needed — the `stage_id` column on the item itself tracks ownership.

**Modifying existing live content in a stage:**
Insert a new row into `item_revision` with the modified field values. Create or update a `stage_association` entry mapping `(stage_id, item_id) → target_revision_id`. The live item table is not touched. When loading this item in the stage, the `stage_association` override takes precedence.

**Deleting content in a stage:**
Insert a row into `stage_deletion` with `(stage_id, 'item', item_id)`. The item is not actually deleted. When querying in that stage, items in the deletion table are excluded. The deletion is reversible by removing the `stage_deletion` row.

**On revert (single item):** Copy the target revision's data back into the item table and update `current_revision_id`. Do not delete intermediate revisions.

**On publish (stage → Live):**
Publishing is an atomic operation that applies all stage changes to Live in a single transaction:

1. **Modified items:** For each `stage_association` entry, copy the target revision's data into the `item` table (title, status, fields, etc.) and update `current_revision_id`. Delete the `stage_association` entry.
2. **New items:** For each item where `stage_id` = this stage, set `stage_id = 'live'`. The item is now visible to everyone.
3. **Deleted items:** For each `stage_deletion` entry where entity_type = 'item', actually delete the item (or mark it deleted, depending on policy). Delete the `stage_deletion` entry.
4. **Cache invalidation:** Invalidate all cache tags for affected items.

The entire publish runs in a single Postgres transaction. If any step fails, the transaction rolls back and the stage is unchanged.

### Stage-Aware Item Listing

When listing items (for Gather queries or admin pages), the query must account for three stage conditions: include items modified in the stage (use stage revision), exclude items deleted in the stage, and include items created in the stage.

```rust
pub async fn item_list_query_base(
    stage_id: Option<&str>,
) -> String {
    if let Some(st) = stage_id {
        // In a stage: include live items (not deleted in this stage)
        // plus items created in this stage, using stage revision overrides
        format!(
            "SELECT i.*, COALESCE(sa.target_revision_id, i.current_revision_id) as effective_revision_id
             FROM item i
             LEFT JOIN stage_association sa
                 ON sa.stage_id = '{st}' AND sa.item_id = i.id
             WHERE (i.stage_id = 'live' OR i.stage_id = '{st}')
               AND NOT EXISTS (
                   SELECT 1 FROM stage_deletion sd
                   WHERE sd.stage_id = '{st}'
                     AND sd.entity_type = 'item'
                     AND sd.entity_id = i.id
               )"
        )
    } else {
        // Live: only show live items
        "SELECT i.* FROM item i WHERE i.stage_id = 'live'".to_string()
    }
}
```

### Stage-Aware Loading

When loading an item, we check if the active stage has an override for this specific item.

```rust
pub async fn item_load(
    db: &PgPool, item_id: Uuid, stage_id: Option<&str>,
) -> Result<Item, Error> {
    if let Some(st) = stage_id {
        let override_rev: Option<Uuid> = sqlx::query_scalar(
            "SELECT target_revision_id FROM stage_association
             WHERE stage_id = $1 AND item_id = $2"
        )
        .bind(st).bind(item_id)
        .fetch_optional(db).await?;

        if let Some(rev_id) = override_rev {
            return load_revision(db, item_id, rev_id).await;
        }
    }
    // Fallback to live version (stored in main item table)
    load_live_item(db, item_id).await
}
```

### Stage Publish

Publishing a stage atomically applies all changes to Live.

```rust
pub async fn stage_publish(
    db: &PgPool, cache: &CacheLayer, stage_id: &str,
) -> Result<PublishReport, StageError> {
    if stage_id == "live" {
        return Err(StageError::CannotPublishLive);
    }

    let mut tx = db.begin().await?;
    let mut report = PublishReport::default();

    // 1. Apply modified items (stage_association overrides)
    let overrides: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT item_id, target_revision_id FROM stage_association
         WHERE stage_id = $1"
    ).bind(stage_id).fetch_all(&mut *tx).await?;

    for (item_id, rev_id) in &overrides {
        sqlx::query(
            "UPDATE item SET
                title = r.title, status = r.status, fields = r.fields,
                changed = $3, current_revision_id = r.id
             FROM item_revision r
             WHERE r.id = $1 AND item.id = $2"
        )
        .bind(rev_id).bind(item_id)
        .bind(chrono::Utc::now().timestamp())
        .execute(&mut *tx).await?;
        report.modified += 1;
    }

    // 2. Promote new items to live
    let promoted = sqlx::query(
        "UPDATE item SET stage_id = 'live' WHERE stage_id = $1"
    ).bind(stage_id).execute(&mut *tx).await?.rows_affected();
    report.created = promoted;

    // 3. Apply deletions
    let deletions: Vec<Uuid> = sqlx::query_scalar(
        "SELECT entity_id FROM stage_deletion
         WHERE stage_id = $1 AND entity_type = 'item'"
    ).bind(stage_id).fetch_all(&mut *tx).await?;

    for item_id in &deletions {
        sqlx::query("DELETE FROM item WHERE id = $1")
            .bind(item_id).execute(&mut *tx).await?;
        report.deleted += 1;
    }

    // 4. Clean up stage state
    sqlx::query("DELETE FROM stage_association WHERE stage_id = $1")
        .bind(stage_id).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM stage_deletion WHERE stage_id = $1")
        .bind(stage_id).execute(&mut *tx).await?;

    tx.commit().await?;

    // 5. Invalidate cache for all affected items
    for (item_id, _) in &overrides {
        invalidate_tag(cache, &format!("item:{item_id}")).await;
    }
    for item_id in &deletions {
        invalidate_tag(cache, &format!("item:{item_id}")).await;
    }
    invalidate_tag(cache, "item_list:all").await;

    Ok(report)
}

#[derive(Default)]
pub struct PublishReport {
    pub modified: u64,
    pub created: u64,
    pub deleted: u64,
}
```

### Deferred Stage Features

The following are explicitly deferred but the schema supports them: revision comparison (diff view), stage merge conflict resolution (MVP is "Last Publish Wins"), revision moderation (approval queues), revision pruning (should be configurable, not automatic — some sites need full audit trails).

**Future stage-able entity types.** The stage pattern (revisions + stage_association + stage_deletion) can be extended to other entity types as needed: menu items, URL aliases, categories terms, and plugin configuration (variables). The `stage_deletion` table is already entity-type-generic. Each new entity type would need its own revision table and stage_association table (for proper FK constraints). This is post-MVP work but the architecture supports it.

---

## 8. Categories System

### Data Model

```sql
CREATE TABLE category_vocabulary (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    machine_name VARCHAR(64) NOT NULL UNIQUE,
    description TEXT,
    hierarchy SMALLINT NOT NULL DEFAULT 0,
    weight INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE category_term (
    id UUID PRIMARY KEY,
    vocabulary_id UUID NOT NULL REFERENCES category_vocabulary(id),
    name VARCHAR(255) NOT NULL,
    description TEXT,
    weight INTEGER NOT NULL DEFAULT 0,
    data JSONB DEFAULT '{}'::jsonb
);

CREATE TABLE category_term_hierarchy (
    term_id UUID NOT NULL
        REFERENCES category_term(id) ON DELETE CASCADE,
    parent_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000',
    PRIMARY KEY (term_id, parent_id)
);

CREATE INDEX idx_term_vocabulary ON category_term(vocabulary_id);
CREATE INDEX idx_term_hierarchy_parent ON category_term_hierarchy(parent_id);
```

### Item-Term Relationship

Term references are stored as field values in the item's JSONB column, consistent with all other record references:

```json
{
    "field_tags": [
        {"target_id": "550e8400-e29b-41d4-a716-446655440005", "target_type": "category_term"},
        {"target_id": "550e8400-e29b-41d4-a716-446655440012", "target_type": "category_term"}
    ],
    "field_category": {
        "target_id": "550e8400-e29b-41d4-a716-446655440003", "target_type": "category_term"
    }
}
```

This eliminates the need for a separate `categories_index` table. Gather queries filter on JSONB containment:

```sql
WHERE fields @> '{"field_tags": [{"target_id": "550e8400-e29b-41d4-a716-446655440005"}]}'
```

The `@>` containment operator uses GIN indexes efficiently, unlike the path-based extraction operators.

### Hierarchical Queries

Drupal 6's categories hierarchy is a DAG because terms can have multiple parents. For "all items tagged with term X or any children," use a recursive CTE:

```sql
WITH RECURSIVE term_tree AS (
    SELECT id FROM category_term WHERE id = $1
    UNION ALL
    SELECT h.term_id FROM category_term_hierarchy h
    JOIN term_tree t ON h.parent_id = t.id
)
SELECT n.* FROM item n
WHERE n.fields @> ANY(
    SELECT jsonb_build_array(
        jsonb_build_object(
            'target_id', id::text,
            'target_type', 'category_term'
        )
    ) FROM term_tree
);
```

This is expensive. For high-traffic term pages, materialize the term tree into a flat lookup table (`category_term_descendants`) and rebuild it when terms are reparented.

### Gather Integration

The Gather query builder needs specific support for categories: term filters (JSONB containment WHERE clauses), hierarchical term filters (recursive CTE or materialized descendants table), term name display (JOIN to `category_term`), term arguments (contextual filter from URL), and vocabulary filters.

---

