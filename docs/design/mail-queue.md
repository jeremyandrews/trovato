# Durable Mail: an Outbox in Postgres, Workers, Limits and Bounces

BG18. Closes BL-149 (`docs/BACKLOG.md`).

## Goal

Every mail Trovato sends (verification, password reset, account recovery,
comment notifications, plugin mail, and later newsletters, previews and Rules
actions) is written to a durable queue in the same transaction as the event
that caused it, sent by workers with retries and backoff, and never lost when
the mail server is down. Bounces and complaints are recorded and suppress later
mail, each blog has sending limits, and an administrator can see what was sent
and what failed.

**Every message ends in exactly one of four visible states: sent, failed (with
the reason), suppressed, or cancelled.** Nothing is dropped without a row
saying so.

## Problem statement recheck (main at 6d9df7d)

- `crates/kernel/src/services/email.rs`: `EmailService` sends with `lettre`
  0.11 over SMTP (rustls) behind a `CircuitBreaker` (`circuit_breaker.rs`). An
  open breaker refuses the send; nothing retries it.
- No table holds unsent mail. Every sender calls the SMTP transport directly,
  inline or on a `tokio::spawn`, and logs and forgets a failure.
- `plugin_queue` (`cron/mod.rs`) has a lease (`locked_until`), exponential
  backoff (`backoff_secs`, 60 s doubling to 1 h) and dead letters, and is drained
  by cron or by the opt-in resident runner. It dispatches `tap_queue_worker` to a
  plugin by name and **deletes the row on success**.
- The Redis `Queue` trait (`cron/queue.rs`, `RedisQueue`) is `LPUSH`/`BRPOP`: a
  popped item is gone, whatever happens next. The cron `email:send` queue on it
  has no producer in the tree.
- BG4 (`hosted-tenancy.md`) is merged as a design only: `tenant` and
  `user_tenant` exist, no request resolves a blog other than the default yet.
  BG10 (`upgrades-and-probes.md`) is merged and built: migrations must be
  expand only, background workers stop taking work on SIGTERM and leases cover
  the rest. BG14 (`tracing-and-performance-tests.md`) is merged and built:
  `performance_test` counts statements per page.

### Every place that sends mail today

| Sender | File | How | On failure |
|---|---|---|---|
| Registration verification | `routes/auth.rs` `do_register` | inline, after the user insert commits | logged, user told "not sent" |
| Admin notice of a registration | `routes/auth.rs` `do_register` | `tokio::spawn` | logged |
| Email change confirmation | `routes/auth.rs` profile update | inline | logged |
| Password reset | `routes/password_reset.rs` `send_reset_mail` | `tokio::spawn` | logged, token URL at debug |
| Account recovery notice | `routes/recovery.rs` `notify` | inline | logged |
| Account recovery code | `services/recovery_builtins.rs` | inline | flow refused |
| Account deleted farewell | `routes/user_delete.rs` `send_farewell` | inline | logged |
| Comment notification | `routes/comment.rs` `notify_if_published` | `tokio::spawn` | logged |
| SMTP test mail | `routes/admin_config.rs` | inline | flash message |
| Plugin `send-to-site-contacts` | `host/mail.rs` | inline in the host call | `ERR_MAIL_SEND_FAILED` |
| Plugin `send-to-user` | `host/mail.rs` | inline in the host call | `ERR_MAIL_SEND_FAILED` |
| Legacy `email:send` Redis queue | `cron/tasks.rs` `process_email_item` | cron pop then send | item gone |

## Survey: how others do it

