# Argus: try-it report

A first-run account of bringing Argus up from a clean clone against real feeds
and a real provider, on the kernel in this tree (0.102.0), and checking the six
claims the milestone documents make about it.

**Status: session 1 of 2.** This session built the image, brought the stack up,
configured the provider and the content, and confirmed ingestion, relevance
scoring and analysis against live feeds. Every verdict below states plainly
whether it was reached.

**Read finding F8 first.** The intended shape of this exercise was to leave the
stack running and let a second session collect the verdicts that need elapsed
time. That is not possible on this build: the queue workers wedge within 30 to
60 seconds of every container start and only a restart recovers, which is why
four of the six verdicts are unreached and why an external watchdog (section 9)
had to be added for the overnight window. The defect that stops the pipeline is
the most important thing this run found.

Run date: 2026-09-19. Host: Apple silicon, Docker 29.4.0, Docker Desktop.

---

## 1. Build

| Measure | Value |
|---|---|
| Cold build, `docker compose --profile argus build --no-cache argus` | **244 s (4 m 04 s)** |
| Result | `rc=0`, image `ghcr.io/jeremyandrews/trovato:nightly` built locally |
| `up -d` from built image to healthy | ~25 s |

`--no-cache` was used deliberately: this is a fresh clone, but the host's shared
BuildKit cache holds layers from other Trovato checkouts, and reusing them would
have measured the cache rather than the build. 244 s is the honest from-scratch
figure for the whole tree — kernel binary plus 26 WASM plugins — and it makes
`README.md`'s "about four minutes on an Apple silicon laptop" **accurate**, not
stale. That is worth recording as such, because most of the other numbers in
that file are not.

The `Dockerfile` does `COPY . .` and then `cargo build`, with no dependency
pre-build layer, so any source change re-runs the full 244 s. That is a cost an
iterating developer pays every time, not a defect.

---

## 2. Configuration, exactly as run

Ports are shifted off the defaults because this host already runs three other
Trovato stacks; nothing else about the compose file was changed.

`.env` (gitignored; secrets removed):

```
PORT=3000
CRON_KEY=<32 hex chars, generated>
POSTGRES_IMAGE=pgvector/pgvector:pg17
POSTGRES_USER=trovato
POSTGRES_PASSWORD=trovato
POSTGRES_DB=trovato
POSTGRES_PORT=5434          # 5432 and 5433 already bound on this host
REDIS_PORT=6381             # 6379 and 6380 already bound
ARGUS_PORT=3003             # 3002 already bound
ARGUS_CRON_INTERVAL=60
ANTHROPIC_API_KEY=<redacted>
OPENAI_API_KEY=<redacted>
```

`docker-compose.override.yml` (gitignored; the repo's own `.gitignore` line 34
anticipates exactly this file, "may contain API keys or local config"):

```yaml
services:
  argus:
    environment:
      ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY:-}"
      OPENAI_API_KEY: "${OPENAI_API_KEY:-}"
```

This override is not a convenience. The tracked `argus` service passes no AI
credential of any kind and declares no `env_file`, while `README.md` instructs
the operator to name one in `api_key_env`. See finding **F2**.

### Site config

`ai_providers`:

```jsonc
[
  { "id": "anthropic", "label": "Anthropic (chat)",
    "protocol": "anthropic", "base_url": "https://api.anthropic.com/v1",
    "api_key_env": "ANTHROPIC_API_KEY",
    "models": [{ "operation": "chat", "model": "claude-sonnet-5" }],
    "rate_limit_rpm": 0, "enabled": true },
  { "id": "openai", "label": "OpenAI (embeddings only)",
    "protocol": "open_ai_compatible", "base_url": "https://api.openai.com/v1",
    "api_key_env": "OPENAI_API_KEY",
    "models": [{ "operation": "embedding", "model": "text-embedding-3-small" }],
    "rate_limit_rpm": 0, "enabled": true }
]
```

`ai_defaults`: `{ "chat": "anthropic", "embedding": "openai" }`

`ai_pricing`: `claude-sonnet-5` 0.002/0.010, `claude-haiku-4-5` 0.001/0.005,
`text-embedding-3-small` 0.00002/0.0, all USD per 1 000 tokens.

Two providers rather than one because **Anthropic serves no embeddings** and the
semantic route needs an OpenAI-compatible `/embeddings` endpoint. Both base URLs
are public and pass `validate_base_url` on their own merits; nothing was done to
soften that check, and no private, loopback or link-local address was used
anywhere in this run.

### Argus site variables

