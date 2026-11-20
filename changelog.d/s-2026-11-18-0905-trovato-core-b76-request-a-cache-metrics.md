`/metrics` now counts the cache hits and misses and the HTTP requests it has
always advertised. `cache_hits_total` and `cache_misses_total` carry a `cache`
label, one series per kernel cache (`item`, `item_stage`, `stage_view`,
`field_access`, `user`, `permissions`, `content_type`, `gather_query`,
`gather_distinct`, `category`, `category_tags`, `menu`, `locale`,
`template_suggestion`, and `layer_l1` and `layer_l2` for the two tiers of the
cache layer), and `http_requests_total` and `http_request_duration_seconds` are
recorded for every request, labelled by method, status and the matched route
template, never the raw path.

The root cause was wiring, not counting: the series were registered with the
Prometheus registry, and so exported, but `record_cache_hit`, `record_cache_miss`
and `record_request` had no caller anywhere in the kernel, and none of the
services that own a cache held the metrics handle at all. Every one of them read
zero on a busy site. Each cache-owning service now takes the one `Metrics` from
`AppState` when it is built, and every read goes through a single helper,
`Metrics::cache_lookup`; the request series are recorded by the timing middleware
that already measured the request. The upload, rate-limit and active-connection
series are still registered and never recorded.