| System | Storage | Same transaction as the event? | Retry | Source |
|---|---|---|---|---|
| Transactional outbox pattern | a table in the service's own database, relayed by a poller | yes, by construction | relay retries until acknowledged | <https://microservices.io/patterns/data/transactional-outbox.html> |
| Rails Action Mailer `deliver_later` on Active Job, Solid Queue backend | `solid_queue_jobs` tables in the app database, claimed with `FOR UPDATE SKIP LOCKED` | yes when the job is enqueued inside the record's transaction (Rails 7.2 adds `enqueue_after_transaction_commit` for non-database backends, which is the problem it works around) | `retry_on` with exponential backoff and jitter | <https://guides.rubyonrails.org/action_mailer_basics.html>, <https://github.com/rails/solid_queue> |
| Oban (Elixir) | Postgres `oban_jobs`, `SKIP LOCKED`, unique jobs by key | yes, `Oban.insert` inside `Ecto.Multi` | backoff with jitter, `max_attempts`, discarded state | <https://oban.hexdocs.pm/Oban.html> |
| WordPress Action Scheduler (WooCommerce) | `actionscheduler_actions` table in MySQL, claimed by a claim id | yes, same database | failed actions logged, not retried by default | <https://actionscheduler.org/> |
| Drupal Symfony Mailer + Queue API | `queue` table (`DatabaseQueue`), lease by `expire` | yes, same database | item released for retry, no backoff in core | <https://www.drupal.org/project/symfony_mailer>, <https://api.drupal.org/api/drupal/core%21core.api.php/group/queue> |
| Ghost newsletters | email and recipient rows in its own database, sent in batches through Mailgun, "the only supported bulk email provider" | yes, rows written with the post publish | failed batches retried, then shown as failed in the post's email analytics | <https://docs.ghost.org/newsletters/> |

**The best of these is a Postgres outbox claimed with `FOR UPDATE SKIP
LOCKED`, with unique keys and backoff with jitter: Oban and Solid Queue.** It is
the only shape that makes "a rolled back signup sends nothing, a committed one
always sends" true by construction, because the message and the event commit or
roll back together. Ghost adds the two things a blog network needs on top:
recipient level rows that the owner can see, and a provider chosen for bulk
mail. Trovato takes the outbox from Oban and Solid Queue and the visibility
from Ghost.

## Decisions

### 1. Storage: a Postgres outbox, not the Redis queue

The message row is inserted with the same executor as the write that caused it,
so it commits or rolls back with it. Redis cannot join a Postgres transaction:
a push before commit sends mail for a signup that rolled back, a push after
commit loses mail when the process dies between the two. Redis durability also
depends on its `appendonly` setting, which a hosted Redis may not offer, and
`BRPOP` removes the item before the send. Postgres is already the durable store
the site is backed up from.

`plugin_queue` is not reused as the table. Its rows belong to a plugin and are
dispatched to `tap_queue_worker`; it deletes rows on success, so it could not
answer "what was sent"; and it has no recipient, tenant, kind, dedupe key or
suppression. What **is** reused is its shape and code: the lease
(`locked_until`) and reclaim rule, the attempt count and the dead letter state
become `mail_outbox`'s, `cron/mod.rs`'s `backoff_secs` curve is the base of the
mail backoff (with jitter added), and the worker is started and stopped the way
BG10 starts and stops the resident queue runner (`workers_token`).

The table, `mail_outbox`, one row per message and recipient:

| Column | Meaning |
|---|---|
| `id` | `BIGSERIAL`; also the `Message-ID` local part, so a receiver can drop a duplicate |
| `tenant_id` | the blog; `DEFAULT_TENANT_ID` until BG4 part (a) resolves blogs |
| `kind` | `transactional` or `bulk` |
| `priority` | 0 account mail (verification, reset, recovery), 1 notifications, 2 bulk |
| `category` | what made it: `password_reset`, `registration_verify`, `comment_notification`, `plugin:<name>`, ... |
| `recipient` | one address, validated at enqueue |
| `subject`, `text_body`, `html_body`, `attachments` | the rendered message; bodies cleared by retention |
| `unsubscribe_url` | bulk only; becomes `List-Unsubscribe` and one click headers |
| `secret` | a *reference* to a one time secret minted at send time (below), never the secret |
| `dedupe_key` | unique per tenant; a second insert with the same key is a no-op |
| `status` | `queued`, `sending`, `sent`, `failed`, `suppressed`, `cancelled` |
| `attempts`, `max_attempts`, `next_attempt_at`, `locked_until`, `last_error`, `held_reason` | worker bookkeeping |
| `created_at`, `sent_at`, `finished_at`, `bounced_at`, `complained_at`, `body_purged_at` | history |

**The rendered message is stored, not a template and its data.** Rendering at
enqueue uses the request's theme, language and site name, catches a broken
template while the caller can still report it, and lets the admin view show
what was sent. HTTP API providers want the parts (subject, text, HTML), not raw
MIME, so the parts are stored and the transport builds the message.

