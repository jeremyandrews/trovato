Mail is no longer lost when the mail server is down. Every message Trovato
sends (registration and email change verification, password resets, account
recovery codes and notices, comment notifications, the administrator's
registration notice, the SMTP test mail and plugin mail) is now a row in a new
`mail_outbox` table, written with the same database transaction as the event
that caused it where there is one: a registration that fails sends nothing,
and one that succeeds always sends. Mail workers in every web process claim
due messages with `FOR UPDATE SKIP LOCKED`, send them over SMTP (now with
pooled connections), and retry temporary failures with exponential backoff and
jitter for about six hours; a permanent refusal fails the message at once.
Every message ends sent, failed with its reason, suppressed or cancelled.
Before, each sender talked to SMTP inline or on a spawned task and logged and
forgot a failure, and the circuit breaker stopped attempts without retrying
them (BL-149).

Reset links, verification links and recovery codes are minted by the worker at
send time, so they are never stored in the clear, and a link sent after an
outage is fresh. Transactional mail is never delayed behind bulk mail; each
blog has hourly and daily limits, past which its mail is held, not dropped;
addresses that hard bounced or complained are suppressed network wide;
finished messages lose their bodies after 7 days and are deleted after 90.
The transport is a `MailTransport` trait, so an HTTP API provider can be added.
Plugin mail calls keep their signatures and codes; the WIT now says that
success means accepted for delivery, not delivered. Settings and DNS records
are in `docs/deployment/mail.md`; the design is
`docs/design/mail-queue.md`.
