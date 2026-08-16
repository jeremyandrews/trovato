# Argus Milestone 1 — Friction Log (PF-5 freeze-record input)

**This document is the final PF-5 freeze-record input** (CLOSE 05 consumes it),
produced by building Argus milestone 1 as a pure WASM plugin — the first real
consumer of queue v2, the streaming HTTP host, the background AI principal +
cost accounting, and lightweight records. Every item is severity-tagged with
`file:line` evidence. Severity is Argus's view of impact on the pure-plugin
shape. Findings labeled **NEW** were surfaced or made concrete by this build;
findings labeled **RESIDUAL** are the §9.5 gaps re-confirmed from the consumer
side. "No-friction" findings (surfaces that just worked) are listed last, as
required.

Verified at HEAD `a2cfffa` (argus-m1 commits on top of `a35f5a8`),
`KERNEL_API_VERSION (0,2)`, no kernel/wit/sdk changes in the build session.

> **Amended 2026-07-20 (p11j, pre-freeze).** Jeremy's triage of these findings
> promoted **G-HTTP-META** to a pre-freeze kernel fix and folded **G-COST-OPAQUE**
> in as its additive companion; both are now marked **FIXED** (the `http-open`
> signature widened; `AiResponse.cost_estimate` added), and the forced deviations
> #2 (feeds on one-shot) and #4 (cost read via SQL) are **struck**.
> **G-QUEUE-RETRY-SIGNAL** and **G-SSRF-LOCAL** are **accepted** as documented 1.0
> limitations, routed to the CLOSE 05 freeze record (not fixed).
> `KERNEL_API_VERSION` stays `(0,2)`.

---

## M1-12 live smoke results (real feeds, local kernel)

Ran `plugins/argus/smoke/run.sh` against the release server on Postgres + Redis,
seeding one topic and **4 real public feeds** (Hacker News, Ars Technica, The
Verge, BBC Tech), driving `POST /cron/default-cron-key` for 6 cycles. Numbers
are actual, not faked:

| Metric | Value |
|---|---|
| Feeds | 4 (real, public) |
| **Articles ingested (fetch → dedup → store)** | **71** |
| Decide jobs enqueued | 72 (one per article + a re-fetch cycle) |
| Articles decided / discarded | 0 / 0 — no AI provider configured |
| **Decide cost per 100 articles** | **$0.00** (no successful decide call; G-COST-OPAQUE + no provider) |
| DLQ (dead) jobs at end | 0 (decide jobs were still in retry backoff, attempts=1 of 5) |
| Dedup | article count stayed **71** after an extra cron cycle (re-fetch ingested 0) |
| Conditional-GET validators captured | 3 of 4 feeds persisted an ETag/Last-Modified |
| Per-cycle drain time | ~5.8 s (first, 55 jobs) then ~0.2 s |

**Provider note (honest):** no AI provider was configured, so decide jobs
`ai-request → ERR_AI_NO_PROVIDER (-20) → transient → panic → retry`, sitting in
queue-v2 backoff (they would dead-letter at 5 attempts). Fetch/ingest/dedup and
the fetch→decide handoff are fully exercised against real feeds; a real
decide-cost number requires a configured external provider (a local stub is
SSRF-blocked, G-SSRF-LOCAL). No numbers were invented.

---

## Freeze-blocking-candidate findings

### G-HTTP-META — **[High, NEW → FIXED by p11j]** streaming HTTP host exposes no response status or headers

