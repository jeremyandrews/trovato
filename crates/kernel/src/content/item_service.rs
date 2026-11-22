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

use super::file_refs::{self, FileReferences};
use super::reentry::{self, MAX_ITEM_WRITE_DEPTH, WriteTooDeep};
use crate::cache::generation::{CacheKind, bump};
use crate::metrics::{CacheName, Metrics};
use crate::models::stage::{LIVE_STAGE_ID, Stage, StageVisibility};
use crate::models::workflow::{WORKFLOW_KEY, Workflow};
use crate::models::{CreateItem, Item, ItemRevision, SiteConfig, UpdateItem};
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

/// Every stage, and the workflow resolved against them.
///
/// What a stage move is decided against, loaded in one go (two queries) by
/// [`ItemService::stage_view`], so deciding a move, or listing the moves a form
/// may offer, costs no query per item.
#[derive(Debug)]
pub struct StageView {
    stages: Vec<Stage>,
    workflow: Workflow,
}

impl StageView {
    /// Every stage, in the order `Stage::list_all` returns them.
    pub fn stages(&self) -> &[Stage] {
        &self.stages
    }

    /// The stage with this id.
    pub fn stage(&self, id: Uuid) -> Option<&Stage> {
        self.stages.iter().find(|s| s.id == id)
    }

    /// The stage with this machine name.
    pub fn by_machine_name(&self, machine_name: &str) -> Option<&Stage> {
        self.stages.iter().find(|s| s.machine_name == machine_name)
    }

    /// The stage new content lands on: the one marked default, else Live.
    pub fn default_stage(&self) -> Uuid {
        self.stages
            .iter()
            .find(|s| s.is_default)
            .map_or(LIVE_STAGE_ID, |s| s.id)
    }

    /// The resolved workflow.
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }
}

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
    /// Cached field access decisions keyed by "perm_hash:item_type:field_name:operation".
    /// Deny-wins aggregation result, 5-minute TTL. **Shared** (`Arc`) with the
    /// `tap_services` `RequestServices` so a plugin config write flushes the same
    /// cache these read paths consult (amendment α).
    field_access_cache: Arc<Cache<String, bool>>,
    /// Counts hits and misses in every cache above.
    metrics: Arc<Metrics>,
    /// Optional embedding provider used to (re)generate item embeddings on
    /// index. `None` in builds without an AI provider wired (e.g. tests).
    ai_providers: Option<Arc<AiProviderService>>,
    /// Optional pgvector store the kernel writes item embeddings into. Typed as
    /// the concrete `PgVectorStore` because the [`VectorStore`] trait is not
    /// object-safe (see its `async_fn_in_trait` allow); the trait stays the
    /// swap seam.
    vector_store: Option<Arc<PgVectorStore>>,
    /// The page builder registry saved pages are validated against: the
    /// standard set until `AppState` sets the merged one at startup.
    page_builder_components:
        std::sync::OnceLock<Arc<crate::content::page_builder_components::ComponentRegistry>>,
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
/// of `fields`. Both operations are live: every read path asks for `"view"`,
/// and every item write path asks for `"edit"` before it persists a field.
///
/// Granularity is deliberately **type-level**, and a decision is *asked* to be a
/// pure function of `(permissions, item_type, field, operation)`, which is what
/// lets the kernel batch one dispatch per result-set-per-type. That is **advice
/// about batching, not a safety property**: the kernel's decision cache is keyed
/// on the viewer as well (`user_id` and `authenticated` alongside the permission
/// hash — see `ItemService::field_access_cache_key`), so a plugin that does read
/// the `user` block's identity gets a per-user answer rather than leaking one
/// user's decision to the next. See design
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

/// The permission that decides whether a user may set an item's published
/// state.
///
/// Site-wide rather than per type, matching `create content`, `edit own
/// content` and `edit any content`. Publishing is one authority over what the
/// site shows the world; a per-type split would need a permission for every
/// type a plugin declares, with nothing in the kernel to enumerate them
/// against, and no screen asks the question per type. A site that wants it
/// narrower gives its per-type editors a role that does not hold this.
pub const PUBLISH_CONTENT: &str = "publish content";

/// The permission that lets a user read a stage other than Live
/// (`routes::helpers::read_stages`).
pub const USE_STAGES: &str = "use stages";

/// A write refused by one of the two item write gates.
///
/// Returned inside `anyhow::Error` so every caller finds it the same way —
/// `anyhow::Error::downcast_ref::<WriteDenied>()` — and maps it to 403 rather
/// than to the 500 an unrecognized service error gets. It deliberately does
/// **not** say "access denied": that string is the item-level denial, which
/// [`ItemService::update`] `bail!`s and the MCP layer maps to "not found" so
/// item existence does not leak. A field or publish refusal is a different
/// answer and should read as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDenied {
    /// The listed field keys would have been changed by a user who may not edit
    /// them. Sorted, so the message a client sees is stable.
    Fields(Vec<String>),
    /// The write would have changed whether the item is published, and the user
    /// does not hold [`PUBLISH_CONTENT`].
    Publish,
    /// The write would add a reference to a file the user may not be served:
    /// someone else's unattached upload, or a file only items they cannot view
    /// hold. Attaching it to a published item would publish it.
    Files,
}

impl std::fmt::Display for WriteDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Field names are already public through `/api/content-types`, so
            // naming the refused ones costs nothing and is the only way a client
            // can fix its request.
            Self::Fields(keys) => write!(f, "you may not edit the field(s): {}", keys.join(", ")),
            Self::Publish => write!(
                f,
                "changing whether an item is published requires `{PUBLISH_CONTENT}`"
            ),
            Self::Files => write!(f, "you may not attach a file you cannot view"),
        }
    }
}

impl std::error::Error for WriteDenied {}

/// A write refused because a page builder field holds a page the component
/// registry does not accept: an unknown component, a value outside its
/// field's type or limits, an image without alt text.
///
/// Returned inside `anyhow::Error`, found with `downcast_ref`, and answered
/// with 422 and every error, each naming the item field, the component's
/// place in the page and the component field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageBuilderInvalid(
    /// `(item field name, error)` pairs, in page order.
    pub Vec<(String, crate::content::page_builder_components::FieldError)>,
);

impl std::fmt::Display for PageBuilderInvalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> = self
            .0
            .iter()
            .map(|(field, error)| format!("{field}: {error}"))
            .collect();
        write!(f, "the page is not valid: {}", parts.join("; "))
    }
}

impl std::error::Error for PageBuilderInvalid {}

/// Why [`ItemService::translate`] or [`ItemService::withdraw_translation`]
/// refused, as an error a caller can map without reading a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranslationRefused {
    /// No item has that id.
    NoItem,
    /// The user may not translate the item ([`ItemService::may_translate`]).
    Denied,
    /// The language is not one of the site's, or is the item's own.
    Language,
}

impl std::fmt::Display for TranslationRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoItem => "no such item",
            Self::Denied => "access denied",
            Self::Language => "not a language this item can be translated into",
        })
    }
}

impl std::error::Error for TranslationRefused {}

/// The permission that lets a user translate an item they may view.
pub const TRANSLATE_CONTENT: &str = "translate content";

/// Whether a file may be streamed to a viewer: [`ItemService::file_access`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAccess {
    /// No `file_managed` row has this URI: no ownership or reference model
    /// applies, and the path serves as before.
    Unmanaged,
    /// A managed file the viewer may fetch, with its stored, validated type.
    Allowed {
        /// `file_managed.filemime`.
        filemime: String,
    },
    /// A managed file the viewer may not fetch: answer as if it were absent.
    Denied,
}

