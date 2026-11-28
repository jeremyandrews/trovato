The AI search endpoints trusted the browser for the content and size of the
prompt they sent to the configured model: `summarize` and `followup` took free
text (`context`, `excerpts`, a conversation history) straight from the request
and formatted it into the prompt with no retrieval, no access check and no
length cap, and a follow-up's turn cap counted only a role of exactly `user`,
so any other role reached the model uncounted. The three endpoints now build
their own context: `summarize` and `followup` take search result URLs, resolve
each one to an item and read only the title and fields the calling viewer may
see, cut to the configured summary length; a follow-up message's role must be
`user` or `assistant`; a query over the kernel's usual length limit is refused
on all three endpoints; and each route carries a 64 KiB request body limit. An
anonymous visitor's AI search spend, previously unlimited on a site that never
configured the anonymous role's budget, now has a default cap. In the browser,
an AI summary's Markdown links render only for an `http(s)` or same-site URL,
so a planted link cannot carry a `javascript:` or other unsafe scheme.