> **RESOLVED (p11j, pre-freeze, 2026-07-20).** `http-open` now returns
> `HttpOpenResponse {handle, status, headers}` — the status and headers in the
> **same representation** as one-shot `request` (`crates/wit/kernel.wit`
> `http-open`; `crates/plugin-sdk/src/types.rs` `HttpOpenResponse`;
> `crates/kernel/src/host/http.rs` `extract_status_headers` + `build_open_metadata`).
> A `304` streaming GET exposes the status in the metadata and its body-less
> handle reads immediate EOF; an oversized header block errors the open
> (`ERR_HTTP_RESPONSE_TOO_LARGE`, parity with `request`) rather than truncating.
> Argus feed fetch is back on the streaming path with conditional GET on the
> stream (see the amended deviation #2 below). No new error code; no new
> host call/WIT surface beyond the widened `http-open` return; `KERNEL_API_VERSION
> (0,2)`. The description below is the pre-fix record.

`http-open` returns only an opaque handle; `http-read` returns only body bytes.
The `reqwest::Response` (which carries status and headers) is consumed inside
`HttpStream` and never surfaced (`crates/kernel/src/host/http.rs:889-898`; WIT
`crates/wit/kernel.wit:144-157`). Consequences for a feed consumer:

- **Conditional GET is impossible via streaming.** A `304 Not Modified` returns
  an empty body indistinguishable from an empty `200`, and a fresh
  `ETag`/`Last-Modified` cannot be read back to persist. M1-5 requires
  conditional GET, so it cannot be built on the streaming path.
- You cannot combine large-body streaming with conditional GET; you must pick
  one.

**Argus response:** feed fetch uses the one-shot `http-request` (returns
`{status, headers, body}`, so conditional GET works; feeds fit the 1 MB cap),
and the streaming path is used only as an oversized-body fallback (`>1 MB`)
where conditional GET does not apply
(`plugins/argus/src/host_ports.rs`, `HostFetcher::fetch` + `stream_body`). This
is a deviation from M1-5's "fetch via the streaming http host" wording, forced
by the limitation.

**Freeze recommendation:** have `http-open` return `{handle, status, headers}`
(or add an `http-headers(handle)` companion). Without it the streaming host is
usable only for opaque body download, not for any protocol that depends on
response metadata. Additive; does not widen the freeze surface's guarantees.

### G-COST-OPAQUE — **[Medium, NEW → FIXED by p11j]** a plugin cannot read its own AI cost

> **RESOLVED (p11j, pre-freeze, 2026-07-20).** `AiResponse` now carries
> `cost_estimate: Option<f64>` (`crates/plugin-sdk/src/types.rs`), populated by the
> `ai-request` host from the same `estimate_cost` figure it writes to
> `ai_usage_log` (`crates/kernel/src/host/ai.rs`). A plugin reads its own per-call
> cost from the response; `None` is the honest "unpriced/unknown", distinct from a
> genuine `Some(0.0)`. Argus threads it `ChatResponse.cost_estimate` → `decide` →
> `DecideReport.cost_estimate`, and the decide worker emits it in its result JSON
> (see the struck deviation #4). `AiResponse` also gained `#[non_exhaustive]` at
> the freeze boundary so future additive fields stay minor. `KERNEL_API_VERSION
> (0,2)`. The description below is the pre-fix record.

`AiResponse` carries `usage` (prompt/completion/total tokens) but **no cost**
(`crates/plugin-sdk/src/types.rs:884-897`; `AiUsage` `:873-881`). Cost is
computed and persisted entirely kernel-side into the kernel-owned
`ai_usage_log.cost_estimate` (`crates/kernel/src/services/ai_token_budget.rs`
`estimate_cost`/`record_usage`; written at `crates/kernel/src/host/ai.rs:751-782`).
There is no plugin-facing host function that returns cost.

**Impact:** M1-4's "read cost from `cost_estimate`" and M1-12's "decide cost per
100 articles" **cannot be done inside the plugin** — the plugin sees tokens
only. The M1-12 smoke target reads cost by querying `ai_usage_log` in SQL, not
through any plugin API. A plugin that wants to surface or budget-report its own
spend has no way to.

**Freeze recommendation:** add a `cost_estimate` field to `AiResponse` (the
kernel already computes it on the same call), or a host fn returning the
plugin's period cost. Either is additive.

### G-QUEUE-RETRY-SIGNAL — **[Medium, NEW]** the only retry signal is a WASM trap

A queue worker's **normal return is always success** (the row is deleted) — even
a returned `{"status":"error"}` — and the **only** way to signal "retry this
job" is a WASM trap (`panic!`): `dispatched.is_some()` ⇒ `mark_job_succeeded`,
else `mark_job_failed` (`crates/kernel/src/cron/mod.rs:221-233`).

**Impact:** to use queue-v2 retry/backoff/DLQ at all, a worker must `panic!`.
This is a blunt, surprising coupling: the panic is the *only* DLQ `dead_reason`
the operator sees, the structured error is lost across the trap, and the trap
logs as a scary plugin failure even when the retry is expected (e.g. a transient
provider blip). It is also easy to get backwards — an author who returns an
error struct silently drops the job. Argus encodes the contract explicitly (its
core marks every propagated error transient, and the worker panics only on
those, recording terminal state and returning normally for permanent outcomes;
`plugins/argus/src/lib.rs` `tap_queue_worker`), but the contract lives in a
comment, not the type system.

**Freeze recommendation:** let `tap_queue_worker` return a typed outcome
(`succeed` | `retry{reason}` | `dead{reason}`) so retry is a value, not a trap,
and the DLQ carries a real reason. Additive (default the current behavior).

### G-SSRF-LOCAL — **[Low, NEW]** the SSRF fence blocks loopback, so real host paths can't be smoke-tested locally

The p11i SSRF fence correctly blocks private/reserved/loopback targets
(`crates/kernel/src/host/http.rs`, `is_ssrf_block` + `ValidatingResolver`). A
welcome consequence for security, but a testing one for consumers: a **local**
OpenAI-compatible stub provider or a **local** fixture feed server is
SSRF-blocked, so the *success* paths of fetch and decide can only be exercised
against real external services. This is not a defect — it validated the fence
(see no-friction G-SSRF-OK) — but it shapes how a consumer can integration-test.

**Argus response:** success-path logic is tested via `argus-core` in-memory
fakes (47 unit tests); the running-kernel paths are tested for the *blocked* and
*no-provider* cases (`crates/kernel/tests/argus_pipeline_test.rs`); real-feed
ingest is left to the M1-12 smoke run against public feeds.

**Freeze note (non-blocking):** a test-only loopback allowlist (env-gated) would
let consumers integration-test the real host paths without weakening production.
Not a freeze change; a testability nicety.

---

## Residual findings re-confirmed from the consumer side (§9.5)

- **[Medium, RESIDUAL] 64 KB tap I/O buffer** (`crates/kernel/src/tap/dispatcher.rs:322-334`).
  Shaped the design as predicted: large feed bodies are pulled via the HTTP host
  (not handed into a tap) and articles are stored to `argus_articles` and passed
  by id, never carried through a queue payload. No workaround needed; the
  constraint held.
- **[Medium, RESIDUAL] 150 s background epoch is a trap for multi-call loops**
  (`crates/kernel/src/plugin/limits.rs`). The "one AI call per queue job" design
  rule held with zero friction — decide is exactly one call per job. Stage
  granularity followed from this rule, as designed.
- **[Medium, RESIDUAL] cron cadence is external-only**
  (`crates/kernel/src/routes/cron.rs:20-24`). `tap_cron` fires every cycle with
  only a `{timestamp}` and **no cron-key**, so a plugin self-schedules. Argus
  does this cleanly via `last_fetched_at + interval` and a persisted round-robin
  cursor (`argus_state`). Minor: a plugin with several independent cron duties
  must multiplex them inside one `tap_cron`.
- **[Low, RESIDUAL] 5 s DB statement timeout** (`crates/kernel/src/host/db.rs`).
  No hit — every argus query is single-row or small.
- **[Low, RESIDUAL] record admin is read-only** (`routes/admin_record_type.rs`,
  G3). No hit — Argus writes records through the `db` host; read-only admin is
  exactly right for operational inspection.

---

## No-friction findings (surfaces that just worked)

- **Lightweight record types (P11g / D-53..D-59).** Declaring
  `argus_article`/`argus_feed`/`argus_topic` as `[[record_types]]`, admission
  against the migration-owned allowlist, gather over the record table, the
  logical→column field map, filter-by-record-field, and story→articles reverse
  reference (gather filtered by the `story_id` field) **all worked first try**
  (`crates/kernel/tests/argus_record_test.rs`). The record tier is solid and is
  the right home for high-volume articles. The `record_type` key on a persisted
  `gather_query` definition routes through the normal gather path with no special
  handling.
- **Queue v2** (enqueue with priority/delay, claim/drain, backoff, DLQ). Behaved
  exactly as documented; the DLQ surfaced a poison decide job cleanly with a
  `dead_reason` (`argus_pipeline_test.rs::decide_without_provider_dead_letters`).
  The inversion from "build our own queue" to "use the kernel's" was pure
  subtraction of code.
- **Background AI principal + `ai_background` capability** (P11c / D-40/D-41).
  The capability gate worked: decide reached the provider call with no
  `ERR_AI_BACKGROUND_DENIED (-28)`; the only failure without a provider is the
  expected `ERR_AI_NO_PROVIDER (-20)`. The `[capabilities] ai_background = true`
  + `"ai-api"` declaration is the whole ceremony.
- **`db` host `raw_sql` upsert / `query_raw` JSON decode.** The array-of-row-
  objects shape, `id::text AS id` casting to control JSON typing, and
  `INSERT ... ON CONFLICT (url) DO NOTHING` idempotency all worked as expected
  (`argus_pipeline_test.rs::article_upsert_is_idempotent`).
- **p11i SSRF hardening**, validated from the consumer side: a link-local feed
  URL (`169.254.169.254`) was blocked and surfaced as `ERR_HTTP_INVALID_URL`,
  handled as a clean per-feed failure with no worker crash
  (`argus_pipeline_test.rs::fetch_ssrf_blocked_flags_feed_without_crashing`).
  This is the exact gap G1 p11i closed, now exercised by its intended consumer.
- **`http_max_transfer` manifest declaration** (16 MB) was accepted and clamped
  with no ceremony.
- **The host-agnostic core split** (`argus-core` behind injected ports) cost
  nothing and kept 47 of the pipeline's tests pure, fast, and DB-free.

---

## Deviations from the M1 story list (with reasoning)

1. **Feeds and topics are operational plugin tables, not Items** (M1-2 said
   "argus_story/argus_topic/argus_feed stay content types (Items)"). The same
   story's schema paragraph specifies dedicated `argus_feeds`/`argus_topics`
   tables with mutable operational columns (etag, `failure_count`,
   `last_fetched_at`, `relevance_prompt`). Representing those as Items would force
   `item-api` writes with full Item tax on every fetch-state update. Only
   `argus_story` stays an Item (§9.4, semantic search). Feeds/topics are also
   declared as read-only record types so their rows are visible in the record
   admin. `entity`/`reaction`/`discussion` were dropped (out of M1 scope).
2. ~~**Feed fetch uses one-shot `http-request`, not the streaming host**~~ —
   **STRUCK (p11j fix).** This deviation was forced by G-HTTP-META; now that
   `http-open` returns the response status and headers, `HostFetcher::fetch` is
   back on the streaming path with conditional GET on the stream (replay stored
   `ETag`/`Last-Modified`, short-circuit `304` via the metadata status, read a
   fresh validator back, stream the body in 64 KB chunks up to the 16 MB manifest
   ceiling). Small feeds and multi-MB article bodies both take the one streaming
   path; the one-shot fallback is gone.
3. **The wasm artifact is CI-built, not committed** — argus is a first-class
   workspace member (like `trovato_record_ref`/`trovato_blog`), so both CI wasm
   blocks + `pre-commit-check.sh --full` build it; committing is reserved for the
   out-of-tree `ritrovo_importer` pattern. `-p argus` was added to all three.
4. ~~**"Read cost from `cost_estimate`" (M1-4) is done in the smoke SQL, not the
   plugin**~~ — **STRUCK (p11j fix).** This was forced by G-COST-OPAQUE; now that
   `AiResponse.cost_estimate` exists, the decide worker reads cost from the
   response (`ChatResponse.cost_estimate` → `DecideReport.cost_estimate` → the
   worker's result JSON), not a kernel-side SQL query on `ai_usage_log`.
5. **"Survivor advances stages" (M1-8) is core-tested, not integration-tested** —
   a survivor requires a working provider, which the SSRF fence + no-local-mock
   (G-SSRF-LOCAL) prevent in-process; the argus-core pipeline tests prove
   decide→analyze handoff with a fake provider.
