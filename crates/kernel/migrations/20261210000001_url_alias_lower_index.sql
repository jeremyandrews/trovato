-- no-transaction
-- Case-insensitive alias lookup (B161): `/BLOG/Tillicum` finds the alias
-- `/blog/tillicum` and is redirected to it. The lookup is still one query; this
-- index keeps its case-insensitive half an index scan beside the exact match on
-- `idx_url_alias_alias`.
--
-- CONCURRENTLY, alone in this no-transaction file, so building it does not
-- block writes to `url_alias` for the build's duration.
CREATE INDEX CONCURRENTLY idx_url_alias_alias_lower ON url_alias (lower(alias));
