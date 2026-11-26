# Mail: transport, DNS, limits and retention

Trovato queues every message it sends in the `mail_outbox` table, in the same
transaction as the event that caused it, and sends it from mail workers that
run inside every web process. A message ends **sent**, **failed** (with the
reason), **suppressed** (the address bounced or complained) or **cancelled**.
Nothing is dropped while the mail server is down: the workers retry with
backoff for about six hours. The design is in
[`docs/design/mail-queue.md`](../design/mail-queue.md).

## Transport

SMTP is the only transport built in. Mail is off until `SMTP_HOST` is set;
with it unset, nothing is queued and the pages that would send mail say so.

| Setting | Default | Meaning |
|---|---|---|
| `SMTP_HOST` | unset | The relay. Unset turns mail off. |
| `SMTP_PORT` | 587 | |
| `SMTP_ENCRYPTION` | `starttls` | `starttls`, `tls` (implicit TLS, usually port 465), or `none` (local development only) |
| `SMTP_USERNAME`, `SMTP_PASSWORD` | unset | Credentials, when the relay wants them |
| `SMTP_FROM_EMAIL` | `noreply@localhost` | The network's sending address. Its domain is the sending domain below. |

Connections are pooled and reused. A command that gets no answer for 30
seconds fails and is retried later.

## Workers

| Setting | Default | Meaning |
|---|---|---|
| `MAIL_WORKERS` | 2 | Worker tasks per web process. With two or more, the first sends only transactional mail (verification, password reset, notifications), so it never waits behind a newsletter. |
| `MAIL_BATCH` | 10 | Messages one claim takes on the general lane |
| `MAIL_POLL_SECS` | 2 | How often an idle worker looks for due mail; queueing a message wakes a worker at once |
| `MAIL_MAX_ATTEMPTS` | 12 | Attempts before a message is failed; with the backoff (30 seconds doubling to an hour, with jitter) that is about six hours |
| `MAIL_CLAIM_LEASE_SECS` | 300 | How long a claimed message is held before another process may take it, if its worker died (at least 60). A worker starts no send after half the lease, and puts the rest of its batch back. |

A permanent refusal (an SMTP 5xx answer about the recipient or the message)
fails the message at once. Authentication failures (530, 534, 535, 538) are
retried like an outage, so a wrong password does not fail every queued
message. On SIGTERM the workers stop taking mail, finish the message in
flight and put back what they had claimed.

Delivery is at least once: if a process dies after the server accepted a
message and before the row was marked, the message goes again after the
lease. Every message carries `Message-ID: <outbox-<id>@<sending domain>>`, the
same on every attempt, so the provider and the receiver can drop the copy.

## DNS for the sending domain

The sending domain is the domain of `SMTP_FROM_EMAIL`. Every blog sends from
it, under its own name (`"My Blog" <noreply@mail.example.net>`), with
`Reply-To` set to the blog's own address when it has one. Before sending real
mail, publish these records. Your provider gives the exact values.

- **SPF**, a TXT record on the sending domain naming the hosts allowed to send
  for it. For example: `v=spf1 include:mailgun.org ~all`.
- **DKIM**, a TXT record at the selector the provider gives (for example
  `k1._domainkey.mail.example.net`) holding its public key. The provider signs
  each message with the private key.
- **DMARC**, a TXT record at `_dmarc.mail.example.net`, for example
  `v=DMARC1; p=quarantine; rua=mailto:dmarc-reports@example.net`. Start with
  `p=none` and read the reports for a week before you tighten it.
- A **return path** (bounce) domain, a CNAME the provider gives, so SPF aligns.

Bulk mail (mail from a plugin to members, and later newsletters) carries
`List-Unsubscribe` and `List-Unsubscribe-Post: List-Unsubscribe=One-Click`.
Gmail requires one click unsubscribe of senders of more than 5,000 messages a
day.

## Limits

One blog must not burn the network's sender reputation. The workers count what
each blog sent and hold, not drop, what is over a limit. A held message stays
queued with the reason on it and goes when the window allows.

| Setting | Default | Counts |
|---|---|---|
| `MAIL_BLOG_HOURLY_LIMIT` | 1000 | bulk mail per blog per hour |
| `MAIL_BLOG_DAILY_LIMIT` | 5000 | bulk mail per blog per 24 hours |
| `MAIL_BLOG_TRANSACTIONAL_HOURLY_LIMIT` | 500 | transactional mail per blog per hour |
| `MAIL_NETWORK_HOURLY_LIMIT` | 20000 | bulk mail for the whole network per hour; transactional mail is never held by it |

0 means no limit. To give one blog different limits, set them on its tenant
row:

```sql
UPDATE tenant
SET data = jsonb_set(coalesce(data, '{}'), '{mail_limits}',
                     '{"hourly": 200, "daily": 1000, "transactional_hourly": 100}')
WHERE machine_name = 'some_blog';
```

The request rate limit buckets (`mail`, `mail_users`, `mail_recipient`,
`reset_mail`, `recovery`) still apply before a message is queued.

## Bounces and complaints

Suppression is network wide: an address that hard bounced for one blog is not
mailed by any. A hard bounce stops all mail to the address; a spam complaint
stops bulk mail only, so the member can still reset their password. The
suppression list is in `mail_suppression`.

The bounce webhook that fills it from the provider arrives in the second part
of BG18; until then, suppress an address by hand:

```sql
INSERT INTO mail_suppression (address, reason, detail)
VALUES (lower('someone@example.com'), 'manual', 'asked by support ticket 123');
```

## Retention and personal data

`mail_outbox` holds each recipient's address, the subject and the body, which
may quote a comment or a member's name. Cron clears the bodies of finished
messages after `MAIL_BODY_RETENTION_DAYS` (default 7) and deletes finished
messages after `MAIL_LOG_RETENTION_DAYS` (default 90). Reset links,
verification links and recovery codes are never stored: the worker mints them
when it sends, and the stored body holds a marker in their place.

## Watching it

```sql
-- What is waiting, failing or held, per blog.
SELECT tenant_id, kind, status, count(*), max(last_error), max(held_reason)
FROM mail_outbox
WHERE status IN ('queued', 'sending', 'failed')
GROUP BY 1, 2, 3;
```

`/metrics` exposes `trovato_circuit_breaker_email` (0 closed, 1 open, 2 half
open); `/health` lists the breaker as `email_smtp`.
