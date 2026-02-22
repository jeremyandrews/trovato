# Trovato Search Architecture

**Status:** Design complete; Phase 1 index pipeline implemented
**Date:** 2026-02-22
**Context:** Progressive enhancement search architecture inspired by [The Practical Path to AI Search](https://www.tag1.com/how-to/the-practical-path-to-ai-search/). Adapted from Tag1's production implementation to Trovato's Rust/WASM plugin architecture.

---

## The Problem

Trovato already has tsvector-based full-text search (PostgreSQL, server-side). It works, but:

1. **Every search is a round-trip.** User types, browser hits the server, PostgreSQL runs the query, results come back. Fast, but never instant.
2. **Keyword-only.** Searching "performance issues" won't find an article about "site speed optimization." Users don't think in keywords; they think in concepts.
3. **No progressive enhancement.** Results are all-or-nothing. The user waits for the server, sees results, done. No opportunity to enrich results after the initial display.

The goal: instant client-side results with no server load for basic searches, AI-powered semantic understanding layered on top, and streaming AI summaries for a conversational search experience. All without requiring vector databases or embedding pipelines as a prerequisite.

---

## Architecture: Four-Stage Progressive Enhancement

The core insight: show something instantly, then make it smarter. Each stage adds intelligence, and each stage is independently useful. A site can ship Stage 1 alone and add stages later.

```
User types query
    |
    +-> Stage 1: Pagefind (client-side, ~50ms, no server)
    |        +-> Instant keyword results displayed
    |
    +-> Stage 2: AI Query Expansion (server-side, ~500ms)
    |        +-> Expanded terms sent back to client
    |              +-> Client re-queries Pagefind with expanded terms
    |                    +-> Additional results merged into display
    |
    +-> Stage 3: Result Merging & Re-ranking (client-side)
    |        +-> Deduplicate, apply ranking signals, reorder display
    |
    +-> Stage 4: AI Summary (server-side, ~2s, streamed via SSE)
             +-> Synthesized answer streams above results
```

### Stage 1: Pagefind (Client-Side Instant Search)

[Pagefind](https://pagefind.app/) is a Rust/WASM-based static search library. The search index is pre-built, chunked, and served as static files. The browser downloads only the index fragments it needs (~300kB even for 50,000+ pages). Search runs entirely in the browser -- zero server load, zero latency beyond the local WASM execution.

**How it works in Trovato:**

- **Change detection (plugin):** The `trovato_search` plugin's `tap_cron` handler compares `MAX(changed)` of published live-stage items against a stored `last_indexed_at` timestamp. When content is newer, it sets `rebuild_requested = true` in the `pagefind_index_status` signal table.
- **Index generation (kernel):** The kernel cron task checks the signal table. When a rebuild is requested, it exports live-stage published items as HTML fragments (title, body, configured search fields, metadata), runs the Pagefind CLI, and atomically deploys the index via rename-swap to `./static/pagefind/`.
- **Why the split:** WASM plugins run in sandboxed instances with no access to the filesystem or subprocess spawning. The plugin detects *when* a rebuild is needed; the kernel performs the *how*.
- **Client-side search:** The Pagefind JS library loads in the browser. When the user types, Pagefind searches its local index and returns results. No HTTP request, no database query, no server involvement.
- **Fallback:** If JavaScript is disabled or Pagefind fails to load, the search form submits to the server-side tsvector endpoint. Progressive enhancement means the server-side search is always available.

**Why Pagefind, not tsvector-first:**

- **Zero server load** for the most common operation (basic search). The server only gets involved when AI enrichment is requested.
- **Scales with CDN.** The index is static files. 10,000 concurrent searches cost nothing -- they're all local WASM execution.
- **Offline-capable.** Once the index is cached, search works without a network connection (Service Worker territory, but the foundation is there).
- **Faster cold start.** Even the very first search is ~50ms. tsvector requires a round-trip regardless.

**tsvector's role:** tsvector remains the server-side search backbone for programmatic use -- Gather queries in plugin code, API consumers, admin search, cron jobs, anything without a browser. Pagefind is the user-facing default; tsvector is the server-side workhorse.

### Stage 2: AI Query Expansion

When the user searches, the query also goes to the server (async, non-blocking -- Stage 1 results are already visible). The server calls `ai_request(operation: Chat)` with a prompt like:

```
Given the search query "{query}", generate 4-6 related search terms
that cover synonyms, related concepts, and alternative phrasings.
Return only the terms, comma-separated. Be specific to the domain;
avoid generic terms like "technology" or "solution."
```

The LLM returns expanded terms. These go back to the client, which runs them through Pagefind again. New results merge into the existing display.

**Key design decisions:**

- **Expansion runs server-side** because it needs `ai_request()` and the LLM API. But the expanded terms feed back into Pagefind on the client -- the actual search is still client-side.
- **Cache aggressively.** The same query always produces similar expansions. Cache the expansion results (KV store, in-memory, or even a simple lookup table) to avoid redundant LLM calls. The article recommends this explicitly.
- **Filter aggressively.** Early implementations found that generic expansions ("technology", "software") pollute results. The prompt constrains this, and a blocklist of overly-generic terms provides a safety net.
- **This is where tsvector could optionally supplement.** If there are Items not in the Pagefind index (content on internal-visibility stages visible to editors, recently-created content before re-index), a parallel tsvector query with the expanded terms catches them. The results merge client-side.

### Stage 3: Result Merging & Re-ranking

With results from both the original query and expanded terms, the client deduplicates by URL/Item ID and applies ranking signals:

**Ranking signals:**

- **Exact match boost:** Results where search terms appear together rank higher than results where they're scattered. Title matches outweigh body matches.
- **Recency decay:** Exponential decay with a configurable half-life (default: 1 year). New content gets a 50% boost, 1-year-old content 25%, 2-year-old 12.5%. Past a threshold (default: 5 years), an age penalty kicks in, capped at 30%. Authoritative older content can still surface.
- **Priority pages:** Admin-configured mapping of keywords to authoritative pages. Searching "team" always shows the Team page first, regardless of keyword frequency elsewhere.
- **Content type boost:** Configurable per content type. Documentation pages might rank higher than blog posts for technical queries.

```
final_score = base_relevance x exact_match_boost x recency_factor x priority_boost x type_boost
```

These signals are multiplicative. The formula runs entirely client-side -- it's just arithmetic on the result metadata Pagefind already provides (or that we inject into the index at build time).

**UI elements (not AI, but part of the search UX):**

- Autocomplete with recent search history and title suggestions
- Content type filters (Blog, Documentation, Case Study, etc.)
- Date range filtering
- Result count announced for screen readers, proper ARIA labels on interactive elements

### Stage 4: AI Summary Generation

The top N results (with excerpts) go to the server. An `ai_request(operation: Chat)` call synthesizes a summary answer. This streams back via SSE (Server-Sent Events) -- the same infrastructure the chatbot Tile uses (Story 31.7) -- appearing word-by-word above the result list.

**Prompt engineering matters:**

- Keep responses concise (configurable max tokens)
- Cite sources with clickable links to the actual results
- Acknowledge when the results don't actually answer the question (don't hallucinate)
- Include the user's original query in the prompt for relevance

**Conversational follow-up:** After the initial summary, the user can ask follow-up questions. The system extracts keywords from the follow-up, runs additional Pagefind searches for newly relevant content, merges with original context, and generates a response that builds on the previous answer. This uses the same SSE streaming and conversation history as the chatbot Tile from Story 31.7.

**Sentiment analysis:** Each interaction carries a signal. Follow-up phrasing reveals satisfaction, confusion, or unmet needs. A fast, cheap model classifies sentiment asynchronously. The classifications feed into analytics -- high "unsatisfied" rates on a topic flag content gaps, "confused" responses flag clarity problems. This runs async; users never wait for it.

---

## The Vector Database Question

The standard AI search architecture generates embeddings for all content, stores them in a vector database, converts each query to an embedding at search time, and finds nearest neighbors. This works, but for most content sites it's unnecessary complexity.

**Query expansion gets you semantic understanding without embeddings.** The AI knows that "performance issues" and "site speed" refer to the same concept, so it expands the query. Multiple fast keyword searches across related terms, then AI synthesizes. Intelligence at read time rather than index time.

**When you DO need embeddings/vectors:**

- Millions of documents (Pagefind index size becomes unwieldy)
- Content rarely uses the terms users search for (highly technical or domain-specific jargon mismatch)
- Recommendation systems ("show me similar items" based on content similarity, not keywords)
- Multi-modal search (images, audio alongside text)

**Trovato's position:** The `VectorStore` trait and `SemanticSimilarity` Gather operator exist in the AI Integration design (Story 31.11). They're the upgrade path. A site that outgrows query expansion can add pgvector + embeddings, and the summarization/conversation layers transfer over -- you're just swapping how results are found. But query expansion is the default, not embeddings.

---

## Plugin Architecture

The architecture splits responsibility between the `trovato_search` WASM plugin and the kernel:

- **Plugin (WASM):** Detects content changes via `tap_cron`, signals rebuild needed via the `pagefind_index_status` database table. Cannot access the filesystem or spawn subprocesses due to WASM sandbox constraints.
- **Kernel (`cron/pagefind.rs`):** Checks the signal table during cron runs. When `rebuild_requested = true`, exports items as HTML, runs the Pagefind CLI, and atomically deploys the index. Only runs when the `trovato_search` plugin is enabled (`pagefind_enabled` flag on `CronService`).

### `trovato_search` Plugin

| Component | Implementation | Notes |
|---|---|---|
| Change detection | `tap_cron` handler | Compares `MAX(changed)` against `last_indexed_at`; sets `rebuild_requested` in signal table |
| Index generation | Kernel `cron/pagefind.rs` | Exports live-stage items as HTML, runs Pagefind CLI with timeout, atomic rename-swap deploy |
| Index serving | Static files in `./static/pagefind/` | Served by web server/CDN, not Trovato app code |
| Pagefind client JS | Frontend asset bundled with plugin | Loaded on search pages, optional preload on all pages |
| Query expansion endpoint | `tap_menu` -> `/api/v1/search/expand` | Calls `ai_request(operation: Chat)`, returns expanded terms as JSON |
| Expansion cache | KV store or in-memory cache | Same query -> same expansion. Configurable TTL. |
| Re-ranking config | TOML config: priority pages, decay half-life, type boosts | Admin UI for non-technical configuration |
| AI summary endpoint | `tap_menu` -> `/api/v1/search/summarize` | SSE streaming via `ai_request(operation: Chat)` |
| Conversational follow-up | Session-scoped history on summary endpoint | Reuses chatbot SSE infrastructure (Story 31.7) |
| Sentiment classification | Async `ai_request()` post-response | Fast model, fire-and-forget to analytics |
| tsvector fallback | Existing Gather search | Activated when Pagefind unavailable or for server-side consumers |

### Configuration

```toml
[search]
engine = "pagefind"              # "pagefind" (default) or "server" (tsvector only)
index_path = "pagefind"          # relative to static directory
rebuild_on_save = true           # or false for cron-only rebuild

[search.ai]
enabled = true                   # enables Stages 2-4
expansion_cache_ttl = 3600       # seconds; 0 = no cache
expansion_max_terms = 6
summary_max_tokens = 300
summary_model = "default"        # or specific provider/model override
sentiment_enabled = true
sentiment_model = "default"      # fast/cheap model recommended
conversation_max_turns = 3       # follow-up limit per session

[search.ranking]
recency_half_life_days = 365
recency_penalty_after_days = 1825  # 5 years
recency_penalty_max = 0.30
exact_match_boost = 1.5
title_match_boost = 2.0

[[search.ranking.priority_pages]]
keywords = ["team", "about us", "who we are"]
path = "/team"
boost = 10.0

[[search.ranking.type_boosts]]
item_type = "documentation"
boost = 1.3

[[search.ranking.type_boosts]]
item_type = "blog"
boost = 1.0
```

### Permissions

No new permissions needed. Search itself is public (same as today) -- Pagefind indexes live-stage content with public visibility only. The AI-enhanced stages (2-4) require `use ai` permission for authenticated users. Anonymous users can be configured to receive AI features or not (site-wide toggle).

### Rate Limiting

AI search endpoints use the existing Tower rate-limiting middleware. Recommended defaults:

- Query expansion: 30 requests/minute per IP (fast, cached)
- AI summary: 10 requests/minute per IP (expensive, streamed)
- Follow-up: 5 requests/minute per IP (most expensive, context-heavy)

Token budgets from Story 31.3 apply to all search-related AI operations.

---

## Pagefind Integration Details

### Index Build Pipeline

```
Cron interval fires
    |
    +-> Plugin tap_cron:
    |     Compare MAX(changed) vs last_indexed_at
    |     If newer: SET rebuild_requested = true
    |
    +-> Kernel cron/pagefind.rs:
          Check rebuild_requested
          If true:
              |
              +-> Clear signal (prevent double builds)
              |
              +-> Export live-stage published Items as HTML fragments
              |     (title, body, configured search fields, metadata)
              |     Items on stages with non-public visibility excluded --
              |     AI-modified content not indexed until human-promoted
              |
              +-> Run Pagefind CLI (with 2-minute timeout)
              |     (Rust binary, ~2s for 10K pages)
              |
              +-> Deploy index files via rename-swap
              |     (old -> pagefind.old, new -> pagefind, remove backup)
              |
              +-> Update last_indexed_at timestamp
```

For a site with thousands of Items, a full re-index takes seconds. Incremental would be nice but isn't critical at this scale. The cron-based approach naturally debounces -- if 50 Items are imported in batch, one re-index fires on the next cron interval, not 50 individual rebuilds.

### What Goes in the Index

Only items on the **live stage** (which has **public** visibility) are indexed. This is the human-in-the-middle guarantee: AI-enriched content sitting on stages with internal visibility is invisible to search until a human promotes it to the live stage.

Trovato's stage system uses visibility levels (`public`, `internal`, `accessible`) on stage definitions. The live stage is a deterministic UUID seeded during installation with `public` visibility. See the [Stage model](../../crates/kernel/src/models/stage.rs) for details.

Each live-stage item produces an index entry containing:

- **Title** (boosted weight)
- **Body text** (stripped of HTML)
- **Configured search fields** (from `search_field_config` table -- all fields marked searchable for the item's type)
- **Metadata fields** (configurable per Item type: date, location, author, etc.)
- **URL** (for linking results)
- **Content type** (for filtering and type boost)
- **Publish date** (for recency ranking)

Pagefind supports custom metadata per page, which is how we inject ranking signals into the client-side index.

For editor search (items on internal-visibility stages + live stage), the tsvector fallback handles it server-side with stage-scoped queries -- same as the existing stage-aware kernel search. Editors don't use Pagefind for editorial work; they use the admin search which is already stage-aware.

### Rust/WASM Synergy

Trovato is Rust. Pagefind is Rust. The Pagefind indexer can potentially be called as a library rather than shelled out to a binary -- same language, same toolchain. This isn't required for the initial implementation (CLI invocation via cron is fine), but it's a future optimization: embed the Pagefind indexer directly in the Trovato binary for zero-overhead index rebuilds.

---

## What This Means for the Existing Design

### AI Integration Updates

The AI Integration design doc (`docs/design/ai-integration.md`) describes embeddings + `SemanticSimilarity` as an advanced search upgrade path. With this architecture:

- **Query expansion becomes the default AI search path.** Simpler, no infrastructure, works with Pagefind.
- **Embeddings/pgvector become the advanced path.** For sites that outgrow keyword + expansion.
- **`SemanticSimilarity` Gather operator still exists** -- it's the upgrade, not the default.
- **The `VectorStore` trait still exists** -- it's the pluggable backend for sites that need it.

### Epic 3 Integration

[Epic 3 (AI as a Building Block)](../ritrovo/epic-03.md) provides the AI Core that Stages 2-4 depend on:

- **Story 31.1-31.2:** Provider registry and `ai_request()` host function -- required for query expansion and summary generation
- **Story 31.3:** Token budgets -- apply to all search AI operations
- **Story 31.7:** Chatbot Tile with SSE streaming -- shared infrastructure with search summaries
- **Story 31.11:** VectorStore trait + pgvector -- the upgrade path beyond query expansion

The tutorial teaches query expansion first because it's simpler and demonstrates the AI subsystem without introducing pgvector. Embeddings become an optional "going further" section.

---

## Phasing

**Phase 1: Pagefind integration** (no AI, just client-side search)
- Pagefind index generation on content change (tap_cron + kernel cron builder)
- Client-side search UI replacing or supplementing server-side search
- tsvector fallback for server-side consumers
- Basic ranking signals (recency, exact match, title boost)

**Phase 2: AI query expansion** (requires AI Core from Epic 3 Phase A)
- `/api/v1/search/expand` endpoint
- Expansion cache
- Client-side merging of expanded results
- Priority pages configuration

**Phase 3: AI summaries & conversation** (requires Phase 2)
- SSE streaming summaries
- Conversational follow-up
- Sentiment classification
- Analytics integration

**Phase 4: Embeddings upgrade path** (optional, requires Epic 3 Phase B)
- pgvector + `SemanticSimilarity` for sites that need it
- Hybrid: Pagefind for instant results + semantic similarity for precision
- Recommendation-driven "related content" features

---

## Related

- [The Practical Path to AI Search](https://www.tag1.com/how-to/the-practical-path-to-ai-search/) -- Source article. Tag1's production implementation.
- [Pagefind](https://pagefind.app/) -- Rust/WASM static search library
- [Epic 2: Search That Thinks](../ritrovo/epic-02.md) -- BMAD stories implementing this architecture
- [Epic 3: AI as a Building Block](../ritrovo/epic-03.md) -- AI Core architecture, provider registry, `ai_request()`, token budgets
- [AI Integration Design](ai-integration.md) -- Full AI design document
