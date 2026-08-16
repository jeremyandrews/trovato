#!/usr/bin/env bash
# argus-m1 M1-12: live smoke run.
#
# Drives POST /cron/{key} against a handful of REAL public feeds end to end on
# the local kernel (Postgres + Redis), then prints ingest counts and the decide
# cost per 100 articles read from the kernel's ai_usage_log.cost_estimate.
#
# Prerequisites:
#   - docker compose up -d           (Postgres + Redis)
#   - cargo build --release --bin trovato && cargo run --release --bin trovato &
#     (server on http://localhost:3000, installer completed once)
#   - the argus wasm built + copied:
#       cargo build -p argus --target wasm32-wasip1 --release \
#         && cp target/wasm32-wasip1/release/argus.wasm plugins/argus/
#
# AI provider (optional): the decide stage calls the kernel ai-request host fn.
# Configure an OpenAI-compatible provider + default in site_config and set the
# ARGUS_DECIDE_MODEL variable for real decide/cost numbers. WITHOUT a provider,
# fetch/ingest/dedup still run against real feeds and are reported honestly;
# decide jobs dead-letter (no numbers faked). NOTE: a *local* stub provider does
# not help — the SSRF fence blocks loopback, so the AI path can only be smoke-
# tested against a real external provider (reported friction G-HTTP-META /
# G-SSRF-LOCAL).
set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:3000}"
CRON_KEY="${CRON_KEY:-default-cron-key}"
PSQL="$(brew --prefix libpq)/bin/psql"
DB="${DATABASE_URL:-postgres://trovato:trovato@localhost:5432/trovato}"
CYCLES="${CYCLES:-8}"

# A few real, public, high-volume feeds (public IPs → not SSRF-blocked).
FEEDS=(
  "https://hnrss.org/frontpage|Hacker News"
  "https://feeds.arstechnica.com/arstechnica/index|Ars Technica"
  "https://www.theverge.com/rss/index.xml|The Verge"
  "https://feeds.bbci.co.uk/news/technology/rss.xml|BBC Tech"
)

say() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }

say "health check"
curl -fsS "$BASE_URL/health" >/dev/null && echo "server up at $BASE_URL"

say "seed topic + feeds"
TOPIC_ID=$("$PSQL" "$DB" -tAc "
  INSERT INTO argus_topics (id, name, relevance_prompt, relevance_threshold, enabled, created, changed)
  VALUES (gen_random_uuid(), 'ai-and-tech',
          'Is this article about artificial intelligence, machine learning, or the technology industry?',
          40, true, EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
  ON CONFLICT (name) DO UPDATE SET changed = EXCLUDED.changed
  RETURNING id;" | head -n1 | tr -d '[:space:]')
echo "topic: $TOPIC_ID"

for entry in "${FEEDS[@]}"; do
  url="${entry%%|*}"; name="${entry##*|}"
  "$PSQL" "$DB" -tAc "
    INSERT INTO argus_feeds (id, url, name, topic_id, fetch_interval_seconds, enabled, created, changed)
    VALUES (gen_random_uuid(), '$url', '$name', '$TOPIC_ID'::uuid, 0, true,
            EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
    ON CONFLICT (url) DO UPDATE SET enabled = true, last_fetched_at = NULL;" >/dev/null
  echo "feed: $name  ($url)"
done

say "drive cron ($CYCLES cycles)"
for i in $(seq 1 "$CYCLES"); do
  out=$(curl -fsS -X POST "$BASE_URL/cron/$CRON_KEY")
  echo "cycle $i: $out"
  sleep 2
done

say "ingest counts (by pipeline_state)"
"$PSQL" "$DB" -c "
  SELECT pipeline_state, count(*) AS n
  FROM argus_articles GROUP BY pipeline_state ORDER BY n DESC;"

TOTAL=$("$PSQL" "$DB" -tAc "SELECT count(*) FROM argus_articles;")
DECIDED=$("$PSQL" "$DB" -tAc "SELECT count(*) FROM argus_articles WHERE relevance_score IS NOT NULL;")
DISCARDED=$("$PSQL" "$DB" -tAc "SELECT count(*) FROM argus_articles WHERE pipeline_state = 'discarded';")
echo "total ingested: $TOTAL | decided (scored): $DECIDED | discarded: $DISCARDED"

say "decide cost from ai_usage_log (kernel cost_estimate)"
"$PSQL" "$DB" -c "
  SELECT count(*)                                   AS calls,
         COALESCE(sum(total_tokens), 0)             AS tokens,
         COALESCE(sum(cost_estimate), 0)            AS cost_usd
  FROM ai_usage_log WHERE plugin_name = 'argus';" || true

if [ "${DECIDED:-0}" -gt 0 ]; then
  "$PSQL" "$DB" -c "
    SELECT round(100.0 * COALESCE(sum(cost_estimate),0) / NULLIF($DECIDED,0), 6) AS cost_per_100_articles_usd
    FROM ai_usage_log WHERE plugin_name = 'argus';"
else
  echo "no articles were decided (no AI provider configured, or none reachable)."
  echo "decide cost per 100 = 0.00 (not faked; the decide stage produced no successful calls)."
fi

say "DLQ (dead decide jobs, if any)"
"$PSQL" "$DB" -c "
  SELECT count(*) AS dead_jobs FROM plugin_queue
  WHERE plugin_name = 'argus' AND status = 'dead';"

echo
echo "smoke run complete."
