-- EXPLAIN ANALYZE of the search statements (B248), for one word, on a site
-- seeded by seed.sh. Run with psql -v author="'<gen_user_1 id>'".
-- `public` is the anonymous shape (the same text on main and the branch),
-- `own` is a signed-in author's (main used it for every signed-in viewer,
-- the branch only for a holder of a permission that shows their drafts),
-- `admin` is the branch's administrator shape (main gave the administrator
-- `own`).
\set q '''river'''
\echo === count, public
EXPLAIN (ANALYZE, BUFFERS) SELECT COUNT(*) FROM item
WHERE search_vector @@ websearch_to_tsquery('english', :q)
  AND status = 1 AND stage_id = ANY(ARRAY['0193a5a0-0000-7000-8000-000000000001']::uuid[]);
\echo === page, public
EXPLAIN (ANALYZE, BUFFERS) SELECT id, ts_rank(search_vector, websearch_to_tsquery('english', :q)) AS rank
FROM item WHERE search_vector @@ websearch_to_tsquery('english', :q)
  AND status = 1 AND stage_id = ANY(ARRAY['0193a5a0-0000-7000-8000-000000000001']::uuid[])
ORDER BY rank DESC, created DESC LIMIT 10;
\echo === count, own
EXPLAIN (ANALYZE, BUFFERS) SELECT COUNT(*) FROM item
WHERE search_vector @@ websearch_to_tsquery('english', :q)
  AND (status = 1 OR author_id = :author) AND stage_id = ANY(ARRAY['0193a5a0-0000-7000-8000-000000000001']::uuid[]);
\echo === count, admin
EXPLAIN (ANALYZE, BUFFERS) SELECT COUNT(*) FROM item
WHERE search_vector @@ websearch_to_tsquery('english', :q)
  AND stage_id = ANY(ARRAY['0193a5a0-0000-7000-8000-000000000001']::uuid[]);
