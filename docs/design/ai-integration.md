# Trovato AI Integration

**Status:** Design complete
**Date:** 2026-02-22
**Context:** Inspired by Drupal's AI module ecosystem, adapted to Trovato's WASM plugin architecture, tap system, and existing concepts.

---

## What Drupal Does (and What We Can Learn)

Drupal's [AI module](https://www.drupal.org/project/ai) (stable since October 2025, 3,200+ production sites) is built around a **provider abstraction layer** that normalizes access to 48+ AI services through a unified API. The key pieces:

1. **AI Core** -- Provider plugin system. Each provider (OpenAI, Anthropic, Ollama, etc.) implements a standard interface for one or more "operation types" (chat, embeddings, text-to-image, speech-to-text, text-to-speech, moderation). Site builders pick a default provider per operation type. Swapping providers doesn't break site logic.

2. **Key module** -- Centralized, secure API key storage. Keys stored in files, env vars, or external vaults. Providers reference keys by name, never by value in config.

3. **AI Automators** -- Field-level AI integration. An automator links a prompt + source field to a target field. On entity save, the automator runs the prompt through the configured provider and populates the target. Automators can be chained (output of one feeds the next). This is the workhorse -- content summarization, taxonomy suggestion, alt-text generation, translation, tone adjustment.

4. **AI Assistant API + Chatbot** -- Conversational interface. An assistant has a system prompt and a set of "actions" (plugin-based). The LLM decides which actions to invoke based on user input. The chatbot is a thin frontend; the assistant API is the orchestration layer.

5. **AI Search** -- Embeddings + vector database integration via Search API. Content chunked, embedded, stored in Milvus/Pinecone/Qdrant. Enables semantic search and RAG (Retrieval-Augmented Generation) for chatbot context.

6. **AI CKEditor** -- Inline AI in the rich text editor. Rewrite, translate, summarize, expand, adjust tone -- all without leaving the editor.

7. **AI Content** -- Assistive tools for content editing: taxonomy suggestions, moderation checks, automatic alt-text, tone adjustment.

8. **AI Validations** -- LLM-powered field validation rules.

### What's Good

- Provider abstraction is the right core idea. Nobody wants vendor lock-in.
- Automators on fields are clever. They make AI invisible -- content just gets better as you save it.
- Chaining automators creates complex workflows from simple pieces.
- Search API integration for embeddings/RAG is well-thought-out.
- Key module as a separate dependency is clean separation of concerns.

### What's Overengineered (for Trovato's Purposes)

- The sheer number of sub-modules (AI Automators, AI Search, AI Assistants API, AI Chatbot, AI CKEditor, AI Content, AI Validations, AI External Moderation, AI Agents, AI Agents Explorer, AI Agents Modeler API...) is classic Drupal module sprawl. Many do one small thing.
- The "AI Agents" layer on top of "AI Assistants" on top of "AI Core" is three levels of abstraction where Trovato can do it in one.
- ECA (Event-Condition-Action) as the workflow engine is a Drupal-specific concept with no Trovato equivalent, and honestly, the tap system is cleaner.

### The Trovato Opportunity

Trovato's WASM plugin architecture, tap system, and host functions give us a natural integration surface that Drupal has to bolt on. A Trovato AI plugin can be simpler, more integrated, and more powerful because:

- **Taps are the automator system.** `tap_item_presave` already fires before save. An AI-powered tap that enriches content before save IS the automator, without a separate "automator" abstraction.
- **Host functions are the provider interface.** An `ai_request()` host function is the clean equivalent of Drupal's provider abstraction -- one function, multiple backends, the kernel routes to the right provider.
- **Gather is already the query engine.** RAG context retrieval can integrate with Gather rather than requiring a parallel "AI Search" module.
- **The permission system already exists.** `tap_item_access` and role-based permissions handle who can use AI features -- no separate "AI governance" layer needed.
- **Rate limiting already exists.** Tower middleware for the REST API handles rate limiting. AI endpoints get the same treatment.

---

## Proposed Architecture

