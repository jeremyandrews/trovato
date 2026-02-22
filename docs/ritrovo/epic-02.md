# Epic 2: Search That Thinks

**Tutorial Part:** Supporting (spans Part 2 search + AI Integration)
**Trovato Phase Dependency:** Phase 3 (Search infrastructure), Phase 6 (AI Core for Stages 2-4)
**BMAD Epic:** 30
**Status:** Story 30.1 partially complete (index pipeline implemented); remaining stories not started

---

## Narrative

*Your conference site has content. Users need to find it. But search shouldn't mean "type keywords, wait for the server, scan a wall of links." In this epic, you build search that responds instantly in the browser, understands what the user means (not just what they typed), and can explain what it found in natural language.*

This epic implements the four-stage progressive enhancement search architecture inspired by [The Practical Path to AI Search](https://www.tag1.com/how-to/the-practical-path-to-ai-search/). Stage 1 (Pagefind) is independent -- a pure client-side WASM search that works without AI, without server load, without even a network connection once the index is cached. Stages 2-4 layer AI intelligence on top, each stage independently useful, each optional.

The architecture splits responsibility between the `trovato_search` plugin and the kernel. WASM plugins cannot spawn subprocesses or access the filesystem, so the plugin handles *change detection* via `tap_cron` while the kernel handles the *index build* (exporting items, running the Pagefind CLI, and deploying the index). The plugin signals the kernel through a `pagefind_index_status` database table. The existing tsvector search remains the server-side workhorse for Gather queries, API consumers, admin search, and cron jobs. Pagefind becomes the user-facing default.

---

## Tutorial Steps

### Step 1: Instant Search with Pagefind

Enable the `trovato_search` plugin. The plugin's `tap_cron` handler compares `MAX(changed)` of published live-stage items against a stored "last indexed at" timestamp. When content is newer, it sets `rebuild_requested = true` in the `pagefind_index_status` signal table. The kernel cron task picks up the signal, exports live-stage items as HTML fragments (title, body, configured search fields, metadata), runs the Pagefind CLI, and atomically deploys the index files to `./static/pagefind/`.

**What to cover:**

- Installing and configuring the `trovato_search` plugin
- How Pagefind generates a chunked WASM index (~300kB even for 50K+ pages)
- The cron-based index pipeline: `tap_cron` detects changes → signal table → kernel exports items → Pagefind CLI → atomic deploy
- Why the split architecture exists (WASM sandbox cannot spawn subprocesses or write to the filesystem)
- Integrating the Pagefind JS client into the search page
- tsvector fallback for no-JavaScript browsers (progressive enhancement)
- What goes in the index: only live-stage items with public visibility (the human-in-the-middle guarantee)

Trigger a manual index rebuild. Search for a conference. Results appear in ~50ms with zero server involvement.

### Step 2: Smarter Rankings

Configure the client-side ranking signals that make results useful without any AI. These run entirely in the browser as arithmetic on metadata Pagefind already provides.

**What to cover:**

- Recency decay: exponential decay with configurable half-life (default 1 year), age penalty after 5 years
- Exact match boost: co-located search terms rank higher than scattered matches
- Title match boost: title matches outweigh body matches
- Content type boosts: documentation pages rank higher than blog posts for technical queries
- Priority pages: admin-configured keyword-to-page mappings (searching "team" always shows the Team page first)
- The ranking formula: `final_score = base_relevance x exact_match_boost x recency_factor x priority_boost x type_boost`
- UI elements: autocomplete, content type filters, date range filtering, ARIA (Accessible Rich Internet Applications) labels

Edit the TOML configuration, rebuild, and see how results reorder.

### Step 3: Teaching Search to Understand You

Enable AI query expansion (requires AI Core from [Epic 3 — AI as a Building Block](epic-03.md), Phase A). When the user searches "performance issues," the server calls `ai_request(operation: Chat)` to generate expanded terms like "site speed optimization, latency, throughput." These terms feed back to the client, which re-queries Pagefind and merges the additional results into the display.

**What to cover:**

- The `/api/v1/search/expand` endpoint and its prompt engineering
- Expansion caching: same query produces the same expansion, configurable TTL
- Term filtering: blocking overly-generic expansions ("technology", "software")
- Client-side merging: deduplicate by Item ID, interleave expanded results
- Optional tsvector supplement for recently-created content not yet in the Pagefind index
- Rate limiting: 30 requests/minute per IP for expansion

Search for "performance issues" and watch results for "site speed optimization" appear after ~500ms, merged below the instant Pagefind results.

### Step 4: AI Summaries & Conversation

Enable AI summary streaming. The top N results with excerpts go to the server. An `ai_request(operation: Chat)` call synthesizes a concise answer that streams word-by-word above the result list via SSE (Server-Sent Events).

**What to cover:**

- The `/api/v1/search/summarize` SSE endpoint
- Prompt engineering: concise answers, cited sources with clickable links, acknowledging when results don't answer the question
- Conversational follow-up: the user can ask follow-up questions that build on previous context (reuses chatbot SSE infrastructure from Story 31.7)
- Session-scoped conversation history (configurable max turns, default 3)
- Sentiment classification: async lightweight model classifies user satisfaction, feeds analytics
- Rate limiting: 10 requests/minute for summaries, 5/minute for follow-ups
- Token budgets from Story 31.3 apply

Search for "Rust conferences in Europe" and see a synthesized answer stream above the result list: "There are 3 upcoming Rust conferences in Europe: ..."

---

## BMAD Stories

### Story 30.1: Pagefind Index Generation Pipeline

**Status:** Partially complete (plugin `tap_cron` handler and kernel `cron/pagefind.rs` builder implemented; admin UI trigger and CLI trigger not yet implemented)

**As a** site operator,
**I want** the search index to rebuild automatically when content changes,
**So that** search results are always current without manual intervention.

**Acceptance criteria:**

- `trovato_search` plugin implements `tap_cron` to detect content changes by comparing `MAX(changed)` against `pagefind_index_status.last_indexed_at`
- Plugin sets `rebuild_requested = true` in the signal table when content is newer
- Kernel cron task checks the signal table, exports live-stage published items as HTML fragments (title, body, configured search fields, metadata, URL, content type, publish date)
- Content on stages with non-public visibility is excluded from the index (human-in-the-middle guarantee)
- Pagefind CLI runs on the exported fragments, generating a chunked WASM index
- Index files deploy atomically via rename-swap to `./static/pagefind/`
- Cron-based approach naturally debounces: batch imports produce one re-index per cron interval, not N individual rebuilds
- Admin can trigger a manual full re-index via admin UI or CLI
- Index generation completes in seconds for thousands of Items

### Story 30.2: Client-Side Pagefind Search UI

**Status:** Not started

**As a** site visitor,
**I want** search results to appear instantly as I type,
**So that** I can find conferences without waiting for server responses.

**Acceptance criteria:**

- Pagefind JS library loads on search pages
- User typing triggers Pagefind search against the local WASM index (~50ms)
- Results display with title, excerpt, and link
- No HTTP request or database query for basic search
- tsvector fallback: if JavaScript is disabled or Pagefind fails, the search form submits to the existing server-side search endpoint
- Search box available in the site header on all pages
- Result count announced for screen readers with proper ARIA labels
- Empty state handled gracefully (no results message)

### Story 30.3: Client-Side Ranking Signals & Configuration

**Status:** Not started

**As a** site administrator,
**I want** to configure how search results are ranked,
**So that** the most relevant and timely content surfaces first.

**Acceptance criteria:**

- Recency decay: configurable half-life (default 365 days), penalty after configurable threshold (default 5 years), max penalty cap (default 30%)
- Exact match boost: co-located search terms rank higher (configurable boost factor, default 1.5x)
- Title match boost: title matches outweigh body matches (configurable, default 2.0x)
- Content type boost: configurable per Item type (e.g., documentation 1.3x, blog 1.0x)
- Priority pages: admin-configured keyword-to-path mappings with configurable boost (default 10.0x)
- All ranking signals configurable via TOML (`[search.ranking]` section)
- Ranking runs entirely client-side using metadata injected into the Pagefind index at build time
- Autocomplete with recent search history and title suggestions
- Content type filter and date range filter in the search UI

### Story 30.4: AI Query Expansion with Caching

**Status:** Not started

**As a** site visitor,
**I want** search to understand what I mean, not just what I type,
**So that** I can find content using natural language concepts.

**Acceptance criteria:**

- `POST /api/v1/search/expand` endpoint accepts a query, returns expanded terms as JSON
- Expansion prompt generates 4-6 related terms covering synonyms, related concepts, and alternative phrasings, specific to the site's domain
- Expansion results cached with configurable TTL (default 3600s) -- same query returns cached expansion
- Blocklist of overly-generic terms prevents result pollution ("technology", "software", "solution")
- Expanded terms sent back to the client via JSON response
- Client re-queries Pagefind with expanded terms and merges new results into the display
- Deduplication by Item ID: expanded results don't duplicate original results
- Rate limiting: 30 requests/minute per IP
- Requires `use ai` permission for authenticated users; configurable site-wide toggle for anonymous
- Falls back gracefully when AI Core is unavailable (Stage 1 results remain visible)

### Story 30.5: AI Summary Streaming via SSE

**Status:** Not started

**As a** site visitor,
**I want** a synthesized answer to my search query,
**So that** I can get a direct answer without scanning individual result pages.

**Acceptance criteria:**

- `GET /api/v1/search/summarize` SSE endpoint accepts a query and top N result excerpts
- Summary streams word-by-word above the result list using `ai_request(operation: Chat)`
- Summary is concise (configurable max tokens, default 300)
- Summary cites sources with clickable links to actual result pages
- Summary acknowledges when results don't adequately answer the question (no hallucination)
- Reuses SSE infrastructure from the chatbot Tile (Story 31.7)
- Rate limiting: 10 requests/minute per IP
- Token budgets from Story 31.3 apply

### Story 30.6: Conversational Follow-Up & Sentiment Analysis

**Status:** Not started

**As a** site visitor,
**I want** to ask follow-up questions after an AI search summary,
**So that** I can refine my understanding without starting over.

**Acceptance criteria:**

- After an AI summary, a follow-up input appears for additional questions
- Follow-up extracts keywords, runs additional Pagefind searches for newly relevant content
- New results merge with original context; AI generates a response building on previous answers
- Session-scoped conversation history with configurable max turns (default 3)
- Sentiment classification: async lightweight model classifies each interaction as satisfied/confused/unmet
- Sentiment data feeds into analytics -- high "unsatisfied" rates flag content gaps
- Sentiment runs async; users never wait for it
- Rate limiting: 5 requests/minute per IP for follow-ups
- Requires `use ai` permission

### Story 30.7: Search Plugin Configuration & Admin UI

**Status:** Not started

**As a** site administrator,
**I want** a unified configuration for all search features,
**So that** I can enable/disable stages and tune behavior without code changes.

**Acceptance criteria:**

- Full TOML configuration under `[search]`, `[search.ai]`, `[search.ranking]` sections
- Admin UI page for search configuration (engine selection, AI toggles, ranking parameters)
- Admin UI for managing priority pages (keyword-to-path mappings)
- Admin UI for content type boost configuration
- Search analytics dashboard: query volume, top queries, expansion cache hit rate, sentiment distribution
- Toggle to enable/disable each stage independently (Pagefind only, +expansion, +summary, +follow-up)
- `engine` setting: `"pagefind"` (default) or `"server"` (tsvector only, for sites without static file serving)

---

## Payoff

A search experience that starts fast and gets smarter. The reader understands:

- How Pagefind provides instant, zero-server-load search via client-side WASM
- How the split architecture works (plugin detects changes, kernel builds the index) and why
- How the index pipeline keeps search current with content changes
- How AI query expansion bridges the gap between user intent and keyword matching
- How SSE streaming provides conversational search with cited sources
- How all four stages are independently useful and incrementally adoptable

The architecture avoids the complexity of vector databases and embedding pipelines while delivering semantic search understanding. Sites that outgrow this approach have a clear upgrade path via pgvector and the `SemanticSimilarity` Gather operator (Story 31.11).

---

## What's Deferred

These are explicitly **not** in this epic (and the tutorial should say so):

- **Vector embeddings / pgvector** -- Phase 4 in the architecture. Optional upgrade for sites with millions of documents, domain-specific jargon mismatch, or recommendation systems. The `VectorStore` trait and `SemanticSimilarity` Gather operator from Story 31.11 are the upgrade path.
- **Multi-modal search** -- Image/audio search. Would require embeddings.
- **Personalized search** -- Per-user ranking based on browsing history or preferences.
- **Editor/admin Pagefind** -- Editors use the existing tsvector-based admin search which is already stage-aware. Pagefind indexes live-stage content with public visibility only.
- **Pagefind as embedded Rust library** -- Future optimization: embed the Pagefind indexer directly in the Trovato binary instead of CLI invocation. Same language, same toolchain, but not required for initial implementation.

---

## Sequencing

```
Story 30.1 (index pipeline)
  └─→ Story 30.2 (search UI) ─→ Story 30.3 (ranking)
                                      │
                                      └─→ Story 30.7 (config & admin)

Story 30.4 (query expansion)  ──→  Story 30.5 (AI summary)
                                      └─→ Story 30.6 (follow-up & sentiment)
```

Stories 30.1-30.3 + 30.7 are Phase 1 (no AI dependency). Stories 30.4-30.6 require AI Core from [Epic 3](epic-03.md) Phase A.

---

## Related

- [Ritrovo Overview](overview.md)
- [The Practical Path to AI Search](https://www.tag1.com/how-to/the-practical-path-to-ai-search/) -- Source architecture
- [Pagefind](https://pagefind.app/) -- Rust/WASM static search library
- [Trovato Search Architecture](../design/search-architecture.md) -- Full design document
- [Epic 3: AI as a Building Block](epic-03.md) -- AI Core architecture, provider registry, `ai_request()`, token budgets
- Story 31.3 -- Token budgets (apply to search AI operations)
- Story 31.7 -- Chatbot Tile with SSE streaming (shared infrastructure with search summaries)
- Story 31.11 -- VectorStore trait + pgvector (the upgrade path)
