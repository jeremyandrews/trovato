Trovato's performance claim has a written plan now: `docs/performance/PLAN.md`
sets per-scenario latency and throughput targets at 10 thousand, 1 million and
10 million items, names `scripts/bench.sh` and the query count fixtures as the
one way to measure them, states the guardrails a performance change follows,
reads where today's time goes in the item, gather, search, session and save
paths, and ranks the first ten fixes. No code changes with it.
