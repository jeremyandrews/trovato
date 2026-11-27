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

- **Change detection (kernel):** Every write that changes what an item's search page holds queues that item in `search_index_queue`, with a reason. Writes to `item`, `comment` and `url_alias` rows are queued by triggers on those tables, in the write's own transaction, whoever makes them (the services, pathauto, a plugin's raw SQL). The other writers are the content type search switches and field weights (every item of the type), workspace publishing, and plugins changing something no trigger sees, such as data their `tap_field_access` reads, which call `host::search_index_enqueue` ([the rule](../plugin-development.md#the-static-search-index-queue-what-you-change)). Scheduled publishing writes through the item API. Nothing polls. The `pagefind_index_status` table remains the switch that says the site wants the index, and carries an operator's full rebuild request (`trovato search rebuild`).
- **Index generation (kernel):** Each cron run drains the queue in batches of 200, rewriting only the queued items' fragments, the word chunks their words are in, the meta file and `pagefind-entry.json` (`cron/search_update.rs`; see [incremental-search-index.md](incremental-search-index.md#part-3-as-built)). A full build, which exports live-stage published items as HTML fragments (title, every text field, published comments, metadata; see [What Goes in the Index](#what-goes-in-the-index)), builds the Pagefind 1.3.0 index natively (crate `trovato-search-index`; no `pagefind` binary), and publishes it into `PAGEFIND_OUTPUT_DIR`, entry file last (`./static/pagefind/` by default, served at `/static/pagefind/`), runs only when there is no index yet, when an operator asks, when tombstones pass the compaction threshold, or when a precondition of the update fails (logged at `warn` with the reason).
- **Why the kernel:** WASM plugins run in sandboxed instances with no access to the filesystem. The kernel writes the index files; the `trovato_search` plugin switches the index on and reports the queue length from `tap_cron`.
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

## Search Settings (as built)

The ranking signals, the AI stage switches and the analytics switches are one
typed model, `SearchSettings` (`crates/kernel/src/search/settings.rs`), stored as
one JSON value under the `site_config` key `search` and edited at
`/admin/config/search` (permission `administer site`). Config export carries it
inside the `variable` entity like any other `site_config` row. Every reader goes
through `SearchSettings::load`, which reads the kernel's two tier cache (key
`search_settings`) and falls back to the database: a search request costs no
database round trip for its settings. Saving through the screen drops the cached
copy; a value written another way (config import, a hand edit) takes effect when
the cached copy expires, within five minutes. A missing key is the defaults; a
value that does not parse or is out of range is the defaults with a warning in
the log.

The defaults are the values `templates/search.html` carried as a literal JSON
block before the model existed, so a site that never opens the screen ranks
exactly as it did. The search page now renders that block from the model, with
Tera's `json_encode` filter.

| Setting | Default | Range |
|---|---|---|
| `recency_boost_max` | 0.5 | 0 to 10 |
| `recency_half_life_days` | 365 | 1 to 36500 |
| `recency_penalty_after_days` | 1825 | 1 to 36500 |
| `recency_max_penalty` | 0.3 | 0 to 1 |
| `title_match_boost` | 1.0 | 0 to 100 |
| `title_all_terms_multiplier` | 1.5 | 0 to 100 |
| `content_match_boost` | 0.4 | 0 to 100 |
| `excerpt_length` | 300 | 50 to 2000 |
| `results_per_page` | 10 | 1 to 100 |
| `max_pagefind_results` | 50 | 1 to 500 |
| `ai_summary_top_n` | 5 | 1 to 20 |
| `ai_summary_max_chars` | 2000 | 100 to 20000 |
| `type_boosts` | none (every type 1.0) | each 0 to 100, keyed by item type machine name |
| `priority_pages` | none | at most 50 keyword and path pairs; keyword 1 to 100 characters, path starting with `/` |
| `priority_boost` | 10.0 | 1 to 100 |
| `expand`, `summarize`, `followup` | on | |
| `max_followups` | 3 | 0 to 20 |
| `analytics_enabled` | on | |
| `analytics_retention_days` | 90 | 1 to 365 |
| `sentiment_enabled` | off | |
| `index_compaction_percent` | 5.0 | 0.5 to 50: the share of a language's Pagefind page slots that may be tombstones (left by deleted or unpublished items and not yet reused) before cron compacts the index with one full build that renumbers densely; see [incremental-search-index.md](incremental-search-index.md#compaction) |

