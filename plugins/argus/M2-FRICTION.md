# Argus Milestone 2 — Friction Log

> **Status note (added 2026-08-13).** Two findings here are **closed** by the
> post-freeze kernel-usability batch that shipped in 0.99.0:
> **G-AI-EMBED-UNROUTED** (an embedding request now reaches an embeddings
> endpoint; Argus gains an opt-in semantic route via `argus.embed_model`) and
> **G-ITEM-NO-EMBED** (a plugin-saved Item is now queued for kernel embedding).
> The rest are carried on the road to 1.0; see `KNOWN-ISSUES.md`. This log is
> the record of what the build met at the time.

Produced by building the Argus intelligence stages (analyze, entity extract,
embed, cluster, summarize, budget, retention) as a pure WASM plugin against the
**frozen** PF-5 contract. Every item is severity-tagged with `file:line`
evidence and phrased as a concrete, decidable kernel issue for the post-1.0
ledger. **NEW** findings were surfaced by this build; **RESIDUAL** ones are
re-confirmed from the consumer side. No-friction findings are listed last, as
required.

Verified at `KERNEL_API_VERSION (0,99)` with **no kernel, WIT, SDK or
kernel-migration change** in the build session. The design decisions these
findings forced are argued in `M2-DESIGN.md`.

Two findings are load-bearing for the milestone rather than cosmetic:
**G-AI-EMBED-UNROUTED** is why M2 ships lexical vectors instead of semantic
embeddings, and **G-QUEUE-CONCURRENCY-COLLAPSED** produced a real duplicate-story
bug that had to be worked around inside the plugin.

---

## Findings

### G-AI-EMBED-UNROUTED — **[High, NEW]** `ai-request` serves an embedding request as a chat completion

`AiRequest` has an `operation` field whose values include `Embedding`, and an
`input` field for it. Neither reaches the wire:

- `execute_ai_request` branches on the **protocol only**, never the operation
  (`crates/kernel/src/host/ai.rs:170-179`).
- `build_openai_request` unconditionally posts to `{base_url}/chat/completions`
  with a `messages` array and **never reads `request.input`**
  (`crates/kernel/src/host/ai.rs:240-288`); the Anthropic builder is the same
  shape (`:291-354`).
- `parse_openai_response` unconditionally reads `choices[0].message.content`
  (`crates/kernel/src/host/ai.rs:361-391`).

So `operation: Embedding` changes which permission is required
(`:101-106`), which configured provider resolves (`:537-551`), and what string
lands in `ai_usage_log` — and changes nothing about the request. A plugin asking
for a vector gets a chat completion posted with an empty `messages` array.

The kernel *can* embed: `AiProviderService::embed` is a real
`POST {base_url}/embeddings` client with input capping and SSRF re-validation
(`crates/kernel/src/services/ai_provider.rs:782-870`), reachable from kernel code
only. This is the same shape as the accepted `plugin-mail-host-interface` gap:
**the kernel embeds; plugins in Trovato 1.0 do not.**

**Impact:** M2's embed stage cannot produce a semantic vector, so story
clustering runs on deterministic lexical vectors computed in `argus-core`. The
clustering math is unchanged by the substitution (cosine over a normalized
vector either way), but recall is lower: two reports of one event in different
vocabulary cluster only if they share entities or distinctive terms. See the
observed numbers under "Clustering quality" below.

**Worth stating plainly:** the failure is *silent*. A plugin author gets a
plausible-looking `AiResponse` back and only discovers the problem when the
"vector" fails to parse as a float array. M1 shipped a `HostProvider::embed`
written against the assumption that this worked
(`plugins/argus/src/host_ports.rs:131-155`); M2 is the first caller, and the
assumption does not hold.

**Recommendation (post-1.0, additive):** branch `execute_ai_request` on the
operation and reuse the existing `/embeddings` client. If routing is not wanted,
**reject `Embedding` at the host boundary with a distinct error code** — that is
strictly better than today even on its own, because the current behaviour is the
bad kind of quiet.

### G-QUEUE-CONCURRENCY-COLLAPSED — **[High, NEW]** per-queue concurrency is collapsed to one per-plugin maximum

