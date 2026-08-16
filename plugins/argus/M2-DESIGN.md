# Argus Milestone 2 — Design Gate

**Written before any M2 code**, as the milestone requires: three decisions
resolved against the *actual* platform surface (the PF-5 frozen contract as
shipped), each with `file:line` evidence, each with the fallback chosen and the
gap it leaves recorded as a concrete post-1.0 item for `M2-FRICTION.md`.

Verified against `KERNEL_API_VERSION (0,99)` (post-freeze), M1 at
`plugins/argus` + `crates/argus-core` as built by `argus-m1-pure-plugin` and
amended by `p11j`. No kernel, WIT, SDK or kernel-migration change is proposed or
made by M2.

---

## Decision 1 — Embeddings: **no host embedding call exists; use deterministic lexical vectors**

### What the frozen `ai` host actually exposes

The plugin-facing AI surface is exactly one function, `ai-request`
(`crates/wit/kernel.wit:86`). `AiRequest` carries an `operation` field whose
values include `Embedding`, and `AiRequest.input` exists for it
(`crates/plugin-sdk/src/types.rs`). That is where the resemblance to an
embedding API ends. Following the call through the host:

1. `execute_ai_request` branches on the **protocol only**, never on the
   operation (`crates/kernel/src/host/ai.rs:170-179`).
2. `build_openai_request` unconditionally posts to
   `{base_url}/chat/completions` with a `messages` array, and **never reads
   `request.input`** (`crates/kernel/src/host/ai.rs:240-288`). The Anthropic
   builder is the same shape (`:291-354`).
3. `parse_openai_response` unconditionally reads
   `choices[0].message.content` (`crates/kernel/src/host/ai.rs:361-391`).

So `operation: Embedding` from a plugin changes three things — which permission
is required (`crates/kernel/src/host/ai.rs:101-106`), which configured provider
is resolved (`:537-551`), and what string is written to `ai_usage_log` — and
changes **nothing** about the wire request. A plugin asking for an embedding
gets a chat completion posted to `/chat/completions` with an empty `messages`
array, and the vector it wanted was never requested.

The kernel *can* embed. `AiProviderService::embed` is a real
`POST {base_url}/embeddings` client with input capping and SSRF re-validation
(`crates/kernel/src/services/ai_provider.rs:782-870`). It is `pub` on a kernel
service, reachable from kernel code only; nothing bridges it to `ai-request`.
This is the same shape as the accepted `plugin-mail-host-interface` gap: **the
kernel embeds fine; plugins in Trovato 1.0 do not.**

M1's `HostProvider::embed` (`plugins/argus/src/host_ports.rs:131-155`) is the
pre-existing evidence — it was written against the assumption that
`operation: Embedding` routes to an embeddings endpoint, and it was never
exercised because M1 stubbed the embed stage. M2 is the first caller, and the
assumption does not hold.

### pgvector is not available either

Independently of the vector *source*, a plugin migration cannot rely on a
`vector` column:

- The extension is optional. The kernel's own embedding migration wraps
  `CREATE EXTENSION` in an exception-swallowing `DO` block and creates
  `item_embeddings` only `IF EXISTS (SELECT 1 FROM pg_type WHERE typname =
  'vector')` (`crates/kernel/migrations/20260402000001_create_item_embeddings.sql:7-22`),
  and `PgVectorStore` degrades at runtime on the same probe
  (`crates/kernel/src/services/vector_store.rs:120-127`).
- It is absent in the shipped dev/test environment. `docker-compose.yml:3`
  pins stock `postgres:17`; the probe returns `f` on a running container here.

A plugin that declared `vector(N)` would fail to install on a stock deployment,
or would need the kernel's two-branch conditional DDL and two query paths.
Buying that complexity for a vector we cannot obtain is not worth it.

### Decision

**The embed stage makes no AI call.** It computes a deterministic lexical
feature vector in `argus-core` — a signed hashing-trick projection over title +
summary + extracted entity names, sublinear term weighting, L2-normalized,
dimension from config (default 256) — and stores it as a JSON float array in a
plugin-owned `argus_article_vectors` table. Entity names enter the vector at
the heaviest weight, so cosine over these vectors already carries entity
overlap; clustering therefore keeps **one** similarity term rather than blending
a separate Jaccard, and the precision guard lives in SQL instead: a story is
only a clustering candidate if it shares an entity with the incoming article or
sits in the same topic inside the publication window.