| Variable | Value | Why |
|---|---|---|
| `argus.decide_model` | `claude-haiku-4-5` | highest volume, cheapest model |
| `argus.judge_model` | `claude-haiku-4-5` | low volume but trivially cheap work |
| `argus.analyze_model` | `claude-sonnet-5` | the stage whose output feeds clustering |
| `argus.summarize_model` | `claude-sonnet-5` | reader-facing prose |
| `argus.embed_model` | `text-embedding-3-small` | **selects the semantic route** |
| `argus.daily_limit_usd` | `2.00` | small, deliberately |
| `argus.alert_threshold_usd` | `0.50` | warn well before the cap |
| `argus.notify_threshold` | `60` | 70 was unreachable in a short run |
| `argus.quiet_hours_start` / `_end` | `0` / `0` | equal disables quiet hours; the run began at 04:38 UTC, inside the stock 23–07 window, which would have held every notification |
| `argus.queue_stuck_seconds` | *unset (900)* | left at default as instructed |

Everything else is at its default.

### Route, and the threshold that goes with it

**This run is on the semantic route.** `argus.embed_model` is set, so
`plugins/argus/src/lib.rs:1051-1061` selects
`DEFAULT_SEMANTIC_JOIN_THRESHOLD = 0.82` (`crates/argus-core/src/cluster.rs:54`)
rather than the lexical `0.55` (`:41`), and `StageConfig::vector_recipe()` stores
vectors under a semantic recipe id that lexical vectors are not compared against.

Quoting a threshold without its route says nothing, so: **every threshold in this
report is a semantic-route threshold.** The 0.55 in `README.md`'s clustering
table is the lexical default and does not apply to this configuration.

### Content

One topic, six deliberately overlapping feeds, one ntfy channel, all created
through the documented admin routes (`/admin/content/add/argus_topic` and
siblings) as the superuser, with the caveat in finding **F3**.

Topic: "Datacentre, networking and Postgres", threshold 45, priority normal.
Relevance prompt, in full:

> Datacentre operations, network engineering, Linux systems administration,
> Postgres and other open-source databases, Rust and WebAssembly systems
> programming, and the infrastructure economics behind all of it. Also relevant:
> outages, security advisories and hardware supply that would change how an
> operator runs a fleet. Not relevant: consumer gadgets, phone launches, gaming,
> cryptocurrency prices, social media drama, and company financial news that
> carries no engineering content.

Threshold 45 rather than the README's implied 70: a short run needs survivors to
reach the clustering stage at all, and the point of the exercise is the join
behaviour, not the filter's sharpness.

| Feed | URL | Interval |
|---|---|---|
| The Register | `https://www.theregister.com/headlines.atom` | 300 s |
| Ars Technica | `https://feeds.arstechnica.com/arstechnica/index` | 300 s |
| Hacker News front page | `https://hnrss.org/frontpage` | 300 s |
| Slashdot | `http://rss.slashdot.org/Slashdot/slashdotMain` | 300 s |
| BleepingComputer | `https://www.bleepingcomputer.com/feed/` | 300 s |
| Phoronix | `https://www.phoronix.com/rss.php` | 300 s |

Channel: kind `ntfy`, server blank (so `https://ntfy.sh`), target a random
per-run topic name, minimum priority `normal`, events blank (all). The topic is
readable at `https://ntfy.sh/<topic>/json?poll=1` and answered 200 before the
run began.

---

## 3. What the run actually did

First fetch tick confirmed at 04:45 UTC. As of the end of session 1:

| Measure | Value |
|---|---|
| Articles ingested | 154 |
| Feeds fetched without error | 6 of 6, `failure_count = 0` on all |
| Decided (survived the threshold) | 36 |
| Discarded | 110 |
| Analyzed | 4 |
| Awaiting decide | 0 (queue fully drained) |
| Analyze jobs queued | 34 |
| Embed jobs queued, never yet attempted | 4 |
| Vectors written | 0 |
| Stories | 0 |

Relevance scoring is working, and working well. A sample, verbatim from
`argus_articles`:

| Score | Title |
|---|---|
| 87 | Microsoft agentically ports Copilot runtime to Rust for $120K |
| 85 | Saving another 100TB of RAM |
| 85 | New Check Point flaw lets hackers execute code with root privileges |
| 85 | Cisco drops another exploited zero-day, this time a perfect 10 |
| 85 | Google's "Painful To Maintain" Binder C Linux Driver Being Removed In Favor Of Rust |
| 0 | After being sidelined, Boeing's Starliner to get starring role in NASA's spaceflight plans |
| 0 | A dolphin named Bubbles makes other fish vomit, then eats it |

The stored `relevance_reason` for the discards names the prompt's own exclusions
("Not relevant to datacentre operations…"), so the prose prompt is genuinely
reaching the model and genuinely discriminating. That part of Argus does what
the README says it does.

Analyze reached only four articles, and embed none, because of **F8** — not
because of throughput. The decide queue drained completely within one cycle of
clearing the wedged jobs, and analyze began immediately; it then wedged again
after two jobs. Every stage this run reached works; the pipeline simply cannot
stay running long enough to get through them.

---

## 4. The six verdicts

