A site served over plain HTTP at a LAN address can be logged in to.
`create_session_layer` always marked the session cookie `Secure`, and a browser
drops a `Secure` cookie from a plain HTTP page (except on `localhost`), so the
login answered, the cookie was thrown away, and the next page was anonymous
again. The new `COOKIE_SECURE` setting decides the flag: `always`, the default
and what every existing site keeps, or `never`, for a plain HTTP LAN or
development site only. The session cookie is the only cookie the kernel sets
(CSRF tokens and "remember me" live inside the session), so it is the one
place the setting applies. It is read once at startup.

`never` cannot reach a public site quietly. It logs a warning naming
`COOKIE_SECURE` on every start, the site settings page shows it under "Session
cookie" with a warning, and it is refused at startup when `SITE_URL` is
`https://` or `COOKIE_SAME_SITE` is `none` (browsers reject a `SameSite=None`
cookie that is not `Secure`). Any other value is refused too. There is no
`auto`: the flag is fixed when the session layer is built, and choosing it per
request from the scheme would rest on a forwarded header. `INSTALL.md` has a
"Serving over plain HTTP" section saying when to use `never` and why it is
unsafe on the internet. `HttpOnly` and `SameSite` are unchanged.
