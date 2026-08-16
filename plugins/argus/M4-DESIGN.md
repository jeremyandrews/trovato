# Argus Milestone 4 — Design Gate

Notifications and deployment, built as **pure plugin** against the frozen PF-5
contract. There is no `argusd`: outbound notification is plugin work over the
kernel `http` host, and "deployment" means packaging Trovato-with-the-argus-plugin
as something an operator can run.

Every decision below is forced by something — a kernel limitation recorded in
M1/M2/M3, a WASM constraint, or a property the milestone needs (idempotency,
observability, bounded spend). Where a decision is a workaround rather than a
design, it says so.

**No kernel, WIT, SDK or kernel-migration change.** Verified at
`KERNEL_API_VERSION (0,99)`.

---

## Decision 1 — A notification channel is an Item

M3 established the rule the hard way: **there is no surface in Trovato 1.0
through which an authenticated user writes a plugin-owned table**
(G-NO-PLUGIN-HTTP), and the record-type admin is read-only. Anything an
administrator must *edit* has to be a kernel Item, because the generic content
forms are the only writable surface a plugin gets.

So `argus_notify_channel` joins `argus_feed` and `argus_topic` as a
configuration Item:

| Field | Type | Meaning |
|---|---|---|
| `field_kind` | Text | `ntfy`, `slack` or `webhook` |
| `field_target` | Text | ntfy topic name, or the full webhook/Slack URL |
| `field_server` | Text | ntfy server base URL; blank means `https://ntfy.sh` |
| `field_headers` | TextLong | JSON object of extra request headers (webhook only) |
| `field_min_priority` | Text | `normal` or `high`; a channel can take only the loud ones |
| `field_events` | Text | comma-separated event filter; blank means every event |
| `field_config_note` | Text | what the last presave coercion changed |

An unpublished channel is a disabled channel — the same "unpublish to pause it"
idiom the feed type already uses, and the only pause mechanism available given
presave cannot refuse a save.

**The alternative considered and rejected:** one JSON blob in a site variable
(`argus.notify_channels`). Variables carry every other Argus tunable, and they
would have avoided a fourth Item type. But a channel set is a *collection* with
per-row validation, and a JSON blob in a text box has no validation, no list
view, no per-channel error surface and no way to disable one channel without
editing the others. The Item tier gives all four for free, and M3 already paid
the integration cost.

**Global** notification settings — quiet hours, debounce window, digest
threshold, relevance floor, whether the judge runs — stay site variables,
because they are single scalars and that is what `stage_config()` is for.

## Decision 2 — Notification is a pipeline stage with an outbox, not a side effect

The naive shape is "when a story is summarized, POST to the channels". It is
wrong here for four separate reasons, each of which is a hard constraint rather
than a preference:

1. **At-least-once delivery.** Queue v2 re-delivers a job whose worker died
   mid-flight (D-47). A summarize job that also sent notifications would send
   them again on every replay. The outbox row's `(event_type, dedup_key)`
   unique constraint is what makes the *decision to notify* idempotent, and the
   per-channel delivery row is what makes the *send* idempotent.
2. **One AI call per job.** The 150 s background epoch makes the
   one-call-per-job rule (held since M1) load-bearing. The story-update trigger
   needs a judge call; summarize already spends one. Two calls in one job is the
   shape the epoch punishes.
3. **Rate limiting needs a moment of decision.** Debounce, quiet hours and
   digest collapse are all "at send time, given everything else pending" — they
   cannot be evaluated at trigger time, because the fifth story that makes a
   digest has not arrived yet.
4. **Observability.** An operator asking "did the 3 a.m. spike notify anyone,
   and what did it say" needs rows to look at. `argus_notify_events` and
   `argus_notify_deliveries` are those rows.

So: `Stage::Notify` is added to the stage enum with its own `argus_notify`
queue, the pipeline writes an event to the outbox and enqueues, and the worker
judges, rate-limits, dispatches and records.

Adding a `Stage` variant is additive to the plugin's own enum and touches no
kernel contract; the queue name is plugin-chosen.

## Decision 3 — `JobPayload` gains an optional channel scope

Per-channel retry and per-channel overflow both need a job that means "this
event, this channel only". `JobPayload` gains
`channel: Option<String>`, `#[serde(default)]`, so every payload already in a
live queue deserializes unchanged. `JobPayload::new` keeps the two-field
construction ergonomic at the ~10 existing call sites.