### 4.1 Does the semantic embed route reach the embeddings endpoint and join on similarity? — **NOT REACHED**

No embed job has run, because no article has reached analyze yet. `ai_usage_log`
contains 132 rows, every one `operation = Chat`; there is no `Embedding` row.
`argus_article_vectors` is empty. There is nothing to report from the running
instance, and a status note is not evidence.

What can be said without the instance, and should be said because it narrows
what session 2 has to check: the kernel-side half of `G-AI-EMBED-UNROUTED` is
demonstrably closed **in this tree**.
`crates/kernel/src/host/ai.rs:185-199` branches on `request.operation` before
the protocol and routes `Embedding` to `build_openai_embedding_request`, which
posts to `/embeddings` and reads `request.input`
(`:323-353`). It also refuses an `Embedding` against an Anthropic-protocol
provider with `ERR_AI_OPERATION_UNSUPPORTED` (`:327-335`) — which is why the
configuration above needs a second provider and cannot route embeddings to
Anthropic.

The open question is entirely the behavioural one: whether a real
`text-embedding-3-small` vector, L2-normalised by
`crates/argus-core/src/pipeline.rs:689-700`, actually joins two differently-worded
reports at cosine ≥ 0.82. Session 2 answers that.

### 4.2 Is `/stories` a story list or a recency list? — **NOT REACHED**

`argus_stories` is empty; the clustering stage has not run. `/stories` returns
200 and renders its empty state.

### 4.3 Does `/articles` still return 500 in its blank-filter default state? — **NOT REPRODUCED**

`GET /articles` with no query string, the page's default state, returns **200**,
authenticated and anonymous alike. `M3-FRICTION.md`'s claim that
`G-EXPOSED-FILTER-NO-MATCH-ALL` closed holds at runtime, and the 500 that log
describes is gone.

The page is not, however, usable, and the reason is a different defect that the
500 was previously masking. Every request logs:

```
gather template render failed template=gather/query--table.html
error: Variable `columns` not found in context while rendering 'gather/query--table.html'
```

`templates/gather/query--table.html:7,15` iterates a `columns` variable, and
nothing in the kernel ever inserts one — `grep -rn '"columns"' crates/kernel/src`
returns only unrelated hits in `host/db.rs` and `page_builder_components.rs`. The
route falls back to dumping every column *name* as a flat list and then every raw
field *value*, including full HTML article bodies, with no table structure at
all; the rendered page is 198 KB of unformatted text.

This is not an Argus problem. `format: table` is used by **22 of the 28** gather
queries in this install, including all six Argus admin screens
(`/admin/argus/feeds`, `topics`, `channels`, `notifications`, plus the pipeline
health and article list) and nine core ones (`core.user_list`,
`core.all_items`, `core.roles`, …). All four Argus admin pages return 200 and all
four log the same failure. Recorded as **F5**.

### 4.4 Does a summarized story above `argus.notify_threshold` reach the ntfy topic? — **NOT REACHED**

No story has been summarized, so nothing has been offered to the notifier. The
channel exists, is published, and points at a public ntfy topic that polls 200.

One notification-path fact *was* established, and it is the one the task asked to
leave enabled: with `argus.queue_stuck_seconds` at its default 900, the pipeline
**did** report its own stall. `argus_notify_events` holds an `alert.queue_stuck`
row raised at 05:00:39 UTC, during the window when the queue was not draining.
The stuck-queue alert works.

### 4.5 What did the day cost per stage? — **ANSWERED, and the answer is a defect**

`argus_cost_daily`, verbatim:

| day | stage | calls | unpriced_calls | cost_usd |
|---|---|---|---|---|
| 2026-09-19 | argus_decide | 150 | **150** | **0** |
| 2026-09-19 | argus_analyze | 4 | 0 | 0.0453 |

Every decide call is unpriced and its recorded spend is zero, despite
`ai_pricing` containing an entry for the model decide was routed to; analyze,
configured identically, is priced correctly. This is finding **F4**, and it means
`argus.daily_limit_usd = 2.00` was enforcing against roughly a quarter of the
actual spend.

The real cost, computed by hand from `ai_usage_log` at the published Haiku 4.5
rate:

| Stage | Model | Calls | Input tok | Output tok | Cost | Metered by Argus? |
|---|---|---|---|---|---|---|
| decide | claude-haiku-4-5 | 150 | 89 426 | 10 441 | $0.142 | **no** — unpriced |
| analyze | claude-sonnet-5 | 4 | 2 351 | 2 444 | $0.045 | yes |
| embed | text-embedding-3-small | 0 | 0 | 0 | $0.000 | — not reached |
| summarize | claude-sonnet-5 | 0 | 0 | 0 | $0.000 | — not reached |
| judge | claude-haiku-4-5 | 0 | 0 | 0 | $0.000 | — not reached |
| **Total** | | **154** | **91 777** | **12 885** | **$0.187** | |

