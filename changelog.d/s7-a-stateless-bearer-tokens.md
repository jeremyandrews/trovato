API tokens work again behind the full middleware stack, leave no session
behind, and stop working the moment they are revoked. The OAuth JWT middleware
runs before the API token middleware and answered 401 to every
`Authorization: Bearer` value that was not a valid JWT, and to every bearer
value at all while OAuth was disabled, so an API token never reached the
middleware that checks it; the test harness omitted the JWT middleware, which is
why no test saw it. That middleware now judges only values shaped like a JWT,
and only while OAuth is enabled, and passes everything else through; a real JWT
is handled exactly as before. A token request no longer writes its user into the
cookie session: the token middleware gives the rest of the request an ephemeral
in-memory session holding the token's user, so no `Set-Cookie` is sent, no
session record is stored in Redis and no entry appears in the per-user session
index. Previously that write turned every token request into a site-wide
browser session that carried CSRF state, lasted a day of inactivity and kept
authenticating after the token was deleted. Token lookups no longer go through a
process-local cache, which kept a revoked token working for up to a minute, kept
an expired one working past its expiry, and could not be invalidated from the MCP
server's process or another instance; every lookup now asks the database's unique
index. A token-authenticated request is exempt from the JSON and form CSRF
checks, because no browser attaches a bearer token to a cross-site request by
itself; a cookie-authenticated request needs a CSRF token exactly as before, and
a request carrying both a session cookie and a token still acts as the cookie's
user. `ApiToken::delete_all_for_user` is new.