## Decision 4 — Retry with backoff is a delayed re-enqueue, not a sleep

The scope asks for "Slack incoming webhook (retry + backoff)". A WASM plugin
has **no clock and no sleep**: `std::thread::sleep` is not available, the epoch
deadline kills a busy-wait, and there is no timer host function. In-process
retry with backoff is therefore not expressible.

What *is* expressible is the queue: a failed channel re-enqueues itself as a
channel-scoped `Stage::Notify` job with
`delay = base * 2^attempt`, capped, and the delivery row carries the attempt
count. The observable behaviour is the same (a transient Slack 5xx is retried
with growing gaps and eventually gives up); the mechanism is the queue rather
than the process. Recorded as a deviation, not hidden.

This is the same substitution M2 made three times over (budget pause, summarize
rate limit, cluster-lease retry) and it keeps working for the same reason:
queue-v2 `delay` is the plugin's only timer.

## Decision 5 — Quiet hours are a configured UTC offset, because wasm has no timezone

"Quiet hours 23:00–07:00 **local**" needs a local. A wasm plugin has no clock,
no `TZ`, and no tz database; `host_now()` is a Postgres `EXTRACT(EPOCH FROM
NOW())`, i.e. UTC seconds. Shipping a tz database inside the plugin to resolve
one offset would be absurd.

So quiet hours are `argus.quiet_hours_start` / `argus.quiet_hours_end` (hours
`0..=23`) interpreted in a site-configured
`argus.quiet_hours_utc_offset_minutes`. The window wraps midnight by
construction (`start > end` means "evening through morning"), which is the
default case and therefore the one the tests hammer.

An operator in a DST-observing zone must move the offset twice a year, or accept
an hour of drift on when the quiet window starts. That is honest and documented;
the alternative is a fiction about a timezone the plugin cannot know.

**Operator alerts bypass quiet hours by default** (`argus.quiet_hours_alerts`,
default false = do not silence alerts). A pipeline that has stopped ingesting is
exactly the thing worth waking someone for.

## Decision 6 — Digest is a collapse at send time, not a scheduler

"5+ qualifying stories in a window collapse to one digest." Implemented as: when
a story event comes due, count the *pending, due, same-priority* story events in
`argus.digest_window_seconds`. Past `argus.digest_threshold` (default 5), the
oldest due event is rewritten as a digest carrying all of them and the rest are
marked `digested` in one statement. Under the threshold, each sends on its own.

Why at send time: there is no scheduler to run a window job on, and a
window-timer would need a clock the plugin does not have. Why the *oldest*
becomes the digest: it is already due, so the digest fires immediately rather
than waiting for a sixth story that may never come.

The collapse is a single `UPDATE ... WHERE state = 'pending' AND ...` returning
the affected ids, so a concurrent worker cannot fold the same event into two
digests — the same "make it one idempotent statement" discipline G-DB-NO-TX has
forced on Argus since M2.

## Decision 7 — Priority override is a bypass, not a queue jump

`high` priority (a story on a high-priority topic, or an operator alert) skips
debounce, skips quiet hours and skips digest collapse. It does **not** get a
higher queue priority than everything else in the plugin, because queue priority
is per-plugin ordering and a notification that jumps ahead of the fetch jobs
producing the next notification is a false economy. `NOTIFY_PRIORITY` sits
between cluster and analyze.

## Decision 8 — The story-update judge is one cheap call, budget-counted, and skippable

The scope asks for "a cheap AI judge call" on whether a re-summarized story
materially changed. It is a real call, so:

- It is **counted against the daily budget** exactly like decide, analyze and
  summarize, under `Stage::Notify`. M2's fence stands: notification spend is
  spend, and the budget is not weakened to make notifications cheaper.
- It is **gated** by the budget, unlike decide. A paused judge defers to the
  next UTC day like analyze and summarize do — a missed update notification is
  a much smaller loss than a stopped pipeline, so this one is safe to gate.
- It is **skippable** (`argus.notify_judge`, default `on`). With the judge off,
  a re-summarize notifies when the summary's text distance exceeds
  `argus.notify_change_ratio` — a cheap deterministic fallback so that turning
  the judge off degrades quality rather than removing the feature.
- The **new-story** trigger makes no judge call at all. A story that did not
  exist before is unambiguously new.

## Decision 9 — The queue-stuck alert reads `plugin_queue` through raw SQL, and that is a finding