**The run cost 18.7 cents.** Argus's own `argus_cost_daily` reports $0.045 of
that — the analyze rows only — and $0.00 for decide, which was 97 % of the calls
and 76 % of the money.

### 4.6 Did the queue concurrency collapse produce a duplicate story? — **NOT REACHED** (the collapse itself: **CONFIRMED**)

No story exists yet, so the duplicate-story outcome is unreached.

The cause `M2-FRICTION.md` describes is, however, **still present in 0.102.0** and
can be confirmed without waiting. `crates/kernel/src/cron/mod.rs`'s
`plugin_concurrency` still "reads the maximum `concurrency` it declares across
its queues" — its own doc comment, at `:1169-1175` — and clamps to
`QUEUE_CONCURRENCY_CAP = 4` (`:49`). Argus declares
`fetch: 4, decide: 4, analyze: 2, embed: 2, cluster: 1, summarize: 1, notify: 2`
(`plugins/argus/src/lib.rs:484-490`). So the two stages that declare `1` run
4-wide, exactly as the friction log says, and the plugin-owned lease in
`argus_state` is still the only thing standing between that and a split story.

Session 2 reports whether the lease held.

---

## 5. Findings

Numbered so the report can be cited. No code was changed; every one of these is
a finding, not a fix.

### F1 — A fresh compose volume is not a running pipeline, and nothing says so **[High]**

`docker compose --profile argus up -d --build` brings up four healthy
containers, `/health` returns `"status":"healthy"` with every service green, and
the pipeline is completely dead. The site has not been installed, so the install
middleware 303s **every** request to `/install`, including `POST /cron/<key>`:

```
HTTP/1.1 303 See Other
location: /install
```

`argus-cron` runs `curl -fsS`, which treats a 303 as success, so the loop logs
nothing and keeps going. `run_cron` is never entered — its `info!("cron triggered
via HTTP")` does not appear once — so no feed is polled, no maintenance runs, and
no operator alert fires, which is precisely the failure mode the README says
`argus-cron` exists to prevent.

`README.md`'s quickstart goes straight from `docker compose up -d --build` to
"Then, in the admin UI", with no step between. `INSTALL.md:251` does document the
web installer, but for the `cargo run` path, and nothing links the two. Running
`/install/admin` and `/install/site` once fixed it permanently; the next cron
poke returned `{"status":"completed","tasks":[…,"tap_cron:argus",…]}`.

The honest fix is one sentence in the quickstart. The better fix is for
`argus-cron` to notice it is being redirected.

### F2 — The compose file passes no AI credential to the argus container **[Medium]**

`docker-compose.yml`'s `argus` service sets `DATABASE_URL`, `REDIS_URL`,
`DATABASE_MAX_CONNECTIONS`, `RUST_LOG`, `PORT` and `CRON_KEY`, and nothing else.
There is no `env_file`. `README.md` tells the operator to "put the key in the
container's environment (compose `environment:` or an `env_file:`)" but the file
it ships has neither, so following the README's own AI-provider section requires
editing a tracked file or writing an override. `.env.example`'s closing section
explains why credentials are *not* environment for Argus's own settings, which is
correct and unrelated, and reads as though the question were settled.

A two-line `ANTHROPIC_API_KEY: "${ANTHROPIC_API_KEY:-}"` pair in the tracked
service, defaulting to empty, would cost nothing and close it.

### F3 — Plugin content types render no fields in the admin UI **[High]**

`GET /admin/content/add/argus_topic` renders a form containing a title input and
a "Published" checkbox. The type's four declared fields — including
`field_relevance_prompt`, which is `required: true` and is the entire point of a
topic — are absent. Same for `argus_feed` and `argus_notify_channel`. So the
flow `README.md` documents in its "Then, in the admin UI" section cannot be
completed through the UI at all.

Root cause, and it is a plain reader/writer disagreement in the kernel:

- Writer, `crates/kernel/src/content/type_registry.rs:142`:
  `settings: Some(serde_json::to_value(&def.fields)?)` — stores a **bare array**.
- Reader, `:157-162`: `settings.get("fields").and_then(…).unwrap_or_default()` —
  asks for an **object with a `fields` key**, and silently returns an empty
  `Vec` when it does not find one.

In the database both shapes exist side by side:

```
argus_topic |argus         |array   [{"label": "Relevance prompt", …}]
blog        |trovato_blog  |array   [{"label": "Body", …}]
page        |core          |object  {"fields": [{"label": "Body", …}]}
```

`page` is seeded in the object shape by a core migration and works — it renders
its body field. Every type registered through `tap_item_info` is written by
`:142` in the array shape and reads back as zero fields. Verified across
plugins: `page` renders 1 field input, `blog` renders 0, `argus_feed` renders 0.
**This is kernel-wide and hits every plugin-declared content type, not just
Argus.**

