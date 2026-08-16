# Argus

Argus is a news-intelligence pipeline that runs inside Trovato as a WASM plugin.
It polls RSS and Atom feeds, scores every article's relevance against a topic you
describe in prose, and puts the survivors through analysis, entity extraction,
clustering and synthesis so that several reports of one event become **one
story** rather than five headlines. Readers browse those stories on the site;
operators get told about the ones that matter over ntfy, Slack or a webhook.

Everything Argus does happens inside the plugin boundary: no daemon, no
sidecar, no kernel patch.

---

## Quickstart

```bash
cp .env.example .env          # edit CRON_KEY at minimum
docker compose --profile argus up -d --build
```

That brings up four containers:

| Container | What it is |
|---|---|
| `postgres` | Postgres 17 with pgvector available |
| `redis` | queue and cache backing |
| `argus` | Trovato with the argus plugin installed and enabled |
| `argus-cron` | the pipeline's clock (see below) |

The site is on `http://localhost:3002` (change with `ARGUS_PORT`). The first
build compiles the kernel and every WASM plugin from source — about four minutes
on an Apple silicon laptop. Once the image exists, `up -d` to a healthy,
migrated, plugin-enabled server takes about half a minute.

**Upgrading an existing compose volume:** the Postgres image now defaults to
`pgvector/pgvector:pg17`, which is built on a different base than the plain
`postgres:17` it replaces. On a volume created by the old image Postgres will
warn about a collation version mismatch. Repair it once —

```bash
docker compose exec postgres psql -U trovato -d trovato \
  -c 'REINDEX DATABASE trovato' \
  -c 'ALTER DATABASE trovato REFRESH COLLATION VERSION'
```

— or set `POSTGRES_IMAGE=postgres:17` in `.env` and forgo pgvector on that
volume. A fresh volume needs neither.

**`argus-cron` is not optional.** The Trovato kernel runs no internal scheduler:
`tap_cron` fires only when something POSTs to `/cron/<CRON_KEY>`. That container
is a `curl` loop doing exactly that. Without it Argus never polls a feed, never
runs maintenance, and never raises an alert — the site comes up and then sits
there.

### Then, in the admin UI

1. **Create a topic.** `/admin/content/add/argus_topic`. The relevance prompt is
   prose: "Datacentre operations, network engineering and Postgres. Not consumer
   gadgets." Every article is scored 0–100 against it and kept at or above the
   threshold.
2. **Create a feed.** `/admin/content/add/argus_feed`. Paste the topic's item id
   into the topic field (a plain uuid, not a reference widget — see
   `M3-FRICTION.md`, G-ITEM-FORM-MISMATCH).
3. **Create a notification channel.** `/admin/content/add/argus_notify_channel`.
   Until you do, Argus notifies nobody — which is the only safe default for
   something that sends messages.
4. **Configure an AI provider** (below). Without one, articles are ingested and
   deduplicated but never scored: the decide jobs retry and eventually
   dead-letter.

Everything Argus has is listed at `/admin/argus/feeds`, `/admin/argus/topics`,
`/admin/argus/channels` and `/admin/argus/notifications`. Readers get `/stories`,
`/stories/topic?topic=<id>` and `/stories/archive`.

---

## Pointing Argus at a model

Argus never holds an API key. It asks the kernel for a *chat completion at this
model name* and the kernel decides which provider serves it, so a provider is
configured once, site-wide, and every plugin uses it.

Two protocols are supported: `open_ai_compatible` and `anthropic`. The first
covers OpenAI itself, Azure OpenAI, Ollama, vLLM, LM Studio, together.ai and
anything else speaking `/chat/completions`.

Providers live in the `ai_providers` site config key, defaults in `ai_defaults`,
and per-model prices in `ai_pricing`. Configure them through the admin UI; the
shapes are:

```jsonc
// ai_providers
[
  {
    "id": "ollama",
    "label": "Local Ollama",
    "protocol": "open_ai_compatible",
    "base_url": "http://ollama.example:11434/v1",
    "api_key_env": "",                 // the NAME of an env var, never the key
    "models": [{ "operation": "chat", "model": "llama3.1:8b" }],
    "rate_limit_rpm": 0,
    "enabled": true
  },
  {
    "id": "anthropic",
    "label": "Anthropic",
    "protocol": "anthropic",
    "base_url": "https://api.anthropic.com/v1",
    "api_key_env": "ANTHROPIC_API_KEY",
    "models": [{ "operation": "chat", "model": "claude-sonnet-4-5" }],
    "rate_limit_rpm": 0,
    "enabled": true
  }
]

// ai_defaults — which provider serves each operation
{ "chat": "anthropic" }

// ai_pricing — what a call costs, so the budget means something
{ "models": { "claude-sonnet-4-5": { "input_per_1k": 0.003, "output_per_1k": 0.015 } } }
```

