`full_html`, the text format with no filter configured, was never rendered
from stored data: every view path downgraded it to `plain_text`, so a body in
`full_html` showed as escaped text instead of the rich HTML the `use full_html`
permission was meant to let an author write. The downgrade existed because the
format's filter was genuinely raw — whatever an author wrote came back
unchanged — and no render path could tell a value an author wrote in `full_html`
from one that reached storage some other way holding that format name.

A format with no filter configured now renders as rich HTML, cleaned by one
fixed, permissive sanitizer (`TrustedHtmlFilter`, built on ammonia) rather than
escaped or passed through raw. It keeps ordinary article markup — `div`,
`span`, `figure`, tables, headings, lists, images, `class` and `id` on every
tag, `target` on links — and always strips `<script>`, `<style>`, every `on*`
event handler attribute and a `javascript:` or `data:` URL, whatever wrote the
value and however it reached storage. `FilterPipeline::for_format_safe`, which
existed only to downgrade an unsafe format, is gone: `FilterPipeline::for_format`
is now safe to call with any format string, including one a user, a plugin, or
stored data supplies, since every pipeline it builds is sanitized before it is
returned. A `custom_html` tile's body goes through the same sanitized path as
every other render, rather than trusting the tile's own `has_full_html = true`.
Theme templates no longer need `| safe` on a `text_format`-filtered value: the
filter now reports its output as already safe, since it is sanitized by
construction.

`plain_text` and `filtered_html` render exactly as before.