It also disables validation, because
`crates/kernel/src/content/compound::validate_required_fields` is handed the same
empty list: a topic with no relevance prompt saves without complaint.

Workaround used for this run, which is what an operator is forced into: the
*submit* path is unaffected, because `extract_content_fields`
(`crates/kernel/src/routes/admin_content.rs:23-33`) copies every non-underscore
form key straight through without consulting the type. POSTing
`field_relevance_prompt=…` to the same documented URL, with the real CSRF token
and the real handler, creates a correct item. The field values below were
verified present in `item.fields` afterwards. The route works; the form does not.

### F4 — `ai_pricing` cannot match an aliased model, so the spend cap protects nothing **[High]**

132 of 132 calls were logged unpriced and `argus_cost_daily.cost_usd` stayed at
`0` while real money was spent, with `claude-haiku-4-5` present in `ai_pricing`.

The lookup key is wrong. `crates/kernel/src/host/ai.rs:881-886` prices the call
from **`ai_response.model`** — the model string parsed out of the provider's
*response*, at `:527` (`json["model"]`). Anthropic resolves an alias server-side
and returns the dated snapshot, so a request for `claude-haiku-4-5` comes back
as `claude-haiku-4-5-20251001`, and that is what both `ai_usage_log.model` and
the price lookup see:

```
           model           | calls | in_tok | out_tok | unpriced
---------------------------+-------+--------+---------+----------
 claude-haiku-4-5-20251001 |   132 |  84818 |    9254 |      132
```

The operator priced the model they configured. The kernel priced a string they
never typed and cannot predict, because which snapshot an alias resolves to is
the provider's choice and changes over time.

**And it fails silently for some models while working for others in the same
configuration**, which is what makes it genuinely dangerous rather than merely
wrong. Once analyze started, `ai_usage_log` held both cases side by side:

```
           model           | calls | unpriced | kernel_cost
---------------------------+-------+----------+-------------
 claude-haiku-4-5-20251001 |   150 |      150 |      0.0000
 claude-sonnet-5           |     4 |        0 |      0.0453
```

Anthropic echoes `claude-sonnet-5` back unchanged, so analyze is priced
correctly and contributes to the cap. It resolves `claude-haiku-4-5` to a dated
snapshot, so decide is invisible. Same site, same `ai_pricing`, same provider,
both entered the same way — and an operator has no way to tell which of their
models is being metered without querying `ai_usage_log` by hand. A partially
enforced spend cap is worse than none, because the dashboard looks like it is
working.

This is worse than a reporting nuisance. `README.md` is explicit that "a model
that is not in `ai_pricing` is charged as *unknown*, not as free… so a low spend
figure can never be misread as a cheap day" — the intent is right, but the
mechanism means the **normal** case of naming a model by its stable alias is
permanently unpriced, `daily_limit_usd` never trips, and `alert_threshold_usd`
never warns. A site that sets a $20 cap and routes analyze to
`claude-sonnet-5` has no cap.

Either key the lookup on the **requested** model, or fall back to it when the
returned string is not priced, or price on both. An operator-side workaround
exists — add the dated snapshot to `ai_pricing` as well — but it requires the
operator to first discover the snapshot name empirically and then chase it every
time the provider re-points the alias.

### F5 — The `table` gather format renders a variable nothing supplies **[High]**

Described in full under verdict 4.3. `templates/gather/query--table.html`
iterates `columns`; no kernel code inserts it. 22 of 28 gather queries in this
install use `format: table`, including every Argus admin screen and nine core
administrative listings. Each affected page returns 200, logs a render failure,
and displays an untabulated dump of column names followed by raw values.

### F6 — Per-queue concurrency is still collapsed **[High, re-confirmed]**

`G-QUEUE-CONCURRENCY-COLLAPSED` from `M2-FRICTION.md` is unfixed in 0.102.0.
Evidence under verdict 4.6. Worth stating because `M2-FRICTION.md`'s status note
lists which of its findings closed, and a reader could reasonably assume the
remaining High one had been picked up since. It has not.

### F7 — A trapping queue job is re-claimed past `max_attempts` instead of dying **[High]**

Four `argus_decide` jobs sat at `attempts = 5, max_attempts = 5`, status
`claimed`, `last_error = "tap_queue_worker failed (trap or error result)"`, and
were still being re-claimed. `claim_batch`
(`crates/kernel/src/cron/mod.rs:1129-1140`) selects
`status = 'ready' AND next_attempt_at <= now` **or**
`status = 'claimed' AND locked_until <= now`, with no `attempts < max_attempts`
term, and unconditionally increments `attempts`. A job whose lease expires is
therefore reclaimed however many times it has already failed, and
`dead_at`/`dead_reason` stay null. Four poison jobs occupied four of the four
available worker slots on each cycle they were picked up.

