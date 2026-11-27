-- The 404 log (B306): every address a visitor requested that the site could
-- not find, aggregated per path and language so a crawl of thousands of
-- distinct broken links costs one row each rather than one per hit.
--
-- No IP address, user id, user agent or query string: this is a report for
-- fixing broken links, not a visitor log.
CREATE TABLE IF NOT EXISTS not_found_log (
    path VARCHAR(2048) NOT NULL,
    language VARCHAR(12) NOT NULL,
    referrer VARCHAR(2048),
    hits BIGINT NOT NULL DEFAULT 1,
    first_seen BIGINT NOT NULL,
    last_seen BIGINT NOT NULL,
    PRIMARY KEY (path, language)
);

CREATE INDEX IF NOT EXISTS idx_not_found_log_hits ON not_found_log (hits);
CREATE INDEX IF NOT EXISTS idx_not_found_log_last_seen ON not_found_log (last_seen);