`api_key_env` names an **environment variable**, not the key. Put the key in the
container's environment (compose `environment:` or an `env_file:`) and name the
variable here.

**A model that is not in `ai_pricing` is charged as *unknown*, not as free.**
Argus tracks unpriced calls separately from dollars so a low spend figure can
never be misread as a cheap day when it means an unpriced model.

### Routing stages to different models

Argus runs four kinds of AI call, and they are not the same class of work:

| Stage | Runs on | Volume | Site variable |
|---|---|---|---|
| decide | every ingested article | highest | `argus.decide_model` |
| analyze | every survivor | medium | `argus.analyze_model` |
| summarize | every story, rate-limited | low | `argus.summarize_model` |
| notify judge | every story update | low | `argus.judge_model` |

Set them to model names your configured provider serves. Decide and the judge
belong on the cheapest model you have; analyze and summarize on the strongest.
Leaving one unset uses the provider default, and `summarize_model` falls back to
`analyze_model`, `judge_model` to `decide_model`.

### Ollama specifically

`base_url` must include `/v1`. Ollama has no embeddings routing through this
path and needs no key, so leave `api_key_env` empty. A local Ollama on the
docker host is reachable at `http://host.docker.internal:11434/v1` from inside
the compose network on Docker Desktop.

---

## Configuration reference

Argus's own tuning is **site variables**, not environment: an operator changes
them at runtime without a redeploy. The variables host namespaces them, so the
`site_config` row key for `argus.notify_threshold` is
`plugin.argus.argus.notify_threshold`.

Every one has a working default. An operator who sets none of them gets a
running pipeline with no spending limit, quiet overnight notifications, and only
stories that scored 70 or better.

### Models

| Variable | Default | Meaning |
|---|---|---|
| `argus.decide_model` | provider default | model for relevance scoring |
| `argus.analyze_model` | provider default | model for deep analysis |
| `argus.summarize_model` | `analyze_model` | model for story synthesis |
| `argus.judge_model` | `decide_model` | model for the change judge |

### Spend

| Variable | Default | Meaning |
|---|---|---|
| `argus.daily_limit_usd` | `0` (no limit) | analyze, summarize and the judge pause for the rest of the UTC day past this |
| `argus.alert_threshold_usd` | `0` (off) | warn (and notify) past this, without pausing |

Decide is **counted** but not **gated**: it runs on every ingested article and is
the one thing keeping volume down, so pausing it would raise the eventual bill
rather than lower it. See `M2-FRICTION.md` deviation 6.

### Clustering and retention

| Variable | Default | Meaning |
|---|---|---|
| `argus.vector_dim` | `256` | feature-vector dimension |
| `argus.cluster_threshold` | `0.55` | cosine similarity at which an article joins a story |
| `argus.near_dup_threshold` | `0.98` | similarity at which it is the same report |
| `argus.cluster_window_seconds` | `1209600` | how far back a story can reach for members |
| `argus.story_inactive_seconds` | `604800` | idle time after which a story is retired |
| `argus.max_cluster_waits` | `3` | deferrals before an article is forced to found a story |
| `argus.entity_match_threshold` | `0.85` | fuzzy-match bound for entity resolution |
| `argus.summarize_min_interval` | `600` | minimum seconds between two syntheses of one story |
| `argus.article_retention_days` | `90` | age at which a terminal article's body text is reclaimed |

`cluster_threshold` is calibrated against lexical vectors and is **provisional** —
see `M2-FRICTION.md`, "Clustering quality, observed".

### Notifications

| Variable | Default | Meaning |
|---|---|---|
| `argus.notify_threshold` | `70` | relevance score at or above which a story notifies |
| `argus.notify_debounce_seconds` | `3600` | minimum gap between two notifications about one story |
| `argus.digest_threshold` | `5` | due story events in the window that collapse into one digest (`0` or `1` disables) |
| `argus.digest_window_seconds` | `900` | the window the digest counts within |
| `argus.quiet_hours_start` | `23` | first hour of the quiet window |
| `argus.quiet_hours_end` | `7` | hour the quiet window ends (equal to start = no quiet hours) |
| `argus.quiet_hours_utc_offset_minutes` | `0` | minutes to add to UTC to get your local time |
| `argus.quiet_hours_alerts` | `off` | whether operator alerts are silenced overnight too |
| `argus.notify_judge` | `on` | whether a story update costs one AI call to judge |
| `argus.notify_change_ratio` | `0.35` | token-distance threshold used when the judge is off |
| `argus.notify_retry_base_seconds` | `60` | first retry delay for a failed channel |
| `argus.notify_max_attempts` | `5` | attempts per channel before a delivery is abandoned |

**Quiet hours have no timezone.** A WASM plugin has no clock and no tz database,
so the window is expressed in hours plus an explicit UTC offset. In a
DST-observing zone you move the offset twice a year or accept an hour of drift on
when the window opens. `M4-DESIGN.md` Decision 5 argues why that beats a fiction.

