A browser is shown every error as one page: the site's themed `error.html`
(with `error--{status}.html` tried first) inside the normal page wrapper, the
admin wrapper on an admin path, with a link to `/` or `/admin`. Any other
caller gets the `AppError` JSON body. Nobody signed in is sent to the login
form.

There were five error pages. An unknown path answered `404` with an empty
body; `/item/{id}` and the item add and edit forms answered a browser with
JSON; the gather pages and the four error helpers wrote their own unthemed
HTML, whose "Go back" link was a `javascript:` URL the CSP blocks; and some
thirteen handlers carried a hand-written "server error" page for when a
template failed to render.

The four helpers and `AppError` now attach an `ErrorPage` marker (status and a
message safe to show) instead of deciding what the error looks like. One outer
layer, `middleware::error_page::render_error_pages`, renders a marked response
from the request's `Accept`: `text/html` gets the themed page, everything else
the `AppError` body. Axum's own plain-text refusals (a body that does not
parse, a wrong method) are treated the same. A plugin's own response body is
left alone. The thirteen fallbacks are gone; when the theme itself cannot
render, the layer's one built-in page remains. The 404 page offers a search
box on the front end.

`tests/error_page_test.rs` covers the unknown path, an unknown admin path, a
missing item, the item forms, a missing gather and an admin `403` for a
browser, the login redirect, and the `AppError` body for every other caller.

Contract: themes may add `error.html` or `error--{status}.html`. A script that
calls an HTML page and fails now gets `AppError` JSON instead of HTML; see
`UPGRADING.md`.
