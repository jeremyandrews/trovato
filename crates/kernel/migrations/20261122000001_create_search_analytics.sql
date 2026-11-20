-- Search analytics: query counts per UTC day, and per day counters.
--
-- Nothing here identifies a person: no IP address, user, session, user agent,
-- referrer or time finer than the day. The query is stored after
-- `search::analytics::normalize_for_analytics` (lowercased, email addresses and
-- runs of six or more digits replaced, at most 100 characters). Rows older than
-- the search settings' `analytics_retention_days` are deleted by cron.

CREATE TABLE search_query_daily (
    day date NOT NULL,
    query text NOT NULL,
    searches integer NOT NULL DEFAULT 0,
    zero_results integer NOT NULL DEFAULT 0,
    PRIMARY KEY (day, query)
);

CREATE TABLE search_daily (
    day date PRIMARY KEY,
    expansion_hits integer NOT NULL DEFAULT 0,
    expansion_misses integer NOT NULL DEFAULT 0,
    satisfied integer NOT NULL DEFAULT 0,
    confused integer NOT NULL DEFAULT 0,
    unmet integer NOT NULL DEFAULT 0
);
