Every AI call is now recorded and held to a budget in one place: the send path
on `AiProviderService` that every call already goes through. It checks the
caller's budget before the request leaves and writes the `ai_usage_log` row,
with the prompt and completion tokens the provider reported and a cost
estimate, after it returns, including for embeddings and for a streamed chat,
whose row is written when the stream reports its usage. The per-route copies of
both are gone.

Several paths spent provider money without leaving a trace. AI search's
expansion, summary and follow-up recorded nothing and checked no budget, and
neither did the sentiment classifier or the kernel's embedding of items and
semantic filter queries. The writing assistant (`/api/v1/ai/assist`) checked no
budget and discarded the error if its usage write failed. All of them now record
real usage, and a failed write is logged.

Whose budget a call counts against follows from whose call it is. A signed-in
person's calls count against their per-user or per-role token budget, now also
for the writing assistant and AI search. A plugin's background calls count
against the plugin's token and currency caps, as before. Anonymous visitors
share one site-level cap: the `anonymous user` role's limit on the AI budget
screen (`/admin/system/ai-budgets`), metered over every call made for an
anonymous visitor, which is now recorded under the anonymous user's id rather
than with no user at all. That limit used to be ignored, because the budget
check treated an anonymous caller as unlimited. The sentiment classifier counts
against the anonymous cap too. A spent budget is a 429 from AI search, the
writing assistant and the chatbot, and `ERR_AI_BUDGET_EXCEEDED` to a plugin.

The AI search endpoints now ask for `use ai`, as every other AI route does;
before, anyone who could reach them could spend the site's tokens. So that no
site loses AI search on upgrade, a migration grants `use ai` to every role that
already holds `search content`, which in a default install is the anonymous and
the authenticated role. `use ai` alone sends nothing anywhere else: the chatbot,
the writing assistant and the AI Assistant also ask for `use ai chat` or
`use ai assistant`. One visible effect: the `trovato_ai` plugin's field rules
check `use ai` before running at item save, so on a site that has configured
field rules, a member without `use ai chat` now has the plugin attempt the call
and be refused at the permission gate, instead of being skipped silently.

The writing assistant has its own rate limit bucket, `ai_assist`, at 10
requests a minute, instead of sharing the generic `api` bucket at 100. The
chatbot and the AI Assistant already counted their hourly limits in the shared
Redis limiter.

`trovato_circuit_breaker_ai` now carries an `operation` label and reports every
per-operation breaker the send path uses (`chat`, `embedding` and the rest),
beside `connection_test`, the breaker the admin connection test uses, which was
the only one it reported before. The per-operation breakers are what open when
a provider fails during real traffic, so the gauge read closed while chat was
being refused. A query that selected the unlabelled series still matches; one
that expects a single series now gets one per breaker.