`tap_queue_info` lets a plugin declare a concurrency per queue. The drain reads
the **maximum across every queue** and applies that one number to all of the
plugin's jobs (`crates/kernel/src/cron/mod.rs`, `plugin_concurrency` at
`:1077-1099`, over `parse_max_concurrency` at `:176-192`). Argus declares
`fetch: 4, decide: 4, analyze: 2, embed: 2, cluster: 1, summarize: 1`
(`plugins/argus/src/lib.rs`, `tap_queue_info`) and every stage runs 4-wide.

**Impact, concrete:** clustering is the one stage that must not run concurrently
with itself. Two workers scoring the same event at the same moment each see no
candidate story and each create one, permanently splitting a story that should
be single — nothing downstream merges them. This was not theoretical: the first
integration run of `embed_and_cluster_build_a_story_item` produced **two stories
for two reports of one event**, and did so reproducibly.

**Argus response (a workaround, not a fix):** a plugin-owned lease in
`argus_state`, taken and released around the clustering body
(`crates/argus-core/src/pipeline.rs`, `run_cluster`;
`plugins/argus/src/intelligence_ports.rs`, `try_acquire_cluster_lease`). It is
one `INSERT ... ON CONFLICT DO UPDATE ... WHERE`, so the check and the take are
atomic without a transaction; it carries an expiry, so a worker killed by the
epoch deadline cannot wedge the stage; and a worker that loses the lease
re-enqueues itself with a two-second delay rather than proceeding. Covered by
three unit tests, including expiry recovery.

That is roughly twenty lines of lock in a plugin to express something the
manifest already has syntax for. Every plugin with one serial stage and one
parallel stage will write it again.

**Recommendation (post-1.0):** honour the declared per-queue concurrency, or
document plainly that the value is per-plugin and remove it from the per-queue
declaration so it cannot be read as a promise.

### G-ITEM-NO-MERGE — **[Medium, NEW]** `save-item` replaces the whole `fields` object

`Item::update` does `let fields = input.fields.unwrap_or(current.fields)`
(`crates/kernel/src/models/item.rs:295`), so a `save-item` that supplies `fields`
replaces the entire object. There is no partial update and no field-level patch.

**Impact:** any plugin that owns an Item and updates it from more than one place
must write the complete field set every time. Argus mutates a story Item from
three paths (join, summarize, retire), and the first implementation wrote only
what each path knew — which silently erased the synthesized summary the next
time an article joined. The bug was caught by the end-to-end test, not by
review; the field simply reverted to its placeholder.

**Argus response:** the story's narrative (`title`, `summary`, `sources`) is
duplicated into `argus_stories` and the Item is rewritten as a **complete
projection** of that row on every mutation
(`plugins/argus/src/intelligence_ports.rs`, `sync_story_item`). Three columns of
duplication to avoid a read-modify-write that could not be made atomic anyway
(see G-DB-NO-TX).

**Recommendation (post-1.0, additive):** merge supplied field keys into the
existing object instead of replacing, or add an explicit `fields_merge` flag.
Replacement is a defensible default only if it is documented at the host
boundary; today it is documented nowhere a plugin author would look.

### G-ITEM-NO-EMBED — **[Medium, NEW]** a plugin-created Item is never indexed for semantic search

The item host calls `Item::create` / `Item::update` directly, deliberately, to
avoid re-entrant tap dispatch (`crates/kernel/src/host/item.rs:1-6`). Auto-embed
and `tap_item_update_index` both hang off `ItemService::index_item`
(`crates/kernel/src/content/item_service.rs:603-656`), which that path bypasses.

**Impact:** an `argus_story` Item written by the plugin is full-text findable
(the `search_vector` trigger is inside the save transaction) but has **no
embedding**, so it is invisible to `SemanticSimilarity` gathers. Stories are
Items specifically *because* §9.4 wanted them semantically searchable
(`plugins/argus/src/lib.rs`, `tap_item_info`), so this negates the reason for the
Item tier in this plugin's case. The only recovery is an operator running the
admin backfill (`crates/kernel/src/routes/admin_embed.rs:88` →
`embed_index::enqueue_backfill`), which is manual and unprompted.

**Argus response:** none available inside the fences. Recorded as the documented
operator step; `argus_story` stays opted in to embedding in intent.

**Recommendation (post-1.0, additive):** enqueue the embed job from the item host
too. The enqueue is a plain insert into `item_embed_queue` and needs no tap
dispatch, so it does not reintroduce the re-entrancy the direct-model path exists
to avoid.