**One time secrets are minted at send time.** A password reset link, a
verification link and a recovery code are stored hashed (`password_reset_tokens`,
`email_verification_tokens`, `recovery_email_challenges`) so a database read
cannot sign anyone in. Storing the rendered mail would put the plain secret in
`mail_outbox` for as long as it is queued, which weakens that. So the body is
rendered with a marker where the secret goes, the row carries
`secret = {"kind": "password_reset", "user_id": ...}`, and the worker mints the
token and substitutes it immediately before the send. The secret exists only in
memory and in the message on the wire. A side effect is that a reset link sent
after an hour long outage is fresh, not expired. So that a late send cannot
hand a fresh secret to an address the account has left, the worker mints only
when the row's recipient is still the account's address (for an email change,
the address still pending); otherwise the message fails for good with that
reason and nothing is minted.

**Dedupe.** `UNIQUE (tenant_id, dedupe_key)`, insert with `ON CONFLICT DO
NOTHING`. Kernel keys: `register:<user>`, `admin-new-user:<user>`,
`password-reset:<user>:<10 minute window>`, `comment-published:<comment>`,
`account-deleted:<user>`. A double submitted reset form, a retried request, or
a comment approved twice sends one message. An email change has no key: each
request invalidates the links sent before it, so each must send a fresh one.

### 2. Workers

