Markdown renders tables, footnotes, strikethrough and task lists. A pipe table
used to come out as one paragraph of pipes, wherever Markdown is rendered: the
`markdown` Tera filter and every page builder `rich_text` field.

The root cause: Markdown was rendered with pulldown-cmark's CommonMark only
defaults. `Parser::new` enables no extensions, and all four are extensions that
must be switched on with `Options`; nobody had decided against them. Markdown now
becomes HTML in one place, `theme::render_markdown`, with one options set
(tables, footnotes, strikethrough, task lists and nothing else), and the page
builder's test copy of the filter calls it instead of repeating the old code.

What the extensions write is shaped to survive the sanitizer without loosening
it. pulldown-cmark writes column alignment as an inline `style`, which the
sanitizer drops and a strict CSP forbids, so a cell carries an `align-left`,
`align-center` or `align-right` class instead. Footnotes are numbered `fn-<n>`
and `fnref-<n>` ids, the only ids the sanitizer admits. A task list marker is a
disabled checkbox `<input>`: the sanitizer forces `disabled` on every input,
keeps `type` only as `checkbox` and allows `checked` and nothing else, so no
input that leaves the renderer can be used. A single
tilde stays a tilde: pulldown-cmark, like the GFM spec, also strikes `~a~`,
which would have struck out "~1,200 lines … ~4%" in existing text. CommonMark
output that uses none of the four is byte for byte what it was.