### G-SDK-NO-ITEM — **[Medium, NEW]** the SDK ships no `item-api` binding

The kernel registers `trovato:kernel/item-api`
(`crates/kernel/src/host/mod.rs:62` → `crates/kernel/src/host/item.rs:19-197`)
and `item-api` is a valid manifest capability
(`crates/kernel/src/plugin/info_parser.rs:356-369`), but
`crates/plugin-sdk/src/host.rs` declares externs for db, ai, http, logging,
queue, crypto, variables, user and plugin invocation — and none for items.

**Impact:** a plugin that needs Items must hand-declare
`#[link(wasm_import_module = "trovato:kernel/item-api")]` and re-implement the
SDK's pointer/length/output-buffer convention itself, including the native stub.
Argus does, in `plugins/argus/src/item_host.rs`. Every detail of that convention
(the 256 KB buffer, the "full buffer means truncation" rule, the negative return
code) is now duplicated outside the SDK, where it will not track SDK changes.

M1 could not have hit this: it declared `argus_story` as a content type but never
created one.

**Recommendation (post-1.0, additive):** add `get_item` / `save_item` /
`delete_item` / `query_items` to `crates/plugin-sdk/src/host.rs`. Pure addition,
no contract change; the host functions already exist and are already frozen.

### G-DB-NO-TX — **[Medium, NEW]** a plugin cannot open a transaction

The `db` host exposes `query-raw` and `execute-raw` and rejects any statement
containing a semicolon (`crates/kernel/src/host/db.rs:70-73`, applied at `:172`
and `:240`). Each call runs in its own transaction
(`crates/kernel/src/host/db.rs:249-256`); there is no `BEGIN`/`COMMIT` a plugin
can reach and no way to batch statements.

**Impact:** every multi-row write is a sequence of independently atomic
statements. Argus creates a story as `save-item` → insert `argus_stories` →
update the article, and an interruption between the first two leaves an
`argus_story` Item with no operational row: inert (invisible to clustering and
to summarize) and detectable, but débris. Entity resolution is four statements
per entity for the same reason. The mitigation available inside the fences is to
make every write idempotent and to order the sequence so a partial failure is
inert rather than corrupt — which M2 does, and which the re-delivery tests cover
— but that is a discipline, not a guarantee.

**Recommendation (post-1.0):** a scoped transaction host surface
(`db-begin`/`db-commit`/`db-rollback` bound to the current dispatch, refused
across host calls that could suspend), **or** an explicit statement in the plugin
documentation that plugin writes are per-statement atomic and nothing more, so
authors design for it rather than discover it.

### G-AI-BASEURL-UNCHECKED — **[Medium, NEW, security]** the chat path never re-validates a provider's base URL