This is the mechanism that turns **F8** from a transient stall into a permanent
one. Marking those four rows `dead` by hand drained the entire remaining decide
queue in a single cycle and let analyze start — the first real progress in
twenty minutes.

### F8 — The queue workers wedge within a minute of every start, and only a restart recovers **[Critical]**

**This is the headline finding, and it is why four of the six verdicts are not
reached.** Argus cannot run unattended on this build at all.

I first saw this after firing twelve `POST /cron/<key>` requests in a tight loop
and initially wrote it up as self-inflicted. That was wrong. It has now
reproduced **three times**, twice with `argus-cron`'s ordinary 60-second poke as
the only driver and no interference from me. The corrected account:

Within **30 to 60 seconds** of every container start, two to four tokio worker
threads enter state `R` and stay there, burning 100 % of a core each, and the
container never does useful work again. Measured across the three occurrences:

| Restart at | Last useful work | Time to wedge |
|---|---|---|
| 04:57 UTC | 05:01:08 | ~4 min (under my cron burst) |
| 05:05:44 | 05:06:38 | **54 s** |
| 05:14:53 | 05:15:08 | **~15 s** |

While wedged:

- CPU sits at 123–130 %, with the thread table showing the spinners directly:
  `/proc/1/task/*/stat` fields 14+15 reach 25 758 and 16 754 jiffies (257 s and
  167 s of CPU) on threads in state `R`, against single digits for every other
  worker.
- `TTL cron:lock` in Redis stays pinned near 300 and **rises** between samples.
  The renewal task at `crates/kernel/src/cron/mod.rs:773` refreshes the lock every
  `LOCK_TTL_SECS / 2` for as long as the run has not returned — it has no
  liveness condition, so it refreshes because the run is stuck, not because it is
  working.
- Every subsequent poke returns
  `{"status":"skipped","message":"Another instance is running cron"}`.
- Every other queue starves. At the third occurrence: 34 analyze, 16 fetch and 1
  notify job sat `ready` and untouched behind 2 `claimed` analyze jobs.
- The HTTP server stays healthy and responsive throughout, `/health` returns
  green, and **nothing alerts.** `alert.queue_stuck` fires once and does not
  re-fire.

The jobs it wedges on are ordinary. At the third occurrence the two `claimed`
jobs were `analyze` on articles of 6 245 and 3 437 characters — close to the
1 980-character corpus average, nothing near the 7 020-character maximum — and
both were on `attempts = 1`, their **first** attempt. So this is not a retry
loop, not a poison-content problem, and not size-related. It is a general
worker-lifecycle defect that hits whatever happens to be in flight.

Recovery is `docker compose restart argus`, deleting `cron:lock`, and returning
the claimed rows to `ready`. That works every time, and then it wedges again
within a minute.

Two things this interacts with. **F7** is what makes it permanent rather than
self-limiting: a job that spins is re-claimed forever instead of dead-lettering,
so the same four occupied all four worker slots across restarts until I marked
them dead by hand — at which point the decide queue drained completely in one
cycle and analyze started immediately. And **F6**'s collapsed concurrency sets
how many it takes: `QUEUE_CONCURRENCY_CAP = 4` means four spinning jobs is total
starvation.

Because the pipeline cannot run unattended, an external watchdog was added for
the overnight window so that session 2 has data to report at all. It lives
outside the repository and is described in section 9; the restart count it
records is the closest thing to a mean-time-between-failures figure this run can
produce.


### F9 — Argus declares record types that collide with its own content types **[Low]**

Every boot logs twice:

```
skipping invalid lightweight-record declaration
error=record type 'argus_feed' (plugin 'argus'): name collides with content type 'argus_feed'
error=record type 'argus_topic' (plugin 'argus'): name collides with content type 'argus_topic'
```

The kernel drops the record declarations and carries on. Since configuration
moved onto Items the record types appear to be vestigial, in which case the
declarations should go; if anything still expects them, this is louder than Low.

### F10 — `argus_feeds` carries dead configuration columns **[Low]**

`argus_feeds` rows have `name = ''`, `url = ''`, `topic_id = NULL` and
`fetch_interval_seconds = 900` while the corresponding Items hold the real URL,
topic id and 300-second interval, and fetching works correctly from the Item. The
columns are leftovers from before configuration moved onto Items. They are
harmless but actively misleading to anyone reading the table to debug a feed —
the row says the feed has no URL.

---

## 6. The assistant question

**Argus should declare the assistant taps, and the case is stronger for Argus
than for most plugins, but only for part of its configuration.**

The split is between settings that have a right answer an operator can look up
and settings that are calibrated by feel against a corpus nobody has read yet. A
form is fine for the first kind: `argus.notify_debounce_seconds`, quiet hours,
`notify_max_attempts`, a channel's kind and target are all things an operator
either knows or can decide from the README in one reading. A conversation adds
nothing.

