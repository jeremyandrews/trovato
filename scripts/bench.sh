#!/usr/bin/env bash
# Timing benchmark of a running Trovato server: median and p99 per page.
#
#   scripts/bench.sh [--base-url URL] [--runs N] [--requests N] [--pages "front item ..."]
#                    [--save | --compare] [--label TEXT]
#
# Measures pages of a site made by `trovato generate` (the default seed):
# `front` (the front page), `item` (the newest post with its comments), `feed`,
# `category` (a tag page) and `search` (one word). The default is every page
# but `search`, which takes seconds a request on a 20,000,000 post site; name
# it in --pages to measure it. Each page in turn gets a warm-up, then
# --requests timed requests from 8 workers, --runs times over; the result is
# the median over the runs.
#
#   --save      write the result to benchmarks/baseline.json
#   --compare   show the change against benchmarks/baseline.json
#
# The server is not started here: start it the way you want it measured
# (release build, tracing on or off) against a generated database, and raise
# the page rate limit, which a benchmark would otherwise hit:
#
#   TROVATO_RATE_LIMIT_PAGE=100000000 TROVATO_RATE_LIMIT_STATIC=100000000 \
#   TROVATO_RATE_LIMIT_SEARCH=100000000 \
#     target/release/trovato serve
#
# Timings belong to the machine they were taken on, so this is run by hand,
# never on a pull request. See docs/performance-tests.md.
set -euo pipefail

cd "$(dirname "$0")/.."

base_url="http://127.0.0.1:3000"
runs=3
requests=2000
label="$(git rev-parse --short HEAD 2>/dev/null || echo local)"
mode=""
pages="front item feed category"
while [ $# -gt 0 ]; do
  case "$1" in
    --base-url) base_url="$2"; shift 2 ;;
    --runs) runs="$2"; shift 2 ;;
    --requests) requests="$2"; shift 2 ;;
    --label) label="$2"; shift 2 ;;
    --pages) pages="$2"; shift 2 ;;
    --save) mode=save; shift ;;
    --compare) mode=compare; shift ;;
    -h|--help) sed -n '2,/^set -euo/{/^set -euo/d;s/^# \{0,1\}//;p;}' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# The newest generated post: its id is md5('<seed>:item:1') read as a uuid.
if command -v md5sum >/dev/null; then
  hash=$(printf '1:item:1' | md5sum | cut -c1-32)
else
  hash=$(md5 -qs '1:item:1')
fi
item="${hash:0:8}-${hash:8:4}-${hash:12:4}-${hash:16:4}-${hash:20:12}"

if ! curl -sf "$base_url/health/live" >/dev/null; then
  echo "no Trovato server answers at $base_url" >&2
  exit 1
fi

baseline=benchmarks/baseline.json
args=(--base-url "$base_url" --label "$label" --runs "$runs" --requests "$requests")
for page in $pages; do
  case "$page" in
    front) args+=(--page "front=/") ;;
    item) args+=(--page "item=/item/$item") ;;
    feed) args+=(--page "feed=/blog/feed.xml") ;;
    category) args+=(--page "category=/blog/tag/topic-1") ;;
    search) args+=(--page "search=/search?q=river") ;;
    *) echo "unknown page: $page (front, item, feed, category, search)" >&2; exit 2 ;;
  esac
done
case "$mode" in
  save) args+=(--out "$baseline") ;;
  compare) args+=(--baseline "$baseline") ;;
esac

cargo run --quiet --release -p trovato-loadtest --bin bench -- "${args[@]}"
