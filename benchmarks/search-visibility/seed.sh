#!/usr/bin/env bash
# Seed a search benchmark site (B248): `trovato generate` blog posts, then
# turn about 15 percent unpublished and 5 percent onto an internal stage, and
# give `gen_user_1` the permission that shows an author their own drafts.
#
#   DATABASE_URL=postgres://trovato:trovato@127.0.0.1:55248/search_bench \
#   PSQL="psql postgres://trovato:trovato@127.0.0.1:55248/search_bench" \
#   TROVATO=target/release/trovato ITEMS=60000 \
#   benchmarks/search-visibility/seed.sh
#
# Run from the repository root (the plugin install reads `plugins/`). It only
# writes to a database whose name starts with `search_bench`.
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL}"
: "${PSQL:?set PSQL to a psql command connected to the benchmark database}"
TROVATO="${TROVATO:-target/release/trovato}"
ITEMS="${ITEMS:-60000}"
PASSWORD="${BENCH_PASSWORD:-bench-password-1234}"

case "${DATABASE_URL##*/}" in
  search_bench*) ;;
  *) echo "refusing: the database name must start with search_bench" >&2; exit 2 ;;
esac

"$TROVATO" migrate
"$TROVATO" plugin install trovato_blog
"$TROVATO" plugin enable trovato_blog
"$TROVATO" migrate
"$TROVATO" generate --items "$ITEMS" --comments 0 --tags 24 --users 20 \
  --admin-password "$PASSWORD"

$PSQL -v ON_ERROR_STOP=1 <<'SQL'
-- An internal stage (the column default) for 5 percent of the posts.
INSERT INTO category_tag (id, category_id, label, weight, created, changed)
SELECT '0199aaaa-0000-7000-8000-000000000248', 'stages', 'Bench review', 10,
       extract(epoch FROM now())::bigint, extract(epoch FROM now())::bigint
WHERE NOT EXISTS (SELECT 1 FROM category_tag WHERE id = '0199aaaa-0000-7000-8000-000000000248');
INSERT INTO stage_config (tag_id, machine_name)
SELECT '0199aaaa-0000-7000-8000-000000000248', 'bench_review'
WHERE NOT EXISTS (SELECT 1 FROM stage_config WHERE tag_id = '0199aaaa-0000-7000-8000-000000000248');

-- Row number order is the generator's: deterministic for the same seed.
WITH numbered AS (
  SELECT id, row_number() OVER (ORDER BY id) AS n FROM item WHERE type = 'blog'
)
UPDATE item SET
  status = CASE WHEN numbered.n % 20 IN (1, 2, 3) THEN 0 ELSE item.status END,
  stage_id = CASE WHEN numbered.n % 20 = 4 THEN '0199aaaa-0000-7000-8000-000000000248'::uuid
                  ELSE item.stage_id END
FROM numbered WHERE numbered.id = item.id;

-- gen_user_1 may view their own unpublished posts.
INSERT INTO roles (id, name) VALUES ('0199aaaa-0000-7000-8000-000000000249', 'bench author')
ON CONFLICT (name) DO NOTHING;
INSERT INTO role_permissions (role_id, permission)
VALUES ('0199aaaa-0000-7000-8000-000000000249', 'view own unpublished content')
ON CONFLICT DO NOTHING;
INSERT INTO user_roles (user_id, role_id)
SELECT id, '0199aaaa-0000-7000-8000-000000000249' FROM users WHERE name = 'gen_user_1'
ON CONFLICT DO NOTHING;

ANALYZE item;
SELECT status, stage_id = '0199aaaa-0000-7000-8000-000000000248' AS internal, count(*)
FROM item GROUP BY 1, 2 ORDER BY 1, 2;
SQL