The second kind is where the form fails, and this run is the evidence. Three of
the settings I chose were guesses I had no way to ground. `argus.cluster_threshold`
was left at its route default of 0.82 because I had no corpus to calibrate
against — and the README's table does not even mention that setting
`argus.embed_model` re-bases that default from 0.55, so a form-driven operator
who typed 0.55 into the clustering box would be quietly joining nearly everything
to everything. I set the topic threshold to 45 rather than 70 purely because a
short run needs survivors, which is a judgement about my situation, not about the
topic. I set `notify_threshold` to 60 for the same reason. And the relevance
prompt itself — eight lines of prose that decide what 154 articles were worth — got
exactly one draft, with no way to ask what it would have kept or thrown away
before committing it.

Every one of those is a question with a real answer sitting in tables the plugin
already owns. That is what makes this a good assistant scope rather than a
chatbot bolted onto a settings page.

The read tools write themselves from the data this run produced. `score_distribution`
returns the histogram of `relevance_score` for a topic, so "your threshold of 45
is keeping 35 of 154 articles; at 70 you would keep 6" is a fact rather than a
feeling. `sample_decisions` returns the top and bottom scored articles with their
stored `relevance_reason` — the same rows quoted in section 3, which are exactly
what a person needs to see to know whether the prompt is discriminating the way
they meant. `join_candidates` returns the pairwise cosine distribution over
recent articles under the current recipe, with the near-misses either side of the
threshold named, which turns "0.82 or 0.78?" into a question about seven specific
article pairs a person can read. `spend_projection` reads `argus_cost_daily` and
`ai_usage_log` and projects the daily bill at the current volume and routing —
and, incidentally, would have surfaced F4 in the first conversation, because a
projection of $0.00 against 132 real calls is the kind of thing a person notices
when it is put in a sentence and does not notice in a table cell. `channel_status`
reads `argus_notify_deliveries` and says what each channel actually received,
including the `blocked` rows the SSRF fence produces, which is the single most
confusing thing a new operator meets.

The write proposal has to be one thing, and `propose_relevance_prompt` is the one
worth building, because it is the setting with the highest consequence and the
least feedback. End to end, on the kernel's existing contract: the person says
"it is letting through too much AI industry news that has no engineering in it".
The model calls `sample_decisions` and reads back the surviving articles, spots
the three that match the complaint, and calls `propose_relevance_prompt` with a
revised prose block. Because it is declared `AssistantToolKind::Write`, the
kernel does not execute it — it dispatches `tap_assistant_tool` with
`mode: Describe`, and Argus returns an `AssistantToolResult` whose `summary`
is the proposal card the person reads: "adds an exclusion for AI product and
funding announcements without technical content; re-scoring the last 154 articles
under this prompt would have discarded 4 more and kept everything currently above
80." That last clause is the part a form can never offer, and Argus can compute
it cheaply because it still holds the article text. The person reads the card,
reads the diff, and applies it; only then does the kernel dispatch the same call
with `mode: Execute`, and Argus writes the new prompt onto the topic Item through
its normal save path, where `tap_item_presave` clamps and notes it exactly as it
would for a form submission. Nothing about the write bypasses the validation the
form path already has, and the person has seen the consequence before agreeing to
it.

The scope should be `AssistantIdKind::Item` over `item_types: ["argus_topic"]`,
so the launcher appears on the topic that the conversation is about and the
`scope_id` is the thing being tuned. Its permission should be the same one that
governs editing a topic, so the assistant grants nothing the person could not do
by hand.

Two honest caveats. First, none of this helps until **F3** is fixed: an assistant
that proposes a relevance prompt for a form that cannot display one is the wrong
order of work. Second, the scope is only worth its cost on a site with a corpus —
on day one every read tool returns "no data yet", and the stock prompt plus the
README is genuinely the better experience. The assistant earns its place at the
second calibration, not the first.

---

## 7. Stale text for A2

Listed by file, with what is wrong and what it should say. No edits were made.

### `INSTALL.md`

1. **Line 226** — "`argus` — Drupal 6 site monitoring". Argus is a news-intelligence
   pipeline over RSS/Atom feeds; nothing about it touches Drupal. The line also
   sits under "Specialized Plugins (separate install)", described as living in
   `plugins-disabled/`, while Argus ships in `plugins/` and is built by the
   `Dockerfile`'s plugin list. Two wrong claims in one line.

### `plugins/argus/README.md`

2. **Clustering table** — `argus.article_retention_days` is documented as
   defaulting to `90`. The code default is `180`
   (`crates/argus-core/src/pipeline.rs:329`).
3. **Clustering table** — `argus.max_cluster_waits` is documented as defaulting
   to `3`. The code default is `1` (`ClusterConfig::default`,
   `crates/argus-core/src/cluster.rs:99-107`).
