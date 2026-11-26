Languages are managed from the admin and served the moment they change, and the
default language's own prefix (`/en/...`) works.

The site's languages were read once at startup, from configuration only: there
was no list of them in the admin, no way to add one, and a language added
behind the server's back was not served until every server restarted.
`/admin/config/languages` lists them in order with their address prefixes, adds
one (from a list of common languages, or with a code of the site's own,
private use `x-...` codes included), edits its name, direction, weight and
whether it is the default, and deletes one that nothing is written in. The
language set now lives in `AppState` behind a lock (`LanguageSet`) and is
replaced whole on every change; the change also bumps a new `languages` cache
kind, so every other server reads it again within a poll. It takes the new
`administer languages` permission.

`/en/...` was a 404, because the URL prefix negotiator and the alias fallback
both skipped the default language's code, so every old link that carried it
was broken. Both read it now. A new setting on the languages page decides what
happens next: off (the default), `/en/about` is a permanent redirect to
`/about`, so the page keeps one address; on, it is served, and the language
switcher, menus, hreflang links and the sitemap write the default language's
pages under `/en` too.