### Which backend applies which signal

| Signal | Browser (Pagefind, `scolta.js` `scoreResults`) | Server (`SearchService::search`, tsvector) |
|---|---|---|
| Field weights (`search_field_config`, per type) | yes, as `data-pagefind-weight` in the index | yes, as `setweight` in the trigger |
| Title as weight A | yes | yes |
| Type boost | yes, multiplies the score by `TYPE_BOOSTS[meta.type]` | yes, multiplies `ts_rank`; the boosts are bound as two arrays and joined with `unnest`, in the same query |
| Priority pages | yes, multiplies by `priority_boost` when the normalized query holds the keyword as a whole word and the result URL is the path | no |
| Recency boost and age penalty | yes | no |
| Title and excerpt match boosts | yes | no |

Recency, the match boosts and priority pages stay browser signals by design: the
brief puts them in the browser, where Pagefind's results already carry the date,
title and excerpt they need, and server search is the fallback and the API, not
the ranked experience. Type boosts are applied in both because a type boost is a
statement about the content, not about the page it is shown on.

### Field weights in the Pagefind index

Each text field is its own element inside `data-pagefind-body`, with a
`data-pagefind-weight` from its letter, at the ratios of PostgreSQL's default
`ts_rank` weights scaled so A is Pagefind's maximum:

| Letter | `ts_rank` weight | `data-pagefind-weight` |
|---|---|---|
| A | 1.0 | 10.0 |
| B | 0.4 | 4.0 |
| C | 0.2 | 2.0 |
| D | 0.1 | 1.0 |

The title is A, as in the trigger, and sits inside the indexed region with
`data-pagefind-meta="title"`, so a title alone is findable. Fields are ordered by
weight, then by name. A text field with no configured weight is indexed at D. A changed weight reaches the browser at the
next index build.

### AI stages are enforced on the server

`/api/v1/search/expand`, `/summarize` and `/followup` read the settings first
and answer 404 with a short JSON error when their stage is off, before any cache
or provider work. `/followup` answers 400 to a conversation holding more `user`
turns than `max_followups` plus one (the opening question), and to a message
whose role is anything but `user` or `assistant`.

Both answer plain JSON, not an SSE stream: an earlier draft of this document
described them as streaming, which they have never done in the kernel's
implementation.

`/summarize` and `/followup` do not take the browser's text at all. Each
takes result **URLs**, `{ "results": [{ "url": "..." }] }`, never excerpt
text or page content; `/summarize` also answers 200 with an empty summary,
spending no provider call, when none of its URLs resolve to an item the
caller may see. The server resolves each URL to an item on the Live stage
(`/item/<uuid>` directly, or through the URL alias table), runs it through
the same item- and field-level access check every other read path uses
(`ItemService::filter_page_for_view`), and builds the prompt from the title
and the fields of whatever survives, each item cut to `ai_summary_max_chars`.
A URL naming an unpublished item, or a field the caller may not see, puts
nothing into the prompt. This closed two defects in the original design: a
caller could send any text it liked as "excerpts" for the model to read, and
the summary could describe content the caller had no right to see in the
first place.

---

## Search Analytics (as built)

What visitors search for, counted per UTC day, without the kernel storing
anything that identifies a person. On by default; switched off, and its
retention set, on `/admin/config/search`. Read at `/admin/reports/search`.

**What is stored, and only this.** Two tables:

| Table | Key | Counters |
|---|---|---|
| `search_query_daily` | UTC `day`, normalized `query` | `searches`, `zero_results` |
| `search_daily` | UTC `day` | `expansion_hits`, `expansion_misses`, `satisfied`, `confused`, `unmet` |

