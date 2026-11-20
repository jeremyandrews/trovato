A plugin can mail one of the site's members by user id: `mail` gains
`send-to-user` (SDK: `host::mail_send_to_user`), gated by a new manifest
capability, `mail_users`. **API change:** a new host function in an existing
interface and a new `[capabilities]` boolean, defaulting to false; no existing
signature changed. **Behaviour change:** a new table, `user_mail_preference`,
is created by migration and is on the protected table floor, and every member
mail carries an unsubscribe link (BL-94).

The root cause was that `mail` only ever reached the site contact address, so
a plugin with something to tell a member (a digest, a reply notice) had no
way to do it short of reading addresses out of `users`, which the protected
floor forbids. `send-to-user` resolves the address in the kernel and never
hands it back: not in a return value, an error or a log line. Each message
goes through the one `EmailService` builder with a footer and
`List-Unsubscribe` / `List-Unsubscribe-Post: List-Unsubscribe=One-Click`
headers. Opening the link shows a confirmation page that changes nothing, so
link scanners cannot unsubscribe anyone; the POST records the opt out (it has
no CSRF token, by design: the unguessable token in its path is the
credential, and a mail client's one-click POST has no session). A member sees
the plugins they opted out of on their account page, with a CSRF-protected
Resubscribe button. Two new rate limit buckets apply: `mail_users`, 1000 an
hour per plugin, and `mail_recipient`, 10 an hour per plugin and member. New
error codes `ERR_MAIL_USERS_DENIED` (-56), `ERR_MAIL_RECIPIENT_UNAVAILABLE`
(-57) and `ERR_MAIL_OPTED_OUT` (-58).