`validate_base_url` blocks private, loopback, link-local and cloud-metadata
targets (`crates/kernel/src/services/ai_provider.rs:394-424`). It is called by
`AiProviderService::embed` as explicit defence in depth
(`:801-805`, commented "SSRF defense-in-depth: re-validate the base URL before
the outbound request") and by `test_connection` (`:876`). It is **not** called on
the `ai-request` execution path (`crates/kernel/src/host/ai.rs:170-237`), and
`save_provider` does not validate either (`:578-593`) — only the admin form does.

**Impact:** a provider row whose `base_url` points into private space is called
by the plugin-facing `ai-request` host. This is admin-configured, so it is not a
privilege escalation; it is a missing seatbelt, and the asymmetry with the
embedding path is the tell — the same author wrote the same check twice and left
the third path out.

**Honest disclosure:** M2's end-to-end tests depend on this gap. The fixture
provider binds to loopback, and the chain test would fail if the check were
added (`crates/kernel/tests/argus_pipeline_test.rs`, `start_fixture_provider`,
where the dependency is written down). If the gap is closed, that fixture needs a
non-loopback bind or an env-gated test allowance — which is the same
`G-SSRF-LOCAL` testability nicety M1 asked for, arriving from the other
direction.

**Recommendation (post-1.0):** call `validate_base_url` on the `ai-request` path
and in `save_provider`, and add the env-gated loopback allowance for tests at the
same time so consumers keep a way to integration-test the AI path.

---

## Residual findings re-confirmed from the consumer side

- **[Medium, RESIDUAL] the only retry signal is a WASM trap** (G-QUEUE-RETRY-SIGNAL,
  accepted at CLOSE 05). M2 hit this from a new angle: the budget pause **cannot**
  be a failure. A paused analyze job that panicked would burn its five queue
  attempts against a limit that will not move for hours and dead-letter work that
  was never wrong. So a pause re-enqueues itself for the next UTC day and returns
  success (`crates/argus-core/src/pipeline.rs`, `budget_gate`). The typed
  `succeed | retry{reason} | dead{reason}` outcome recorded as the designated
  post-1.0 fix would express this directly; the workaround is sound but the
  contract is still a comment rather than a type.
- **[Medium, RESIDUAL] 256 KB host output buffer**
  (`crates/plugin-sdk/src/host.rs:13`). This became a **correctness** constraint
  in M2, not a performance one: clustering candidates carry a full vector each,
  so `MAX_STORY_CANDIDATES` / `MAX_ARTICLE_CANDIDATES` (64 at dimension 256, about
  2 KB of JSON per row) are bounds that keep the result inside the buffer. The
  visible consequence is that near-duplicate detection only considers articles
  sharing an entity — an article the analyze stage named no entities in gets no
  duplicate detection at all. Accepted; recorded so a future reader knows the
  limit is deliberate.
- **[Medium, RESIDUAL] 150 s background epoch.** The one-AI-call-per-job rule held
  again with zero friction across analyze and summarize. It also set the
  clustering lease duration (30 s), which must be comfortably under the epoch so a
  worker cannot outlive its own lock.
- **[Medium, RESIDUAL] cron cadence is external-only.** `tap_cron` fires with a
  timestamp and no cron key, so M2's three periodic duties (retire idle stories,
  reclaim old article bodies, re-enqueue deferred articles) multiplex inside one
  `tap_cron` alongside M1's feed scheduling, exactly as the M1 note predicted.
- **[Low, RESIDUAL] 5 s DB statement timeout.** No hit. The clustering candidate
  queries are the heaviest thing M2 runs and they are `LIMIT`ed and index-backed.

---

## No-friction findings (surfaces that just worked)

- **`AiResponse.cost_estimate` (the p11j companion).** The fix paid off
  immediately and repeatedly: analyze, summarize and decide all read their own
  cost from the response and accumulate it in a plugin-owned table, with no
  kernel-side query anywhere in M2. The `None`-means-unpriced distinction turned
  out to matter in practice, and is tracked separately from the dollar total so a
  low spend figure cannot be misread as cheap when it means unpriced.
- **Queue v2 `delay`.** Three separate mechanisms in M2 are just an enqueue with a
  delay — the budget pause (until the next UTC day), the summarize rate limit
  (until the story is next due), and the clustering-lease retry. Each is one line
  and needs no timer, no poll and no state machine.
- **`ON CONFLICT` through `execute-raw`.** Every idempotent write M2 needs
  (entity upsert, link insert, vector replace, cost accumulation, the lease) is
  expressible as one conflict-handling statement, which is what makes
  at-least-once delivery survivable without transactions.
- **Lightweight record types.** Declaring `argus_entity` as a read-only record
  type made the extracted entities visible in the record admin with a
  four-line manifest block and no code.
- **The host-agnostic core split.** Six new pure modules (analyze, entity, embed,
  cluster, summarize, budget) with 204 native unit tests, all fast and DB-free.
  Every clustering edge — threshold boundary, time decay, topic coherence,
  join/create/wait, near-duplicate, stale story, lease expiry — is tested against
  synthetic vectors with no host in sight.

---

## Deviations from the M2 scope list (with reasoning)

1. **Embeddings are lexical, not semantic** — forced by G-AI-EMBED-UNROUTED, and
   the design gate's explicitly permitted fallback. `M2-DESIGN.md` Decision 1.
2. **No pgvector.** The extension is optional (the kernel's own embedding
   migration is conditional on it, and stock `postgres:17` in `docker-compose.yml`
   does not ship it — verified by probe, not assumed), and there is no vector
   source to feed it. Vectors are a JSON float array in `TEXT`; similarity is
   computed in `argus-core`. `M2-DESIGN.md` Decision 1.
3. **A clustering lease was added** — not in the scope list, forced by
   G-QUEUE-CONCURRENCY-COLLAPSED.
4. **`argus_stories` duplicates the story's title, summary and sources** from the
   Item — forced by G-ITEM-NO-MERGE.
