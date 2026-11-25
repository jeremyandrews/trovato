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

The provider reports bounces and complaints to Trovato through a signed
webhook. Mailgun is the one supported today (`docs/design/mail-queue.md` §3
says why).

### Mailgun webhook setup

1. In the Mailgun dashboard, open **Settings → Webhooks** (on older dashboards
   **Sending → Webhooks**), choose the sending domain, and copy the **HTTP
   webhook signing key**. Set it on every web server and restart them:

   ```sh
   MAIL_WEBHOOK_SIGNING_KEY=<the HTTP webhook signing key>
   ```

   Without it, `POST /mail/webhook/mailgun` answers 404, as it does on a site
   with no mail configured.
2. On the same page, add `https://<your site>/mail/webhook/mailgun` to the
   **Permanent Failure** and **Spam Complaints** events. Other events are
   acknowledged and ignored, so adding them does no harm.
3. Press **Test webhook** on the Permanent Failure event. Mailgun shows the
   answer: 200 means the signature checked. A test event names a message
   Trovato never sent, so it changes no message but does suppress the test
   address; remove it afterwards on the suppression page (below).

Every request must carry Mailgun's signature: the hex HMAC-SHA256 of its
`timestamp` and `token` under the signing key, compared in constant time. A
timestamp more than five minutes from the server's clock is refused, so keep
the servers on NTP. A token is remembered in Redis for fifteen minutes, and a
second request with it changes nothing.

| Answer | Meaning | Mailgun |
|---|---|---|
| 200 | recorded, ignored (another event type), or already seen | done |
| 401 | bad signature, or a timestamp outside five minutes | retries; check the key and the clock |
| 404 | no `MAIL_WEBHOOK_SIGNING_KEY`, or no mail configured | retries |
| 406 | not a Mailgun event, or no usable recipient | does not retry |
| 503 | Redis or the database failed | retries |

The webhook has its own rate limit bucket, `mail_webhook`, 600 requests a
minute per client IP (`TROVATO_RATE_LIMIT_MAIL_WEBHOOK` changes it), so a
burst of bounces after a large send is not refused as form spam.

A permanent failure (except Mailgun's reasons `suppress-unsubscribe`, `old`
and `espblock`, which are ignored, and `suppress-complaint`, which counts as a
complaint) fails the message it names, by its `Message-ID`
(`outbox-<id>@<sending domain>`) and only when it was sent to the reported
address, with "hard bounce: <code> <reason>". A complaint stamps the message
and leaves it sent. Both put the address on the suppression list; a hard
bounce replaces an earlier complaint, and nothing replaces a hard bounce or an
address added by hand.

## The mail log pages

Under **System** in the admin menu:

- **Mail log**, `/admin/reports/mail` (`administer site`): every message on
  the network, newest first, filtered by status, kind, blog, category and
  recipient address, with a count per status. A message's page shows its
  headers, its body (the place of a reset link or code marked, never the
  secret), its attempts and its last error. **Retry** puts a failed or
  cancelled message back in the queue with its attempts reset; **Cancel**
  takes a queued message out. A message whose body retention cleared cannot
  be retried.
- **Suppressed addresses**, `/admin/reports/mail/suppressions`
  (`administer site`, linked from the mail log): the network list, with
  **Remove** on each address and a form to suppress one by hand.
- **Blog mail**, `/admin/mail`: a blog owner's view of their own blog's log,
  without message bodies and without the suppression list. It takes the
  `view own blog mail` permission and an active membership of the blog.
  Until blog resolution lands (BG4) every request is for the default blog,
  and membership is a `user_tenant` row:

  ```sql
  INSERT INTO user_tenant (user_id, tenant_id, is_active)
  SELECT id, '0193a5a0-0001-7000-8000-000000000001', TRUE FROM users WHERE name = 'someone'
  ON CONFLICT (user_id, tenant_id) DO UPDATE SET is_active = TRUE;
  ```

  An administrator passes both checks. Until BG4 tags mail with its blog,
  every message is the default blog's, so an owner of the default blog sees
  the whole network's account mail (recipients, subjects, errors), and every
  account that existed when the `user_tenant` table was created is already
  an active member of the default blog. Grant `view own blog mail` to network
  staff only.

Screenshots: `docs/images/bg18/`.

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