/// A managed file with every item that references it, as the access rule
/// needs it. Read by [`MANAGED_FILE_REFS_SQL`].
#[derive(Debug, Clone, sqlx::FromRow)]
struct ManagedFileRefs {
    id: Uuid,
    owner_id: Uuid,
    uri: String,
    filename: String,
    filemime: String,
    alt_text: Option<String>,
    item_ids: Vec<Uuid>,
}

/// The [`ManagedFileRefs`] of every managed file whose URI is in `$1` or whose
/// id is in `$2`.
const MANAGED_FILE_REFS_SQL: &str = "SELECT f.id, f.owner_id, f.uri, f.filename, f.filemime, f.alt_text, \
     COALESCE(array_agg(fr.item_id) FILTER (WHERE fr.item_id IS NOT NULL), '{}') AS item_ids \
     FROM file_managed f LEFT JOIN file_reference fr ON fr.file_id = f.id \
     WHERE f.uri = ANY($1) OR f.id = ANY($2) GROUP BY f.id";

/// Which of an item's revisions [`ItemService::load_revisions`] loads.
#[derive(Debug, Clone, Copy)]
pub enum RevisionSelection<'a> {
    /// Every revision, newest first: the history page.
    All,
    /// These revisions of the item, in this order: a preview or a compare.
    Ids(&'a [Uuid]),
}

/// An item and the revisions of it a user asked for, each cut to the fields
/// that user may view.
#[derive(Debug, Clone)]
pub struct ItemRevisions {
    /// The item, as stored.
    pub item: Item,
    /// The revisions, in the order [`RevisionSelection`] gives.
    pub revisions: Vec<ItemRevision>,
}