### Layer 1: AI Core (Kernel-Level)

This is NOT a plugin. It's a core Trovato subsystem, like caching or file storage. It provides:

#### 1.1 Provider Registry

```rust
// Providers are configured in site config, not in plugin code.
// Each provider implements a standard trait.

enum AiOperationType {
    Chat,           // Text generation / conversation
    Embedding,      // Vector embeddings for semantic search
    ImageGeneration,// Text-to-image
    SpeechToText,   // Audio transcription
    TextToSpeech,   // Audio generation
    Moderation,     // Content safety classification
}

struct AiProviderConfig {
    id: String,              // "openai", "anthropic", "ollama", etc.
    api_key_ref: String,     // Reference to key in secure key store
    base_url: Option<String>,// For self-hosted (Ollama, vLLM, etc.)
    models: HashMap<AiOperationType, String>, // Default model per operation type
    rate_limit: Option<RateLimit>,            // Per-provider rate limit
}
```

The site administrator configures providers via admin UI or config file. Each operation type has a site-wide default provider, overridable per-request.

#### 1.2 Secure Key Store

API keys stored outside the database, similar to Drupal's Key module:

- **Environment variables** (recommended for production)
- **Config file** (for development -- `trovato.toml` or similar, gitignored)
- **External vault** (HashiCorp Vault, AWS Secrets Manager -- stretch goal)

Keys referenced by name in all config. Never stored in plain text in the database. Never exposed in admin UI after initial entry (masked display). Never accessible to WASM plugins -- the kernel handles all authenticated API calls.

#### 1.3 Host Function: `ai_request()`

The single integration point for plugins:

```rust
// Plugin calls:
let response = ai_request(AiRequest {
    operation: AiOperationType::Chat,
    model: None,  // None = use site default for this operation type
    messages: vec![
        AiMessage { role: "system", content: "You are a helpful assistant." },
        AiMessage { role: "user", content: "Summarize this conference description." },
    ],
    options: AiRequestOptions {
        max_tokens: Some(200),
        temperature: Some(0.3),
        // No API key here. Ever. The kernel handles auth.
    },
});
```

