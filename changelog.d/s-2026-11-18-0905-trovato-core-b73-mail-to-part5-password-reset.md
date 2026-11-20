Password reset by email is a flow a person can finish in a browser:
`/user/password-reset` is a page asking for an email address, and the link the
mail carries opens a set-password page that, on success, signs out every
device and lands on the login page with "Your password has been reset". The
login page's "Forgot password?" link points at it. **Behaviour change:** `GET`
on the reset link answers HTML unless the request sends `Accept:
application/json`; `POST` with a JSON body keeps its JSON answers (BL-07).

The root cause was that both steps were JSON endpoints and nothing rendered
them, so the link in the mail returned JSON to whoever clicked it. The pages
are plain forms with CSRF tokens and no inline script or style, so they work
under the enforcing CSP; the set-password page is sent with
`Referrer-Policy: no-referrer` because the token is in its URL. The request
page keeps the existing limits, the spawned send and the identical answer for
every address. Two defects found on the way are fixed: the token was marked
used only after the password changed, so two concurrent posts of one token
both succeeded (it is now spent first, in one `UPDATE ... RETURNING`); and the
reset mail was built by an untemplated sender while a `password_reset` email
template sat unused (the mail now uses the template, and the untemplated
sender is removed). A third: the security headers middleware overwrote every
response's `Referrer-Policy` with the site default, so a route's
`no-referrer` never reached the browser. It now keeps a route's
`no-referrer`, the one stricter value; this also makes the unsubscribe page's
`no-referrer` (BL-94) take effect.