/// Why [`ItemService::load_revisions`] answered nothing.
#[derive(Debug)]
pub enum RevisionAccess {
    /// No such item, an item the user may not view, or a revision id that
    /// is not one of this item's.
    NotFound,
    /// The user may view the item but not edit it.
    Forbidden,
    /// A load or an access decision failed.
    Failed(anyhow::Error),
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
        metrics: Arc<Metrics>,
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
                field_access_cache,
                metrics,
                ai_providers,
                vector_store,
                page_builder_components: std::sync::OnceLock::new(),
            }),
        }
    }

    /// Validate saved pages against `components` (the standard set plus the
    /// enabled plugins') rather than the standard set. Set once, at startup;
    /// a second call is ignored.
    pub fn set_page_builder_components(
        &self,
        components: Arc<crate::content::page_builder_components::ComponentRegistry>,
    ) {
        if self.inner.page_builder_components.set(components).is_err() {
            warn!("page builder components were already set; keeping the first registry");
        }
    }

    /// The registry saved pages are validated against.
    fn page_builder_components(
        &self,
    ) -> Arc<crate::content::page_builder_components::ComponentRegistry> {
        self.inner
            .page_builder_components
            .get_or_init(|| Arc::new(Default::default()))
            .clone()
    }

    /// Refuse a write whose page builder fields hold a page the registry does
    /// not accept, with every error at once.
    ///
    /// Only a value the write changes is checked: a page stored before a
    /// rule existed does not make an unrelated edit to its item unsaveable,
    /// and the next edit of the page itself is checked in full.
    async fn validate_page_builder_fields(
        &self,
        item_type: &str,
        fields: Option<&serde_json::Value>,
        existing: Option<&serde_json::Value>,
    ) -> Result<()> {
        let Some(fields) = fields.and_then(|f| f.as_object()) else {
            return Ok(());
        };
        let mut conn = self.inner.pool.acquire().await?;
        let definitions = file_refs::load_field_definitions(&mut conn, item_type).await?;
        drop(conn);
        let registry = self.page_builder_components();
        let mut refused = Vec::new();
        for def in definitions
            .iter()
            .filter(|d| matches!(d.field_type, trovato_sdk::types::FieldType::PageBuilder))
        {
            let Some(value) = fields.get(&def.field_name) else {
                continue;
            };
            if value.is_null() || existing.and_then(|e| e.get(&def.field_name)) == Some(value) {
                continue;
            }
            for error in registry.validate_page(value) {
                refused.push((def.field_name.clone(), error));
            }
        }
        if refused.is_empty() {
            Ok(())
        } else {
            Err(PageBuilderInvalid(refused).into())
        }
    }

    /// Build a `RequestState` for tap dispatch with the user context and services.
    fn tap_state(&self, user: &UserContext) -> RequestState {
        RequestState::new(user.clone(), self.inner.tap_services.clone())
    }

    /// Refuse a write nested [`MAX_ITEM_WRITE_DEPTH`] write taps deep, before
    /// anything is decided or written. See [`reentry`].
    fn refuse_if_too_deep() -> Result<()> {
        if reentry::write_depth() >= MAX_ITEM_WRITE_DEPTH {
            return Err(WriteTooDeep.into());
        }
        Ok(())
    }

    /// Dispatch one of the item write taps (`tap_item_presave`, `insert`,
    /// `update`, `update_index`, `delete`) under the re-entrancy rule.
    ///
    /// Each handler runs with its plugin on the write stack, and a plugin whose
    /// write tap is already running around this call is skipped: a write a
    /// plugin makes from its own tap does not fire back into that plugin. Weight
    /// order and the log-and-skip handling of a failed handler are the
    /// dispatcher's, unchanged.
    async fn dispatch_write_tap(
        &self,
        tap: &str,
        input: &str,
        user: &UserContext,
    ) -> Vec<TapResult> {
        let dispatcher = &self.inner.dispatcher;
        let mut results = Vec::new();
        for handler in dispatcher.registry().get_handlers(tap) {
            let plugin = handler.plugin.info.name.as_str();
            if reentry::is_handling_write(plugin) {
                continue;
            }
            let call = dispatcher.dispatch_to_plugin(tap, input, plugin, self.tap_state(user));
            if let Some(result) = reentry::handling_write(plugin, call).await {
                results.push(result);
            }
        }
        results
    }

    /// Create a new item with tap_item_presave and tap_item_insert invocations.
    ///
    /// The presave tap fires before the item is persisted, allowing plugins
    /// to modify fields (e.g., AI content enrichment). The insert tap fires
    /// after persistence for post-save side effects.
    pub async fn create(&self, mut input: CreateItem, user: &UserContext) -> Result<Item> {
        Self::refuse_if_too_deep()?;
        // A create that names a stage other than the default is a move from the
        // default stage, and is decided as one. Naming none lands on the
        // default, which `Item::create` resolves in its one INSERT.
        if let Some(stage_id) = input.stage_id {
            let view = self.stage_view().await?;
            let from = view.default_stage();
            if stage_id != from && !self.may_move(&view, from, stage_id, user) {
                warn!(user_id = %user.id, stage = %stage_id, "create refused: no transition to this stage");
                anyhow::bail!("access denied");
            }
        }
        // The two write gates, on the client's own submission and before the
        // presave tap: a plugin's presave edits are trusted code, not user
        // input. Both return `WriteDenied`, which every caller maps to 403.
        input.status = Self::gate_publish(user, input.status, None)?;
        let gated_fields = self
            .gate_field_writes(user, &input.item_type, input.fields.as_ref(), None)
            .await?;
        input.fields = gated_fields;

        // Invoke tap_item_presave — plugins can modify fields before save.
        // Serialize the input as a JSON object so plugins can read/modify fields.
        let presave_json = serde_json::json!({
            "item_type": input.item_type,
            "title": input.title,
            "fields": input.fields,
            "status": input.status,
        });
        let presave_input = serde_json::to_string(&presave_json).context("serialize presave")?;
        let presave_results = self
            .dispatch_write_tap("tap_item_presave", &presave_input, user)
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

        // Pages are checked after presave, so what is stored is what passed.
        self.validate_page_builder_fields(&input.item_type, input.fields.as_ref(), None)
            .await?;

        // The files the item will reference: any it may not attach refuse the
        // write, and the rest are indexed and promoted in the item's own
        // transaction.
        let refs = self
            .file_references(&input.item_type, input.fields.as_ref())
            .await?;
        self.authorize_new_file_references(None, &refs, user)
            .await?;

        // Create the item in the database
        let item = Item::create_with_file_refs(&self.inner.pool, input, Some(&refs)).await?;

        // tap_item_insert is a notification: the row exists, and what a handler
        // returns is not read (BL-88). Failures are logged by the dispatcher.
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        self.dispatch_write_tap("tap_item_insert", &item_json, user)
            .await;

        // Fire tap_item_update_index and regenerate the kernel embedding so the
        // new item is searchable via the SemanticSimilarity gather operator.
        self.index_item(&item, user).await;

        info!(item_id = %item.id, item_type = %item.item_type, "item created");
        Ok(item)
    }

    /// The cached copy of item `id`, counted as an item cache hit or miss.
    fn cached_item(&self, id: Uuid) -> Option<Item> {
        self.inner
            .metrics
            .cache_lookup(CacheName::Item, self.inner.cache.get(&id))
    }

    /// Load an item by ID.
    pub async fn load(&self, id: Uuid) -> Result<Option<Item>> {
        // Check cache first
        if let Some(item) = self.cached_item(id) {
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
        if let Some(item) = self.cached_item(id) {
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

    /// Create or replace an item's translation in one language.
    ///
    /// The first writer of `item_translation` in the kernel. Every other method
    /// here reads it, and the overlay in `routes/helpers.rs` and the join in the
    /// gather query builder have been reading it since the table was added; the
    /// only thing that ever put a row in was SQL, or a test.
    ///
    /// Replace rather than merge, on the primary key `(item_id, language)`: a
    /// translation is one language's version of the whole item, so a save that
    /// merged would leave a field behind at whatever a previous save set it to,
    /// with nothing on the form to say so.
    ///
    /// `created` is preserved across a replace, because the row is the same
    /// translation being edited rather than a new one; `changed` moves. Neither
    /// is taken from the caller.
    ///
    /// # Not validated here
    ///
    /// That `language` is a language the site knows, and that `item_id` names an
    /// item, are the caller's to check. The route checks both, and says which
    /// one failed; this would only be able to return a foreign-key error, and
    /// `item_translation` has no foreign keys to give one.
    pub async fn save_translation(
        &self,
        item_id: Uuid,
        language: &str,
        title: &str,
        fields: &serde_json::Value,
        user: &UserContext,
    ) -> Result<ItemTranslation> {
        // A translation writes field values, so field-level edit access applies
        // to it like to any other write ([`Self::gate_field_writes`] has the
        // rules). The decision is keyed on the item's type; an item that is not
        // there has no type to decide against, and whether `item_id` names an
        // item stays the caller's to check, as documented above.
        let gated = match self.load(item_id).await? {
            Some(item) => {
                let stored = self
                    .load_translation(item_id, language)
                    .await?
                    .map(|t| t.fields);
                self.gate_field_writes(user, &item.item_type, Some(fields), stored.as_ref())
                    .await?
            }
            None => None,
        };
        let fields = gated.as_ref().unwrap_or(fields);

        let row = sqlx::query_as::<_, ItemTranslation>(
            "INSERT INTO item_translation (item_id, language, title, fields) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (item_id, language) DO UPDATE \
                SET title = EXCLUDED.title, \
                    fields = EXCLUDED.fields, \
                    changed = EXTRACT(EPOCH FROM NOW())::bigint \
             RETURNING item_id, language, title, fields, created, changed",
        )
        .bind(item_id)
        .bind(language)
        .bind(title)
        .bind(fields)
        .fetch_one(&self.inner.pool)
        .await
        .context("failed to save item translation")?;
        // A translated title is what a localized menu label is read from.
        crate::menu::invalidate_config_menus();

        Ok(row)
    }

    /// Remove an item's translation in one language.
    ///
    /// Returns whether a row was there to remove. The counterpart of
    /// [`Self::save_translation`]: a screen that can add a translation and not
    /// take it back leaves a wrong translation on the site with no way to
    /// withdraw it short of SQL, which is the situation this whole path exists
    /// to end.
    pub async fn delete_translation(&self, item_id: Uuid, language: &str) -> Result<bool> {
        let result =
            sqlx::query("DELETE FROM item_translation WHERE item_id = $1 AND language = $2")
                .bind(item_id)
                .bind(language)
                .execute(&self.inner.pool)
                .await
                .context("failed to delete item translation")?;
        crate::menu::invalidate_config_menus();

        Ok(result.rows_affected() > 0)
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

    /// Whether `user` may write a translation of `item`: `edit` access on it,
    /// or [`TRANSLATE_CONTENT`] and `view` access, the rule the admin
    /// translation screens apply. The background principal passes, as it does
    /// [`Self::check_access`].
    pub async fn may_translate(&self, item: &Item, user: &UserContext) -> bool {
        if self.check_access(item, "edit", user).await.unwrap_or(false) {
            return true;
        }
        user.has_permission(TRANSLATE_CONTENT)
            && self.check_access(item, "view", user).await.unwrap_or(false)
    }

    /// Load the item a translation write is for and decide it, or refuse.
    async fn translatable(&self, item_id: Uuid, user: &UserContext) -> Result<Item, anyhow::Error> {
        Self::refuse_if_too_deep()?;
        let Some(item) = self.load(item_id).await? else {
            return Err(TranslationRefused::NoItem.into());
        };
        if !self.may_translate(&item, user).await {
            return Err(TranslationRefused::Denied.into());
        }
        Ok(item)
    }

    /// A translation write is an edit of the item: the cache drops it and
    /// `tap_item_update` fires with it, so a plugin that indexes or mirrors
    /// items hears about it as it hears about any other edit.
    async fn translation_written(&self, item: &Item, user: &UserContext) -> Result<()> {
        self.invalidate(item.id);
        let item_json = serde_json::to_string(item).context("serialize item")?;
        self.dispatch_write_tap("tap_item_update", &item_json, user)
            .await;
        Ok(())
    }

    /// Write `item_id`'s translation into `language` as `user`: decided as an
    /// edit ([`Self::may_translate`]), field-gated by [`Self::save_translation`],
    /// then the item cache is dropped and `tap_item_update` fires.
    ///
    /// Refuses, as a [`TranslationRefused`] error, an item that is not there,
    /// a user who may not translate it, and a language that is not one of the
    /// site's or is the item's own.
    pub async fn translate(
        &self,
        item_id: Uuid,
        language: &str,
        title: &str,
        fields: &serde_json::Value,
        user: &UserContext,
    ) -> Result<ItemTranslation> {
        let item = self.translatable(item_id, user).await?;
        let known = crate::models::Language::find_by_id(&self.inner.pool, language)
            .await?
            .is_some();
        if !known || language == item.language {
            return Err(TranslationRefused::Language.into());
        }
        let row = self
            .save_translation(item_id, language, title, fields, user)
            .await?;
        self.translation_written(&item, user).await?;
        Ok(row)
    }

    /// Remove `item_id`'s translation in `language` as `user`, decided as
    /// [`Self::translate`] is. Returns whether a row was there; only a removal
    /// fires `tap_item_update`.
    pub async fn withdraw_translation(
        &self,
        item_id: Uuid,
        language: &str,
        user: &UserContext,
    ) -> Result<bool> {
        let item = self.translatable(item_id, user).await?;
        let removed = self.delete_translation(item_id, language).await?;
        if removed {
            self.translation_written(&item, user).await?;
        }
        Ok(removed)
    }

    /// The translations of `item_ids` that `user` may view, by item in the
    /// order asked and then by language, with the field tier applied to each
    /// translation's `fields` as it is to an item's.
    ///
    /// Three queries and one field decision per distinct type, whatever the
    /// number of items: the items, their translations, and the decisions.
    pub async fn translations_for_view(
        &self,
        item_ids: &[Uuid],
        user: &UserContext,
    ) -> Result<Vec<ItemTranslation>> {
        let items = Item::find_by_ids(&self.inner.pool, item_ids).await?;
        let count = items.len();
        let visible = self.filter_page_for_view(items, user, "view", count).await;
        let type_of: HashMap<Uuid, String> = visible
            .into_iter()
            .map(|item| (item.id, item.item_type))
            .collect();
        let ids: Vec<Uuid> = type_of.keys().copied().collect();
        let mut rows = sqlx::query_as::<_, ItemTranslation>(
            "SELECT item_id, language, title, fields, created, changed \
             FROM item_translation WHERE item_id = ANY($1)",
        )
        .bind(&ids)
        .fetch_all(&self.inner.pool)
        .await
        .context("failed to list item translations")?;

        // The background principal reads with kernel authority, as `get-item`
        // does for it: no field tier.
        let mut fields_by_type: HashMap<&str, Vec<String>> = HashMap::new();
        for row in rows.iter().filter(|_| !user.is_background()) {
            let names = fields_by_type
                .entry(type_of[&row.item_id].as_str())
                .or_default();
            for key in row.fields.as_object().into_iter().flat_map(|o| o.keys()) {
                if !names.contains(key) {
                    names.push(key.clone());
                }
            }
        }
        let mut decisions_by_type: HashMap<String, HashMap<String, bool>> = HashMap::new();
        for (item_type, names) in fields_by_type {
            let decisions = self
                .field_access_decisions(user, item_type, &names, "view")
                .await;
            decisions_by_type.insert(item_type.to_string(), decisions);
        }
        for row in &mut rows {
            if let Some(decisions) = decisions_by_type.get(&type_of[&row.item_id]) {
                apply_field_decisions(&mut row.fields, decisions);
            }
        }

        let rank: HashMap<Uuid, usize> = item_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();
        rows.sort_by(|a, b| {
            rank[&a.item_id]
                .cmp(&rank[&b.item_id])
                .then_with(|| a.language.cmp(&b.language))
        });
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
        Self::refuse_if_too_deep()?;
        // Load existing item
        let Some(existing) = self.load(id).await? else {
            return Ok(None);
        };

        // Check access
        if !self.check_access(&existing, "edit", user).await? {
            anyhow::bail!("access denied");
        }

        // A stage change is a move, decided by the workflow (the edit access
        // just checked is the other half of the rule). Asking for the stage the
        // item is already on is no move at all.
        if input.stage_id == Some(existing.stage_id) {
            input.stage_id = None;
        }
        if let Some(to) = input.stage_id {
            let view = self.stage_view().await?;
            if !self.may_move(&view, existing.stage_id, to, user) {
                warn!(user_id = %user.id, item_id = %id, stage = %to, "move refused: no transition");
                anyhow::bail!("access denied");
            }
        }

        // Field-level and publish gates, after item-level access and before the
        // presave tap. A denied field the submission left out is copied back
        // here, so a partial `fields` object cannot erase it.
        input.status = Self::gate_publish(user, input.status, Some(existing.status))?;
        let gated_fields = self
            .gate_field_writes(
                user,
                &existing.item_type,
                input.fields.as_ref(),
                Some(&existing.fields),
            )
            .await?;
        input.fields = gated_fields;

        // Invoke tap_item_presave — plugins can modify fields before save.
        let presave_json = serde_json::json!({
            "item_type": existing.item_type,
            "title": input.title.as_deref().unwrap_or(&existing.title),
            "fields": input.fields.as_ref().unwrap_or(&existing.fields),
            "status": input.status.unwrap_or(existing.status),
        });
        let presave_input = serde_json::to_string(&presave_json).context("serialize presave")?;
        let presave_results = self
            .dispatch_write_tap("tap_item_presave", &presave_input, user)
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

        self.validate_page_builder_fields(
            &existing.item_type,
            input.fields.as_ref(),
            Some(&existing.fields),
        )
        .await?;

        // A write that replaces `fields` replaces the item's file references,
        // in the same transaction; one that leaves `fields` alone leaves them.
        let refs = match input.fields.as_ref() {
            Some(fields) => {
                let refs = self
                    .file_references(&existing.item_type, Some(fields))
                    .await?;
                self.authorize_new_file_references(Some(id), &refs, user)
                    .await?;
                Some(refs)
            }
            None => None,
        };

        // Update the item
        let item = Item::update_with_file_refs(&self.inner.pool, id, user.id, input, refs.as_ref())
            .await?;

        if let Some(ref i) = item {
            // tap_item_update is a notification, as tap_item_insert is.
            let item_json = serde_json::to_string(i).context("serialize item")?;
            self.dispatch_write_tap("tap_item_update", &item_json, user)
                .await;

            // Fire tap_item_update_index and regenerate the kernel embedding so
            // the updated item's searchable vector reflects the new content.
            self.index_item(i, user).await;

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
        self.dispatch_write_tap("tap_item_update_index", &item_json, user)
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
        Self::refuse_if_too_deep()?;
        // Load item
        let Some(item) = self.load(id).await? else {
            return Ok(false);
        };

        // Check access
        if !self.check_access(&item, "delete", user).await? {
            anyhow::bail!("access denied");
        }

        // tap_item_delete is a notification: it cannot abort the delete, and
        // what a handler returns is not read (BL-88).
        let item_json = serde_json::to_string(&item).context("serialize item")?;
        self.dispatch_write_tap("tap_item_delete", &item_json, user)
            .await;

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

    /// The files `fields` references, for an item of `item_type`. See
    /// [`file_refs`].
    async fn file_references(
        &self,
        item_type: &str,
        fields: Option<&serde_json::Value>,
    ) -> Result<FileReferences> {
        let Some(fields) = fields else {
            return Ok(FileReferences::default());
        };
        let mut conn = self.inner.pool.acquire().await?;
        let definitions = file_refs::load_field_definitions(&mut conn, item_type).await?;
        Ok(file_refs::extract_file_references(fields, &definitions))
    }

    /// Refuse a write that adds a reference to a file `user` may not be served
    /// ([`Self::can_serve_file`]'s rule). A reference the item already holds
    /// (`item_id` on an update) is not re-decided: losing access to a file does
    /// not make an item unsaveable.
    ///
    /// One query for the whole set of files the write names, and one for the
    /// items that already reference them. Administrators and the kernel
    /// background principal pass, as they do every other write gate. Config
    /// import does not come through here: it is operator trusted and has no
    /// acting user (see `config_storage::direct`).
    async fn authorize_new_file_references(
        &self,
        item_id: Option<Uuid>,
        refs: &FileReferences,
        user: &UserContext,
    ) -> Result<()> {
        if user.is_admin() || user.is_background() || (refs.uris.is_empty() && refs.ids.is_empty())
        {
            return Ok(());
        }
        let files = self.managed_file_refs(&refs.uris, &refs.ids).await?;
        let new: Vec<ManagedFileRefs> = files
            .into_iter()
            .filter(|f| item_id.is_none_or(|id| !f.item_ids.contains(&id)))
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        let servable = self.servable_files(&new, user).await?;
        if new.iter().any(|f| !servable.contains(&f.id)) {
            warn!(user_id = %user.id, "write refused: it attaches a file the user may not view");
            return Err(WriteDenied::Files.into());
        }
        Ok(())
    }

    /// The managed files named by `uris` or `ids`, each with the items
    /// referencing it, in one query.
    async fn managed_file_refs(
        &self,
        uris: &[String],
        ids: &[Uuid],
    ) -> Result<Vec<ManagedFileRefs>> {
        Ok(sqlx::query_as(MANAGED_FILE_REFS_SQL)
            .bind(uris)
            .bind(ids)
            .fetch_all(&self.inner.pool)
            .await?)
    }

    /// Which of `files` may be streamed to `viewer` under the any-referencing
    /// policy (D-29): the viewer is an admin; **or** an item referencing the
    /// file passes item-level `check_access("view")`; **or** the file is
    /// referenced by no item (orphan / in-flight upload) and the viewer is its
    /// uploader. The referencing items of the whole set load in one query.
    async fn servable_files(
        &self,
        files: &[ManagedFileRefs],
        viewer: &UserContext,
    ) -> Result<std::collections::HashSet<Uuid>> {
        if viewer.is_admin() {
            return Ok(files.iter().map(|f| f.id).collect());
        }
        let mut item_ids: Vec<Uuid> = files.iter().flat_map(|f| f.item_ids.clone()).collect();
        item_ids.sort();
        item_ids.dedup();
        let mut viewable = std::collections::HashSet::new();
        for item in Item::find_by_ids(&self.inner.pool, &item_ids).await? {
            if self.check_access(&item, "view", viewer).await? {
                viewable.insert(item.id);
            }
        }
        Ok(files
            .iter()
            .filter(|f| {
                if f.item_ids.is_empty() {
                    viewer.authenticated && viewer.id == f.owner_id
                } else {
                    f.item_ids.iter().any(|id| viewable.contains(id))
                }
            })
            .map(|f| f.id)
            .collect())
    }

    /// FR-8 Story 3.5 — decide whether the file at `uri` may be streamed to
    /// `viewer`, and with which stored type. See [`FileAccess`].
    pub async fn file_access(&self, uri: &str, viewer: &UserContext) -> Result<FileAccess> {
        let managed = self.managed_file_refs(&[uri.to_string()], &[]).await?;
        let Some(file) = managed.into_iter().next() else {
            // Not a managed file — no ownership/reference model to enforce.
            return Ok(FileAccess::Unmanaged);
        };
        let servable = self
            .servable_files(std::slice::from_ref(&file), viewer)
            .await?;
        Ok(if servable.contains(&file.id) {
            FileAccess::Allowed {
                filemime: file.filemime,
            }
        } else {
            FileAccess::Denied
        })
    }

    /// FR-8 Story 3.5 — whether the file at `uri` may be streamed to `viewer`
    /// ([`Self::file_access`] without the type). An **unmanaged** path (no
    /// `file_managed` row) is not access-controlled here and serves as before.
    /// `false` maps to the surface's not-found posture (404), leaking no
    /// existence.
    pub async fn can_serve_file(&self, uri: &str, viewer: &UserContext) -> Result<bool> {
        Ok(!matches!(
            self.file_access(uri, viewer).await?,
            FileAccess::Denied
        ))
    }

    /// The File field values of `item`, resolved to the files `viewer` may be
    /// served, keyed by field name: `{id, url, filename, mime, alt}` each, in
    /// the field's order. One query for every file the item names, and one for
    /// the items referencing them (the access rule of [`Self::can_serve_file`]).
    /// A uuid naming no file, or a file the viewer may not fetch, is left out.
    pub async fn referenced_files(
        &self,
        item: &Item,
        definitions: &[trovato_sdk::types::FieldDefinition],
        public_url: impl Fn(&str) -> String,
        viewer: &UserContext,
    ) -> Result<HashMap<String, Vec<serde_json::Value>>> {
        let file_fields: Vec<(&str, Vec<Uuid>)> = definitions
            .iter()
            .filter(|d| matches!(d.field_type, trovato_sdk::types::FieldType::File))
            .map(|d| {
                (
                    d.field_name.as_str(),
                    file_refs::file_field_ids(item.fields.get(&d.field_name)),
                )
            })
            .filter(|(_, ids)| !ids.is_empty())
            .collect();
        let ids: Vec<Uuid> = file_fields
            .iter()
            .flat_map(|(_, ids)| ids.clone())
            .collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let files = self.managed_file_refs(&[], &ids).await?;
        let servable = self.servable_files(&files, viewer).await?;
        let by_id: HashMap<Uuid, &ManagedFileRefs> = files
            .iter()
            .filter(|f| servable.contains(&f.id))
            .map(|f| (f.id, f))
            .collect();
        Ok(file_fields
            .into_iter()
            .filter_map(|(name, ids)| {
                let entries: Vec<serde_json::Value> = ids
                    .iter()
                    .filter_map(|id| by_id.get(id))
                    .map(|f| {
                        serde_json::json!({
                            "id": f.id,
                            "url": public_url(&f.uri),
                            "filename": f.filename,
                            "mime": f.filemime,
                            "alt": f.alt_text,
                        })
                    })
                    .collect();
                (!entries.is_empty()).then(|| (name.to_string(), entries))
            })
            .collect())
    }

    /// FR-8 Story 3.7 — route search results through the shared access seam.
    ///
    /// Search's own SQL applies only a coarse `status`/`author`/`stage` filter
    /// and builds `ts_headline` snippets from the raw long text field, with no
    /// field-level access. This runs each result row through the same seam
    /// REST/SSR/gather use: (1) drop any result the viewer cannot see at the
    /// item level (`check_access("view")`) — this catches plugin `tap_item_access`
    /// denies the coarse SQL filter misses; (2) **redact the snippet** (the
    /// `ts_headline` source) for any result whose type denies the viewer that
    /// field under either of its two names, so a restricted field cannot leak
    /// through the highlight.
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

        // Tier 2 — redact the snippet where the long text field is denied, one
        // field decision per distinct type. The snippet is built from that field
        // under either of its two names (BL-21), so a deny on either name has to
        // redact, or the name this type happens to use leaks past a deny on the
        // other.
        let body_names: Vec<String> = crate::content::body_field::BODY_FIELD_NAMES
            .iter()
            .map(|n| (*n).to_string())
            .collect();
        let mut redact_by_type: HashMap<String, bool> = HashMap::new();
        for r in &visible {
            if !redact_by_type.contains_key(&r.item_type) {
                let decisions = self
                    .field_access_decisions(viewer, &r.item_type, &body_names, "view")
                    .await;
                let denied = body_names
                    .iter()
                    .any(|name| !decisions.get(name.as_str()).copied().unwrap_or(true));
                redact_by_type.insert(r.item_type.clone(), denied);
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
    /// 1. Admin and background-principal bypass (always allowed)
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
        // 1. Admin always has access, and so does the kernel-internal background
        //    principal, for the reason given in [`Self::gate_field_writes`]: it
        //    holds no permissions, and only cron and the queue worker carry it.
        //    `item-api` hands it here only for a plugin that declared
        //    `item_background`, so this is how a declared background write keeps
        //    kernel authority while still going through this service.
        if user.is_admin() || user.is_background() {
            return Ok(true);
        }

        // 2. Resolve stage visibility, read from Postgres: a stage change on
        //    any server must hold on every server at once.
        let (is_internal, stage_machine_name) = if item.stage_id == LIVE_STAGE_ID {
            // Live stage is always public — no DB lookup needed.
            (false, Some("live".to_string()))
        } else {
            match Stage::find_by_id(&self.inner.pool, item.stage_id).await {
                Ok(Some(stage)) => {
                    let internal = stage.visibility == StageVisibility::Internal;
                    (internal, Some(stage.machine_name))
                }
                Ok(None) => {
                    // A stage that cannot be found must not make an item public:
                    // deny, as the database error below does.
                    warn!(
                        stage_id = %item.stage_id,
                        item_id = %item.id,
                        "stage not found for item, denying access"
                    );
                    return Ok(false);
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

        // Invoke tap_item_access, with the re-entrancy marker set: a handler
        // calling item-api would ask for another decision (see `reentry`).
        let results = reentry::deciding_access(self.inner.dispatcher.dispatch(
            "tap_item_access",
            &input_json,
            state,
        ))
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

    /// The field-access cache key for one decision.
    ///
    /// Keyed on the **viewer**, not only on their permission set: the payload a
    /// plugin receives ([`FieldAccessUser`]) carries `user_id` and
    /// `authenticated` as well as `permissions`, and nothing stops a plugin
    /// reading them. The documented contract asks a plugin to decide from
    /// `(permissions, item_type, field, operation)` alone, and the in-tree
    /// reference plugin does — but a contract is not an enforcement. With the
    /// key on the permission hash alone, one plugin author taking the payload
    /// at face value meant one user's answer was served to every other user
    /// holding the same permissions for up to five minutes, an anonymous
    /// visitor and a logged-in user with the same effective set included.
    ///
    /// Every anonymous viewer carries the nil id and `authenticated = false`,
    /// so they still share entries with each other, which is where the cache
    /// does most of its work.
    fn field_access_cache_key(
        user: &UserContext,
        item_type: &str,
        field: &str,
        operation: &str,
    ) -> String {
        let perm_hash = Self::perm_hash(user);
        let authenticated = u8::from(user.authenticated);
        format!(
            "{perm_hash:x}:{}:{authenticated}:{item_type}:{field}:{operation}",
            user.id
        )
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
        let mut missing: Vec<String> = Vec::new();
        for name in field_names {
            let cache_key = Self::field_access_cache_key(user, item_type, name, operation);
            if let Some(allowed) = self.inner.metrics.cache_lookup(
                CacheName::FieldAccess,
                self.inner.field_access_cache.get(&cache_key),
            ) {
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
                let results = reentry::deciding_access(self.inner.dispatcher.dispatch(
                    "tap_field_access",
                    &input_json,
                    state,
                ))
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
            let cache_key = Self::field_access_cache_key(user, item_type, name, operation);
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

    /// Refuse a write that would change a field this user may not edit, and
    /// copy back the ones they may not touch but did not submit.
    ///
    /// The field-access decision has always taken an `operation` and the design
    /// has always named two, `"view"` and `"edit"` — but every caller asked for
    /// `"view"` and no write path asked at all. A user who could edit an item
    /// could therefore overwrite a field they were not even allowed to see.
    /// This is the one place `"edit"` is consulted: [`Self::create`],
    /// [`Self::update`], [`Self::revert_to_revision`] and
    /// [`Self::save_translation`] all run it on the **client-supplied** fields,
    /// before `tap_item_presave`, because a plugin's presave edits are trusted
    /// code rather than user input.
    ///
    /// `submitted` is the incoming `fields` object; `None` means the caller is
    /// not changing fields at all, and nothing is decided. `stored` is the
    /// item's current `fields`, `None` on a create. The return value is the
    /// object to persist.
    ///
    /// Per key the user may not edit:
    ///
    /// - submitted with a value that differs from the stored one ⇒ **refused**;
    /// - submitted with the identical value ⇒ accepted, so a form that
    ///   round-tripped a value is not an error;
    /// - **absent** ⇒ copied back from `stored`, because a JSON update replaces
    ///   the whole object and leaving a field out would otherwise erase it;
    /// - submitted as `null` where the stored value is not ⇒ a different value
    ///   like any other, so refused: an explicit removal is still a change.
    ///
    /// **Refused, not silently dropped.** A client whose field is quietly
    /// discarded is told its save succeeded when part of it was thrown away. On
    /// the HTML forms a field the user may not edit is never rendered as an
    /// input at all ([`crate::content::FieldMode`]), so a refusal here means
    /// the request was built by hand.
    ///
    /// The administrator bypass is the one in [`Self::field_access_decisions`],
    /// so it is the same bypass the read paths get, spelled once.
    pub async fn gate_field_writes(
        &self,
        user: &UserContext,
        item_type: &str,
        submitted: Option<&serde_json::Value>,
        stored: Option<&serde_json::Value>,
    ) -> Result<Option<serde_json::Value>, WriteDenied> {
        // The kernel-internal background principal (P11c / D-40) writes as
        // nobody: it carries no human identity and holds no permissions, so
        // every governed field would be denied to it and a cron job re-saving
        // an item would fail. It is also constructed by no web, session or auth
        // path, so this is not a channel a user can reach.
        if user.is_background() {
            return Ok(submitted.cloned());
        }
        let Some(submitted) = submitted else {
            return Ok(None);
        };
        let Some(submitted_obj) = submitted.as_object() else {
            // Not a JSON object, so it has no field keys to decide. The model
            // layer stores it as it always did.
            return Ok(Some(submitted.clone()));
        };
        let stored_obj = stored.and_then(|v| v.as_object());

        // The union of both key sets. A key only in the *stored* item has to be
        // decided too: its absence from the submission is what has to be copied
        // back rather than erased.
        let mut names: Vec<String> = submitted_obj.keys().cloned().collect();
        for key in stored_obj.iter().flat_map(|o| o.keys()) {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
        if names.is_empty() {
            return Ok(Some(submitted.clone()));
        }

        let decisions = self
            .field_access_decisions(user, item_type, &names, "edit")
            .await;

        let mut out = submitted_obj.clone();
        let mut refused: Vec<String> = Vec::new();
        for name in &names {
            // Fail-open default, as everywhere else: a field no plugin decided
            // is editable.
            if decisions.get(name).copied().unwrap_or(true) {
                continue;
            }
            match (
                submitted_obj.get(name),
                stored_obj.and_then(|o| o.get(name)),
            ) {
                // Submitted unchanged: nothing is being written.
                (Some(new), Some(old)) if new == old => {}
                // Submitted as anything else: a change to a denied field.
                (Some(_), _) => refused.push(name.clone()),
                // Not submitted: keep what is stored.
                (None, Some(old)) => {
                    out.insert(name.clone(), old.clone());
                }
                (None, None) => {}
            }
        }

        if !refused.is_empty() {
            refused.sort();
            return Err(WriteDenied::Fields(refused));
        }
        Ok(Some(serde_json::Value::Object(out)))
    }

    /// Decide the `status` an item write may store, refusing a published state
    /// this user may not set.
    ///
    /// Whether an item is published came straight from the client — a `status`
    /// key in a JSON body, a checkbox in a form — and no permission stood
    /// behind it, so anyone who could create content could put it on the live
    /// site. [`PUBLISH_CONTENT`] is that permission, and this is the one place
    /// it is checked; author, promote, sticky and stage were already server
    /// controlled on these paths, and status was the last client-controlled one.
    ///
    /// `submitted` is the status the caller asked for (`None` = said nothing);
    /// `stored` is the item's current status, `None` on a create. The return
    /// value is the status to store, where `None` means "leave it".
    ///
    /// Without the permission:
    ///
    /// - on a create, an explicit published state is **refused**, and saying
    ///   nothing stores `0` — because [`crate::models::Item::create`] defaults a
    ///   missing status to published, so silence has to be answered explicitly
    ///   or the default publishes for a user who may not;
    /// - on an update or a revert, **any** change is refused, in either
    ///   direction (unpublishing someone else's live page is a change too), and
    ///   an unchanged status is accepted so an ordinary edit still saves.
    ///
    /// Pure and `&self`-free: the answer cannot differ between the paths that
    /// ask it, and it needs no dispatch.
    pub fn gate_publish(
        user: &UserContext,
        submitted: Option<i16>,
        stored: Option<i16>,
    ) -> Result<Option<i16>, WriteDenied> {
        // `can` is the administrator bypass plus the role grant, spelled once.
        // The background principal passes for the reason given in
        // [`Self::gate_field_writes`]: it holds no permissions at all, and cron
        // and the queue worker publish on the site's behalf, not a user's.
        if user.is_background() || user.can(PUBLISH_CONTENT) {
            return Ok(submitted);
        }
        match stored {
            None => match submitted {
                None | Some(0) => Ok(Some(0)),
                Some(_) => Err(WriteDenied::Publish),
            },
            Some(current) => match submitted {
                None => Ok(None),
                Some(new) if new == current => Ok(Some(new)),
                Some(_) => Err(WriteDenied::Publish),
            },
        }
    }

    /// Flush this server's field-access decision cache (design amendment α).
    ///
    /// What another server's `FieldAccess` bump does here. The writing server
    /// flushes the same shared (`Arc`) cache from the `variables::set` host path
    /// and bumps. Blunt whole-cache invalidation is acceptable: config writes
    /// are rare admin actions and moka coalesces the refill.
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

    /// Load an item and some or all of its revisions, for this user.
    ///
    /// The one access path every revision read takes: the history page, the
    /// preview and the compare. A revision is an editor's view of the item, so
    /// the item answers to `view` first (a refusal is [`RevisionAccess::NotFound`],
    /// as for a missing item) and then to `edit` ([`RevisionAccess::Forbidden`]).
    ///
    /// [`RevisionSelection::Ids`] loads the named revisions with one query scoped
    /// to the item, so a revision id is never trusted on its own: one that
    /// belongs to another item, or to none, is not found. They come back in the
    /// order asked for.
    ///
    /// Every revision's `fields` is cut to what `accessible_fields(.., "view")`
    /// allows, in one batched decision over the names all of them carry. A field
    /// the user may not see is therefore absent from every snapshot, and a
    /// comparison of two snapshots cannot report it even as removed.
    pub async fn load_revisions(
        &self,
        item_id: Uuid,
        which: RevisionSelection<'_>,
        user: &UserContext,
    ) -> std::result::Result<ItemRevisions, RevisionAccess> {
        let item = self
            .load(item_id)
            .await
            .map_err(RevisionAccess::Failed)?
            .ok_or(RevisionAccess::NotFound)?;
        if !self
            .check_access(&item, "view", user)
            .await
            .map_err(RevisionAccess::Failed)?
        {
            return Err(RevisionAccess::NotFound);
        }
        if !self
            .check_access(&item, "edit", user)
            .await
            .map_err(RevisionAccess::Failed)?
        {
            return Err(RevisionAccess::Forbidden);
        }

        let mut revisions = match which {
            RevisionSelection::All => Item::get_revisions(&self.inner.pool, item_id)
                .await
                .map_err(RevisionAccess::Failed)?,
            RevisionSelection::Ids(ids) => {
                let found = Item::get_revisions_by_ids(&self.inner.pool, item_id, ids)
                    .await
                    .map_err(RevisionAccess::Failed)?;
                ids.iter()
                    .map(|id| found.iter().find(|r| r.id == *id).cloned())
                    .collect::<Option<Vec<_>>>()
                    .ok_or(RevisionAccess::NotFound)?
            }
        };

        let mut names: Vec<String> = revisions
            .iter()
            .filter_map(|r| r.fields.as_object())
            .flat_map(|o| o.keys().cloned())
            .collect();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            let visible: std::collections::HashSet<String> = self
                .accessible_fields(user, &item.item_type, &names, "view")
                .await
                .into_iter()
                .collect();
            for revision in &mut revisions {
                if let Some(fields) = revision.fields.as_object_mut() {
                    fields.retain(|name, _| visible.contains(name));
                }
            }
        }

        Ok(ItemRevisions { item, revisions })
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

        // A revert is a write, so it runs both gates — on the revision's own
        // content, which is what it is asking to store. Reverting to a revision
        // whose published state differs needs `publish content`, and reverting a
        // field the user may not edit to an older value is refused.
        let revision = Item::get_revision(&self.inner.pool, revision_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("revision not found"))?;
        if revision.item_id != item_id {
            anyhow::bail!("revision does not belong to this item");
        }

        let status = Self::gate_publish(user, Some(revision.status), Some(item.status))?;
        let fields = self
            .gate_field_writes(
                user,
                &item.item_type,
                Some(&revision.fields),
                Some(&item.fields),
            )
            .await?;

        // The gated input is applied here rather than through
        // `Item::revert_to_revision`, which restores the revision's fields
        // verbatim and would undo the copy-back above. That model method, like
        // `Item::create` and `Item::update`, enforces nothing by design — the
        // service is where the gates live.
        let refs = match fields.as_ref() {
            Some(fields) => {
                let refs = self.file_references(&item.item_type, Some(fields)).await?;
                self.authorize_new_file_references(Some(item_id), &refs, user)
                    .await?;
                Some(refs)
            }
            None => None,
        };
        let updated = Item::update_with_file_refs(
            &self.inner.pool,
            item_id,
            user.id,
            UpdateItem {
                stage_id: None,
                title: Some(revision.title),
                status,
                promote: None,
                sticky: None,
                fields,
                log: Some(format!("Reverted to revision {revision_id}")),
            },
            refs.as_ref(),
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("failed to revert item"))?;

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

    /// Invalidate a cached item, here and on every other server (which clear
    /// their whole item cache). Call it after the write commits.
    pub fn invalidate(&self, id: Uuid) {
        self.inner.cache.invalidate(&id);
        bump(CacheKind::Items);
    }

    /// Clear all cached items, here and on every other server.
    pub fn clear_cache(&self) {
        self.clear_local();
        bump(CacheKind::Items);
    }

    /// Clear this server's item cache only: what another server's bump does
    /// here.
    pub fn clear_local(&self) {
        self.inner.cache.invalidate_all();
    }

    /// Every stage and the resolved workflow, read from Postgres.
    ///
    /// Two queries (`Stage::list_all` and the [`WORKFLOW_KEY`] variable), not
    /// cached: stages are few, and a stage change made on one server must hold
    /// on every server at once.
    pub async fn stage_view(&self) -> Result<Arc<StageView>> {
        let stages = Stage::list_all(&self.inner.pool).await?;
        let value = SiteConfig::get(&self.inner.pool, WORKFLOW_KEY).await?;
        let workflow = Workflow::resolve(value.as_ref(), &stages);
        Ok(Arc::new(StageView { stages, workflow }))
    }

    /// Whether `user` may move an item from stage `from` to stage `to`: the one
    /// rule `create`, `update` and [`Self::move_targets`] apply
    /// (`docs/design/stage-workflow.md`).
    ///
    /// The workflow half: a transition from `from` to `to` whose permission the
    /// user holds. The other half, that the user may edit the item (a plugin's
    /// Deny included), is the caller's, decided before this is asked. An
    /// administrator, or the kernel background principal, may move any item to
    /// any stage that exists. A stage that does not exist is never a target.
    fn may_move(&self, view: &StageView, from: Uuid, to: Uuid, user: &UserContext) -> bool {
        if view.stage(to).is_none() {
            return false;
        }
        if user.is_admin() || user.is_background() {
            return true;
        }
        view.workflow().allows(from, to, user)
    }

    /// The stages `user` may move `item` to, in stage order, excluding the one
    /// it is on. Empty when they may move it nowhere. What a stage selector
    /// offers, decided by the same rule [`Self::update`] applies.
    pub async fn move_targets(&self, item: &Item, user: &UserContext) -> Result<Vec<Stage>> {
        if !self.check_access(item, "edit", user).await? {
            return Ok(Vec::new());
        }
        let view = self.stage_view().await?;
        Ok(view
            .stages()
            .iter()
            .filter(|s| s.id != item.stage_id)
            .filter(|s| self.may_move(&view, item.stage_id, s.id, user))
            .cloned()
            .collect())
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
    // =======================================================================
    // S4 — the decision cache is keyed on the viewer, not only on their
    // permission set.
    //
    // The key was `{perm_hash}:{type}:{field}:{operation}` and the payload a
    // plugin receives carries `user_id` and `authenticated` as well. The
    // documented contract asks a plugin to ignore those; nothing enforced it,
    // so one plugin author reading the payload literally meant one user's
    // decision was served to every other user with the same permissions for
    // up to five minutes.
    // =======================================================================

    fn key(user: &UserContext, field: &str) -> String {
        ItemService::field_access_cache_key(user, "person", field, "view")
    }

    #[test]
    fn two_users_with_the_same_permissions_get_different_cache_keys() {
        let perms = vec!["view pii".to_string()];
        let a = UserContext::authenticated(Uuid::now_v7(), perms.clone());
        let b = UserContext::authenticated(Uuid::now_v7(), perms);
        assert_ne!(
            key(&a, "ssn"),
            key(&b, "ssn"),
            "one user's decision must not be served to another"
        );
    }

    #[test]
    fn an_anonymous_and_an_authenticated_viewer_do_not_share_a_cache_key() {
        // The sharpest case: the same (nil) id and the same empty permission
        // set, differing only in being logged in.
        let anon = UserContext::anonymous();
        let authed = UserContext::authenticated(Uuid::nil(), Vec::new());
        assert_ne!(key(&anon, "ssn"), key(&authed, "ssn"));
    }

    #[test]
    fn two_anonymous_viewers_still_share_a_cache_key() {
        // What keeps the cache worth having: every anonymous visitor carries
        // the nil id and the same flag, so they share entries with each other.
        assert_eq!(
            key(&UserContext::anonymous(), "ssn"),
            key(&UserContext::anonymous(), "ssn")
        );
    }

    #[test]
    fn the_key_still_separates_type_field_and_operation() {
        let user = UserContext::authenticated(Uuid::now_v7(), vec!["view pii".to_string()]);
        let view = ItemService::field_access_cache_key(&user, "person", "ssn", "view");
        let edit = ItemService::field_access_cache_key(&user, "person", "ssn", "edit");
        let other_field = ItemService::field_access_cache_key(&user, "person", "salary", "view");
        let other_type = ItemService::field_access_cache_key(&user, "record", "ssn", "view");
        assert_ne!(view, edit, "view and edit are different questions");
        assert_ne!(view, other_field);
        assert_ne!(view, other_type);
    }

    #[test]
    fn a_permission_change_still_changes_the_key() {
        let id = Uuid::now_v7();
        let before = UserContext::authenticated(id, Vec::new());
        let after = UserContext::authenticated(id, vec!["view pii".to_string()]);
        assert_ne!(key(&before, "ssn"), key(&after, "ssn"));
    }

    // =======================================================================
    // S4 — the publish gate. Whether an item is published came from the client
    // with no permission behind it, so anyone who could create content could
    // put it on the live site.
    // =======================================================================

    fn creator() -> UserContext {
        UserContext::authenticated(Uuid::now_v7(), vec!["create content".to_string()])
    }

    fn publisher() -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            vec!["create content".to_string(), PUBLISH_CONTENT.to_string()],
        )
    }

    #[test]
    fn a_create_that_says_nothing_is_stored_unpublished_without_the_permission() {
        // `Item::create` defaults a missing status to published, so silence has
        // to be answered with an explicit 0 or the default publishes for a user
        // who may not.
        assert_eq!(
            ItemService::gate_publish(&creator(), None, None),
            Ok(Some(0))
        );
    }

    #[test]
    fn a_create_asking_to_publish_is_refused_without_the_permission() {
        assert_eq!(
            ItemService::gate_publish(&creator(), Some(1), None),
            Err(WriteDenied::Publish)
        );
    }

    #[test]
    fn a_create_asking_for_a_draft_is_allowed_without_the_permission() {
        assert_eq!(
            ItemService::gate_publish(&creator(), Some(0), None),
            Ok(Some(0))
        );
    }

    #[test]
    fn an_update_leaving_the_status_alone_is_allowed_without_the_permission() {
        // The ordinary case: editing a published page without touching whether
        // it is published has to keep working.
        assert_eq!(
            ItemService::gate_publish(&creator(), None, Some(1)),
            Ok(None)
        );
        assert_eq!(
            ItemService::gate_publish(&creator(), Some(1), Some(1)),
            Ok(Some(1))
        );
    }

    #[test]
    fn an_update_changing_the_status_either_way_is_refused_without_the_permission() {
        assert_eq!(
            ItemService::gate_publish(&creator(), Some(1), Some(0)),
            Err(WriteDenied::Publish),
            "publishing a draft needs the permission"
        );
        assert_eq!(
            ItemService::gate_publish(&creator(), Some(0), Some(1)),
            Err(WriteDenied::Publish),
            "unpublishing a live item is a change too"
        );
    }

    #[test]
    fn the_permission_holder_gets_the_status_they_asked_for() {
        assert_eq!(
            ItemService::gate_publish(&publisher(), Some(1), None),
            Ok(Some(1))
        );
        assert_eq!(
            ItemService::gate_publish(&publisher(), Some(0), Some(1)),
            Ok(Some(0))
        );
        // And saying nothing still means "leave it", not "unpublish".
        assert_eq!(
            ItemService::gate_publish(&publisher(), None, Some(1)),
            Ok(None)
        );
    }

    #[test]
    fn the_administrator_bypass_applies_to_the_publish_gate() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec!["administer site".to_string()]);
        assert_eq!(
            ItemService::gate_publish(&admin, Some(1), None),
            Ok(Some(1))
        );
    }

    #[test]
    fn the_background_principal_publishes_and_is_not_asked_for_a_permission() {
        // Cron and the queue worker write as nobody: no human identity, no
        // permissions at all. Holding them to a human permission plane would
        // mean a scheduled job could not publish what it was scheduled to
        // publish. The marker is constructed by no web, session or auth path.
        let bg = UserContext::background();
        assert!(!bg.can(PUBLISH_CONTENT), "it holds no permissions");
        assert_eq!(ItemService::gate_publish(&bg, Some(1), None), Ok(Some(1)));
        assert_eq!(
            ItemService::gate_publish(&bg, Some(0), Some(1)),
            Ok(Some(0))
        );
    }
}
