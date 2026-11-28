`full_html` content migrated from an older site, written with presentational
HTML instead of CSS classes, used to come out stripped: ammonia's
`TRUSTED_HTML_BUILDER` kept only its defaults plus `class`, `id` and
`a[target]`, so a coloured interview question in a `<font>` tag, a `name`
anchor, an image's `border`/`hspace`/`vspace`, a table's presentation
attributes and every `style` attribute were all removed on render, pushing
sites toward `| safe` to get their content back. `full_html` now keeps those
tags and attributes, and keeps a `style` attribute too, but only the
properties on a small allowlist (`color`, layout and spacing properties, not
`position` or anything that can reach outside the element). A style
declaration is still dropped outright if its value contains `url(`,
`expression(`, a `javascript:` scheme, a backslash escape or a `/*` comment,
so none of the newly kept attributes open a path for a `<script>`, an event
handler or a dangerous URL, which full_html still strips as it always did.