The kernel:
1. Resolves the provider for the requested operation type
2. Injects the API key from the secure key store
3. Makes the HTTP request (plugins can't make arbitrary HTTP requests to AI providers -- they go through this function)
4. Enforces rate limits (per-provider, per-role, per-plugin)
5. Logs the request for observability (token count, latency, model, calling plugin)
6. Returns a normalized response

This is the equivalent of Drupal's `getDefaultProviderForOperationType()`, but as a host function that any WASM plugin can call.

#### 1.4 AI-Aware Permissions

New permissions declared via `tap_perm`:

- `use ai` -- Base permission to trigger any AI operation
- `use ai chat` -- Use chat/completion operations
- `use ai embeddings` -- Use embedding operations
- `use ai image generation` -- Use image generation
- `configure ai` -- Admin: manage providers, keys, defaults
- `view ai usage` -- Admin: view token usage, logs, costs

These compose with existing role-based access. An editor with `use ai chat` can use AI-assisted content editing. An anonymous user without `use ai` cannot. Rate limits can differ per role (authenticated: 20 req/hr, editor: 100 req/hr, admin: unlimited).

### Layer 2: AI Plugin (WASM)

A `trovato_ai` plugin that uses the core AI subsystem to provide user-facing features. This is the equivalent of Drupal's AI Automators + AI Content + AI CKEditor + AI Assistants in one coherent plugin.

#### 2.1 Content Enrichment via Taps

The plugin implements taps that fire on standard content lifecycle events:

**`tap_item_presave`** -- Before an Item is saved, optionally run configured AI operations:

- **Summarization:** If the `description` field changed and a `summary` field exists, generate a summary.
- **Taxonomy suggestion:** Analyze text content, suggest categories from the existing taxonomy. Present as suggestions (not auto-applied) unless configured otherwise.
- **Alt-text generation:** If an image file was uploaded without alt text, generate it.
- **Translation:** If content is in a non-default language, queue a translation (or translate inline if configured).
- **Moderation:** Run content through a moderation endpoint. Flag or block content that violates site policy.

Each of these is a **configurable rule** on a per-field or per-item-type basis. The admin configures which AI operations run on which fields, with what prompts, in what order. This is the "automator" concept, but expressed as tap configuration rather than a separate entity type.

Configuration stored as site config:

```toml
[[ai.field_rules]]
item_type = "conference"
field = "description"
trigger = "on_change"      # only when field value changes
operation = "chat"
prompt = "Summarize this conference description in 2-3 sentences for a listing card."
target_field = "summary"
behavior = "fill_if_empty"  # or "always_update"
weight = 10                 # ordering when multiple rules exist

[[ai.field_rules]]
item_type = "conference"
field = "description"
trigger = "on_change"
operation = "chat"
prompt = "Suggest up to 3 topic categories from this list: {categories}. Return only the category names, comma-separated."
target_field = "topics"
behavior = "suggest"        # presents suggestions in UI, doesn't auto-apply
weight = 20
```

**`tap_item_view`** -- Inject AI-powered elements into the render tree:

- "Ask AI about this conference" expandable section on detail pages
- AI-generated "related conferences" suggestions (via embeddings similarity)
- Smart content warnings (via moderation API)

**`tap_form_alter`** -- Add AI assistance to forms:

- "AI Assist" button next to WYSIWYG fields (rewrite, expand, shorten, translate, adjust tone)
- "Suggest categories" button on taxonomy fields
- "Generate alt text" button on image upload fields

#### 2.2 AI-Powered Search

Trovato's search architecture uses progressive enhancement: instant client-side results via Pagefind, with AI query expansion and streaming summaries layered on top. See [search-architecture.md](search-architecture.md) for the full four-stage design.

**Default path: Pagefind + AI query expansion**

- Pagefind (Rust/WASM) runs entirely in the browser. The search index is pre-built on content save and served as static files. Zero server load for basic search.
- AI query expansion adds semantic understanding at read time: the LLM expands "performance issues" into related terms ("site speed", "optimization", "bottleneck"), which feed back into Pagefind on the client. Multiple fast keyword searches, merged and re-ranked.
- AI summary generation streams a synthesized answer via SSE, with conversational follow-up.
- tsvector remains the server-side search backbone for Gather queries, API consumers, and admin search.

This requires no embedding pipeline, no vector database, no chunking strategy. Intelligence is added at read time, not index time. The `trovato_search` plugin provides all four stages; no kernel changes needed.

**Advanced path: Embeddings + Gather (upgrade)**

For sites that outgrow keyword + expansion (millions of documents, heavy jargon mismatch, recommendation systems, multi-modal search):

- Embedding generation on Item save via `ai_request(operation: Embedding)`
- pgvector stores embeddings alongside the existing tsvector index
- `SemanticSimilarity` Gather operator: works like any other filter, composes with existing filters (stage-aware, role-aware, exposed)
- The `VectorStore` trait (see [D1](#d1-pgvector-with-pluggable-vector-backend-abstraction)) allows plugin backends (Qdrant, Milvus) for sites that outgrow pgvector
- The summarization and conversation layers from the default path transfer over -- you're just swapping how results are found

**RAG context for chat:**
- The chatbot runs a search query (Pagefind-sourced results or semantic Gather, depending on what's configured) to find relevant content
- Those results become context for the chat prompt
- Access control is enforced on the Gather query, so the chatbot never leaks content the user can't see

#### 2.3 Conversational Interface (Chatbot)

A chatbot Tile that can be placed in any Slot:

**Frontend:** A simple chat interface rendered as a Tile (assignable to Sidebar, or as a floating widget via a dedicated Slot). No external JS framework dependency -- Tera template + vanilla JS + SSE (Server-Sent Events) for streaming responses.

**Backend:** The plugin implements:

- `tap_menu` -- Registers `/api/v1/chat` endpoint (POST, streaming SSE response)
- Messages go through `ai_request(operation: Chat)` with:
  - System prompt configured by admin (site personality, instructions, boundaries)
  - RAG context injected from semantic Gather query on user's message
  - Conversation history (stored in session, configurable depth)
- Rate limited per-role via existing Tower middleware
- Requires `use ai chat` permission

**Actions (Tool Calling):**

The chatbot can be configured with "actions" -- things it can do beyond answering questions:

- **Search content:** Runs a Gather query based on user's request. "Find me Rust conferences in Europe" triggers a Gather with appropriate filters.
- **Subscribe/unsubscribe:** If `ritrovo_notify` is enabled, the chatbot can toggle subscriptions. "Subscribe me to RustConf" calls `invoke_plugin("ritrovo_notify", "subscribe", data)`.
- **Navigate:** Returns links to relevant pages. "Where can I submit a conference?" returns the submission form URL.

Actions are declared as an enum in the plugin and described to the LLM via function-calling / tool-use in the system prompt. The LLM decides which action to invoke; the plugin executes it with full access control checks. This is the equivalent of Drupal's AI Assistant Actions, but using the existing `invoke_plugin` mechanism rather than a separate action plugin system.

#### 2.4 Admin UI

Under Configuration > AI:

- **Providers:** Add/edit/remove AI providers. Select API key from key store. Set default models per operation type. Test connection.
- **Field Rules:** Configure per-field AI operations (the automator equivalent). Visual list showing item_type > field > operation > prompt > target. Drag to reorder.
- **Chat Settings:** System prompt, RAG configuration (which Item types to include, which fields to embed), conversation depth, available actions.
- **Usage & Logs:** Token usage over time, per-provider costs, request log with latency/model/plugin/user. Gander integration for profiling.
- **Rate Limits:** Per-role AI rate limits (separate from REST API rate limits, though using the same Tower middleware).

### Layer 3: What Stays in Core vs. Plugin

| Concern | Where | Why |
|---|---|---|
| Provider registry + config | Core | Security: API keys never touch WASM boundary |
| `ai_request()` host function | Core | Security + abstraction: plugins can't make direct AI API calls |
| Secure key store | Core | Security: keys in env vars / vault, not DB |
| Rate limiting | Core (Tower) | Consistency: same mechanism as REST API |
| AI permissions | Core (`tap_perm`) | Consistency: same permission system as everything else |
| Token budget tracking | Core | Consistency: applies uniformly across all AI usage |
| `VectorStore` trait + pgvector impl | Core | Performance: native PostgreSQL; trait allows plugin backends (upgrade path) |
| `SemanticSimilarity` Gather op | Core | Consistency: embeddings are just another filter type (upgrade path) |
| Embedding model tracking | Core | Migration: stale detection, re-indexing queue (upgrade path) |
| Pagefind index + client-side search | Plugin (`trovato_search`) | Default search UX: instant, zero server load |
| AI query expansion + summaries | Plugin (`trovato_search`) | Progressive enhancement on top of Pagefind |
| Content enrichment taps | Plugin (`trovato_ai`) | Configurability: rules are site-specific |
| Chat/assistant endpoint | Plugin (`trovato_ai`) | Optional: not every site needs a chatbot |
| Form alterations (AI Assist) | Plugin (`trovato_ai`) | Optional: UI enhancements |
| Field rules config | Plugin (`trovato_ai`) | Site-specific: prompts, field mappings |
| Chatbot Tile | Plugin (`trovato_ai`) | Optional: deployable via Slots |
| MCP server | Plugin (`trovato_mcp`) | Optional: exposes existing API to external LLM tools |
| Alternative vector backends | Plugin (e.g., `trovato_qdrant`) | Optional: registers via `tap_vector_store` |

---

## How This Maps to Existing Trovato Concepts

| Drupal AI Concept | Trovato Equivalent | Notes |
|---|---|---|
| AI Provider plugin | Provider config in site config | No separate module per provider. Config-driven. |
| Key module | Secure key store (env vars / vault) | Built into core, not a dependency |
| AI Automators | Field rules in `trovato_ai` plugin | Taps + config, not a separate entity type |
| AI Automator Chains | Field rules with weights | Lower weight runs first, can reference prior fields |
| AI Assistant API | `/api/v1/chat` endpoint | One endpoint, not three abstraction layers |
| AI Assistant Actions | `invoke_plugin` calls from chat handler | Existing mechanism, not a separate plugin type |
| AI Chatbot frontend | Chat Tile in a Slot | Standard Tile, standard Slot assignment |
| AI Search (vector DB) | Pagefind + AI query expansion (default); pgvector + `SemanticSimilarity` Gather op (upgrade) | Client-side first. No vector DB for most sites. |
| AI CKEditor | `tap_form_alter` + AI Assist buttons | Same UI, integrated via existing form system |
| AI Content (taxonomy suggest, etc.) | Field rules on `tap_item_presave` | Same result, existing tap system |
| AI Validations | Field rules with `behavior = "validate"` | Reuses existing Form API validation |
| AI External Moderation | Field rule with `operation = "moderation"` | Just another operation type |
| ECA workflows | Tap system | Taps ARE the event system |
| AI Agents + Modeler | Not needed | The tap system + `invoke_plugin` + Gather already provide orchestration |

---

## Design Decisions

### D1: pgvector with pluggable vector backend abstraction

pgvector is the default and ships with core. It's the right call -- Trovato already depends on PostgreSQL, and pgvector avoids operational complexity for the vast majority of sites. But the vector storage interface is abstracted behind a trait so that plugins can register alternative backends (Qdrant, Milvus, Weaviate) for sites that outgrow pgvector:

```rust
trait VectorStore: Send + Sync {
    fn store_embedding(&self, item_id: ItemId, field: &str, model: &str, embedding: &[f32]) -> Result<()>;
    fn similarity_search(&self, embedding: &[f32], limit: usize, threshold: f32) -> Result<Vec<(ItemId, f32)>>;
    fn delete_embeddings(&self, item_id: ItemId) -> Result<()>;
    fn mark_stale(&self, model: &str) -> Result<u64>;
}
```

Core provides `PgVectorStore`. A plugin like `trovato_qdrant` could register an alternative via a new tap (e.g., `tap_vector_store`). The `SemanticSimilarity` Gather operator dispatches to whichever backend is active.

### D2: SSE for streaming, not WebSockets

SSE (Server-Sent Events) is the streaming transport for chat responses. It's one-directional (server to client), runs over plain HTTP (no proxy/CDN issues), auto-reconnects, and is what every major LLM API uses. A chatbot is inherently request-response: the user POSTs a message, the server streams back tokens. There's no need for bidirectional streaming.

If a future feature genuinely needs bidirectional streaming (not chat), WebSocket support can be added alongside SSE without replacing it.

### D3: Granular token budget system

Token budgets are a first-class concept in the kernel. Budgets are per-vendor (because tokens mean different things to different providers) with per-role defaults and per-user overrides:

```toml
[ai.budgets]
period = "monthly"       # or "daily", "weekly"
action_on_limit = "deny" # or "warn", "queue"

[ai.budgets.defaults]
# Per-vendor, per-role defaults
openai.authenticated = 10_000
openai.editor = 50_000
openai.admin = 0          # 0 = unlimited
anthropic.authenticated = 10_000
anthropic.editor = 50_000
anthropic.admin = 0

# Per-user overrides in admin UI, stored in user Record:
# user.ai_budget_override = { "openai": 100_000 }
```

The kernel tracks token usage per request (from the provider's response metadata). When a role/user hits their budget, the configured action fires. The admin UI shows a usage dashboard with burn-down by provider, role, and user.

Key points:
- Budgets are per-vendor because OpenAI tokens != Anthropic tokens != local model tokens
- Per-role defaults keep config manageable; per-user overrides handle exceptions
- `action_on_limit` gives flexibility: hard deny, soft warning, or queue for later
- The usage log (already part of `ai_request()` observability) feeds the budget tracker

### D4: Local models supported by kernel, not shipped

The provider config with `base_url` already supports local models (Ollama, vLLM, llama.cpp server, etc.). The kernel doesn't need special handling -- a local model is just another provider with a `base_url` pointing to `localhost`. A distribution or plugin could bundle a local model setup, but the core Trovato demo doesn't ship one. That's the right boundary: kernel enables it, ecosystem builds on it.

### D5: MCP as a plugin

Trovato exposes an MCP (Model Context Protocol) server via a `trovato_mcp` plugin. This plugin surfaces existing kernel capabilities (content CRUD, Gather queries, search, user management) as MCP tools and resources. External LLM tools (Claude Desktop, Cursor, VS Code, etc.) connect to the Trovato site as an MCP server and interact with content through the standard API.

The plugin:
- Registers MCP endpoints via `tap_menu`
- Declares available tools based on installed plugins and their permissions
- Enforces access control through the same permission system (the MCP client authenticates as a user; that user's role determines what tools are available)
- Uses the existing REST API endpoints where they exist, adds MCP-specific wrappers where needed

This is a separate plugin from `trovato_ai` -- MCP is about exposing Trovato to external AI tools, while `trovato_ai` is about bringing AI capabilities into Trovato. They can work together but neither depends on the other.

### D6: Embedding model tracking with automatic migration

Each embedding row stores the model identifier alongside the vector:

```sql
CREATE TABLE item_embeddings (
    item_id UUID NOT NULL REFERENCES item(id),
    field_name TEXT NOT NULL,
    model TEXT NOT NULL,          -- e.g. "openai/text-embedding-3-small"
    dimensions SMALLINT NOT NULL, -- e.g. 1536
    embedding vector,             -- pgvector column, dimension from config
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (item_id, field_name)
);
```

When the admin changes the embedding model in config, the kernel:
1. Marks all embeddings with the old model as stale
2. Queues re-embedding jobs (using the existing queue system)
3. `SemanticSimilarity` queries filter on `model = current_model`, so stale embeddings are excluded from results
4. As re-embedding completes, new rows replace old ones
5. A progress indicator shows re-indexing status in the admin UI

The pgvector column uses `vector` without a fixed dimension (or the column is recreated if the new model has different dimensions -- this is a migration, not a hot swap). The queue handles the API cost gradually rather than all at once.

### D7: AI modules as kernel building blocks

All AI-related functionality builds on the kernel's AI subsystem. The `ai_request()` host function, provider registry, key store, token budgets, and embedding storage are building blocks that any plugin can use. There is no separate "AI integration" path.

Specifically for Ritrovo: `ritrovo_translate` calls `ai_request(operation: Chat)` with a translation prompt rather than implementing its own LLM integration. Any plugin that needs AI capabilities -- translation, content enrichment, moderation, summarization -- calls the same `ai_request()` function and benefits from the same provider abstraction, key management, rate limiting, and budget tracking.

This means:
- No plugin needs to manage its own API keys or provider connections
- Swapping AI providers is a single config change that affects all plugins
- Token budgets and rate limits apply uniformly across all AI usage
- The kernel's observability (logging, usage tracking) captures all AI operations regardless of which plugin initiated them

### D8: Stage-based human-in-the-middle for all AI mutations

AI can generate, enrich, and transform content freely -- but it never publishes. Every AI-initiated content mutation flows through Trovato's Stage system, ensuring human review before anything reaches site visitors. See the Stage Architecture for the full Stage design, including the `StageVisibility` enum (`Internal`, `Public`, `Accessible`).

**The principle:** AI writes to `Internal` stages only. Humans promote to the `Public` stage. No exceptions.

**How it works:**

- **Field rules with `behavior = "always_update"` or `"fill_if_empty"`:** When AI auto-populates a field (summary, alt-text, translation), the change is written to the Item's current revision. If the Item is already in the `Public` stage, the AI change creates a new revision in the highest-sort-order `Internal` stage (the `highest_internal` policy -- typically Curated). The `Public` version is untouched until a human promotes the new revision. This is the same "stage a new version" workflow from the revision system: the `Public` revision stays visible while the AI-modified draft sits in an `Internal` stage awaiting review.
- **Field rules with `behavior = "suggest"`:** No Stage concern -- suggestions are presented in the editor UI, never written to content automatically.
- **Field rules with `behavior = "validate"`:** No Stage concern -- validation blocks or allows a save, it doesn't create content.
- **Batch AI operations (bulk import enrichment, batch translation):** Items enter at the `default` stage (Incoming) with AI-enriched fields. Editors review in `Internal` stages before promoting to the `Public` stage. The standard editorial pipeline applies.
- **Chatbot actions (subscribe, search, navigate):** These are read operations or user-scoped mutations (subscriptions). They don't modify published content, so no Stage gating needed.

**Configuration:**

```toml
[ai.staging]
# When AI modifies a Public-stage item, what happens?
public_item_behavior = "highest_internal"  # default: create new revision in highest-sort-order Internal stage
# Other options:
# "in_place" -- modify Public revision directly (opt-in, for low-risk fields like alt-text)
# "specific_stage" -- create revision in a named stage (see ai.staging.target_stage)
# target_stage = "incoming"  # only used when public_item_behavior = "specific_stage"

# Per-field override
[[ai.staging.overrides]]
field = "alt_text"
behavior = "in_place"  # alt-text is low-risk, can update Public revision directly

[[ai.staging.overrides]]
field = "summary"
behavior = "highest_internal"  # summaries need human review

[[ai.staging.overrides]]
field = "body"
behavior = "specific_stage"
target_stage = "incoming"  # body changes are high-risk, full editorial pass
```

**Audit trail:** Every AI-initiated change records the originating field rule, the AI model used, and the input/output in the revision metadata. The revision log shows "AI: auto-summary via openai/gpt-4o" alongside human edits. This makes it trivial to diff what the AI changed vs. what a human wrote, and to revert AI-specific changes without losing human edits.

**Preview in production:** Because AI changes land in `Internal` stages (not the `Public` stage), editors can preview AI-modified content on the production site using stage-scoped URLs. The Stage system already handles this: editors see `Internal` + `Public` stage content, anonymous users see `Public` stage only. AI-generated content is visible for review without a separate staging environment.

---

## Phasing

**Phase A: Core foundation** (can ship with Trovato Phase 4-5)
- `ai_request()` host function
- Provider registry + config (TOML-based, `base_url` for local models)
- Secure key store (env vars / config file / external vault)
- AI permissions (`use ai`, `configure ai`, etc.)
- Token budget tracking (per-vendor, per-role, per-user overrides)

**Phase B: Content enrichment** (after Phase A)
- `trovato_ai` plugin with field rules on `tap_item_presave`
- `tap_form_alter` AI Assist buttons (WYSIWYG, taxonomy, alt-text)
- Admin UI: provider management, field rules, usage dashboard
- Embedding generation on Item save (model tracking, stale detection)
- `VectorStore` trait with `PgVectorStore` implementation
- `SemanticSimilarity` Gather operator

**Phase C: Chatbot + RAG** (after Phase B)
- Chat Tile (SSE streaming, vanilla JS frontend)
- `/api/v1/chat` endpoint via `tap_menu`
- RAG context injection via semantic Gather queries
- Action system via `invoke_plugin`
- Admin UI for chat configuration (system prompt, RAG settings, actions)

**Phase D: MCP + ecosystem** (after Phase A, parallel to B/C)
- `trovato_mcp` plugin exposing kernel capabilities as MCP tools/resources
- Other plugins building on `ai_request()` (e.g., `ritrovo_translate` using Chat for translation)

---

## Related

- [Drupal AI module](https://www.drupal.org/project/ai) -- The inspiration. Stable since October 2025.
- [Drupal AI Roadmap 2026](https://www.drupal.org/blog/drupals-ai-roadmap-for-2026) -- Eight capabilities planned for 2026.
- [Search Architecture](search-architecture.md) -- Pagefind + progressive enhancement search design
- [Ritrovo Overview](../ritrovo/overview.md) -- Ritrovo reference application
