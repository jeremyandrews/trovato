//! Item service with tap integration.
//!
//! Provides CRUD operations for items with automatic tap invocations
//! for plugin taps (insert, update, delete, view, access).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use moka::sync::Cache;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use crate::models::stage::{LIVE_STAGE_ID, Stage, StageVisibility};
use crate::models::{CreateItem, Item, ItemRevision, UpdateItem};
use crate::services::ai_provider::{AiProviderService, EMBEDDING_INPUT_MAX_CHARS};
use crate::services::embed_index::{self, EmbedPolicy};
use crate::services::vector_store::{PgVectorStore, VectorStore};
use crate::tap::{RequestServices, RequestState, TapDispatcher, TapResult, UserContext};
use trovato_sdk::types::{AccessResult, FieldAccessResult};

/// Synthetic `item_embeddings.field_name` under which the kernel stores an
/// item's whole-item embedding. One embedding per item keeps each item
/// appearing exactly once in similarity-search results.
///
/// `pub(crate)` so the native embed drain ([`crate::cron`]) writes under the
/// same field the sync path uses (P11f).
pub(crate) const KERNEL_INDEX_FIELD: &str = "_content";

/// Decode a view tap's raw output into the HTML fragment it means.
///
/// `#[plugin_tap]` serializes a tap's return value with `serde_json::to_string`
/// (`crates/plugin-sdk-macros/src/lib.rs`), so a `String`-returning
/// `tap_item_view` arrives here as a **JSON string literal**: wrapped in quotes,
/// with every inner `"` escaped to `\"`. Appending that text to the page
/// verbatim put a stray quote at each end and a backslash inside every
/// double-quoted attribute, so no plugin could render correct markup
/// (**G-VIEW-OUTPUT-JSON-ENCODED**, Argus M3 friction log).
///
/// Four shapes are accepted, in order, which is what makes the fix additive —
/// a tap that already emitted raw HTML keeps working:
///
/// 1. a **JSON string** — decoded to its contents (the `String`-returning tap,
///    i.e. every view tap the SDK macro serializes today);
/// 2. a **JSON object with an `html` string key** — that key's value, the
///    explicit envelope a tap may adopt to carry structure alongside markup;
/// 3. **JSON `null`** or an all-whitespace output — the empty fragment;
/// 4. **anything else** — returned unchanged, so a tap emitting raw HTML (which
///    is not valid JSON) is unaffected.
///
/// A JSON object *without* an `html` key is deliberately left in shape 4 and
/// returned unchanged rather than dropped: silently swallowing a plugin's output
/// would be a worse failure than showing it.
pub fn decode_view_output(raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }

    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::String(html)) => html,
        Ok(serde_json::Value::Null) => String::new(),
        Ok(serde_json::Value::Object(ref map)) => match map.get("html").and_then(|v| v.as_str()) {
            Some(html) => html.to_string(),
            None => raw.to_string(),
        },
        _ => raw.to_string(),
    }
}

/// Build the text the kernel embeds for an item: its title plus the text
/// values of its fields, HTML stripped, newline-joined. A single
/// whole-item representation keeps one embedding per item.
///
/// `pub(crate)` so the native embed drain ([`crate::cron`]) hashes and embeds
/// the identical text the save path captured (P11f coalescing).
pub(crate) fn item_embedding_text(item: &Item) -> String {
    let mut parts = vec![item.title.clone()];
    if let Some(obj) = item.fields.as_object() {
        for value in obj.values() {
            // Field values are either `{ "value": "..." }` or a plain string.
            let text = value
                .get("value")
                .and_then(|v| v.as_str())
                .or_else(|| value.as_str());
            if let Some(t) = text
                && !t.is_empty()
            {
                parts.push(strip_html(t));
            }
        }
    }
    parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strip all HTML tags from a string, keeping only text content. Uses
/// `ammonia` with an empty allowed-tag set so unclosed tags, comments, and
/// self-closing tags are handled correctly.
///
/// `pub(crate)` so [`crate::content::page_meta`] derives its meta description
/// from the same text extraction the embedding text uses. Note that the output
/// is HTML-escaped, as it comes back through a serializer: a caller putting it
/// somewhere that escapes again has to decode first.
pub(crate) fn strip_html(s: &str) -> String {
    ammonia::Builder::default()
        .tags(std::collections::HashSet::new())
        .clean(s)
        .to_string()
}

/// Maximum entries in the item cache.
const MAX_CAPACITY: u64 = 50_000;

/// Maximum entries in the stage cache (stages are few and rarely change).
const STAGE_CACHE_CAPACITY: u64 = 16;

/// TTL for stage cache entries. Short because stage visibility changes are
/// security-relevant — a stage changed from Public to Internal should take
/// effect quickly. 30 seconds is a balance between avoiding per-item DB
/// queries and limiting the window of stale visibility data.
const STAGE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Service for item CRUD operations with tap integration.
#[derive(Clone)]
pub struct ItemService {
    inner: Arc<ItemServiceInner>,
}

struct ItemServiceInner {
    pool: PgPool,
    dispatcher: Arc<TapDispatcher>,
    /// Services template for tap dispatch — cloned per invocation.
    tap_services: RequestServices,
    cache: Cache<Uuid, Item>,
    /// Cached stage lookups — stages rarely change and there are typically only 3.
    stage_cache: Cache<Uuid, Stage>,
    /// Cached field access decisions keyed by "perm_hash:item_type:field_name:operation".
    /// Deny-wins aggregation result, 5-minute TTL. **Shared** (`Arc`) with the
    /// `tap_services` `RequestServices` so a plugin config write flushes the same
    /// cache these read paths consult (amendment α).
    field_access_cache: Arc<Cache<String, bool>>,
    /// Optional embedding provider used to (re)generate item embeddings on
    /// index. `None` in builds without an AI provider wired (e.g. tests).
    ai_providers: Option<Arc<AiProviderService>>,
    /// Optional pgvector store the kernel writes item embeddings into. Typed as
    /// the concrete `PgVectorStore` because the [`VectorStore`] trait is not
    /// object-safe (see its `async_fn_in_trait` allow); the trait stays the
    /// swap seam.
    vector_store: Option<Arc<PgVectorStore>>,
}

/// A translation record for an item in a specific language.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct ItemTranslation {
    /// The item this translation belongs to.
    pub item_id: Uuid,
    /// The language code (e.g., "fr", "de").
    pub language: String,
    /// The translated title.
    pub title: String,
    /// Translated field values (JSONB overlay).
    pub fields: serde_json::Value,
    /// Unix timestamp when the translation was created.
    pub created: i64,
    /// Unix timestamp when the translation was last changed.
    pub changed: i64,
}

