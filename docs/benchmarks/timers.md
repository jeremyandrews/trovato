# Timers benchmark (B278)

`crates/kernel/tests/timer_bench_test.rs`, ignored by default:

```sh
cargo test -p trovato-kernel --test timer_bench_test -- --ignored --nocapture
```

Measured on 2026-11-27 on an Apple M3 Ultra (32 cores), the kernel's debug
test build, against PostgreSQL 16.16 in a local Docker container, the
database otherwise idle.

## What it does

1. Loads 10,000 timers: 100 due a minute ago, 9,900 spread over the next day.
   `ANALYZE`.
2. `EXPLAIN (ANALYZE, BUFFERS)` of the two statements the scheduler runs,
   `timer::CLAIM_SQL` (limit 4, the default per-server concurrency) and
   `timer::NEXT_DUE_SQL`, unscoped as in production. Then each 200 times, each
   in a rolled back transaction, after 20 to warm up.
3. **Wake latency.** Two schedulers run, sleeping toward a timer an hour away.
   Thirty times, another connection sets a timer due now (through
   `timer::set`, so no in-process wake: only the `NOTIFY` reaches the loops);
   the time from the call to the handler starting.
4. **Lateness.** 1,000 timers due over ten seconds, 10 ms apart, among 9,000
   later ones (10,000 in all), run by the two schedulers with a no-op handler:
   each fire's time from its due second to its handler starting, and how many
   times each fired.

## Results

| Measure | n | p50 | p95 | max |
|---|---|---|---|---|
| Claim query (10,000 rows, 100 due, limit 4) | 200 | 0.29 ms | 0.45 ms | 0.96 ms |
| Next-due query (10,000 rows) | 200 | 0.16 ms | 0.19 ms | 0.54 ms |
| Wake latency, set due now by another connection, to handler start | 30 | 19.27 ms | 22.79 ms | 29.62 ms |
| Lateness, 1,000 due over 10 s among 10,000, due to handler start | 1,000 | 6.18 ms | 7.58 ms | 26.43 ms |

All 1,000 fired, none more than once: 471 on one server, 529 on the other.

The wake latency includes the setting transaction's commit, the `NOTIFY`
delivery, the loop's next-due read and the claim. Lateness is lower than
wake latency because a loop already awake for the previous timer is
sleeping toward the next one, so no notification is involved.

## Plans

The claim reads four rows of the due index and locks them; the lease scan is
never executed because the due rows fill the limit. Each query reads a
handful of buffers whatever the table's size.

```text
--- EXPLAIN claim (limit 4, unscoped) ---
Update on scheduled_timer t (actual time=0.098..0.122 rows=4 loops=1)
  Buffers: shared hit=63 dirtied=1
  CTE due
    ->  Limit (actual time=0.018..0.020 rows=4 loops=1)
          Buffers: shared hit=7
          ->  LockRows (actual time=0.018..0.020 rows=4 loops=1)
                Buffers: shared hit=7
                ->  Index Scan using scheduled_timer_due_idx on scheduled_timer (actual time=0.011..0.012 rows=4 loops=1)
                      Filter: ((status = 'scheduled'::text) AND (run_at <= clock_timestamp()))
                      Buffers: shared hit=3
  CTE stale
    ->  Limit (never executed)
          ->  LockRows (never executed)
                ->  Index Scan using scheduled_timer_lease_idx on scheduled_timer scheduled_timer_1 (never executed)
                      Filter: ((status = 'running'::text) AND (claimed_until < clock_timestamp()))
  ->  Nested Loop (actual time=0.053..0.063 rows=4 loops=1)
        Buffers: shared hit=19
        ->  Subquery Scan on d (actual time=0.024..0.028 rows=4 loops=1)
              Buffers: shared hit=7
              ->  Limit (actual time=0.021..0.024 rows=4 loops=1)
                    Buffers: shared hit=7
                    ->  Append (actual time=0.020..0.023 rows=4 loops=1)
                          Buffers: shared hit=7
                          ->  CTE Scan on due (actual time=0.020..0.022 rows=4 loops=1)
                                Buffers: shared hit=7
                          ->  CTE Scan on stale (never executed)
        ->  Index Scan using scheduled_timer_pkey on scheduled_timer t (actual time=0.001..0.001 rows=1 loops=4)
              Index Cond: (id = d.id)
              Buffers: shared hit=12
Planning:
  Buffers: shared hit=169
Planning Time: 0.214 ms
Execution Time: 0.174 ms
```

```text
--- EXPLAIN next due (unscoped) ---
Result (actual time=0.037..0.037 rows=1 loops=1)
  Buffers: shared hit=5
  InitPlan 2 (returns $1)
    ->  Result (actual time=0.015..0.015 rows=1 loops=1)
          Buffers: shared hit=3
          InitPlan 1 (returns $0)
            ->  Limit (actual time=0.014..0.014 rows=1 loops=1)
                  Buffers: shared hit=3
                  ->  Index Only Scan using scheduled_timer_due_idx on scheduled_timer (actual time=0.014..0.014 rows=1 loops=1)
                        Index Cond: (run_at IS NOT NULL)
                        Heap Fetches: 1
                        Buffers: shared hit=3
  InitPlan 3 (returns $3)
    ->  Aggregate (actual time=0.015..0.015 rows=1 loops=1)
          Buffers: shared hit=2
          ->  Index Only Scan using scheduled_timer_lease_idx on scheduled_timer scheduled_timer_1 (actual time=0.015..0.015 rows=0 loops=1)
                Heap Fetches: 4
                Buffers: shared hit=2
Planning:
  Buffers: shared hit=156
Planning Time: 0.268 ms
Execution Time: 0.115 ms
```

## Raw output

```text
scheduled_timer rows: 10000
claim query: n=200 p50=0.29 ms p95=0.45 ms max=0.96 ms
next-due query: n=200 p50=0.16 ms p95=0.19 ms max=0.54 ms
wake latency (set due now by another connection, to handler start): n=30 p50=19.27 ms p95=22.79 ms max=29.62 ms
timers for the lateness run: 10000
fires: 1000 for 1000 timers, 0 fired more than once; server A 471, server B 529
lateness (1,000 due over 10 s among 10,000; due to handler start): n=1000 p50=6.18 ms p95=7.58 ms max=26.43 ms
```
