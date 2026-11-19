Rate limits can differ by role, each request is counted once per budget, and a
person can issue API tokens from their own account.

Every rate-limit bucket had one count for everyone, so a brief like "60 API
requests a minute anonymous, 300 signed in" could not be written down. A bucket
can now give the holders of a role their own count, configured as
`rate_limit.<bucket>.<role>` in the site configuration or
`TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>` in the environment (the environment
winning, a bad value ignored with a warning, as for the bucket defaults). The
role key is the role's name lower-cased with underscores, so `anonymous user` is
`anonymous_user`. A request with no credential is counted per client IP at the
anonymous count; a signed-in request, by cookie or token, is counted per user at
the most generous count among the user's roles, falling back to the bucket
default, with no exemption for administrators. Before authentication, a request
that presents a credential is also counted per IP at the bucket's highest count,
in a budget of its own, so a stream of bearer values that authenticate nobody is
still bounded per address. One function, `RateLimitConfig::count_for`, chooses
every count, and the user's roles come from the permission service's cache, so
the limit costs no query per request.

Login and registration spent their budget twice: the per-IP middleware counted
`POST /user/login*` and `/user/register*`, and the handlers counted the same key
again, so `register: 3 an hour` admitted one registration and `login: 5 a minute`
two or three attempts. The handler counts are gone. The handler checks the
middleware does not duplicate (email verification, account recovery, password
reset mail, profile and password changes, passkey sign-in, account deletion,
data export) stay.

`/user/tokens`, linked from the account page, lists a user's API tokens (name,
created, last used, expiry), creates one with a name and an optional expiry and
shows the raw token once, and revokes one. It calls the existing `/api/tokens`
endpoints with the page's CSRF token; neither they nor `ApiToken` changed.

The admin item edit form (`GET /admin/content/{id}/edit`) now asks
`ItemService::check_access(item, "edit")` before it renders, so a plugin's Deny
on edit refuses the form with a 403 as well as the submit; the submit's item
level refusal is now a 403 too, where it was a 500.

Contract: additive. The configuration keys, the token page and the 403s are new;
no host function, WIT type, SDK type or existing endpoint changed.
`RateLimitConfig` gains a `by_role` field, and `RateLimiter` a `check_as` method
beside `check`.