This is the middle option the milestone offers ("entity/topic-overlap heuristic
clustering"), taken in its strongest expressible form. Three properties make it
the right fallback rather than a consolation prize:

- **The clustering math is unchanged.** Similarity is a cosine over a
  normalized vector either way, so threshold / time-decay / topic-coherence /
  near-duplicate logic is written once, against synthetic vectors, and survives
  a later swap of the vector source intact. If a `vector` host interface lands
  post-1.0, only `embed.rs`'s vector *production* changes.
- **It costs nothing and cannot fail transiently.** No provider, no tokens, no
  retry class, no budget draw. The embed stage becomes pure CPU over data the
  analyze stage already produced.
- **It is testable end to end on a real kernel.** Embed and cluster need no AI
  provider, so unlike analyze and summarize they can be driven through the real
  wasm plugin and the real db host in an integration test.

Honest statement of what is lost: lexical vectors match on shared vocabulary,
not on meaning. Two articles reporting the same event in different words
(different outlet register, translation, heavy paraphrase) cluster only if they
share entities or distinctive terms. The entity-overlap term carries most of
that weight, which is why entity extraction is upstream of embedding rather than
beside it. Expect recall below what a semantic embedding would give, and a
similarity threshold that is not comparable to the 0.82 the milestone suggests
for a vector route — the default is calibrated separately and recorded in
`M2-FRICTION.md` alongside observed clustering quality.

### Gap recorded

**G-AI-EMBED-UNROUTED** — `ai-request` accepts `operation: Embedding` and
silently serves it as a chat completion. Post-1.0, additive, two candidate
fixes: branch `execute_ai_request` on the operation and reuse the existing
`/embeddings` client, or reject `Embedding` at the host boundary with a distinct
error code so the failure is loud instead of silent. Rejecting is strictly
better than today even if routing never lands: the current behaviour is the bad
kind of quiet.

---

## Decision 2 — Entity storage: **manifest-declared plugin tables, as specified**

`argus_entities` and `argus_article_entities` are created by the M2 migration
and therefore plugin-owned and in the `db` allowlist — the allowlist is derived
by scanning `CREATE TABLE` statements out of the declared migration files
(`crates/kernel/src/plugin/db_policy.rs:68-87`), which is exactly the M1 table
pattern.

The unique constraints the design needs are plain SQL in a plugin migration and
need no platform support: `UNIQUE (canonical_name, entity_type)` on entities and
a composite `PRIMARY KEY (article_id, entity_id)` on the join table. Verified
against the same surface M1 used for `uniq_argus_articles_url`
(`plugins/argus/migrations/001_argus_schema.sql:44`), which the M1 idempotent
upsert already targets in production. Both tables are additionally declared as
read-only `[[record_types]]` so their rows are visible in the record admin,
matching the M1 treatment of feeds and topics.

Entities stay out of the Item tier for the reason the milestone states and M1
already established for articles: high write churn, no per-entity page needed
yet, and Item writes carry the full Item tax. Fuzzy alias resolution
(Jaro-Winkler plus normalized Levenshtein, tunable threshold) is pure logic in
`argus-core`, hand-implemented rather than pulled from a crate to avoid widening
the dependency-audit surface for forty lines of string distance.

### Second-order finding, surfaced by this decision

Stories *are* Items, and M2 is the first code anywhere that creates one from a
plugin. Two things turned up that M1 could not have hit, because M1 never
created a story:

- **The SDK ships no `item-api` binding.** `crates/plugin-sdk/src/host.rs`
  declares externs for db, ai, http, logging, queue, crypto, variables, user,
  and plugin invocation, and none for items. The kernel registers the interface
  (`crates/kernel/src/host/mod.rs:62` → `crates/kernel/src/host/item.rs:19-197`)
  and `item-api` is a valid manifest name
  (`crates/kernel/src/plugin/info_parser.rs:356-369`), so the host functions are
  there and reachable — but a plugin that wants them must hand-declare
  `#[link(wasm_import_module = "trovato:kernel/item-api")]` itself. Argus does,
  in `host_ports.rs`, with the SDK's own calling convention. This is friction,
  not a blocker, and it is plugin-side only.
- **Plugin-created Items are never enqueued for embedding.** `save-item` calls
  `Item::create` / `Item::update` directly, deliberately, to avoid re-entrant
  tap dispatch (`crates/kernel/src/host/item.rs:1-6`). Auto-embed and
  `tap_item_update_index` both hang off `ItemService::index_item`
  (`crates/kernel/src/content/item_service.rs:603-656`), which that path
  bypasses. So an `argus_story` Item written by the plugin is full-text
  findable (the `search_vector` trigger is in the save transaction) but has no
  embedding and is invisible to `SemanticSimilarity` gathers until an operator
  runs the admin backfill (`crates/kernel/src/routes/admin_embed.rs:88` →
  `embed_index::enqueue_backfill`).

Consequence for M2, stated plainly rather than worked around: `argus_story`
stays opted in to embedding in intent, the code sets no embedding state itself,
and the documented operator step is the admin backfill. Both items go to
`M2-FRICTION.md`; neither is fixed here, because both fixes are kernel changes.

---

## Decision 3 — Cost accounting: **`response.cost_estimate`, per the p11j companion**

`AiResponse.cost_estimate` is populated by the host from the same `estimate_cost`
figure it writes to `ai_usage_log` (`crates/kernel/src/host/ai.rs:749-794`), and
M1 already threads it `ChatResponse` → `decide` → `DecideReport`
(`crates/argus-core/src/ports.rs:57-61`). M2 extends the same thread to analyze
and summarize and accumulates per UTC day and per stage in a plugin-owned
`argus_cost_daily` table. The struck M1 deviation #4 (cost read via smoke SQL)
stays struck; no kernel table is read.

`None` remains the honest "unpriced or unknown" and is distinct from
`Some(0.0)`. A `None` is counted as a call with unknown cost, never as a free
call: the daily row tracks calls and unpriced-calls separately so an operator
reading a low spend figure can see whether it means "cheap" or "not priced".

---

## What M2 builds on these decisions

| Stage | AI call | Cost | Notes |
|---|---|---|---|
| analyze | 1 per article | yes | structured JSON; entities extracted from the same response |
| extract | none | none | pure normalization + upsert, inside the analyze job |
| embed | none | none | lexical vector (Decision 1) |
| cluster | none | none | cosine + entity overlap, decay, coherence |
| summarize | 1 per changed story | yes | rate-limited, one per story per window |

Two AI calls per article-to-story path, both on the background principal, both
priced from the response. The budget gate pauses exactly the two stages that
spend, and leaves fetch/decide/embed/cluster running, as the milestone requires.