/// Input for checking item access.
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs` for
/// plugin-side deserialization. The kernel serializes this; plugins deserialize
/// it. If you change fields here, update the SDK copy to match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemAccessInput {
    pub item_id: Uuid,
    pub item_type: String,
    pub author_id: Uuid,
    pub operation: String,
    pub user_id: Uuid,

    /// Whether the user is authenticated (false = anonymous).
    #[serde(default)]
    pub user_authenticated: bool,

    /// The user's granted permissions (empty for anonymous).
    #[serde(default)]
    pub user_permissions: Vec<String>,

    /// Stage UUID (None if item has no explicit stage).
    #[serde(default)]
    pub stage_id: Option<Uuid>,

    /// Stage machine name (e.g., "incoming", "curated", "live").
    #[serde(default)]
    pub stage_machine_name: Option<String>,
}

/// Viewer context carried in a [`FieldAccessBatchInput`].
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs`
/// (`FieldAccessUser`). The kernel serializes this; plugins deserialize it. If
/// you change fields here, update the SDK copy to match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldAccessUser {
    /// The viewer's user id (nil for anonymous).
    pub user_id: Uuid,
    /// Whether the viewer is authenticated (false = anonymous).
    #[serde(default)]
    pub authenticated: bool,
    /// The viewer's granted permissions (empty for anonymous).
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Batch input for `tap_field_access` — the frozen 1.0 field-access payload.
///
/// One dispatch carries the viewer, exactly one `item_type`, one `operation`
/// (`"view"` / `"edit"`, mirroring [`ItemAccessInput::operation`]), and a batch
/// of `fields`. Granularity is deliberately **type-level** (a decision is a pure
/// function of `(permissions, item_type, field, operation)`), which is what lets
/// the kernel batch per result-set-per-type and cache the result. See design
/// `fr-8-field-access-and-retrieval-layer.md` §2.
///
/// post-1.0: an additive optional `item` block extends this to per-item
/// granularity (serde-default `Option`) without breaking the frozen schema.
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs`
/// (`FieldAccessBatchInput`). The kernel serializes this; plugins deserialize it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldAccessBatchInput {
    /// The viewer context.
    pub user: FieldAccessUser,
    /// The single item type all `fields` belong to.
    pub item_type: String,
    /// The operation being gated: `"view"` or `"edit"`.
    pub operation: String,
    /// The batch of field names to decide.
    pub fields: Vec<String>,
}

/// Batch result for `tap_field_access` — the frozen 1.0 field-access result.
///
/// Maps each requested field name to a [`FieldAccessResult`]. A field **absent**
/// from `decisions` is treated as [`FieldAccessResult::NoOpinion`]. The kernel
/// aggregates deny-wins across plugins with a fail-open default (design §2.3).
///
/// SYNC: An identical struct exists in `crates/plugin-sdk/src/types.rs`
/// (`FieldAccessBatchResult`). Plugins serialize this; the kernel deserializes it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct FieldAccessBatchResult {
    /// Per-field decision map. Absent field ⇒ `NoOpinion`.
    pub decisions: HashMap<String, FieldAccessResult>,
}

/// Aggregate `tap_field_access` plugin results **deny-wins** with a **fail-open**
/// default, per design §2.3.
///
/// For each requested field, fold every implementing plugin's vote:
/// 1. any `Deny` ⇒ Deny;
/// 2. else any `Allow` ⇒ Allow;
/// 3. else (all `NoOpinion`, no implementer, or all outputs unparseable) ⇒ the
///    shipped default **ALLOW** (fail-open — field access refines item access).
///
/// A plugin whose output does not parse as a [`FieldAccessBatchResult`]
/// contributes no vote (mirrors the dispatcher skipping a trapped handler). The
/// returned map has an entry for every field in `fields`.
fn aggregate_field_decisions(fields: &[String], results: &[TapResult]) -> HashMap<String, bool> {
    // Parse each plugin's batch result once; unparseable output = no vote.
    let parsed: Vec<FieldAccessBatchResult> = results
        .iter()
        .filter_map(|r| serde_json::from_str::<FieldAccessBatchResult>(&r.output).ok())
        .collect();

    let mut out = HashMap::with_capacity(fields.len());
    for field in fields {
        // Deny-wins: a field is hidden only if some plugin explicitly denies it.
        // An explicit `Allow`, a `NoOpinion`/absent vote, no implementer, and an
        // unparseable output all resolve to the same fail-open default (visible),
        // so the aggregated boolean reduces to "no plugin said Deny".
        let denied = parsed
            .iter()
            .any(|p| matches!(p.decisions.get(field), Some(FieldAccessResult::Deny)));
        out.insert(field.clone(), !denied);
    }
    out
}

/// Remove from a `fields` JSON object every key whose decision is `false`
/// (denied). A key absent from `decisions` is kept (fail-open default). No-op if
/// `fields` is not a JSON object. Pure — the field-filtering primitive the seam
/// helpers share.
fn apply_field_decisions(fields: &mut serde_json::Value, decisions: &HashMap<String, bool>) {
    if let Some(obj) = fields.as_object_mut() {
        obj.retain(|k, _| decisions.get(k).copied().unwrap_or(true));
    }
}

impl ItemService {
    /// Create a new item service.
    ///
    /// `ai_providers` and `vector_store` drive kernel-side embedding
    /// (re)generation on `tap_item_update_index`. Pass `None` for both to
    /// disable kernel embedding generation (the tap still fires for plugins).
    pub fn new(
        pool: PgPool,
        dispatcher: Arc<TapDispatcher>,
        tap_services: RequestServices,
        ttl: Duration,
        ai_providers: Option<Arc<AiProviderService>>,
        vector_store: Option<Arc<PgVectorStore>>,
    ) -> Self {
        // Share the field-access cache with `tap_services` so the `variables::set`
        // host path can flush the very cache these read paths consult (amendment
        // α). `AppState` sets the one shared instance on the tap_services template.
        let field_access_cache = tap_services.field_access_cache.clone();
        Self {
            inner: Arc::new(ItemServiceInner {
                pool,
                dispatcher,
                tap_services,
                cache: Cache::builder()
                    .max_capacity(MAX_CAPACITY)
                    .time_to_live(ttl)
                    .build(),
                stage_cache: Cache::builder()
                    .max_capacity(STAGE_CACHE_CAPACITY)
                    .time_to_live(STAGE_CACHE_TTL)
                    .build(),
                field_access_cache,
                ai_providers,
                vector_store,
            }),
        }
    }

    /// Build a `RequestState` for tap dispatch with the user context and services.
    fn tap_state(&self, user: &UserContext) -> RequestState {
        RequestState::new(user.clone(), self.inner.tap_services.clone())
    }

    /// Create a new item with tap_item_presave and tap_item_insert invocations.
    ///
    /// The presave tap fires before the item is persisted, allowing plugins
    /// to modify fields (e.g., AI content enrichment). The insert tap fires
    /// after persistence for post-save side effects.
    pub async fn create(&self, mut input: CreateItem, user: &UserContext) -> Result<Item> {
        // Invoke tap_item_presave — plugins can modify fields before save.
        // Serialize the input as a JSON object so plugins can read/modify fields.
        let presave_json = serde_json::json!({
            "item_type": input.item_type,
            "title": input.title,
            "fields": input.fields,
            "status": input.status,
        });
        let presave_input = serde_json::to_string(&presave_json).context("serialize presave")?;
        let presave_state = self.tap_state(user);

        let presave_results = self
            .inner
            .dispatcher
            .dispatch("tap_item_presave", &presave_input, presave_state)
            .await;

        // Apply presave modifications — if any plugin returned modified fields,
        // merge them into the input. Last plugin wins for each field.
        for result in presave_results {
            if let Ok(modified) = serde_json::from_str::<serde_json::Value>(&result.output)
                && let Some(fields) = modified.get("fields")
                && let Some(obj) = fields.as_object()
            {
                let input_fields = input
                    .fields
                    .get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if let Some(input_obj) = input_fields.as_object_mut() {
                    for (k, v) in obj {
                        input_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Create the item in the database
        let item = Item::create(&self.inner.pool, input).await?;

        // Invoke tap_item_insert for post-insert taps
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = self.tap_state(user);

        let _results = self
            .inner
            .dispatcher
            .dispatch("tap_item_insert", &item_json, state)
            .await;

        // Tap errors are logged by the dispatcher

        // Fire tap_item_update_index and regenerate the kernel embedding so the
        // new item is searchable via the SemanticSimilarity gather operator.
        self.index_item(&item, user).await;

        // Maintain the file→item reference index (FR-8 Story 3.5).
        self.sync_file_references(item.id, &item.fields).await;

        info!(item_id = %item.id, item_type = %item.item_type, "item created");
        Ok(item)
    }

    /// Load an item by ID.
    pub async fn load(&self, id: Uuid) -> Result<Option<Item>> {
        // Check cache first
        if let Some(item) = self.inner.cache.get(&id) {
            return Ok(Some(item));
        }

        // Load from database
        let item = Item::find_by_id(&self.inner.pool, id).await?;

        // Cache if found
        if let Some(ref i) = item {
            self.inner.cache.insert(id, i.clone());
        }

        Ok(item)
    }

    /// Load an item by ID with stage hierarchy overlay.
    ///
    /// Tries to find the item in the nearest stage in the ancestry chain.
    /// For example, with `stage_ids = ["review", "draft", "live"]`, returns
    /// the first match found when checking stage_id = review, then draft, then live.
    ///
    /// Falls back to `load()` if the item exists but isn't in any of the given stages
    /// (e.g., it was loaded by a direct UUID link).
    pub async fn load_with_overlay(&self, id: Uuid, stage_ids: &[Uuid]) -> Result<Option<Item>> {
        // Check cache first (cache is stage-agnostic — items have single stage_id)
        if let Some(item) = self.inner.cache.get(&id) {
            // Verify the item's stage is in our overlay list
            if stage_ids.contains(&item.stage_id) {
                return Ok(Some(item));
            }
        }

        // Load from database — the item has a single stage_id
        let item = Item::find_by_id(&self.inner.pool, id).await?;

        if let Some(ref i) = item {
            // Only return if the item is in one of the visible stages
            if stage_ids.contains(&i.stage_id) {
                self.inner.cache.insert(id, i.clone());
                return Ok(Some(i.clone()));
            }
        }

        Ok(None)
    }

    /// Load a translation for an item in a specific language.
    ///
    /// Returns `None` if no translation exists for the given language.
    pub async fn load_translation(
        &self,
        item_id: Uuid,
        language: &str,
    ) -> Result<Option<ItemTranslation>> {
        let row = sqlx::query_as::<_, ItemTranslation>(
            "SELECT item_id, language, title, fields, created, changed \
             FROM item_translation WHERE item_id = $1 AND language = $2",
        )
        .bind(item_id)
        .bind(language)
        .fetch_optional(&self.inner.pool)
        .await
        .context("failed to load item translation")?;

        Ok(row)
    }

    /// List all translations that exist for an item, ordered by language.
    ///
    /// Returns `(language, title)` pairs for use in admin translation listing.
    pub async fn list_translations(&self, item_id: Uuid) -> Result<Vec<(String, String)>> {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT language, title FROM item_translation \
             WHERE item_id = $1 ORDER BY language",
        )
        .bind(item_id)
        .fetch_all(&self.inner.pool)
        .await
        .context("failed to list translations")?;
        Ok(rows)
    }

    /// List the languages an item has a translation in, ordered.
    ///
    /// Existence only: the caller wants to know which languages to offer, not
    /// what they say.
    pub async fn translated_languages(&self, item_id: Uuid) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT language FROM item_translation WHERE item_id = $1 ORDER BY language",
        )
        .bind(item_id)
        .fetch_all(&self.inner.pool)
        .await
        .context("failed to list translated languages")?;
        Ok(rows.into_iter().map(|(lang,)| lang).collect())
    }

    /// Titles for many items at once: the translated one, and the default-language
    /// one it replaces.
    ///
    /// Both halves in one query because both are needed together. A menu label is
    /// only worth translating when it currently mirrors the target's own title,
    /// and answering "does it?" needs the default title beside the translated one.
    /// Menus render on every page, so this is one query for a whole menu rather
    /// than two per link.
    ///
    /// Holds only the items that have a translation in `language`.
    pub async fn translated_titles_for(
        &self,
        item_ids: &[Uuid],
        language: &str,
    ) -> Result<HashMap<Uuid, (String, String)>> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT t.item_id, t.title, i.title \
             FROM item_translation t \
             JOIN item i ON i.id = t.item_id \
             WHERE t.item_id = ANY($1) AND t.language = $2",
        )
        .bind(item_ids)
        .bind(language)
        .fetch_all(&self.inner.pool)
        .await
        .context("failed to load translated titles")?;

        Ok(rows
            .into_iter()
            .map(|(id, translated, original)| (id, (translated, original)))
            .collect())
    }

    /// Load an item and invoke tap_item_view for rendering.
    ///
    /// Enforces item-level access, then **drops the fields this viewer may not
    /// see** (FR-8 Story 3.6) before the item reaches `tap_item_view` or the SSR
    /// render tree — so restricted fields never render and are never handed to a
    /// view-transform plugin.
    pub async fn load_for_view(
        &self,
        id: Uuid,
        user: &UserContext,
    ) -> Result<Option<(Item, Vec<String>)>> {
        let Some(mut item) = self.load(id).await? else {
            return Ok(None);
        };

        // Check access
        if !self.check_access(&item, "view", user).await? {
            return Ok(None); // Return None for access denied (shows as 404)
        }

        // Field-level access: drop non-accessible fields before render/tap.
        self.filter_item_fields(&mut item, user, "view").await;

        // Invoke tap_item_view for rendering transformations
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = self.tap_state(user);

        let results = self
            .inner
            .dispatcher
            .dispatch("tap_item_view", &item_json, state)
            .await;

        // Collect render outputs, decoding the tap-macro's JSON envelope
        // (G-VIEW-OUTPUT-JSON-ENCODED — see `decode_view_output`).
        let render_outputs: Vec<String> = results
            .into_iter()
            .map(|r| decode_view_output(&r.output))
            .filter(|html| !html.is_empty())
            .collect();

        Ok(Some((item, render_outputs)))
    }

    /// Update an item with tap_item_update invocation.
    pub async fn update(
        &self,
        id: Uuid,
        mut input: UpdateItem,
        user: &UserContext,
    ) -> Result<Option<Item>> {
        // Load existing item
        let Some(existing) = self.load(id).await? else {
            return Ok(None);
        };

        // Check access
        if !self.check_access(&existing, "edit", user).await? {
            anyhow::bail!("access denied");
        }

        // Invoke tap_item_presave — plugins can modify fields before save.
        let presave_json = serde_json::json!({
            "item_type": existing.item_type,
            "title": input.title.as_deref().unwrap_or(&existing.title),
            "fields": input.fields.as_ref().unwrap_or(&existing.fields),
            "status": input.status.unwrap_or(existing.status),
        });
        let presave_input = serde_json::to_string(&presave_json).context("serialize presave")?;
        let presave_state = self.tap_state(user);

        let presave_results = self
            .inner
            .dispatcher
            .dispatch("tap_item_presave", &presave_input, presave_state)
            .await;

        // Apply presave modifications — merge plugin-returned fields into input.
        for result in presave_results {
            if let Ok(modified) = serde_json::from_str::<serde_json::Value>(&result.output)
                && let Some(fields) = modified.get("fields")
                && let Some(obj) = fields.as_object()
            {
                let input_fields = input.fields.get_or_insert_with(|| existing.fields.clone());
                if let Some(input_obj) = input_fields.as_object_mut() {
                    for (k, v) in obj {
                        input_obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        // Update the item
        let item = Item::update(&self.inner.pool, id, user.id, input).await?;

        if let Some(ref i) = item {
            // Invoke tap_item_update
            let item_json = serde_json::to_string(i).context("serialize item")?;
            let state = self.tap_state(user);

            let _results = self
                .inner
                .dispatcher
                .dispatch("tap_item_update", &item_json, state)
                .await;

            // Tap errors are logged by the dispatcher

            // Fire tap_item_update_index and regenerate the kernel embedding so
            // the updated item's searchable vector reflects the new content.
            self.index_item(i, user).await;

            // Maintain the file→item reference index (FR-8 Story 3.5).
            self.sync_file_references(i.id, &i.fields).await;

            // Invalidate cache
            self.invalidate(id);

            info!(item_id = %id, "item updated");
        }

        Ok(item)
    }

    /// Dispatch `tap_item_update_index` and regenerate the kernel's own
    /// embedding for `item`.
    ///
    /// **OQ-5 / FR-8 Story 3.7 forward-compat:** this dispatch is the extension
    /// point for post-1.0 per-user/per-tier index *scoping* (Cairn) — a plugin
    /// can partition or field-restrict the indexed document per tier here
    /// without a new tap and without an index rebuild. The 1.0 search-result
    /// field enforcement (snippet redaction) rides the read path in
    /// [`Self::filter_search_results`]; this write-path hook is left as the seam
    /// for the ranked-index partitioning that closes the inference channel.
    ///
    /// The tap fires regardless of pgvector availability so plugins can perform
    /// their own (non-pgvector) indexing.
    ///
    /// **Embedding is async-by-default (P11f / D-51, D-52), reversing PF-4
    /// sub-decision 3's synchronous best-effort.** For a content type on the
    /// default (async) path, the kernel *enqueues* an embed job on queue v2 and
    /// marks the item `pending` — one cheap INSERT in place of a blocking
    /// provider round-trip — so the save path no longer pays the AI latency/cost
    /// tax on every write. The job runs under the background AI principal, with
    /// queue-v2 retry/backoff, and a terminal failure dead-letters with the
    /// error recorded as the item's observable `failed` state (the old
    /// silent-drop dies here). A content type listed in [`EmbedPolicy`]'s
    /// `sync_types` opts out and keeps the pre-P11f synchronous best-effort
    /// embed below (unchanged mechanics).
    ///
    /// The findable-by-text half of the freshness contract is unaffected either
    /// way: the `search_vector` `tsvector` is maintained by a DB trigger inside
    /// the save transaction.
    async fn index_item(&self, item: &Item, user: &UserContext) {
        // Fire the indexing tap (plugins may react to reindex).
        let item_json = match serde_json::to_string(item) {
            Ok(j) => j,
            Err(e) => {
                warn!(item_id = %item.id, error = %e, "failed to serialize item for index tap");
                return;
            }
        };
        let state = self.tap_state(user);
        let _results = self
            .inner
            .dispatcher
            .dispatch("tap_item_update_index", &item_json, state)
            .await;

        // Drive kernel-side embedding (re)generation so kernel-managed items
        // become searchable via the SemanticSimilarity gather operator. Both an
        // embedding provider and a vector store must be wired; otherwise there
        // is nothing to embed into (the same gate the sync path always had).
        let (Some(_ai), Some(store)) = (&self.inner.ai_providers, &self.inner.vector_store) else {
            return;
        };

        let text = item_embedding_text(item);
        if text.trim().is_empty() {
            return;
        }

        // Policy (D-51): async is the default; a type in `sync_types` opts out.
        let policy = EmbedPolicy::load(&self.inner.pool).await;
        if policy.is_async(&item.item_type) {
            // Async path: enqueue a kernel embed job + mark the item pending. No
            // provider call and no pgvector dependency on the save path — the
            // drain resolves both. Enqueue is decoupled from live pgvector
            // availability: the durable "this item needs embedding" intent is
            // recorded now; the drain decides what to do with the backend state
            // it sees.
            if let Err(e) = embed_index::enqueue_embed_job(
                &self.inner.pool,
                item.id,
                &embed_index::embed_content_hash(&text),
            )
            .await
            {
                warn!(item_id = %item.id, error = %e, "failed to enqueue item embed job");
            }
            return;
        }

        // Opt-out (synchronous) path: unchanged embed mechanics, now with an
        // observable state write so failures are no longer silent.
        self.embed_sync(item, &text, store.clone()).await;
    }

    /// Synchronous best-effort embed for a content type that opted out of the
    /// async path (P11f / D-51). Mechanics are unchanged from the pre-P11f
    /// path (pgvector-gated, transient failures logged not propagated); the only
    /// addition is recording the observable `item_embed_status` outcome.
    async fn embed_sync(&self, item: &Item, text: &str, store: Arc<PgVectorStore>) {
        let Some(ai) = &self.inner.ai_providers else {
            return;
        };
        if !store.is_available().await {
            return;
        }

        // Item-scoped truncation warning: `embed` caps over-budget input at the
        // choke point (carrying only lengths), so surface *which* item is
        // affected here where the item_id is in scope.
        let text_chars = text.chars().count();
        if text_chars > EMBEDDING_INPUT_MAX_CHARS {
            warn!(
                item_id = %item.id,
                text_chars,
                budget_chars = EMBEDDING_INPUT_MAX_CHARS,
                "item embedding text exceeds budget; embedding will be truncated to leading content"
            );
        }

        match ai.embed(text).await {
            Ok(Some(result)) => {
                if let Err(e) = store
                    .store_embedding(item.id, KERNEL_INDEX_FIELD, &result.model, &result.vector)
                    .await
                {
                    warn!(item_id = %item.id, error = %e, "failed to store item embedding");
                    let _ =
                        embed_index::mark_failed(&self.inner.pool, item.id, &e.to_string()).await;
                } else {
                    let _ = embed_index::mark_indexed(
                        &self.inner.pool,
                        item.id,
                        &result.model,
                        &embed_index::embed_content_hash(text),
                    )
                    .await;
                }
            }
            // No embedding provider configured — nothing to store.
            Ok(None) => {}
            Err(e) => {
                warn!(item_id = %item.id, error = %e, "failed to generate item embedding");
                let _ = embed_index::mark_failed(&self.inner.pool, item.id, &e.to_string()).await;
            }
        }
    }

    /// Delete an item with tap_item_delete invocation.
    pub async fn delete(&self, id: Uuid, user: &UserContext) -> Result<bool> {
        // Load item
        let Some(item) = self.load(id).await? else {
            return Ok(false);
        };

        // Check access
        if !self.check_access(&item, "delete", user).await? {
            anyhow::bail!("access denied");
        }

        // Invoke tap_item_delete (can abort deletion)
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        let state = self.tap_state(user);

        let _results = self
            .inner
            .dispatcher
            .dispatch("tap_item_delete", &item_json, state)
            .await;

        // Tap errors are logged by the dispatcher

        // Delete from database
        let deleted = Item::delete(&self.inner.pool, id).await?;

        if deleted {
            // Invalidate cache
            self.invalidate(id);
            info!(item_id = %id, "item deleted");
        }
        // The item's `file_reference` rows are removed by the FK cascade
        // (`ON DELETE CASCADE` on `item_id`) when the item row is deleted above.

        Ok(deleted)
    }

    /// Rebuild the file→item reference index for `item_id` from its `fields`
    /// (FR-8 Story 3.5). Called after every item create/update. Extracts the
    /// files the item references (`local://` URIs and `/files/` public URLs),
    /// resolves them against `file_managed`, and replaces the item's
    /// `file_reference` rows.
    ///
    /// Best-effort: a failure is logged, never propagated — the item is already
    /// saved. A reference stored under a non-default `FILES_URL` (an absolute
    /// CDN URL, not `/files/…`) is **not** indexed by the runtime extractor;
    /// such files are not served through this kernel's `/files/{path}` route
    /// either, so there is no serve-path enforcement gap. The backfill migration
    /// is broader (a literal path-substring match) and catches any embedded
    /// form for already-stored content.
    async fn sync_file_references(&self, item_id: Uuid, fields: &serde_json::Value) {
        if let Err(e) = self.sync_file_references_inner(item_id, fields).await {
            warn!(item_id = %item_id, error = %e, "failed to sync file references");
        }
    }

    async fn sync_file_references_inner(
        &self,
        item_id: Uuid,
        fields: &serde_json::Value,
    ) -> Result<()> {
        let uris = super::file_refs::extract_file_uris(fields);
        let mut tx = self.inner.pool.begin().await?;
        sqlx::query("DELETE FROM file_reference WHERE item_id = $1")
            .bind(item_id)
            .execute(&mut *tx)
            .await?;
        if !uris.is_empty() {
            sqlx::query(
                "INSERT INTO file_reference (file_id, item_id) \
                 SELECT id, $1 FROM file_managed WHERE uri = ANY($2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(item_id)
            .bind(&uris)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// FR-8 Story 3.5 — decide whether the file at `uri` may be streamed to
    /// `viewer` under the any-referencing policy (D-29).
    ///
    /// Servable iff: the viewer is an admin; **or** any item referencing the
    /// file passes item-level `check_access("view")`; **or** the file is
    /// referenced by no item (orphan / in-flight upload) and the viewer is its
    /// uploader (`file_managed.owner_id`). An **unmanaged** path (no
    /// `file_managed` row) is not access-controlled here and serves as before.
    /// Returns `false` otherwise — the caller maps that to the surface's
    /// existing not-found posture (404), leaking no existence.
    pub async fn can_serve_file(&self, uri: &str, viewer: &UserContext) -> Result<bool> {
        let managed: Option<(Uuid, Uuid)> =
            sqlx::query_as("SELECT id, owner_id FROM file_managed WHERE uri = $1")
                .bind(uri)
                .fetch_optional(&self.inner.pool)
                .await?;
        let Some((file_id, owner_id)) = managed else {
            // Not a managed file — no ownership/reference model to enforce.
            return Ok(true);
        };
        if viewer.is_admin() {
            return Ok(true);
        }
        let item_ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT item_id FROM file_reference WHERE file_id = $1")
                .bind(file_id)
                .fetch_all(&self.inner.pool)
                .await?;
        if item_ids.is_empty() {
            // Orphan / in-flight upload: uploader only (admin handled above).
            return Ok(viewer.authenticated && viewer.id == owner_id);
        }
        // Any-referencing: servable if the viewer can see any referencing item.
        for item_id in item_ids {
            if let Some(item) = self.load(item_id).await?
                && self.check_access(&item, "view", viewer).await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// FR-8 Story 3.7 — route search results through the shared access seam.
    ///
    /// Search's own SQL applies only a coarse `status`/`author`/`stage` filter
    /// and builds `ts_headline` snippets from the raw `field_body`, with no
    /// field-level access. This runs each result row through the same seam
    /// REST/SSR/gather use: (1) drop any result the viewer cannot see at the
    /// item level (`check_access("view")`) — this catches plugin `tap_item_access`
    /// denies the coarse SQL filter misses; (2) **redact the snippet** (the
    /// `ts_headline` source) for any result whose type denies the viewer
    /// `field_body`, so a restricted field cannot leak through the highlight.
    /// The field decision is **batched per distinct type** (N+1-free); the
    /// per-result cost is one `check_access`, bounded to the page.
    ///
    /// Per-viewer/per-tier index *scoping* (OQ-5 / Cairn — excluding a
    /// restricted field from the ranked `search_vector` itself, not just the
    /// snippet) is a post-1.0 partitioning concern whose extension point is the
    /// already-dispatched `tap_item_update_index` (in the private `index_item`);
    /// it needs no new dispatch. This method closes the 1.0 leak (visible snippet);
    /// the ranking-inference channel is left for that per-tier index.
    pub async fn filter_search_results(
        &self,
        results: Vec<crate::search::SearchResult>,
        viewer: &UserContext,
    ) -> Vec<crate::search::SearchResult> {
        // Tier 1 — item-level access over each result (bounded to the page).
        let mut visible: Vec<crate::search::SearchResult> = Vec::with_capacity(results.len());
        for r in results {
            match self.load(r.id).await {
                Ok(Some(item))
                    if self
                        .check_access(&item, "view", viewer)
                        .await
                        .unwrap_or(false) =>
                {
                    visible.push(r);
                }
                _ => {} // missing or denied — absent from results entirely
            }
        }

        // Tier 2 — redact the snippet where `field_body` is denied, one field
        // decision per distinct type.
        let field_body = ["field_body".to_string()];
        let mut redact_by_type: HashMap<String, bool> = HashMap::new();
        for r in &visible {
            if !redact_by_type.contains_key(&r.item_type) {
                let decisions = self
                    .field_access_decisions(viewer, &r.item_type, &field_body, "view")
                    .await;
                let allowed = decisions.get("field_body").copied().unwrap_or(true);
                redact_by_type.insert(r.item_type.clone(), !allowed);
            }
        }
        for r in &mut visible {
            if redact_by_type.get(&r.item_type).copied().unwrap_or(false) {
                r.snippet = None;
            }
        }

        visible
    }

    /// Check if a user has access to perform an operation on an item.
    ///
    /// Access resolution order:
    /// 1. Admin bypass (always allowed)
    /// 2. Stage visibility — anonymous users are denied on internal stages
    /// 3. Published fast-path — public-stage + published + "access content"
    /// 4. Plugin `tap_item_access` — Deny wins, then Grant
    /// 5. Role-based fallback — generic and type-specific permission patterns
    ///
    /// **Design note:** The published-view fast-path (step 3) runs before plugin
    /// dispatch. This means plugins cannot Deny published items on public stages
    /// via `tap_item_access` for "view" operations. This is intentional — it
    /// optimizes the overwhelmingly common case (anonymous/authenticated users
    /// viewing Live content) and matches the CMS convention that "published =
    /// publicly visible." If a plugin needs to restrict specific Live items,
    /// it should use item status (unpublish) rather than access denial.
    pub async fn check_access(
        &self,
        item: &Item,
        operation: &str,
        user: &UserContext,
    ) -> Result<bool> {
        // 1. Admin always has access
        if user.is_admin() {
            return Ok(true);
        }

        // 2. Resolve stage visibility. Use cached lookups — stages are few
        //    and rarely change, but check_access runs on every item view.
        let (is_internal, stage_machine_name) = if item.stage_id == LIVE_STAGE_ID {
            // Live stage is always public — no DB lookup needed.
            (false, Some("live".to_string()))
        } else if let Some(stage) = self.inner.stage_cache.get(&item.stage_id) {
            (
                stage.visibility == StageVisibility::Internal,
                Some(stage.machine_name.clone()),
            )
        } else {
            match Stage::find_by_id(&self.inner.pool, item.stage_id).await {
                Ok(Some(stage)) => {
                    let internal = stage.visibility == StageVisibility::Internal;
                    let name = stage.machine_name.clone();
                    self.inner.stage_cache.insert(item.stage_id, stage);
                    (internal, Some(name))
                }
                Ok(None) => {
                    warn!(
                        stage_id = %item.stage_id,
                        item_id = %item.id,
                        "stage not found for item, treating as public"
                    );
                    (false, None)
                }
                Err(e) => {
                    // DB errors must not silently upgrade access — deny and log.
                    warn!(
                        stage_id = %item.stage_id,
                        item_id = %item.id,
                        error = %e,
                        "failed to resolve stage visibility, denying access"
                    );
                    return Ok(false);
                }
            }
        };

        // Anonymous users cannot access items on internal stages
        if is_internal && !user.authenticated {
            return Ok(false);
        }

        // 3. Published content on public/live stages is viewable by anyone
        //    with "access content". Skip this fast-path for internal stages
        //    so plugins can enforce stage-specific permissions.
        if operation == "view"
            && !is_internal
            && item.is_published()
            && user.has_permission("access content")
        {
            return Ok(true);
        }

        // 4. Build access check input with full context for plugins.
        //    stage_id and stage_machine_name are Option in the SDK for
        //    forward-compatibility, but the kernel always populates them
        //    here since every item has a stage_id.
        let input = ItemAccessInput {
            item_id: item.id,
            item_type: item.item_type.clone(),
            author_id: item.author_id,
            operation: operation.to_string(),
            user_id: user.id,
            user_authenticated: user.authenticated,
            user_permissions: user.permissions.clone(),
            stage_id: Some(item.stage_id),
            stage_machine_name,
        };

        let input_json = serde_json::to_string(&input).context("serialize access input")?;
        let state = self.tap_state(user);

        // Invoke tap_item_access
        let results = self
            .inner
            .dispatcher
            .dispatch("tap_item_access", &input_json, state)
            .await;

        // Aggregate results: Deny wins, then Grant, else Neutral
        let mut has_grant = false;

        for result in results {
            if let Ok(access) = serde_json::from_str::<AccessResult>(&result.output) {
                match access {
                    AccessResult::Deny => return Ok(false),
                    AccessResult::Grant => has_grant = true,
                    AccessResult::Neutral => {}
                }
            }
        }

        // If any plugin granted, allow
        if has_grant {
            return Ok(true);
        }

        // 5. Fall back to role-based permissions. Check both type-specific and
        // generic patterns, plus own-vs-any variants:
        //   "{op} any content"             — generic, any author
        //   "{op} own content"             — generic, own items only
        //   "{op} any {type}"              — type-specific, any author
        //   "{op} own {type}"              — type-specific, own items only
        //   "{op} {type} content"          — legacy pattern
        let is_own = user.id == item.author_id;
        let checks: &[String] = &[
            format!("{operation} any content"),
            format!("{operation} any {}", item.item_type),
            format!("{operation} {} content", item.item_type),
        ];
        for perm in checks {
            if user.has_permission(perm) {
                return Ok(true);
            }
        }
        // "own" variants only apply when the user authored the item
        if is_own {
            let own_checks: &[String] = &[
                format!("{operation} own content"),
                format!("{operation} own {}", item.item_type),
            ];
            for perm in own_checks {
                if user.has_permission(perm) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Hash a user's permission set into the compact `perm_hash` used in the
    /// field-access cache key. Sorting first makes the hash order-independent so
    /// two users with the same permissions (in any order) share cache entries;
    /// a permission change yields a different hash ⇒ an immediate cache miss.
    fn perm_hash(user: &UserContext) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut perms = user.permissions.clone();
        perms.sort_unstable();
        let mut hasher = std::hash::DefaultHasher::new();
        perms.hash(&mut hasher);
        hasher.finish()
    }

    /// Decide access for a **batch** of fields in one `tap_field_access`
    /// dispatch — the type-level, deny-wins, fail-open core (design §2).
    ///
    /// Admin bypasses entirely. Cache-hit fields are served from the perm-hash
    /// cache; the remaining (missed) fields are decided by a **single** batched
    /// dispatch whose result deny-wins-aggregates (see `aggregate_field_decisions`)
    /// with a fail-open default, and **every** missed field's decision is written
    /// back to the cache in that one pass. Returns a decision for every field in
    /// `field_names`.
    ///
    /// This is the one entry point the seam (Story 3.2) and every surface route
    /// through; per-field [`Self::check_field_access`] is a one-element shim over it.
    pub async fn field_access_decisions(
        &self,
        user: &UserContext,
        item_type: &str,
        field_names: &[String],
        operation: &str,
    ) -> HashMap<String, bool> {
        let mut decisions = HashMap::with_capacity(field_names.len());

        // Admin bypass — every field is visible, no dispatch, no cache traffic.
        if user.is_admin() {
            for name in field_names {
                decisions.insert(name.clone(), true);
            }
            return decisions;
        }

        // Split into cache hits (resolved now) and misses (need a dispatch).
        let perm_hash = Self::perm_hash(user);
        let mut missing: Vec<String> = Vec::new();
        for name in field_names {
            let cache_key = format!("{perm_hash:x}:{item_type}:{name}:{operation}");
            if let Some(allowed) = self.inner.field_access_cache.get(&cache_key) {
                decisions.insert(name.clone(), allowed);
            } else if !missing.contains(name) {
                missing.push(name.clone());
            }
        }

        if missing.is_empty() {
            return decisions;
        }

        // One batched dispatch for all missed fields.
        let input = FieldAccessBatchInput {
            user: FieldAccessUser {
                user_id: user.id,
                authenticated: user.authenticated,
                permissions: user.permissions.clone(),
            },
            item_type: item_type.to_string(),
            operation: operation.to_string(),
            fields: missing.clone(),
        };

        let resolved = match serde_json::to_string(&input) {
            Ok(input_json) => {
                let state = self.tap_state(user);
                let results = self
                    .inner
                    .dispatcher
                    .dispatch("tap_field_access", &input_json, state)
                    .await;
                aggregate_field_decisions(&missing, &results)
            }
            Err(e) => {
                // Serialization cannot fail for these plain types, but if it
                // ever did we fail open (field access refines item access).
                warn!(error = %e, "failed to serialize field-access input; defaulting allow");
                missing.iter().map(|f| (f.clone(), true)).collect()
            }
        };

        // Batch-fill the cache for every missed field and merge into the result.
        for name in &missing {
            let allowed = resolved.get(name).copied().unwrap_or(true);
            let cache_key = format!("{perm_hash:x}:{item_type}:{name}:{operation}");
            self.inner.field_access_cache.insert(cache_key, allowed);
            decisions.insert(name.clone(), allowed);
        }

        decisions
    }

    /// Check if a user can access a specific field (view or edit).
    ///
    /// A one-element shim over [`Self::field_access_decisions`]: dispatches
    /// `tap_field_access` deny-wins with a fail-open default, admin bypasses, and
    /// the decision is cached per `(perm_hash, item_type, field_name, operation)`
    /// for 5 minutes.
    pub async fn check_field_access(
        &self,
        user: &UserContext,
        item_type: &str,
        field_name: &str,
        operation: &str,
    ) -> bool {
        let fields = [field_name.to_string()];
        self.field_access_decisions(user, item_type, &fields, operation)
            .await
            .get(field_name)
            .copied()
            .unwrap_or(true)
    }

    /// Filter a set of field names to only those the user can access.
    ///
    /// **Batched** (Story 3.2 AC-1): one `tap_field_access` dispatch per
    /// `(item_type, operation, missing fields)` via
    /// [`Self::field_access_decisions`], not one dispatch per field. Preserves
    /// input order and drops denied fields. This is the batch field seam every
    /// surface routes through.
    pub async fn accessible_fields(
        &self,
        user: &UserContext,
        item_type: &str,
        field_names: &[String],
        operation: &str,
    ) -> Vec<String> {
        let decisions = self
            .field_access_decisions(user, item_type, field_names, operation)
            .await;
        field_names
            .iter()
            .filter(|name| decisions.get(*name).copied().unwrap_or(true))
            .cloned()
            .collect()
    }

    /// Flush the entire field-access decision cache (design amendment α).
    ///
    /// Called when a plugin config value is written (the `variables::set` host
    /// path) so a field-rule change an admin makes takes effect on the **next**
    /// request rather than riding the ≤5-minute TTL. Blunt whole-cache
    /// invalidation is acceptable: config writes are rare admin actions and moka
    /// coalesces the refill. Because the cache is shared (`Arc`) with
    /// `RequestServices`, this is reachable from the host function.
    pub fn flush_field_access_cache(&self) {
        self.inner.field_access_cache.invalidate_all();
    }

    /// Drop from `item.fields` every field the viewer may not access for
    /// `operation`, using one batched [`Self::field_access_decisions`] call for
    /// the item's type. Item-level access is the caller's responsibility (the
    /// seams below enforce it first).
    async fn filter_item_fields(&self, item: &mut Item, user: &UserContext, operation: &str) {
        let field_names: Vec<String> = match item.fields.as_object() {
            Some(obj) if !obj.is_empty() => obj.keys().cloned().collect(),
            _ => return,
        };
        let decisions = self
            .field_access_decisions(user, &item.item_type, &field_names, operation)
            .await;
        apply_field_decisions(&mut item.fields, &decisions);
    }

    /// Hydrated-item seam (Story 3.2 AC-2): load an item, enforce **item-level**
    /// `check_access`, then drop the fields the viewer may not see, returning the
    /// filtered item. `None` when the item is missing or item-level access is
    /// denied (404 semantics). This is the single destination the REST
    /// `get_item`, MCP `get_item`, and SSR `view_item` paths route through
    /// (Stories 3.3 / 3.6). One item ⇒ one item-access decision + one per-type
    /// field decision; N+1-free.
    pub async fn load_for_view_filtered(
        &self,
        id: Uuid,
        user: &UserContext,
        operation: &str,
    ) -> Result<Option<Item>> {
        let Some(mut item) = self.load(id).await? else {
            return Ok(None);
        };
        if !self.check_access(&item, operation, user).await? {
            return Ok(None);
        }
        self.filter_item_fields(&mut item, user, operation).await;
        Ok(Some(item))
    }

    /// Id-stream seam (Story 3.2 AC-3): given rank-ordered candidate items,
    /// return the page the viewer may see.
    ///
    /// Two tiers: (1) per-item `check_access` over the candidates, bounded to the
    /// candidate set (not the corpus), collecting up to `page_size` visible items
    /// in **rank order**; (2) the field layer — **one** `field_access_decisions`
    /// dispatch per **distinct type** (the union of that type's field names),
    /// applied to every item of that type. N+1-free at the field layer
    /// (O(distinct types)).
    ///
    /// The SQL item-access predicate (first tier) and the over-fetch/backfill
    /// loop that feeds more candidates when the page underfills are **Story 3.4**
    /// (§4); this seam is what 3.4 wraps that loop around.
    pub async fn filter_page_for_view(
        &self,
        candidates: Vec<Item>,
        user: &UserContext,
        operation: &str,
        page_size: usize,
    ) -> Vec<Item> {
        // Tier 1 — per-item access, rank order preserved, bounded to page_size.
        let mut visible: Vec<Item> = Vec::with_capacity(page_size.min(candidates.len()));
        for item in candidates {
            if visible.len() >= page_size {
                break;
            }
            // A DB/stage error denies (matches check_access's conservative posture).
            if self
                .check_access(&item, operation, user)
                .await
                .unwrap_or(false)
            {
                visible.push(item);
            }
        }

        // Tier 2 — field layer: one decision per distinct type (union of fields).
        let mut fields_by_type: HashMap<String, Vec<String>> = HashMap::new();
        for item in &visible {
            if let Some(obj) = item.fields.as_object() {
                let names = fields_by_type.entry(item.item_type.clone()).or_default();
                for k in obj.keys() {
                    if !names.contains(k) {
                        names.push(k.clone());
                    }
                }
            }
        }
        let mut decisions_by_type: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (item_type, names) in fields_by_type {
            let decisions = self
                .field_access_decisions(user, &item_type, &names, operation)
                .await;
            decisions_by_type.insert(item_type, decisions);
        }
        for item in &mut visible {
            if let Some(decisions) = decisions_by_type.get(&item.item_type) {
                apply_field_decisions(&mut item.fields, decisions);
            }
        }

        visible
    }

    /// List items by type.
    pub async fn list_by_type(&self, item_type: &str) -> Result<Vec<Item>> {
        Item::list_by_type(&self.inner.pool, item_type).await
    }

    /// List published items.
    pub async fn list_published(&self, limit: i64, offset: i64) -> Result<Vec<Item>> {
        Item::list_published(&self.inner.pool, limit, offset).await
    }

    /// List published items promoted to the front page.
    pub async fn list_promoted(&self, limit: i64, offset: i64) -> Result<Vec<Item>> {
        Item::list_promoted(&self.inner.pool, limit, offset).await
    }

    /// List items with filtering and return total count for pagination.
    pub async fn list_filtered(
        &self,
        item_type: Option<&str>,
        status: Option<i16>,
        author_id: Option<Uuid>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Item>, i64)> {
        let items = Item::list_filtered(
            &self.inner.pool,
            item_type,
            status,
            author_id,
            limit,
            offset,
        )
        .await?;
        let total = Item::count_filtered(&self.inner.pool, item_type, status, author_id).await?;
        Ok((items, total))
    }

    /// Find items that reference `target_id` in the `field_name` RecordReference
    /// field, via GIN-indexed JSONB containment (P11g / D-57).
    ///
    /// Superset-correct reverse-reference resolution — every referring item, no
    /// silent cap — replacing the former `LIMIT 50` per-type scan. `item_type`
    /// optionally scopes to one content type. See [`Item::find_referencing`].
    pub async fn find_referencing(
        &self,
        item_type: Option<&str>,
        field_name: &str,
        target_id: Uuid,
    ) -> Result<Vec<Item>> {
        Item::find_referencing(&self.inner.pool, item_type, field_name, target_id).await
    }

    /// Get revisions for an item.
    pub async fn get_revisions(&self, item_id: Uuid) -> Result<Vec<ItemRevision>> {
        Item::get_revisions(&self.inner.pool, item_id).await
    }

    /// Revert an item to a previous revision.
    pub async fn revert_to_revision(
        &self,
        item_id: Uuid,
        revision_id: Uuid,
        user: &UserContext,
    ) -> Result<Item> {
        // Load item to check access
        let item = self
            .load(item_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("item not found"))?;

        if !self.check_access(&item, "edit", user).await? {
            anyhow::bail!("access denied");
        }

        let updated =
            Item::revert_to_revision(&self.inner.pool, item_id, revision_id, user.id).await?;

        // Invalidate cache
        self.invalidate(item_id);

        // Invoke tap_item_update for the revert
        let item_json = serde_json::to_string(&updated).context("serialize item")?;
        let state = self.tap_state(user);

        let _ = self
            .inner
            .dispatcher
            .dispatch("tap_item_update", &item_json, state)
            .await;

        info!(item_id = %item_id, revision_id = %revision_id, "item reverted");
        Ok(updated)
    }

    /// Invalidate cached item.
    pub fn invalidate(&self, id: Uuid) {
        self.inner.cache.invalidate(&id);
    }

    /// Clear all cached items and stages.
    pub fn clear_cache(&self) {
        self.inner.cache.invalidate_all();
        self.inner.stage_cache.invalidate_all();
    }

    /// Clear cached stage data. Call when stage config changes (visibility,
    /// machine name, etc.) so access checks use fresh data.
    pub fn clear_stage_cache(&self) {
        self.inner.stage_cache.invalidate_all();
    }

    /// Get cache size.
    pub fn cache_size(&self) -> usize {
        self.inner.cache.entry_count() as usize
    }
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // =======================================================================
    // G-VIEW-OUTPUT-JSON-ENCODED (Argus M3): view-tap output decoding
    // =======================================================================

    #[test]
    fn a_json_string_view_output_is_unwrapped_to_its_markup() {
        // Exactly what `#[plugin_tap]` produces for a `String`-returning
        // `tap_item_view`: the fragment, serde-serialized.
        let fragment = r#"<div class="series-nav"><a href="/item/1">Prev</a></div>"#;
        let raw = serde_json::to_string(fragment).unwrap();

        // The defect, stated: the wire form is not the markup.
        assert_ne!(raw, fragment);
        assert!(raw.starts_with('"') && raw.contains("\\\""));

        // The fix: the page gets the markup back, byte for byte.
        assert_eq!(decode_view_output(&raw), fragment);
    }

    #[test]
    fn an_html_enveloped_view_output_reads_the_html_key() {
        let raw = serde_json::json!({"html": "<p>hi</p>", "weight": 5}).to_string();
        assert_eq!(decode_view_output(&raw), "<p>hi</p>");
    }

    #[test]
    fn a_raw_html_view_output_passes_through_unchanged() {
        // Not valid JSON, so it is not a tap-macro envelope: leave it alone.
        let raw = "<section class='x'>plain</section>";
        assert_eq!(decode_view_output(raw), raw);
    }

    #[test]
    fn an_empty_or_null_view_output_contributes_nothing() {
        assert_eq!(decode_view_output(""), "");
        assert_eq!(decode_view_output("   "), "");
        assert_eq!(decode_view_output("null"), "");
        // A tap returning `String::new()` — by far the common "not my item"
        // answer — serializes to a two-character JSON empty string.
        assert_eq!(decode_view_output(r#""""#), "");
    }

    #[test]
    fn a_json_object_without_an_html_key_is_shown_not_swallowed() {
        let raw = r#"{"series_title":"Rust","total":3}"#;
        assert_eq!(decode_view_output(raw), raw);
    }

    #[test]
    fn decoding_survives_a_fragment_full_of_serde_escapes() {
        // The case the Argus single-quote mitigation was written to dodge: a
        // fragment with double-quoted attributes, backslashes and newlines.
        let fragment = "<a href=\"/x?a=1&b=2\" title=\"He said \\\"hi\\\"\">\n\tlink</a>";
        let raw = serde_json::to_string(fragment).unwrap();
        assert_eq!(decode_view_output(&raw), fragment);
    }

    #[test]
    fn item_access_input_serialization() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".to_string(),
            author_id: Uuid::nil(),
            operation: "view".to_string(),
            user_id: Uuid::nil(),
            user_authenticated: false,
            user_permissions: vec![],
            stage_id: None,
            stage_machine_name: None,
        };

        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains("\"operation\":\"view\""));
    }

    fn sample_item(title: &str, fields: serde_json::Value) -> Item {
        Item {
            id: Uuid::nil(),
            current_revision_id: None,
            item_type: "blog".to_string(),
            title: title.to_string(),
            author_id: Uuid::nil(),
            status: 1,
            created: 0,
            changed: 0,
            promote: 0,
            sticky: 0,
            fields,
            stage_id: Uuid::nil(),
            language: "en".to_string(),
            item_group_id: Uuid::nil(),
            retention_days: None,
        }
    }

    #[test]
    fn item_embedding_text_combines_title_and_fields() {
        let item = sample_item(
            "Hello World",
            serde_json::json!({
                "field_body": { "value": "<p>Rust <b>rocks</b></p>" },
                "field_summary": "Plain summary",
                "field_empty": { "value": "" }
            }),
        );

        let text = item_embedding_text(&item);
        assert!(text.contains("Hello World"), "title missing: {text}");
        // HTML stripped from the body field.
        assert!(
            text.contains("Rust rocks"),
            "body text missing/unstripped: {text}"
        );
        assert!(!text.contains("<p>"), "HTML not stripped: {text}");
        // Plain-string field value included.
        assert!(
            text.contains("Plain summary"),
            "plain field missing: {text}"
        );
    }

    #[test]
    fn item_embedding_text_title_only_when_no_fields() {
        let item = sample_item("Just a title", serde_json::json!({}));
        assert_eq!(item_embedding_text(&item), "Just a title");
    }

    #[test]
    fn item_embedding_text_can_exceed_embedding_budget() {
        // Drives the data path that triggers the item-scoped truncation warning
        // in `index_item`: an item whose combined text is over the embedding
        // budget. The warning itself fires inside the async index path; this
        // proves the boundary condition is reachable from real item content.
        let big = "word ".repeat(EMBEDDING_INPUT_MAX_CHARS);
        let item = sample_item("Oversized", serde_json::json!({ "field_body": big }));
        assert!(
            item_embedding_text(&item).chars().count() > EMBEDDING_INPUT_MAX_CHARS,
            "large item text should exceed the embedding budget"
        );
    }

    #[test]
    fn item_access_input_deserialization() {
        // Old-format JSON (without new fields) should deserialize via #[serde(default)]
        let json = r#"{"item_id":"00000000-0000-0000-0000-000000000000","item_type":"page","author_id":"00000000-0000-0000-0000-000000000000","operation":"edit","user_id":"00000000-0000-0000-0000-000000000000"}"#;
        let input: ItemAccessInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.item_type, "page");
        assert_eq!(input.operation, "edit");
        assert!(!input.user_authenticated);
        assert!(input.user_permissions.is_empty());
        assert!(input.stage_id.is_none());
        assert!(input.stage_machine_name.is_none());
    }

    #[test]
    fn item_access_input_roundtrip() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();
        let stage = Uuid::now_v7();

        let input = ItemAccessInput {
            item_id: id1,
            item_type: "article".to_string(),
            author_id: id2,
            operation: "delete".to_string(),
            user_id: id3,
            user_authenticated: true,
            user_permissions: vec!["edit any content".to_string()],
            stage_id: Some(stage),
            stage_machine_name: Some("curated".to_string()),
        };

        let json = serde_json::to_string(&input).unwrap();
        let parsed: ItemAccessInput = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.item_id, id1);
        assert_eq!(parsed.author_id, id2);
        assert_eq!(parsed.user_id, id3);
        assert_eq!(parsed.operation, "delete");
        assert!(parsed.user_authenticated);
        assert_eq!(parsed.user_permissions, vec!["edit any content"]);
        assert_eq!(parsed.stage_id, Some(stage));
        assert_eq!(parsed.stage_machine_name.as_deref(), Some("curated"));
    }

    // ---- Field-access batch contract + deny-wins aggregation (Story 3.1) ----

    /// Build a `TapResult` whose output is a serialized `FieldAccessBatchResult`
    /// from a list of `(field, decision)` pairs.
    fn field_result(plugin: &str, votes: &[(&str, FieldAccessResult)]) -> TapResult {
        let decisions = votes
            .iter()
            .map(|(f, d)| ((*f).to_string(), d.clone()))
            .collect();
        let batch = FieldAccessBatchResult { decisions };
        TapResult {
            plugin_name: plugin.to_string(),
            output: serde_json::to_string(&batch).unwrap(),
        }
    }

    #[test]
    fn field_access_batch_input_kernel_serialization() {
        let input = FieldAccessBatchInput {
            user: FieldAccessUser {
                user_id: Uuid::nil(),
                authenticated: true,
                permissions: vec!["access content".to_string()],
            },
            item_type: "article".to_string(),
            operation: "view".to_string(),
            fields: vec!["ssn".to_string()],
        };
        let json = serde_json::to_string(&input).unwrap();
        assert!(json.contains(r#""item_type":"article""#), "{json}");
        assert!(json.contains(r#""operation":"view""#), "{json}");
        assert!(json.contains(r#""fields":["ssn"]"#), "{json}");
        // Round-trips through the SDK-side type (dual-definition parity).
        let sdk: trovato_sdk::types::FieldAccessBatchInput = serde_json::from_str(&json).unwrap();
        assert_eq!(sdk.item_type, "article");
        assert_eq!(sdk.user.permissions, vec!["access content".to_string()]);
    }

    #[test]
    fn aggregate_no_implementer_defaults_allow() {
        // No plugin implements the tap ⇒ empty results ⇒ every field visible.
        let fields = vec!["ssn".to_string(), "salary".to_string()];
        let decisions = aggregate_field_decisions(&fields, &[]);
        assert_eq!(decisions.get("ssn"), Some(&true));
        assert_eq!(decisions.get("salary"), Some(&true));
    }

    #[test]
    fn aggregate_deny_hides_only_that_field() {
        let fields = vec!["ssn".to_string(), "salary".to_string(), "notes".to_string()];
        let results = vec![field_result(
            "p1",
            &[
                ("ssn", FieldAccessResult::Deny),
                ("salary", FieldAccessResult::Allow),
                // notes: absent ⇒ NoOpinion ⇒ default allow
            ],
        )];
        let d = aggregate_field_decisions(&fields, &results);
        assert_eq!(d.get("ssn"), Some(&false), "denied field hidden");
        assert_eq!(d.get("salary"), Some(&true), "allowed field visible");
        assert_eq!(d.get("notes"), Some(&true), "absent field defaults allow");
    }

    #[test]
    fn aggregate_noopinion_plus_allow_is_allow() {
        let fields = vec!["salary".to_string()];
        let results = vec![
            field_result("p1", &[("salary", FieldAccessResult::NoOpinion)]),
            field_result("p2", &[("salary", FieldAccessResult::Allow)]),
        ];
        let d = aggregate_field_decisions(&fields, &results);
        assert_eq!(d.get("salary"), Some(&true));
    }

    #[test]
    fn aggregate_deny_wins_over_allow() {
        // Two plugins disagree on the same field — deny wins regardless of order.
        let fields = vec!["salary".to_string()];
        let allow_then_deny = vec![
            field_result("p1", &[("salary", FieldAccessResult::Allow)]),
            field_result("p2", &[("salary", FieldAccessResult::Deny)]),
        ];
        assert_eq!(
            aggregate_field_decisions(&fields, &allow_then_deny).get("salary"),
            Some(&false)
        );
        let deny_then_allow = vec![
            field_result("p1", &[("salary", FieldAccessResult::Deny)]),
            field_result("p2", &[("salary", FieldAccessResult::Allow)]),
        ];
        assert_eq!(
            aggregate_field_decisions(&fields, &deny_then_allow).get("salary"),
            Some(&false)
        );
    }

    #[test]
    fn aggregate_unparseable_output_is_skipped() {
        // A plugin returning garbage contributes no vote (mirrors a trapped
        // handler being dropped by the dispatcher). The lone Deny still wins.
        let fields = vec!["ssn".to_string()];
        let results = vec![
            TapResult {
                plugin_name: "broken".to_string(),
                output: "not json".to_string(),
            },
            field_result("p2", &[("ssn", FieldAccessResult::Deny)]),
        ];
        let d = aggregate_field_decisions(&fields, &results);
        assert_eq!(d.get("ssn"), Some(&false));

        // If the ONLY plugin errors, the field falls to the fail-open default.
        let only_broken = vec![TapResult {
            plugin_name: "broken".to_string(),
            output: "not json".to_string(),
        }];
        let d = aggregate_field_decisions(&fields, &only_broken);
        assert_eq!(d.get("ssn"), Some(&true));
    }

    // ---- Field-filtering primitive for the seams (Story 3.2) ----

    #[test]
    fn apply_field_decisions_drops_only_denied_keys() {
        let mut fields = serde_json::json!({
            "ssn": { "value": "123-45-6789" },
            "salary": { "value": "100000" },
            "notes": "public note",
        });
        let mut decisions = HashMap::new();
        decisions.insert("ssn".to_string(), false); // denied
        decisions.insert("salary".to_string(), true); // allowed
        // "notes" absent from decisions ⇒ kept (fail-open default).
        apply_field_decisions(&mut fields, &decisions);
        let obj = fields.as_object().unwrap();
        assert!(!obj.contains_key("ssn"), "denied field must be dropped");
        assert!(obj.contains_key("salary"), "allowed field must remain");
        assert!(obj.contains_key("notes"), "absent decision ⇒ kept");
    }

    #[test]
    fn apply_field_decisions_noop_on_non_object() {
        let mut fields = serde_json::json!("not an object");
        let decisions = HashMap::new();
        apply_field_decisions(&mut fields, &decisions);
        assert_eq!(fields, serde_json::json!("not an object"));
    }

    #[test]
    fn shared_field_access_cache_flush_empties_it() {
        // Amendment α: a config write flushes the whole field-access cache.
        let cache = crate::tap::new_field_access_cache();
        cache.insert("k1".to_string(), true);
        cache.insert("k2".to_string(), false);
        cache.run_pending_tasks();
        assert_eq!(cache.get("k1"), Some(true));
        cache.invalidate_all();
        cache.run_pending_tasks();
        assert!(cache.get("k1").is_none(), "flush must empty the cache");
        assert!(cache.get("k2").is_none());
    }
}