- **Claim** with `UPDATE ... WHERE id IN (SELECT id ... FOR UPDATE SKIP LOCKED
  LIMIT n) RETURNING *`: `status = 'sending'`, `attempts + 1`, `locked_until =
  now() + lease`, ordered by `priority, next_attempt_at, id`. Due rows are
  `queued` past `next_attempt_at`, or `sending` past `locked_until` (a dead
  worker's claim).
- **Two lanes.** Worker 0 claims `transactional` only, one at a time; the
  others claim anything, transactional first, in batches. So a verification
  mail waits at most for the one message the transactional lane is sending,
  never behind a newsletter. `MAIL_WORKERS` (default 2, minimum 1; with 1 the
  single worker still takes transactional first).
- **Send, then mark** `sent` with `sent_at`. On a transport error: permanent
  (an SMTP 5xx other than the authentication codes 530, 534, 535 and 538, or a
  message that cannot be built) marks `failed` at once; temporary
  reschedules `next_attempt_at = now() + backoff`, where backoff is 30 s
  doubling to a 1 hour cap with equal jitter (half fixed, half random), until
  `max_attempts` (`MAIL_MAX_ATTEMPTS`, default 12, about six hours of trying),
  then `failed` with the last error.
- **Suppression** is checked at claim: a suppressed recipient's row goes to
  `suppressed` without a send.
- **Shutdown**: the worker stops claiming when `workers_token` is cancelled
  (BG10), finishes the message in flight, and puts every row it claimed but did
  not start back to `queued` without counting the attempt. A killed worker's
  claim is reclaimed when its lease (`MAIL_CLAIM_LEASE_SECS`, 300) expires.
- **At least once, not exactly once.** A worker that dies after the server
  accepted a message and before it marked the row sends it again after the
  lease. No SMTP or HTTP provider offers better; the stable `Message-ID`
  (`<outbox-<id>@<sending domain>>`) lets receivers and providers drop the
  copy.
- The worker runs in every web process that has a transport configured (it is
  cheap and idle when the outbox is empty: one indexed query per poll,
  `MAIL_POLL_SECS`, default 2, woken early by an in-process notify after an
  enqueue). No configuration enables it beyond configuring SMTP.

### 3. Transports

```rust
#[async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, message: &OutgoingMail) -> Result<(), TransportError>;
    /// The circuit breaker's state, for `/health` and `/metrics`.
    fn breaker_state(&self) -> Option<&'static str>;
}
pub struct TransportError { pub permanent: bool, pub message: String }
```

SMTP (`SmtpTransport`, the existing lettre transport and circuit breaker) is
the default and the only one built here. An HTTP API transport (Postmark, SES,
Mailgun) is an implementation of the trait; the worker never knows which it
has.

**Recommendation for trovato.blog (a recommendation only): start with
Mailgun.** Ghost, the nearest product to a hosted blog network, sends its
newsletters through Mailgun; Mailgun accepts SMTP, so the default transport
works on day one with no code; and it signs its webhooks with HMAC-SHA256 over the timestamp and a token
(<https://documentation.mailgun.com/docs/mailgun/user-manual/webhooks/securing-webhooks/>), which
the bounce endpoint below checks with the `hmac` crate already in the tree.
Postmark has the better reputation for transactional mail and separates
transactional and broadcast streams, but "Postmark does not currently support HMAC webhook signature
verification" (basic auth and an IP list only,
<https://postmarkapp.com/developer/webhooks/webhooks-overview>), so it would need a second verification scheme. SES
is cheapest at newsletter scale, but its bounce notifications come through SNS
with X.509 signatures, which needs certificate fetching and RSA verification
the kernel does not have.

### 4. Bounces and complaints

`POST /mail/webhook/mailgun`, unauthenticated, in the `mail_webhook` rate limit
bucket. It refuses anything whose `signature.signature` is not
`HMAC-SHA256(MAIL_WEBHOOK_SIGNING_KEY, timestamp + token)` (compared in constant
time), whose timestamp is more than 5 minutes old, or whose token was seen in
the last 10 minutes (Redis `SET NX EX`, against replay). With no key configured
the route answers 404. It records:

- a `failed` event with `severity: permanent` → a hard bounce;
- a `complained` event → a complaint.

Each becomes a row in `mail_suppression (address, reason, outbox_id, detail,
created_at)`, keyed by the lower-cased address, **network wide**: an address
that bounced for one blog is not mailed by any. A hard bounce suppresses all
mail; a complaint suppresses bulk mail only, so a member who marked a
newsletter as spam can still reset their password. The outbox row the event
names (by `Message-ID`) gets `bounced_at` (and becomes `failed`, reason "hard
bounce: ...") or `complained_at`.

A blog owner sees, on their own blog's mail log, that a message to one of
their own recipients bounced or was suppressed. The suppression list itself,
with its addresses, is only shown to network administrators, so a blog never
learns which addresses bounced for another blog. No IMAP polling: every
provider worth recommending has webhooks.

### 5. Limits

Counted from `mail_outbox` (`sent_at`), per blog and kind, refreshed every few
seconds by each worker, which also counts what it sends between refreshes:

| Limit | Default | Setting |
|---|---|---|
| Bulk mail per blog per hour | 1000 | `MAIL_BLOG_HOURLY_LIMIT` |
| Bulk mail per blog per 24 hours | 5000 | `MAIL_BLOG_DAILY_LIMIT` |
| Transactional mail per blog per hour | 500 | `MAIL_BLOG_TRANSACTIONAL_HOURLY_LIMIT` |
| Bulk mail for the whole network per hour | 20000 | `MAIL_NETWORK_HOURLY_LIMIT` |

0 means no limit. A blog's own `tenant.data.mail_limits` (`hourly`, `daily`,
`transactional_hourly`) overrides the per blog defaults, so the network can
give one blog more or less. A blog over a limit is **held, not dropped**: its
claimed rows of that kind go back to `queued` with `held_reason` ("blog hourly
limit reached") and the attempt not counted, and the blog's other waiting rows
of that kind get the same reason; all of them become due again a minute
later, when a worker counts again. The hold lives in the table, so every
worker in every process sees it.
Transactional mail is never held by the network bulk limit. These sit on top
of the existing request buckets (`mail`, `mail_users`, `mail_recipient`,
`reset_mail`, `recovery`), which stay where they are.

### 6. Deliverability

- `From: "<blog name>" <SMTP_FROM_EMAIL>`: one network sending domain, the
  blog's name. Built with lettre's `Mailbox`, which quotes the name; names and
  subjects with a control character are refused at enqueue (header injection).
- `Reply-To:` the blog's `site_mail` when it is set.
- Both are resolved by the worker from the blog's settings at send time (cached
  for a minute), so queueing reads no settings and a renamed blog's queued
  mail goes out under its new name.
- `Message-ID: <outbox-<id>@<domain of SMTP_FROM_EMAIL>>`.
- Bulk mail carries `List-Unsubscribe: <url>` and
  `List-Unsubscribe-Post: List-Unsubscribe=One-Click` (RFC 2369, RFC 8058),
  which Gmail requires since 2024 of senders of more than 5,000 messages a day
  to its users (<https://support.google.com/a/answer/81126>); the existing one click
  endpoint (`routes/mail_preference.rs`) answers it.
- SPF, DKIM and DMARC records for the sending domain are an operator task,
  documented in `docs/deployment/mail.md`.

### 7. Retention and personal data

`mail_outbox` holds personal data: the recipient address, the subject, the
body (which may quote a comment or a member's name) and attachments. Each cron
run clears `text_body`, `html_body` and `attachments` of finished rows
older than `MAIL_BODY_RETENTION_DAYS` (default 7) and deletes finished rows
older than `MAIL_LOG_RETENTION_DAYS` (default 90). Suppression rows hold the
address and are kept until an administrator removes one: they exist to stop
mail to that address. One time secrets are never stored (decision 1).

Deleting a blog deletes its outbox rows with it (`ON DELETE CASCADE`),
queued ones included. That is deliberate: a deleted blog has no name to send
under and no members to answer, and its mail is personal data of the kind
the deletion removes. It is the one way a queued message ends without a
status row, and it happens only on that explicit administrative act.

### 8. WIT

`send-to-site-contacts` and `send-to-user` keep their signatures and codes.
Their documentation gains one paragraph: success means the message was
accepted for delivery (queued durably), not that it was delivered;
`ERR_MAIL_SEND_FAILED` now means it could not be queued. Additive, as BL-149
asked; no plugin changes.

### 9. Admin

- `/admin/reports/mail`: every message, filters for status, kind, blog,
  category and recipient, newest first, paged; a summary count per status.
- `/admin/reports/mail/{id}`: headers, body (or "cleared by retention"),
  attempts and last error, with **Retry** (a failed or cancelled message back
  to `queued`, attempts reset) and **Cancel** (a queued message to `cancelled`).
- `/admin/reports/mail/suppressions`: the network suppression list, with
  **Remove**.
- `/admin/mail`: a blog owner's view, the same list limited to their own blog
  (permission `view own blog mail`), no suppression list.

Server rendered forms, POST with CSRF, labels on every control, no JavaScript.

## Plugin API and security

No WIT signature, host function, SDK type or manifest key changes; the WIT
gains documentation only. Security checks are kept or strengthened: the
request rate buckets are unchanged and still checked first; one time secrets
stay hashed at rest; header injection is refused at enqueue for every sender,
not only plugins; the webhook is signature checked, time bounded and replay
checked.

## New crates

None. `lettre`, `hmac`, `sha2`, `rand`, `async-trait` and `tokio` are already
dependencies.

## Parts

1. `bg18-mail-queue`: this document, the migration, the outbox service and
   workers, the transport trait, every sender moved onto the outbox, limits,
   suppression at send, retention, the WIT note, the operator guide, BL-149
   closed, and the tests.
2. `bg18-mail-queue-2`: the bounce webhook, the admin and blog owner pages,
   screenshots and the accessibility check.

## Transaction boundaries, per sender (part 1 as built)

| Sender | The write the message commits with |
|---|---|
| Registration verification, admin notice | the `users` insert (`UserService::create_with_status_and_mail`) |
| Comment notification | the `comment` insert, or the status update that publishes it (`CommentService::create_with_mail`, `update_with_mail`) |
| Password reset, recovery code, recovery notice, SMTP test mail, plugin mail, legacy `email:send` items | none: the request writes nothing else, so the outbox row is the event's only durable record |
| Email change confirmation | queued right after the profile update commits, in the same request; the pending address is in `users.data`, and a resubmitted form sends a fresh link because it invalidated the old one |
| Account deleted farewell | queued before the account row is deleted, in the same request, as before |

The two that are not one transaction keep the window to the gap between two
statements of one request, where before it was a spawned task or an inline
SMTP conversation.

## Performance

- Queueing is one `INSERT ... ON CONFLICT DO NOTHING` in the caller's
  transaction. The new BG14 count tests `signup.anonymous.post` and
  `comment.admin.post` pin the whole statement list of both paths, with one
  `insert:mail_outbox` each, so any further cost fails a test. They are new
  baselines, not a before and after: the comment path now reads the post's
  author and the site name in the request, where the old code read them in a
  spawned task after the response. Page reads are untouched; the existing
  page counts did not move.
- Measured on the Mac Studio against the loopback sink
  (`mail_queue_test::throughput`, release build, 2000 messages): queueing
  costs about 1.1 ms a message from a test client including the round trip,
  and one worker sends about 550 messages a second with pooled connections
  (490 without). Against a real provider the network and TLS dominate;
  `MAIL_WORKERS` and the process count scale it.

## Part 2 as built

**Webhook.** `routes/mail_webhook.rs`. The body is read as bytes and parsed
with serde, so a malformed request is a 406 (Mailgun does not retry it) rather
than an extractor's 415 or 422. The order of checks is: key configured (else
404), payload readable (else 406), token length and signature (`Mac::verify_slice`,
constant time; else 401), timestamp within 300 s (else 401), then the replay
check (`SET mail_webhook:token:<token> 1 NX EX 600`). A token seen before
answers 200 and does nothing, so Mailgun stops retrying an event whose first
answer was lost. If recording fails the token is deleted again and the answer
is 503, so the retry is taken. The key lives on `MailConfig` as a
`WebhookKey`, whose `Debug` prints no secret, and is read through the email
service: no mail configured means no webhook.

**Recording** (`services/mail_log.rs::record_feedback`, one transaction). The
outbox row is matched by the id in `outbox-<id>@domain` *and* the reported
recipient, so a Message-ID from another install on the same sending domain
cannot fail someone else's message. A hard bounce moves a `sent` or `failed`
row to `failed`, sets `bounced_at` and `finished_at` once, and records "hard
bounce: <code> <reason>" (control characters made spaces, 500 characters at
most). A complaint only stamps `complained_at` on a `sent` row. The
suppression upsert replaces an existing row only to upgrade a complaint to a
hard bounce; a manual entry or a hard bounce is never weakened. Later mail is
stopped by the worker's existing suppression check at send.

**Pages** (`routes/admin_mail.rs`, `templates/admin/reports/mail*.html`). The
list builds one filtered `SELECT ... ORDER BY id DESC LIMIT 50 OFFSET n`
(`mail_outbox_tenant_newest` serves one blog and the primary key the network; two later migrations build, concurrently, `(status, id DESC)` for the status filter and the counts (an index only scan) and `(lower(recipient), id DESC)` for the recipient filter)
and one `count(*) ... GROUP BY status` with the same filters except status;
the total for the pager is read from those counts, so there is no third
query. Status and kind are accepted only from their fixed lists. Retry and
Cancel are single guarded `UPDATE`s (`status IN ('failed','cancelled') AND
text_body IS NOT NULL`, and `status = 'queued'`), so neither can touch a row a
worker holds as `sending`; Retry wakes the local workers. Every action is a
form POST that checks `administer site` before the single use CSRF token, and
answers with a 303 back to the page and a flash message.

The blog owner page reads the blog from the request's `TenantContext`
(default blog while `resolve_tenant` is unmounted, BG4), never from the query
string, and takes `view own blog mail` plus an active `user_tenant` row for
that blog; an administrator passes both. It shows no message page, so no
owner sees a body; the suppression list stays network only, as §4 says.

**Accessibility.** Server rendered, no JavaScript: filters are a GET form,
status counts are links in a `nav`, actions are POST forms with buttons, every
control has a label, tables have a caption and `scope` on headers, the long
body is a focusable `pre`. axe-core 4 through Playwright (tags `wcag2a`,
`wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`) reports no violation on the five
pages. Getting there needed three fixes to the shared admin chrome, which
every admin page benefits from: section headings in the sidebar were a `div`
inside the `ul` (now an `li`) at 3.75:1 contrast (now `#94a3b8`, 6.9:1); the
theme's indigo `--primary`, used for links and the active sidebar item, is
4.46:1 on white, so the admin layout uses the next darker step (`#4f46e5`,
6.3:1); and a link inside a paragraph is underlined.

**Tests.** `tests/mail_admin_test.rs` (10): a hard bounce fails the message and
suppresses later mail, including account mail; a complaint stops bulk mail
only and a later hard bounce upgrades it; forged, tampered, stale and
replayed requests change nothing; temporary failures and other events are
ignored and a bounce naming another recipient's message leaves it alone; no
key means 404; every network page and action refuses the anonymous, a member
and a blog owner; list filters, status counts, the detail page and the
redacted secret; Retry and Cancel with and without the token and in each
state, including a cleared body; suppressions added and removed only with the
token; the owner page shows its own blog only, whatever `blog=` says, and
refuses a non member, an inactive member and a member without the
permission. Unit tests cover the signature, the time window, the event
mapping, the bounce detail, Message-ID parsing, the redaction, the filter
query string and the owner filter.
