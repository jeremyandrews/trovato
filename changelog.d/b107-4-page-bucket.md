Page views have a rate-limit bucket of their own. A `GET` or `HEAD` outside
`/api/` that no more specific bucket claims is now a `page` request, 600 a
minute by default, and is configured like every other bucket:
`TROVATO_RATE_LIMIT_PAGE`, `rate_limit.page`, and per role
`TROVATO_RATE_LIMIT_PAGE__<ROLE>`. Such requests used to fall through to the
`api` bucket at 100 a minute, shared with every API call from the same address,
so visitors browsing from a shared address (an office, a school, a carrier NAT)
were served a 429 for reading pages. `/api/*` keeps the `api` bucket. Sign in,
registration, password reset, search, uploads, comment writes and static files
keep their own. A request that is neither a read nor a POST (`PUT`, `DELETE`
outside `/api/`) is still counted as `api`. A site that raised
`TROVATO_RATE_LIMIT_API` to let readers browse can lower it again and set
`TROVATO_RATE_LIMIT_PAGE` instead.
