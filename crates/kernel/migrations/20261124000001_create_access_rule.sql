-- Access rules (B108): block registrations and sign ins by username, email
-- address or client address.
--
-- `pattern` is matched case-insensitively against the whole value, with `%`
-- standing for any run of characters (none included); every other character,
-- `_` too, is literal. An `allow` rule beats a `deny` rule of the same type, so
-- a site can deny `%@example.com` and still allow `editor@example.com`.
--
-- An empty table blocks nothing: every existing site behaves as before.

CREATE TABLE access_rule (
    id UUID PRIMARY KEY,
    -- `username`, `email` or `host`.
    rule_type VARCHAR(16) NOT NULL CHECK (rule_type IN ('username', 'email', 'host')),
    pattern VARCHAR(255) NOT NULL CHECK (pattern <> ''),
    -- `allow` or `deny`.
    status VARCHAR(8) NOT NULL CHECK (status IN ('allow', 'deny')),
    -- Unix seconds.
    created BIGINT NOT NULL
);

CREATE INDEX idx_access_rule_type ON access_rule (rule_type);