### Operator alerts

| Variable | Default | Meaning |
|---|---|---|
| `argus.alerts_enabled` | `on` | whether the alert pass runs at all |
| `argus.feed_failure_threshold` | `3` | consecutive fetch failures before a feed is alerted on |
| `argus.queue_stuck_seconds` | `900` | how long an eligible job may wait before the queue is called stuck (`0` disables) |

The stuck-queue alert is the one that catches a missing `argus-cron`, so leaving
it on is worth more than it costs.

---

## Notification channels

A channel is a content item (`argus_notify_channel`). Unpublish it to pause it.

| Field | ntfy | Slack | webhook |
|---|---|---|---|
| Kind | `ntfy` | `slack` | `webhook` |
| Target | the topic name, e.g. `argus-news` | the incoming-webhook URL | the URL to POST to |
| Server | your ntfy server, blank for `https://ntfy.sh` | — | — |
| Headers | — | — | JSON object, e.g. `{"Authorization":"Bearer …"}` |
| Minimum priority | `normal` or `high` — a phone can take only the loud ones | | |
| Events | comma-separated filter; blank means all | | |
| ntfy priority | `min`/`low`/`default`/`high`/`urgent`, overriding the default mapping | | |

Event names: `story.new`, `story.updated`, `story.digest`,
`alert.feed_failing`, `alert.budget_threshold`, `alert.queue_stuck`. A channel
subscribed to `story.new` also receives the digest that stands in for five of
them.

The generic webhook payload is versioned and stable:

```json
{
  "source": "argus",
  "version": 1,
  "event": "story.new",
  "timestamp": 1767225600,
  "priority": "normal",
  "subject_id": "0199…",
  "title": "Chip maker posts record quarter",
  "body": "Reuters reported record revenue…",
  "link": null,
  "data": { "story_id": "0199…", "article_count": 3, "relevance_score": 85 }
}
```

Branch on `event`; `version` is how you know the shape has not moved.

### What gets notified

A story notifies when it is **first summarized** and again when a later
synthesis **materially changes** it. It qualifies if its topic is high priority,
or if its relevance score reaches `argus.notify_threshold`. Most stories reach
neither, which is why a busy pipeline is not a busy phone.

Then, in order: a high-priority story bypasses everything below; a story about a
subject notified inside the debounce window is dropped; a story inside quiet
hours is held until the window ends; and five or more due stories inside the
digest window collapse into one message.

### Why a channel says `blocked`

The kernel refuses outbound requests to private, loopback, link-local and
cloud-metadata addresses. A channel pointed at `http://localhost:…`,
`10.x`, `192.168.x` or `172.16–31.x` will record `blocked` on every delivery,
with the reason on the row. That is the SSRF fence doing its job, not a bug —
notification targets must be publicly routable.

`/admin/argus/notifications` lists every decision and why it was or was not
sent. The delivery rows behind it carry the exact payload each channel was
handed, which is usually the fastest way to see what a receiver actually got.

---

## Backups

`pg_dump` covers everything. Argus keeps no state outside Postgres: no files, no
external index, no cache that cannot be rebuilt.

```bash
docker compose exec -T postgres pg_dump -U trovato -Fc trovato > argus-$(date +%Y%m%d).dump
```

Restore into an empty database:

```bash
docker compose exec -T postgres dropdb -U trovato --if-exists trovato
docker compose exec -T postgres createdb -U trovato trovato
docker compose exec -T postgres pg_restore -U trovato -d trovato < argus-YYYYMMDD.dump
```

Article body text older than `argus.article_retention_days` is reclaimed
automatically, so a long-running install's dump grows with stories and entities
rather than with every article ever fetched.

---

## How the iOS app will relate to this

The reader surface Argus ships is server-rendered: stories are Trovato items, so
they get a page, full-text search, comments and gathers for free. A native iOS
app would consume the same stories over the site's JSON item routes and receive
push through a fourth notification channel kind (APNS) alongside ntfy, Slack and
webhook — the notifier layer is already shaped for it, carrying a title, a body,
a priority and a deep link per notification. Two things have to exist first: a
plugin-owned HTTP surface, so a reader can write reactions and subscriptions
rather than only read them (`M3-FRICTION.md`, G-NO-PLUGIN-HTTP), and an APNS
credential story. Until then ntfy is the honest answer for phone notifications,
and it is a good one.

---

## Where the reasoning lives

| File | What it argues |
|---|---|
| `M2-DESIGN.md`, `M2-FRICTION.md` | the intelligence stages, and what the kernel made hard |
| `M3-DESIGN.md`, `M3-FRICTION.md` | the reader and admin surface; why configuration is items |
| `M4-DESIGN.md`, `M4-FRICTION.md` | notifications and deployment |

Every friction log is severity-tagged with `file:line` evidence. If something in
Argus looks like a strange choice, the reason is almost always in one of them.