The query is stored as `search::analytics::normalize_for_analytics` leaves it:
trimmed, whitespace collapsed, lowercased (the same `search::normalize_query`
the expansion cache keys on), then every email shaped token replaced by
`[email]` and every number of six or more digits by `[number]`, then cut to 100
characters. A number is a token made only of digits and the punctuation numbers
are written with (`+ - . , ( ) / #`): `12345678`, `+44(0)20-7946-0958`. Digits
inside a word are left alone, so a product code such as `inv12345678` is stored
as typed; five digits or fewer (a postcode, a year) are kept too. A query that
normalizes to nothing is not recorded.

**What is never stored**, in a table or in a log line this work adds: an IP
address, a user id, a session id, a user agent, a referrer, or any time finer
than the UTC day. The one debug line that logged a raw query on an expansion
cache hit was removed.

**Where searches are counted.** Server: `SearchService::search`, the path
`/search`, `/api/search` and `/api/v1/search` all take, once per search on its
first page, with the result total. `SearchService::search_unrecorded` is the same
search uncounted, for callers that are not a visitor searching (the AI chat's
retrieval, the MCP tool, tests). Browser: `scolta.js` sends
`POST /api/search/log` with `{query, results}` through `navigator.sendBeacon`,
once per search the visitor runs, after the primary results render; not for a
filter toggle, not for the search the page runs from its own `?q=` (the server
counted that one when it rendered the page), and only when the rendered config
says `analytics: true`. The endpoint checks the query is at most 500 bytes and
`results` is 0 to 100,000, answers 204, and touches no database. The rate
limiter files it under the `search` bucket. The expansion endpoint counts a
cache hit or miss for every expansion it is asked for.

**Recording never slows a search.** `SearchAnalytics::record` and
`SearchAnalytics::count` take a short synchronous lock on an in memory map and
return. A background task flushes every 30 seconds, and once more on shutdown,
with one `INSERT ... SELECT FROM unnest(...) ON CONFLICT ... DO UPDATE SET n =
n + EXCLUDED.n` per table, so several kernel processes add up correctly. At most
10,000 distinct (day, query) keys wait between flushes; past that, new queries
are dropped and the count dropped is logged at the next flush. A flush that fails
is logged and its batch dropped rather than kept. A crash loses at most one flush
window, 30 seconds of counts.

**Retention.** Cron deletes rows older than `analytics_retention_days` (default
90, 1 to 365) from both tables, one `DELETE` each, at most once a day across all
processes (a Redis claim, `cron:search_analytics_prune`).

**The report.** A 7, 30 or 90 day period (never longer than retention), the
searches and zero result searches with their share, the top 25 queries, the top
25 queries that found nothing, the expansion cache hit rate, and the follow-up
sentiment counts (zero until sentiment classification exists and is switched on). Each is one aggregate query over the period (`GROUP BY`,
`ORDER BY`, `LIMIT`), run concurrently.


### Follow-up sentiment (as built)

