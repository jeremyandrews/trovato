Login lockout, client address resolution and password reset requests are
harder to abuse.

The login lockout was keyed by the submitted username alone, as typed, so five
wrong guesses from anywhere locked the real owner out everywhere, while `admin`,
`Admin` and every other capitalisation each got their own five guesses because
login matches usernames case-insensitively. Every lockout key is now built from
the username trimmed and lower-cased, inside `lockout.rs` so no caller can
forget. Five failures in fifteen minutes now lock only the address they came
from (username and client address together). A second counter across all
addresses locks the account everywhere after fifty failures in fifteen minutes,
which caps guessing spread across many addresses. A successful sign-in clears
only its own address's counter. `LockoutService::is_locked`,
`record_failed_attempt`, `clear_attempts` and `get_lockout_remaining` now take
the client address; `clear_all` still clears everything for a username.

Behind a trusted proxy, the client address was the leftmost `X-Forwarded-For`
entry, which a proxy that appends passes through from the client, so a client
could choose its own rate-limit and lockout identity. The address is now the
first entry from the right that is not itself a trusted proxy; `X-Real-IP` and
then the socket peer remain the fallbacks. The session management audit events
now record that vetted address instead of reading `X-Forwarded-For` themselves.

Signing in as an unknown or blocked user skipped the Argon2 verification that a
wrong password for a real account pays for, so response time showed which
usernames exist. All three now do one production-cost verification, against a
dummy hash when there is no real one.

A password reset request created its token and sent its mail before answering,
but only for an address with an account, so response time showed which
addresses exist. It also had no limit of its own beyond the generic per-client
form limit. The lookup, the token and the mail now happen after the answer. Two
limits apply: five requests per client address per fifteen minutes (the
`recovery` bucket), and three mails per destination address per hour (a new
`reset_mail` bucket, overridable as `TROVATO_RATE_LIMIT_RESET_MAIL`). Over either
limit no mail is sent and the answer is the same success body, so the limit
itself reveals nothing.
