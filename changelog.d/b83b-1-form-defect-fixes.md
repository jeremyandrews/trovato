Five forms that were refused, unprotected or posting the wrong field are fixed,
ahead of moving every form onto the one form pipeline.

The Book admin screens (create a book, place a page, remove a page) are served
by the `trovato_book` plugin as hand written HTML, and none of their three forms
carried a CSRF token. The kernel checks a plugin route's POST for a `_token`
field or an `X-CSRF-Token` header before the plugin sees it, so every
submission from a browser was a 403; the plugin's tests posted with the header,
which no HTML form can send, and so never saw it. Each form now carries the
token the kernel hands the plugin in `ApiRequest::csrf_token`.

The comment form, the reply page and the edit and delete pages posted their
token in `_csrf`, the only forms on the site that did not use `_token`. They
post `_token` now, and the handlers read it from there, so there is one CSRF
field name everywhere. A page served before the upgrade and submitted after it
is refused once as an expired form.

The account recovery page minted a CSRF token and sent it with every step, but
none of the four steps (`/user/recover/start`, `choose`, `verify`, `reset`)
checked it. They do now, through one gate in front of the four, and since tokens
are single use every answer past the gate carries the next one in its
`X-CSRF-Token` header, which the page takes for its next step. The page's
script also never ran in a browser: it was inline, and the site's
Content-Security-Policy allows no inline script, so every button on the page
did nothing. It is a static file now, `static/js/account-recover.js`, reading
the token from the page's `csrf-token` meta tag.

The install wizard had no CSRF protection at all, and its site step, which
names the site and closes the installer, could be finished by anyone who
reached it after the administrator was created. Both forms now carry and check
a token. The site step belongs to the browser session that created the
administrator; any other session is asked for that administrator's username
and password on the same form before it may finish. The administrator step also
no longer creates a second administrator when one already exists: it goes on to
the site step instead.

The login page's inline HTML fallback for a template that fails to render was
already replaced by the standard server error page in B84, so it needed no
change here.