5. **Summarize de-duplication is the rate limit plus a `summarize_pending` flag,
   not a queue-side "one pending job per story" check.** Queue v2 exposes no
   "is this job already pending" query, so a burst of joins does enqueue several
   jobs; the first to run does the work and the rest find the story not yet due
   and defer to the same instant. The observable behaviour is what the scope
   asked for (one call per story per window); the mechanism is different.
6. **Decide is counted against the daily spend but not gated by it.** Decide runs
   on every ingested article and is the pipeline's cost floor; pausing it would
   stop relevance scoring, which is the one thing keeping volume down, so a
   budget pause would *increase* the eventual bill. Analyze and summarize are the
   gated stages. Excluding decide from the *count* would have been the real
   error — in the live run below it is 46% of total spend — so it is recorded.
7. **Near-duplicate detection requires a shared entity.** A consequence of the
   256 KB output buffer bounding the candidate query; an article with no
   extracted entities gets no duplicate detection.
8. **An unparseable decide response loses its cost.** M1's `run_decide` returns
   `cost_estimate: None` on a parse failure even though the call was made and
   priced; M2 records that as an unpriced call (honest "unknown") rather than
   changing the M1 signature. Argus-internal, not kernel friction; worth fixing
   in M3.

---

## M2 live smoke run (real feeds, real kernel, fixture model)

`cargo test -p trovato-kernel --test argus_pipeline_test -- --ignored --nocapture real_feeds_smoke_run`,
against four real public feeds on Postgres + Redis, driving the real
`TapDispatcher` and the real queue-v2 drain. Numbers are actual.

| Metric | Value |
|---|---|
| Feeds | 4 (real, public: Hacker News, Ars Technica, The Verge, BBC Tech) |
| Articles ingested | **71** |
| Discarded at decide | **29** |
| Analyzed → embedded → clustered | **42** |
| Entities extracted | **59** |
| Stories created | **38** (2 with more than one article) |
| Dead-lettered jobs | **0** |
| Jobs drained | 345 in 31 s |
| Decide spend | 71 calls, $0.142 |
| Analyze spend | 42 calls, $0.084 |
| Summarize spend | 42 calls, $0.084 |
| **Total** | **$0.310**, $0.00437 per ingested article |
| Projected at 100 feeds (same articles per feed) | **$7.75** per equivalent run |

**What is real and what is not.** The feeds, articles, titles, bodies,
deduplication, every queue transition, every database write and every clustering
decision are real. The **model is a local fixture**: the relevance score, the
analysis and the synthesis are canned, because the AI path needs a configured
external provider and this run had none. Token counts are therefore the
fixture's (1000 prompt + 500 completion per call, priced at $0.001/$0.002 per 1k),
so **the dollar figures measure the accounting path end to end, not what a real
provider would charge.** The *call counts* are real, and they are what a real
projection should be built from: 1 decide call per ingested article, 1 analyze
call per survivor, and roughly 1 summarize call per story. Nothing here is
extrapolated from a run that did not happen.

### Clustering quality, observed

**2 of 38 stories drew more than one article.** Three things account for that,
and they should be separated:

1. **The confound.** The fixture analyzer returns a near-constant summary for
   every article, so the vectors are driven almost entirely by titles. A real
   analyzer's summaries would carry far more shared vocabulary between two
   reports of one event. This run under-measures the real route.
2. **The real limitation.** Lexical vectors match shared vocabulary, not
   meaning. Two outlets reporting one event in different words cluster only if
   they share entities or distinctive terms — G-AI-EMBED-UNROUTED, priced.
3. **The sample.** Four tech feeds pulled in one snapshot genuinely do not carry
   the same event very often; a 14-day window across 100 feeds is the regime the
   design targets, and a single-snapshot run cannot exercise it.

The near-duplicate path (cosine ≥ 0.98, different feed) fired on no article in
this run, which is the correct outcome for four feeds with no syndication overlap
rather than evidence that it works. It is covered by unit and integration tests
instead.

**The join threshold (default 0.55) is therefore provisional.** It is calibrated
against `lex-v1` vectors and is not comparable to the 0.82 a semantic route would
use. It should be re-calibrated against a real analyzer before A5 puts stories in
front of a reader; the mechanism for doing so is in place (the threshold is a
site variable, and `run_cluster` reports the winning score on every join).