4. **Clustering table** — there is **no `argus.embed_model` row anywhere in the
   file**, though it is the switch that selects the semantic route, changes the
   stored vector recipe, and silently re-bases `argus.cluster_threshold`'s
   default from 0.55 to 0.82 (`plugins/argus/src/lib.rs:1051-1061`). It is
   mentioned only in passing in `M2-FRICTION.md`'s status note. This is the most
   consequential omission in the file.
5. **"`cluster_threshold` is calibrated against lexical vectors and is
   provisional"** — still true, now incomplete: the sentence should say the
   default is route-dependent and that 0.55 and 0.82 are not comparable numbers.
6. **"Ollama specifically"** — "Ollama has no embeddings routing through this
   path and needs no key" was written before 0.99.0 routed `operation:
   Embedding`. Embeddings now route, and Ollama's OpenAI-compatible surface does
   serve `/v1/embeddings`. Needs re-checking rather than deleting.
7. **Quickstart step 2** — "Paste the topic's item id into the topic field (a
   plain uuid, not a reference widget — see `M3-FRICTION.md`,
   G-ITEM-FORM-MISMATCH)" contradicts `M3-FRICTION.md`'s own status note, which
   says that finding closed and "the feed's topic is a `RecordReference` again".
   One of the two is wrong; today neither is observable, because of **F3**.
8. **Quickstart** — goes from `docker compose --profile argus up -d --build`
   directly to "Then, in the admin UI" with no mention that a fresh volume must
   complete the web installer first. See **F1**; this omission silently disables
   the whole pipeline.
9. **Not stale, and worth keeping** — "about four minutes on an Apple silicon
   laptop" is accurate; measured 244 s. Likewise "about half a minute" for
   `up -d` on an existing image; measured ~25 s.

### `plugins/argus/M2-FRICTION.md` and `M3-FRICTION.md`

10. **Both status notes** — "The rest are carried on the road to 1.0; see
    `KNOWN-ISSUES.md`." `KNOWN-ISSUES.md` contains **zero** `G-` findings
    (`grep -c "G-"` returns 0). They are in `docs/BACKLOG.md`, which has 83
    matches. The pointer sends a reader to the wrong file.

### `plugins/argus/M3-FRICTION.md`

11. **G-EXPOSED-FILTER-NO-MATCH-ALL body** — still reads "**It returns a 500.**"
    in the present tense. It returns 200 (verdict 4.3). The status note at the
    top says the finding closed, but a reader who jumps to the finding gets the
    opposite impression. Every closed finding's body wants the same past-tense
    pass.

### `plugins/argus/M4-FRICTION.md`

12. **No status note at all.** M2 and M3 both received one on 2026-08-13; M4
    never did, so a reader cannot tell whether any of its five findings survived
    0.99.0 and 0.102.0. At least one finding from M2 demonstrably did survive
    (**F6**), so the absence is not safe to read as "all fixed".

---

## 8. Recommended threshold for the semantic route

**Deferred to session 2, deliberately.** The whole point of the question is that
a threshold quoted without a corpus behind it is the same guess the default
already encodes, and no story has been clustered yet. Session 2 reads the stories
and the `join_candidates` distribution and recommends a number against them.

The default in force for this run, and the number session 2 argues from, is
`DEFAULT_SEMANTIC_JOIN_THRESHOLD = 0.82`.

---

## 9. State left running

- Compose project `trovato-argus`, four containers up, site on
  `http://localhost:3003`. `argus-cron` poking every 60 s.
- 154 articles ingested, 36 decided, 4 analyzed, 34 analyze and 4 embed jobs
  queued. Four decide jobs marked `dead` by hand (see **F7**).
- Spend to date **$0.187**, of which Argus metered $0.045 (**F4**). The
  `argus.daily_limit_usd = 2.00` cap is set but only partially enforceable, so
  session 2 should trust the hand-computed figure from `ai_usage_log`, not
  `argus_cost_daily`.

### A watchdog is running, and it is not part of Trovato

Because of **F8** the stack cannot make progress unattended, so an external
script polls every 30 s and restarts the argus container when it detects the
wedge — no AI call in 200 s while CPU is above 50 % and work is still queued —
then clears `cron:lock` and returns expired claims to `ready`, dead-lettering
anything already past `max_attempts`.

It lives at
`<session scratchpad>/argus-watchdog.sh`, **outside the repository**, is not
committed, and touches nothing but this test stack. It logs every restart to
`watchdog.log` beside itself.

Session 2 should:

1. Read `watchdog.log` first. The restart count over the elapsed window is the
   real mean-time-between-failures figure for **F8** and belongs in this report.
2. Collect verdicts 1, 2, 4 and 6 from whatever the pipeline managed between
   restarts.
3. Stop it when finished: `pkill -f argus-watchdog.sh`.

Its restarts are disclosed here because they are not a neutral test condition:
every restart re-runs `plugin install`, and a story that spans a restart has
been clustered across two process lifetimes. That is worth stating before
drawing conclusions about clustering from this corpus.