The Ritrovo brief (30.6) asks that each follow-up be classified as satisfied,
confused or unmet, asynchronously, so a high unmet rate can flag a content gap,
without the visitor waiting. The follow-up endpoint and its client already
existed; what the kernel lacked was the server side turn cap (Part 1), a
classifier, and somewhere to count its labels (Part 2's `search_daily`). This is
the small piece.

When `sentiment_enabled` and `analytics_enabled` are both on (sentiment is off by
default), `/api/v1/search/followup`, after it has built a successful answer,
`try_send`s the visitor's newest message and the assistant answer before it to
a bounded channel (256 jobs) and returns. The search results a follow-up
searches up travel as a separate `results` field, not text appended to the
message, so there is nothing to strip out of it here any more. One background worker, owned by `AppState`
(`search::sentiment::SentimentQueue`), drains it: it re-reads the settings,
resolves the Chat provider the same way the endpoints do, sends the
`CLASSIFY_SENTIMENT` prompt (`search/prompts.rs`, resolved through
`prompts::resolve`) through `api_search::complete`, which sends it through
`AiProviderService::chat_once`, the kernel's one send path, and adds the label to the day's counter
through the analytics aggregator. A reply that is not exactly one of the three
words counts nothing and logs one warning, without the reply text. A full channel
drops the job; the number dropped is logged at the next analytics flush. Provider
failures are logged through `log_provider_failure`, like the endpoints'. Like
the three search endpoints, the classifier records no token usage against the
AI budgets.

The text goes to the provider and nowhere else: no table, no log line. It costs
tokens on every follow-up, which is why the settings screen says so beside the
switch. There is no separate "lightweight model" setting: the classifier uses the
site's Chat provider and model, the same resolution the endpoints use.

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

- **Plugin (WASM):** Its `pagefind_index_status` table switches the index on and carries an operator's full rebuild request. Its `tap_cron` reports how many items are queued. Cannot access the filesystem due to WASM sandbox constraints.
- **Kernel (`cron/pagefind.rs`, `cron/search_update.rs`):** Each cron run drains `search_index_queue` through the incremental updater, or takes a full build through the page ledger (`cron/search_index.rs`) when one is requested or needed, and publishes entry file last. Only runs when the `trovato_search` plugin is enabled (`pagefind_enabled` flag on `CronService`).

### `trovato_search` Plugin

| Component | Implementation | Notes |
|---|---|---|
| Change detection | Table triggers and kernel writers | Item, comment and alias writes are queued by triggers in their own transaction; the type screens and workspace publishing queue their items; plugins call `host::search_index_enqueue` for changes no trigger sees |
| Index generation | Kernel `cron/pagefind.rs` | Drains the queue through `cron/search_update.rs` (only the queued items' files), or a full build in `cron/search_index.rs`; publishes entry file last |
| Index serving | Static files in `./static/pagefind/` | Served by web server/CDN, not Trovato app code |
| Pagefind client JS | Frontend asset bundled with plugin | Loaded on search pages, optional preload on all pages |
| Query expansion endpoint | `tap_menu` -> `/api/v1/search/expand` | Calls `ai_request(operation: Chat)`, returns expanded terms as JSON |
| Expansion cache | KV store or in-memory cache | Same query -> same expansion. Configurable TTL. |
| Re-ranking config | `SearchSettings` in `site_config` (see "Search Settings (as built)") | Admin UI at `/admin/config/search` |
| AI summary endpoint | `tap_menu` -> `/api/v1/search/summarize` | Plain JSON response via `ai_request(operation: Chat)`; the server resolves the given result URLs to items itself rather than reading browser-supplied text |
| Conversational follow-up | Client-held conversation on the follow-up endpoint | Plain JSON response, not SSE; the endpoint validates every message's role and caps the conversation's length server-side |
| Sentiment classification | Async `ai_request()` post-response | Fast model, fire-and-forget to analytics |
| tsvector fallback | Existing Gather search | Activated when Pagefind unavailable or for server-side consumers |

### Configuration

The design sketch below predates the build. What exists is the settings model in
"Search Settings (as built)" above; there is no `engine` setting.

```toml
[search]
engine = "pagefind"              # "pagefind" (default) or "server" (tsvector only)
index_path = "pagefind"          # relative to static directory
rebuild_on_save = true           # or false for cron-only rebuild

[search.ai]
enabled = true                   # enables Stages 2-4
expansion_cache_ttl = 3600       # seconds; 0 = no cache
                                 # Implemented as the CACHE_TTL_SEARCH_EXPAND
                                 # environment variable, default 30 days: an
                                 # expansion costs provider tokens and does not
                                 # go stale the way content does. 0 still
                                 # disables the cache.
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
A write (item, comment or alias row, by any writer: a trigger, in the same
transaction; type switches or weights; workspace publish; a plugin's
search_index_enqueue)
    |
    +-> INSERT INTO search_index_queue (item_id, op, reason)
          one row per item, the last write wins

Cron interval fires
    |
    +-> Kernel cron/pagefind.rs (only when trovato_search is enabled):
          A full rebuild requested, no index yet, or no entry file?
              -> full build through the page ledger (cron/search_index.rs)
          Otherwise, per batch of 200 queued items (cron/search_update.rs):
              |
              +-> Export just those items (the same access seam: Live,
              |     published, viewable by anonymous)
              |
              +-> Under pg_advisory_xact_lock('search_index'): render,
              |     tokenize (term cache by fragment hash), free or tombstone
              |     their page slots, rewrite the word chunks their old and
              |     new words are in (split past 40,000 locations, drop when
              |     empty), a new meta file
              |
              +-> Publish: new hashed files, then pagefind-entry.json
              |     (files no longer referenced deleted after 24 hours)
              |
              +-> Record ledger, terms, chunk plan, generation; delete the
                    queue rows applied; commit
          Tombstones above the compaction setting (5% of a language's
          slots by default) -> one full build that renumbers densely
```

An edit or a comment is searchable at the next cron run, and only the files it
touched are rewritten (see
[incremental-search-index.md](incremental-search-index.md#part-3-as-built) for
the measurements). A batch import of 50 items is one update of 50 pages at the
next cron run. Past 1,000 queued items and a quarter of the index, a full build
is cheaper and is taken instead.

### What Goes in the Index

**By default, everything an anonymous visitor can see is searchable**, in both
backends (the Pagefind index and the Postgres vector behind `/search`):

- every published item on the **live stage** that the anonymous role may view;
- its **title** (weight A);
- **every text field** its type declares (`Text` and `TextLong`, value and
  summary), each at its `search_field_config` weight, D when it has none. A
  weight says how much a field counts; it no longer decides whether the field is
  searched. Files, references, numbers, dates, booleans and structured fields are
  not text and are not indexed;
- **every published comment** on it that the item page shows (published, under
  a published parent), at Pagefind's default weight and D in Postgres. A match in
  a comment is a result for the item and links to the item.

Every value is indexed as plain text (`page_meta::plain_text`: tags stripped,
entities decoded), never as markup.

**Only content an anonymous visitor may see goes in the Pagefind index.** The
index is a set of static files any visitor downloads, so it is built as the
anonymous role, through the same item and field access seam a page view uses: an
unpublished item, an item on an internal stage, a field `tap_field_access`
denies to anonymous visitors, and an unpublished or held comment are never in
it. Server side search applies the searching user's own access instead.

#### The two switches

Each content type has two checkboxes at `/admin/structure/types/{type}/search`
(permission `administer site`), both on by default, so a new install and every
existing site search everything:

- **Include this content type in search.** Off, no item of the type is found by
  either backend.
- **Include comments in search.** Off, the type's items stay searchable by
  their title and fields, but no longer by words found only in their comments.

They are stored in the type's `item_type.settings` under `search`
(`{"include": …, "comments": …}`, only an explicit `false` turns one off), so
`config export` and `config import` carry them with the type. Saving recomputes
the type's Postgres vectors at once and queues every item of the type for the
Pagefind index, which the next cron run updates.

#### When the index is updated

Every writer queues the items it changed (B218). Triggers on `item`, `comment`
and `url_alias` queue the item a write changes, in the write's own
transaction, whoever makes it. The type search switches and the field weights
queue every item of the type, workspace publishing every item the publish
moves or deletes, and plugins call `host::search_index_enqueue` for a change no
trigger sees. Scheduled publishing changes the status through the item API. The
next cron run applies the queue, so a new comment is findable one cron interval
after it is posted. A queued item that is not searchable (a draft, an item
anonymous may not see) is taken out of the index or left out.
The Postgres vector needs no rebuild: triggers on `item` and `comment` keep it
current inside the write's own transaction.

Pagefind supports custom metadata per page, which is how we inject ranking signals into the client-side index.

For editor search (items on internal-visibility stages + live stage), the tsvector fallback handles it server-side with stage-scoped queries -- same as the existing stage-aware kernel search. **The Pagefind index is Live only:** it is built from published Live items and served as static files to everyone, so it never contains a stage's content. Server-side search reads a stage only for a holder of `use stages` who switched to it (`routes::helpers::read_stages`; see `docs/design/stage-workflow.md` §8); everyone else, and every Pagefind query, reads Live.

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

### AI Core Integration

The [AI Core](ai-integration.md) provides what Stages 2-4 depend on:

- Provider registry and the `ai_request()` host function, required for query expansion and summary generation
- Token budgets, which apply to all search AI operations
- The chatbot tile with SSE streaming, which shares infrastructure with search summaries
- The `VectorStore` trait plus pgvector, the upgrade path beyond query expansion

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
- [AI Integration Design](ai-integration.md) -- Full AI design document
- [Incremental search index](incremental-search-index.md) -- Design for updating the Pagefind index per item instead of rebuilding it (B216)
