Bounces and complaints now reach Trovato, and administrators can see what mail
was sent. `POST /mail/webhook/mailgun` takes Mailgun's signed webhook events
(set `MAIL_WEBHOOK_SIGNING_KEY`; without it the route answers 404): a
permanent failure fails the message it names and stops all further mail to the
address on every blog, and a spam complaint stops bulk mail to it. Each
request's signature, timestamp and token are checked, so a forged or replayed
request changes nothing; it has its own rate limit bucket, `mail_webhook`.

New pages under System: the **Mail log** (`/admin/reports/mail`) lists every
message with filters and a count per status, and a message's page shows its
headers, body, attempts and last error, with **Retry** for a failed or
cancelled message and **Cancel** for a queued one; **Suppressed addresses**
(`/admin/reports/mail/suppressions`) lists the suppression list with
**Remove** and a form to add an address. Both take `administer site`. **Blog
mail** (`/admin/mail`) shows a blog's own mail to a member of the blog holding
the new `view own blog mail` permission. The admin theme's link colour and
sidebar headings now meet WCAG AA contrast. Setup is in
`docs/deployment/mail.md`.
