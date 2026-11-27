The three AI search endpoints trusted the browser for the content and size of
the prompt they sent to the site's model provider. `summarize` and `followup`
formatted whatever text the client supplied straight into the prompt, with no
server-side retrieval and no length cap, so a caller could spend the site's
provider budget on text of its own choosing; `followup`'s turn cap counted
only messages whose role was literally `user`, so any other role bought
unlimited turns; and an anonymous role with no configured AI token budget read
as unlimited, the per-IP rate limiter being the only brake on its spend.

`summarize` and `followup` now take result URLs instead of text. The server
resolves each URL to an item on the Live stage, runs it through the item- and
field-level access check every other read path uses, and builds the prompt
itself from the title and the fields the calling viewer may see, each item cut
to the configured `ai_summary_max_chars`. An unpublished item, or a field the
viewer may not see, contributes nothing. `followup` accepts only the one
request shape the client actually sends, refuses a message whose role is not
`user` or `assistant`, and bounds the whole conversation's character count, not
just the per-message one. All three endpoints refuse a query over the existing
search length limit and a request body over 64 KiB. An anonymous role with no
configured AI budget now gets a default cap instead of unlimited spend; an
explicit configured limit, including an explicit `0` meaning unlimited, is
unaffected. The browser now renders an AI answer's links only when they point
at `http`, `https` or a same-site path, closing off a planted `javascript:` or
`data:` link reaching a visitor through prompt injection.
