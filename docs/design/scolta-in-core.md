# Scolta in Trovato Core

**Status:** Design, not built
**Date:** 2026-11-22
**Read against:** Trovato `origin/main` at `cca05f9`; Scolta repositories at the commits listed in [Sources](#sources).
**Question:** Can Scolta, Tag1's AI search, be part of Trovato core cleanly, and if so how?

---

## Answer

**Yes, cleanly, as a fourth Scolta binding that lives in the kernel.** Trovato
depends on the published `scolta-core` crate for the server-side AI text work
(prompts, expansion parsing, query sanitizing, context and conversation
trimming). It serves Scolta's canonical browser bundle byte for byte, and it
keeps its own index builder, because that builder is where the access guarantee
lives. This is how Scolta's PHP, Python and Node bindings are already built, so
Trovato would be following Scolta's own design, not working around it.

Nothing needs forking, nothing is patched, and Scolta needs no Trovato-specific
code. Four general improvements, none blocking, would help every binding. They are listed as
proposals for the Scolta team in [Proposals for the Scolta team](#proposals-for-the-scolta-team).

The real finding is that **Trovato already ships a patched copy of Scolta**.
`static/js/scolta.js` is a 1,338-line fork of an early Scolta browser script.
`crates/kernel/src/search/prompts.rs` is a hand port of scolta-core's prompts,
and it no longer matches them (8 KB here against 45 KB upstream, with no check
that they agree). So the clean-fit question is really about how to stop
carrying a fork.

---

## 1. Trovato search today

Trovato has two search backends and one AI layer.

### Server side: Postgres full text

| Piece | Where |
|---|---|
| The search service | `crates/kernel/src/search/mod.rs`: `SearchService::search` (counted in analytics), `search_unrecorded` (AI chat retrieval, MCP, tests) |
| Indexing | Postgres triggers on `item` and `comment` keep the vector current inside the write's own transaction. `SearchService::reindex_item` and `reindex_bundle` recompute on demand (`mod.rs:452,470`). Field weights come from `search_field_config`, through `configure_field`. |
| Coverage (B106, merged in #218 and #219) | Every text field and every published comment. Two switches per type (`SearchSwitches`, `mod.rs:47`), edited at `/admin/structure/types/{type}/search` |
| Snippets (B123, merged in #226) | `search::snippet::render` (`snippet.rs:62`): escaped exactly once |
| Routes | `crates/kernel/src/routes/search.rs`: `/search` (the HTML page, and the no-JavaScript fallback), `/api/search`, `/api/v1/search` |
| Access | The searching user's own access, and stages only for holders of `use stages` (`routes::helpers::read_stages`) |

### Browser side: a static Pagefind index

| Piece | Where |
|---|---|
| Rebuild signal | `pagefind_index_status`, a table owned by the `trovato_search` plugin (`plugins/trovato_search/migrations/001_create_index_status.sql`). The kernel's item and comment services set it through `cron::pagefind::request_rebuild` (`cron/pagefind.rs:161`). The plugin's `tap_cron` also polls `MAX(changed)` for writes that bypass the services (`plugins/trovato_search/src/lib.rs`). |
| What goes in | `cron::pagefind::indexable_documents` (`cron/pagefind.rs:326`), **built as the anonymous visitor** through `ItemService::filter_page_for_view`, covering both the item tier and the field tier (`tap_field_access`). Only comments that the item page shows are included. |
| Markup | `render_fragment` (`cron/pagefind.rs:505`): title at weight A, one `<p>` per text field at its weight, comments, and the meta `type`, `date` (`YYYY-MM-DD`), `description`, `location`, `event_dates` and `url` |
| Build | `maybe_rebuild_index` (`cron/pagefind.rs:185`) runs the **Pagefind CLI v1.3.0** (installed by `Dockerfile:41-47`) with a 2-minute timeout, then swaps the result into `PAGEFIND_OUTPUT_DIR` by renaming it. It runs only while `trovato_search` is enabled. |
| Browser script | `static/js/scolta.js` (the fork, 53 KB, scoring in plain JavaScript, no WASM), `static/js/search-init.js`, `static/css/scolta.css`. The page is `templates/search.html`, which renders a `scolta-config` JSON block from `SearchSettings::scolta_config`. |
| Ranking settings | `SearchSettings` (`search/settings.rs`), stored in `site_config` under `search` and edited at `/admin/config/search` |
| Analytics | `search/analytics.rs`: daily counts with nothing that identifies a visitor, plus a beacon to `POST /api/search/log` sent from the fork |

### The AI layer

| Piece | Where |
|---|---|
| Endpoints | `crates/kernel/src/routes/api_search.rs`: `POST /api/v1/search/expand`, `/summarize` and `/followup`. All three return plain JSON. |
| Send path | `AiProviderService::chat_once` (B89), by way of the file's thin `complete`: the base URL check, the configured timeout and the chat circuit breaker, with the one request builder and parser in `services/ai_tools.rs` |
| Prompts | `search/prompts.rs`: `EXPAND_QUERY`, `SUMMARIZE`, `FOLLOW_UP` ported from scolta-core, plus Trovato's own `CLASSIFY_SENTIMENT` |
| Guards that exist | A stage switch on each endpoint (404 when the stage is off); the follow-up turn cap (`max_followups`), counted only on a `messages[].role` of `user` or `assistant`, anything else refused with 400 (B282); an expansion cache (`CACHE_TTL_SEARCH_EXPAND`, keyed on the normalized query plus site name and slogan); a query over `search::MAX_QUERY_LENGTH` refused with 400 on all three endpoints (B282); a 64 KiB request body limit on all three (B282); per-IP rate buckets of 30, 10 and 5 per minute (`middleware/rate_limit.rs:286-288`); the CSP already allows `'self'` and `'wasm-unsafe-eval'` (`middleware/security_headers.rs:43`) |
| Guards | Since B89 each endpoint asks for `use ai` (granted to every role that may search), and every call goes through `AiProviderService::chat_once`: base URL check, the caller's budget (a member's own, or the site-level cap every anonymous visitor shares, set as the `anonymous user` role's limit — or `DEFAULT_ANONYMOUS_TOKEN_LIMIT` when the site has configured none at all, B282), the configured timeout, the chat circuit breaker, and a usage row with real tokens and cost. The sentiment classifier goes through the same path and counts against the anonymous cap. Since B282, `summarize` and `followup` build their own prompt: they take `results: [{url}]`, not text, resolve each URL through the item service and the caller's own view access, and read only what survives — nothing the browser sends as free text (`context`, `excerpts`, a message's content beyond the two roles above) reaches the model. |

`trovato_scolta`, an earlier plugin attempt, was deleted (see `CHANGELOG.md`,
"`trovato_scolta` is removed"). A WASM plugin cannot write files, run a
subprocess or hold the AI send path, so the plugin never served a request.

---

## 2. Scolta today

Scolta is eleven packages. All of them are MIT except scolta-drupal, which is
GPL-2.0-or-later. Each package is versioned independently with semver, and
compatibility between packages is expressed only through dependency constraints
(`scolta-core/VERSIONING.md`). Here is what matters for Trovato.

**`scolta-core` (Rust).** A browser engine. It **does not build indexes, does
not stem, and makes no HTTP or AI calls.**

- Exports, as WASM and as a Rust API (`src/lib.rs`, module `inner`, plus
  `pub mod prompts, expansion, sanitize, scoring, context, conversation, ...`):
  `score_results`, `batch_score_results`, `merge_results`,
  `match_priority_pages`, `parse_expansion(_with_language)`, `extract_context`,
  `batch_extract_context`, `sanitize_query` (PII redaction),
  `truncate_conversation`, `get_prompt` / `resolve_prompt`, `version` and
  `describe`.
- `WASM_INTERFACE_VERSION = 4`.
- Built as both `cdylib` and `rlib`. The wasm-bindgen glue is gated by
  `cfg(target_arch = "wasm32")`, so the crate builds natively as an ordinary
  dependency.
- Published on crates.io: `scolta-core` 1.0.1, the newest there, was tagged on
  2026-06-05. The repository is at `1.1.0-dev` and carries five months of
  unreleased work, including the slim artifact. `cargo publish` is a manual step
  (`MAINTAINING.md` §2).
- Artifact sizes: the full WASM is 1.22 MB raw and 310 KB Brotli; the slim one
  is 335 KB raw and 109 KB Brotli (`size-budgets.json`).
- The canonical prompt text is authored in `src/prompts.rs`. Each binding keeps
  a mirror and runs an identity test against it in CI.

**Bindings (`scolta-php`, `scolta-python`, `scolta-node`).** Each one implements
its whole server side natively: HTML cleaning, **its own Pagefind-format index
writer** (scolta-php removed its use of the Pagefind binary in September 2026),
stemming pinned to Pagefind's, and an AI proxy. "Parity between the three is
held by tests, not by shared code" (`MAINTAINING.md` §1).

**Browser bundle.** `scolta-php/assets/` holds the canonical copy:

- `js/scolta.js`: 6,667 lines, 316 KB
- `css/scolta.css`: 19 KB
- `wasm/scolta_core.js` and `wasm/scolta_core_bg.wasm`
- A Pagefind 1.5.0 runtime
- Checksums in `assets/ASSETS.sha256`

`scolta.js` reads its configuration from `window.scolta`: about 45 scoring
keys, `endpoints`, `pagefindPath`, `wasmPath`, `allowedLinkDomains` and others.
It loads Pagefind and the WASM module with a dynamic `import()`. If the WASM
module fails to load, it falls back to scoring in JavaScript.

The AI calls are POSTs that return plain JSON:

| Endpoint (default path) | Request | Response |
|---|---|---|
| `expand-query` | `{query}` | `{terms, expand_primary_weight, sort_hint?, filter_hint?, degraded?}` |
| `summarize` | `{query, context}` | `{summary}` |
| `followup` | `{messages}` | `{response, remaining}` |

Hosts extend the bundle without patching it, through:

- bubbling lifecycle events (`scolta:results-rendered` and others; see `emitLifecycle`)
- `Scolta.setResultRenderer`
- `Scolta.createInstance`

The bundle contains no `eval` and no inline `<script>`. It does write inline
`style=` attributes.

**How sites get the bundle.** The WordPress, Node and Python packages each commit
a copy. The Drupal and Laravel packages read it from scolta-php in `vendor/` and
deploy it at run time (`scolta-core/ASSETS.md`).

**Drupal (`scolta-drupal`), the closest analogue to Trovato.**

- Content writes (`hook_entity_insert`, `_update`, `_delete`) queue an
  incremental rebuild on the `scolta_rebuild` queue, which an external
  `drush queue:run` processes.
- `hook_rebuild` redeploys the bundle to `public://scolta-assets`.
- The AI routes require `use scolta ai`, which only authenticated users get by
  default, and are flood limited per IP and site-wide.
- The API key comes only from the environment or `settings.php`.
- **Its access control is weaker than Trovato's.** `ScoltaContentGatherer`
  queries with `accessCheck(FALSE)` and decides what is indexed by published
  status alone. It renders the body as anonymous but takes the title and mapped
  fields straight from storage, and it never consults node access grants. Its
  README says so (`README.md:429`).

Trovato should not copy that gatherer. Its own `indexable_documents` is already
stricter.

**Release cadence.** scolta-core was tagged five times from May to June 2026 and
not since. scolta-php's newest tag is v1.3.0 (2026-08-20) with `2.0.0-dev` on
`main`. The order is core first, then the bindings, then the adapters. A new
bundle reaches the packages that commit a copy only when someone deliberately
re-vendors it (`ASSETS.md`).

---

## 3. Options

Sizes are estimates for lines Trovato adds or deletes, excluding vendored
bundle files.

| | A. Crate only | B. Stock plugin | C. Port the AI layer only | **D. Kernel as a Scolta binding** |
|---|---|---|---|---|
| What it is | The kernel depends on `scolta-core`; the browser keeps the fork | A shipped, enabled plugin wraps Scolta; kernel search is the fallback | Keep Trovato search and the fork; keep porting Scolta's prompts and AI rules by hand | `scolta-core` crate on the server, plus the canonical bundle served unmodified, plus Trovato's own index builder |
| Code | +150, −250 | +1,500 or more (plugin) plus kernel hooks it cannot avoid | ±0 now, recurring | +400, −1,900 |
| Trovato deletes | `search/prompts.rs` (all but sentiment), the expansion parser | Nothing; it adds a second layer | Nothing | The fork (`static/js/scolta.js`, `static/css/scolta.css`), the ported prompts, the hand-written expansion parser |
| Gains | Canonical prompts and expansion parsing | Nothing D lacks | Nothing new | Everything Scolta ships in the browser: WASM scoring, phrase proximity, facets, sort, search as you type, lifecycle events, fixes released upstream. Canonical prompts and PII redaction on the server. |
| Upgrades | A Cargo bump | A plugin release per Scolta release, plus the kernel hooks | A manual diff each time, which has already drifted | A Cargo bump plus a scripted, checksummed re-vendor of the bundle, both pinned |
| Performance | Unchanged | Same as D, plus calls across the plugin boundary | Unchanged | Index build unchanged. Page weight rises by about 330 KB Brotli: 310 KB of WASM (109 KB once slim ships), plus about 70 KB of compressed JS against today's 15 KB. The WASM is cached after the first search. Query latency stays local: Pagefind plus a few milliseconds of scoring. |
| Security | Unchanged | The plugin sandbox cannot see `filter_page_for_view`, so it must trust the kernel anyway | Unchanged, and fixes are missed | The kernel keeps the access seam. The bundle runs under the current CSP. The AI guards stay in the kernel. See §5. |
| Clean fit? | Partial: the fork stays | **No.** The sandbox cannot write the index, serve assets or hold the send path. This is what killed `trovato_scolta`. | **No.** It is the status quo: a patched copy of Scolta | **Yes** |

**Why not B.** A Trovato plugin is a sandboxed component. It has no file system,
no subprocesses and no route to static files, and its AI access goes through the
same `ai-request` host function the kernel would guard anyway. Everything Scolta
needs from a host is something only the kernel can do: write the index, serve
the bundle, call the provider, and decide access. A plugin would be a name on
top of kernel code. Trovato keeps exactly one plugin role here: `trovato_search`
stays the on/off switch for the static index, as it is today.

**Why not A alone.** Prompts are the smallest part. The browser fork is where the
drift costs most: it lacks WASM scoring, phrase proximity, facets and every fix
made upstream since the fork.

**Why not C.** It is what Trovato does now, and the Answer above shows the result: the
prompts have diverged and nothing goes red when they do.

**Option E, considered: use Scolta's index writer instead of the Pagefind CLI.**
Scolta has no Rust index writer. The bindings each wrote their own in PHP,
Python and TypeScript. Trovato's equivalent is the Pagefind CLI, or later the
`pagefind` crate as a library (`search-architecture.md`, "Rust/WASM Synergy").
The index format is Pagefind's, not Scolta's, so the CLI is a valid producer.
E is folded into D as a later, optional step.

---

## 4. Recommendation: D

The kernel becomes a Scolta binding in the same shape as scolta-php, minus the
parts the kernel already does better:

1. **Server: `scolta-core` as a pinned crate dependency** (`scolta-core = "=X.Y.Z"`
   in `crates/kernel/Cargo.toml`, an exact pin). The kernel takes:
   - the expand, summarize and follow-up prompt text from `scolta_core::prompts`
   - `parse_expansion` for the provider's reply
   - `sanitize_query` before a query is cached, logged or counted
   - `truncate_conversation` for follow-ups

   `CLASSIFY_SENTIMENT` stays a Trovato prompt, because it is a Trovato feature.
   There is no mirror and no identity test: the crate *is* the source.
2. **Browser: the canonical bundle, unmodified**, vendored from a tagged
   scolta-php release into `static/scolta/`. The vendor script and a CI test
   check its bytes against the upstream `ASSETS.sha256` (the same model as
   scolta-wp's `assets-in-sync` job). The page sets `window.scolta`, including
   `endpoints` (Trovato's `/api/v1/search/*`), `pagefindPath` (the index
   Trovato built), `wasmPath` and `allowedLinkDomains` (the site's own host).
   Trovato's extras are expressed through configuration and events, not patches:
   - the analytics beacon becomes a `scolta:results-rendered` listener in
     `search-init.js`
   - the metadata chips (location, event dates) become a `setResultRenderer`, or
     are dropped if the stock renderer is acceptable
   - type boosts map to Scolta's `METADATA_BOOSTS` on `meta.type`
3. **Index: Trovato's own, through `indexable_documents`, unchanged in what it
   admits.** The Pagefind runtime is the one the CLI writes beside the index, so
   the index and its runtime always match. The CLI moves to the Pagefind version
   Scolta tests against, currently 1.5.0. Fragments add the attributes the
   bundle's facets and sorting read: `data-pagefind-filter` for type and
   `data-pagefind-sort` for date.
4. **AI endpoints: Trovato's, matching the bundle's contract.**
   - Expansion answers `{terms, expand_primary_weight}`.
   - Follow-up answers `{response, remaining}`.
   - The guards stay where they are and grow through B89 and B116.

   Scolta's PHP AI proxy is not used. It has no rate limiting, trusts browser
   history for the follow-up cap, and would duplicate Trovato's provider registry.

**Who releases what.** The Scolta team releases `scolta-core` (crates.io) and
the bundle (scolta-php tags). Trovato picks both up on its own schedule, through
one pull request that bumps the crate pin and re-vendors the bundle at a named
scolta-php tag. That pull request is gated by the checksum test, the prompt test
and the search suite. The bundle's `WASM_INTERFACE_VERSION` and the crate
version are recorded in `static/scolta/VERSION`, so the pair a site runs can be
read in one place. A Scolta major version is a deliberate Trovato upgrade, never
an accident: both are exact pins.

---

## 5. The access control guarantee

**Rule: the static index holds only what an anonymous visitor can see, and
Scolta never decides what that is.**

- **One producer.** The only code that writes the index is
  `cron::pagefind::build_index`, and the only thing it reads is
  `indexable_documents`. That function runs every candidate through
  `ItemService::filter_page_for_view` as `UserContext::anonymous` with the
  anonymous role's real permissions. It applies the same seam on both tiers that
  a page view uses: item access (published, Live stage, `access content`,
  `tap_item_access`) and field access (`tap_field_access`). Values in the index
  are built from the *filtered* fields only. Comments are included only when the
  item page shows them.
- **Scolta reads the index; it never writes it.** Under D, no Scolta code runs
  server side except pure functions over a query and its text. The browser
  bundle can only fetch files the kernel already chose to publish. That is the
  difference from scolta-drupal, whose gatherer decides by status alone.
- **No per-role indexes.** A static index cannot be partly private. Pagefind
  fetches chunks with plain GETs, so a role index would need authenticated
  serving of every chunk, a second build per role, and invalidation on every
  role change. Content that only some users can see stays on server search,
  which applies the searcher's own access and stages. The search page already
  links to it as the fallback. If per-role static search is ever wanted, it is
  its own design.
- **AI context.** Summaries and follow-ups are built from text the browser
  sends, which Scolta's bundle takes from the public index. The server adds no
  content of its own, so an AI answer cannot reveal private content. The
  endpoints are still a relay for arbitrary text, which raises three points:
  - B89 and B116 must add the `use ai` permission, budgets and a site-wide cap
    for anonymous visitors.
  - The server keeps its own size caps (`ai_summary_max_chars`).
  - The follow-up cap is advisory, as it is in every Scolta binding, because the
    history comes from the client. Budgets are the real limit.
- **Prompt injection.** Comments are indexed (B106), so visitor-written text
  reaches the summarize prompt. Here is what limits the damage:
  - The bundle escapes summaries before formatting them (`formatSummary`, `scolta.js:2025`) and keeps links to
    `allowedLinkDomains`, which Trovato sets to the site's own host.
  - The model has no tools and no secrets.
  - Queries go through `sanitize_query` before they are cached or counted.
  - The worst outcome is a misleading summary on that one page.
- **Keys** stay in Trovato's provider configuration on the server. The browser
  never sees them.
- **CSP.** The current policy already permits the bundle: scripts from
  `'self'`, WASM through `'wasm-unsafe-eval'`, inline style attributes through
  the existing `'unsafe-inline'` in `style-src`, and `connect-src 'self'`. The
  vendored files are served from `/static`, so no new origin is needed.

---

## 6. How the design fits the queued work

| Item | State on `origin/main` | Fit |
|---|---|---|
| **B106**: search covers every type, field and comment | Merged (#218, #219) | Unchanged. D keeps `indexable_documents` and the switches as they are. Its gaps (B139 scheduled unpublish, B140 comments plugin off, B141 block and compound fields) remain Trovato work in the index builder, where they belong. |
| **B89**: one AI send path, budgets | Not merged | D uses scolta-core only for text and parsing, never for sending, so the crate does not compete with B89's send path. PR 3 below should land after B89 so the two do not edit `api_search.rs` at the same time. If B89 lands first, PR 3 is smaller. |
| **B116**: AI search token usage and permission | Not merged | Orthogonal. The endpoints keep their routes and gain B116's `use ai` check and usage recording. The bundle treats a refused call as that stage being unavailable: expansion returns nothing and search carries on (`scolta.js:1420`). |
| **B123**: snippet escaping | Merged (#226) | Server snippets are unchanged. In the browser, rendering moves to Scolta's renderer, so PR 4 adds a test that a title and an excerpt holding markup reach the page as text. |

---

## 7. Migration for existing sites

- **Database:** none. `pagefind_index_status` stays. `SearchSettings` keeps
  every key, and the keys Scolta adds (phrase multipliers, `recency_strategy`,
  `language`) are new optional keys whose defaults are Scolta's.
- **Index:** one rebuild. PR 2 compares the version in the served
  `pagefind-entry.json` with the CLI's version at startup and requests a rebuild
  when they differ. Until that rebuild runs, the old index and its own runtime
  keep working together.
- **Ranking:** results reorder somewhat, because phrase proximity and Scolta's
  merging are new. `UPGRADING.md` says so. Stored boosts keep their values.
- **Expansion cache:** the prompt text changes, so PR 3 drops the
  `search_expand` cache tag once on deploy and adds the scolta-core version to
  the cache key. Later bumps then invalidate it automatically.
- **Themes:** a theme that overrode `templates/search.html` or styled
  `.scolta-*` classes needs a check against the new markup. This goes in
  `UPGRADING.md`.

---

## 8. Implementation plan

Seven pull requests, in order. Each one is a single action with its own tests.

1. **The endpoint contract.** Make `/api/v1/search/*` answer what the canonical
   bundle reads:
   - expansion returns `terms` plus `expand_primary_weight`
   - follow-up returns `response` plus `remaining`
   - a disabled or refused stage answers in the bundle's `degraded` shape

   *Tests:* request and response fixtures taken from scolta-php's
   `AiEndpointHandler` contract pass through each endpoint; a disabled stage
   still answers 404 for API clients that are not the bundle.
2. **Pagefind version.** Move the Dockerfile CLI and `INSTALL.md` to the version
   the bundle is tested with. Add `data-pagefind-filter="type"` and
   `data-pagefind-sort="date"` to `render_fragment`. Request a rebuild when the
   served index's version differs from the CLI's.
   *Tests:* fragment assertions in `pagefind_fragment_test.rs`; a stale
   `pagefind-entry.json` triggers a rebuild request; access tests
   (`search_access_test.rs`) unchanged and green.
3. **`scolta-core` crate.** Add the exact-pinned dependency. Replace the expand,
   summarize and follow-up text in `search/prompts.rs` with
   `scolta_core::prompts` (keeping `CLASSIFY_SENTIMENT`). Replace the hand
   parser with `parse_expansion`, run `sanitize_query` before caching and
   analytics, and run `truncate_conversation` on follow-ups. Add the crate
   version to the expansion cache key.
   *Tests:* the prompt each endpoint sends equals
   `scolta_core::prompts::resolve_template` output; expansion parsing cases
   (JSON, fenced JSON, prose); an email in a query never reaches the cache key or
   `search_query_daily`; an over-long conversation is trimmed.
4. **Vendor the canonical bundle.** Add `scripts/vendor-scolta.sh` (shell, no
   Python), which fetches a named scolta-php tag, verifies `ASSETS.sha256` and
   copies the four files into `static/scolta/`. Point `templates/search.html`
   at them through `window.scolta` (`wasmPath`, `pagefindPath`, `endpoints`,
   `allowedLinkDomains`). Move the analytics beacon and the metadata rendering
   into `search-init.js` through the lifecycle events and `setResultRenderer`.
   Delete `static/js/scolta.js` and `static/css/scolta.css`.
   *Tests:* a checksum test of the committed files against the committed
   manifest; the search page renders the config keys the bundle reads; the page
   loads under the enforcing CSP with no violations (Playwright); the B123
   markup-as-text check in the browser; the beacon fires once per search.
5. **Settings mapping.** Extend `SearchSettings` and `ScoltaConfig` with the
   Scolta scoring keys Trovato exposes (phrase multipliers, `recency_strategy`,
   `language`) and map `type_boosts` to `METADATA_BOOSTS`. Add them to
   `/admin/config/search`.
   *Tests:* `search_settings_test.rs` round trip and range checks; the rendered
   config carries each key the bundle reads.
6. **Slim WASM.** Once a scolta-core release ships the slim artifact and
   scolta-php carries it, re-vendor and point `wasmPath` at the slim loader.
   *Tests:* the checksum test; a page-weight assertion on the search page.
7. **Docs.** Rewrite the browser and AI sections of `search-architecture.md` to
   describe D. Add `UPGRADING.md` notes (ranking, theme overrides, one rebuild).
   Add a "Taking a new Scolta release" section: bump the crate, run the vendor
   script, run the search suite.
   *Tests:* the repository's doc checks.

PRs 1 and 2 can land in either order. PR 3 waits for B89. PR 4 needs 1 and 2.
PR 6 waits on Scolta.

---

## Proposals for the Scolta team

None of these block D. Each is general and would help every binding, not only
Trovato.

1. **Publish `scolta-core` to crates.io on every tag.** The newest crate (1.0.1,
   June 2026) lags `main` by five months, and `cargo publish` is manual. A Rust
   binding can only pin what is published.
2. **Ship the browser bundle as a release artifact.** A tarball of
   `scolta-php/assets/` with `ASSETS.sha256`, attached to each scolta-php
   release, would let bindings outside PHP vendor it without a PHP checkout. The
   Node and Python vendor scripts read a sibling checkout today.
3. **Document the AI endpoint contract** (request and response shapes, the
   `degraded` shape, `remaining`) as a page in scolta-core alongside `API.md`.
   Every binding implements it, and today it is defined only by `scolta.js` and
   `AiEndpointHandler.php`.
4. **Optional:** use classes instead of inline `style=` attributes in
   `scolta.js`, so a site can drop `'unsafe-inline'` from `style-src`. Trovato
   does not need this today.

---

## Sources

Scolta repositories were read at these commits, as shallow read-only clones.
The demo repositories were read for their README only.

| Repository | Commit |
|---|---|
| scolta-core | `8b3a8f6c3ab6ed25daa35eee36ab168a696778e4` |
| scolta-php | `c11f6a32e29092e398c53752ec0dc6c3b0af9354` |
| scolta-drupal | `476497ba7e44c41d12390aa092a20e4457665341` |
| scolta-wp | `17760f53dd7add188933961ecaf874d1da577eda` |
| scolta-laravel | `3a0aa7c8e7e89a6282783deb84bfe7f340e184f6` |
| scolta-python | `f6e39f002f15f16d6075633df6dfd5f974748295` |
| scolta-django | `d5cc1ab828bd9ffa721bc2da7c44b161b7a4ca49` |
| scolta-node | `a832ca4bbb2d7d5951cf1d5ffc7cf0f3757274e1` |
| scolta-next | `af7efef2614be9a42f2d0c0b4d190d1aaba9618b` |
| scolta-nuxt | `a9cf50247f90825b89e2792f5aa315626263d7a3` |
| scolta-astro | `f141394b6a56d0105afae6d1ae4d6137f1671b7f` |
| 13 `scolta-demo-*` repositories | README only |

Related: [Search Architecture](search-architecture.md), [AI Integration](ai-integration.md).