The scope asks for a "queue stuck (oldest unclaimed job older than X)" alert and
says to read what queue-v2 observability actually exposes. What it exposes is:
a database table (`plugin_queue`, with `status`, `next_attempt_at`, `attempts`,
`dead_at`), an admin HTTP surface (`GET /admin/queue/dlq`), and **no host
function**. There is no `queue-stats`, no `queue-depth`, nothing on the plugin
side of the boundary.

Argus reads the table directly with `query-raw`. This works only because Argus
declares `raw_sql = true`, which the kernel documents as "the auditable escape
hatch" that "weakens the table guarantee for that plugin" — raw statements are
**gated, not parsed against the allowlist**
(`crates/kernel/src/plugin/db_policy.rs`). So a plugin that needs to know
whether its own queue is healthy must reach outside its own tables to find out,
using a capability granted for a different purpose. Filed as
G-QUEUE-NO-INTROSPECTION.

The read is deliberately narrow — one aggregate over
`plugin_name = 'argus'`, no writes, no other kernel table — and it is the only
place in Argus that names a table it does not own.

## Decision 10 — The end-to-end test cannot deliver over loopback, so it asserts the payload and the block

G-SSRF-LOCAL, accepted at CLOSE 05: the p11i fence blocks loopback **and** every
RFC-1918 range, at the URL-string layer and again at the resolver layer. A
fixture webhook receiver on the test machine is unreachable from the `http` host
by construction, and no env-gated allowance exists. (M2's fixture *AI* provider
reaches loopback only because `ai-request` never re-validates the provider base
URL — G-AI-BASEURL-UNCHECKED, a gap M2 disclosed and depends on. That gap is on
the AI path and does not help here.)

So the end-to-end test is built the way M1 and M2 built theirs:

- **Real, in the kernel test:** the whole chain from decided articles through
  analyze → embed → cluster → summarize → the notification trigger → the outbox
  → the dispatcher → a per-channel delivery row. The **exact rendered payload is
  persisted on the delivery row before the send is attempted**, so the golden
  assertion is made against what the real pipeline really produced, not against
  a hand-built fixture. The delivery outcome asserted is the clean per-channel
  `blocked` state, which is itself a scope requirement.
- **Real, in `argus-core`:** dispatch, formatting, isolation, debounce, digest,
  quiet-hours boundaries and priority override against an in-memory transport
  that records what it was handed. This is where "a notification fires" is
  proven end to end as behaviour.
- **Manual, `#[ignore]`d:** a live variant that dispatches at a real external
  receiver when `ARGUS_E2E_WEBHOOK_URL` is set, for a human to run.

Nothing is asserted that was not observed. The friction log states plainly which
half of delivery is proven by the integration test and which by the core test.

## Decision 11 — Compose reuses the repo's existing Docker plumbing

The repo already has a production `Dockerfile` (multi-stage, builds the kernel
and every WASM plugin including `argus`, assembles `plugins/<name>/` with
manifest and migrations) and a root `docker-compose.yml` with `full` and `dev`
profiles. There is no parallel stack to invent.

M4 adds an `argus` profile to the same file: the same image, with the argus
plugin enabled, the AI provider variables wired, and an `argus-cron` companion
that pokes the cron endpoint on an interval — because `tap_cron` fires only when
something calls the cron route, which is the "cron cadence is external-only"
residual from M1.

The shared `postgres` service moves from `postgres:17` to
`pgvector/pgvector:pg17`: the same Postgres 17, same data directory layout, with
the `vector` extension *available*. The kernel's embedding migration is
conditional on it (`20260402000001_create_item_embeddings.sql`), so this is what
turns kernel semantic search on for a fresh install. Argus itself still uses
lexical vectors in `TEXT` (M2 Decision 1, G-AI-EMBED-UNROUTED); nothing in Argus
depends on the extension. CI is unaffected — it runs `postgres:16` service
containers, not compose.

## What is explicitly out of scope

- **APNS and the iOS app.** The notifier trait has the shape a push channel
  needs (a target, a priority, a title/body/deeplink), and nothing more is built
  until there is an app to send to.
- **Weakening the budget.** No notification path may spend outside the accounting.
- **Kernel changes.** Post-freeze, absolute. Every limitation met is recorded in
  `M4-FRICTION.md` as a post-1.0 ledger item and worked around inside the fence.
