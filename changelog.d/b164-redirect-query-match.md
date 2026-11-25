A redirect's source can now carry a query string. `/article.php?sid=1` and
`/article.php?sid=2` were the same page to `check_redirect`: it matched
`request.uri().path()` alone, so two different legacy pages that only differed
by query could not redirect to two different destinations — only ever one
redirect for `/article.php`, or none, answered every query. A source with a
query now matches only a request carrying that exact query; a request whose
query matches no stored source falls back to a path-only redirect exactly as
before, so an existing site's redirects behave byte for byte as they did.

The query is normalized at write time — its parameters sorted and its
percent-encoding made canonical — and stored in the same `source` column, so
`?a=1&b=2` and `?b=2&a=1` are the same redirect, `q=a+b` and `q=a%20b` are the
same redirect, and matching stays one indexed lookup (two when the request
carries a query: the exact source first, the bare path second). The admin
form and the JSON API (`/admin/config/redirects`, `/api/v1/redirect`, both new
— `trovato_redirects` declared a menu entry and a config table but no route
ever served them) show a source back as it was stored, not as it was typed,
and refuse one with a fragment.
