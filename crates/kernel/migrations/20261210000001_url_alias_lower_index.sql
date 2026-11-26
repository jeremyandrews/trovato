-- Case-insensitive alias lookup (B161): `/BLOG/Tillicum` finds the alias
-- `/blog/tillicum` and is redirected to it. The lookup is still one query; this
-- index keeps its case-insensitive half an index scan beside the exact match on
-- `idx_url_alias_alias`.
CREATE INDEX IF NOT EXISTS idx_url_alias_alias_lower ON url_alias (lower(alias));
