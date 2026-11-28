# Timers

A timer runs work at a known second: publish this item at 09:00:00, close
this poll at midnight, send this reminder every hour. Kernel code and plugins
set a timer for a Unix second, optionally repeating at a fixed interval, and
the kernel calls its handler at that second, once, however many servers run
the site.

Use cron for routine sweeps that run on an interval whatever is due (cleanup,
reindexing, a digest). Use a timer when something is due at a time you know
in advance and should happen at that time, not up to five minutes later.

Nothing runs while no timer is due: each server's scheduler sleeps until the
earliest timer, wakes when one is set earlier, and otherwise reads the table
once a minute.

## From a plugin

Declare the interface and the tap in the manifest:

```toml
taps = ["tap_timer"]

[capabilities]
host_interfaces = ["timer"]
```

A plugin that does not declare `timer` has none of the functions linked, and
one that imports them without declaring it fails to load.

Set, read and cancel timers by name. The owner is always the calling plugin:
a plugin sees, replaces and cancels only its own timers.

```rust
use trovato_sdk::host;
use trovato_sdk::types::{TimerInput, TimerRequest};

// Once, at a second.
host::timer_set(&TimerRequest {
    name: format!("close:{poll_id}"),
    run_at: closes_at,            // Unix seconds
    repeat_secs: None,
    payload: serde_json::json!({ "poll": poll_id }),
})?;

// Every hour, starting at the next whole hour.
host::timer_set(&TimerRequest {
    name: "hourly-reminder".into(),
    run_at: next_hour,
    repeat_secs: Some(3600),
    payload: serde_json::json!({}),
})?;

let info = host::timer_get("hourly-reminder")?;   // Option<TimerInfo>
let was_there = host::timer_cancel(&format!("close:{poll_id}"))?;  // bool
```

Setting a name that exists replaces it: a new time, repeat and payload, its
attempts reset. That is how a changed date moves a timer. A timer an
administrator paused stays paused when its owner sets it again. Replacing a
timer adds none, so it is never refused for the per-plugin limit: an owner
over a limit the site lowered can still move the timers it holds, and only a
new name is refused.

When it is due the kernel calls `tap_timer` on the owner:

```rust
#[plugin_tap]
pub fn tap_timer(input: TimerInput) -> serde_json::Value {
    // input.name, input.payload, input.run_at (when it was due),
    // input.fired_at (when it was claimed), input.repeat_secs, input.attempt
    match close_poll(&input.payload) {
        Ok(()) => serde_json::json!({}),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}
```

`tap_timer` runs like `tap_cron`, with no user, under the background wall
clock limit. Returning an object with an `error` string, or trapping, is a
failure.

### What happens next

- **A one-time timer** that succeeds is deleted.
- **A repeating timer** that succeeds is due again at its next slot: the
  first `run_at + n × repeat_secs` after now. Its cadence does not drift with
  how long the handler took, and after an outage it fires once, not once per
  missed slot.
- **A failure** is retried after 30 seconds, then after a minute; the third
  failure leaves the timer `failed`, with the error, on the timers report,
  where an administrator can resume it or run it now.

### Exactly once

Every server runs a scheduler. A due timer is claimed in one statement
(`FOR UPDATE SKIP LOCKED`), so two servers never both claim it, and each
claim carries a token: the result of a run is recorded only if the timer was
not replaced, paused or cancelled while it ran. A server that dies while a
handler runs leaves a claim that lapses after the background wall clock limit
(16 minutes by default), and another server runs it again. A server shutting
down starts nothing it has not started: claims it took as the shutdown came
are given back at once, uncounted, for another server to fire.

The one way a handler runs twice is that last one: the server dies after the
handler's work is done but before its result is recorded. Write handlers that
can run twice for the same timer. `trovato_scheduled_publishing` does it by
claiming the item with a conditional update, so a second run finds nothing
to do.

### Errors

| Code | Constant | Meaning |
|------|----------|---------|
| -95 | `ERR_TIMER_INVALID` | The name is empty, longer than 255 bytes or has a control character; or the time is not a valid second |
| -96 | `ERR_TIMER_PAYLOAD_TOO_LARGE` | The payload's JSON is over the site's limit (4096 bytes by default, at most 65536) |
| -97 | `ERR_TIMER_REPEAT_TOO_SHORT` | The repeat interval is under the site's minimum (60 seconds by default) |
| -98 | `ERR_TIMER_LIMIT` | A new name, and the plugin holds as many timers as the site allows (10,000 by default); replacing a timer it holds is never refused for this |
| -12 | `ERR_SQL_FAILED` | The database refused the write |
| -14 | `ERR_PARAM_DESERIALIZE` | The request is not valid JSON of its shape |

A refusal is also logged, naming the plugin, the timer and the limit, so the
operator sees it as well as the plugin.

## From kernel code

```rust
use trovato_kernel::timer::{FiredTimer, KERNEL_OWNER, TimerRequest};

state.timers().register_handler(KERNEL_OWNER, "my-feature", Arc::new(|t: FiredTimer| {
    Box::pin(async move {
        // t.name, t.payload, t.run_at, t.fired_at, t.attempt
        Ok(())
    })
}));

state.timers().set(KERNEL_OWNER, &TimerRequest {
    name: "my-feature:42".into(),
    run_at: when,
    repeat_secs: None,
    payload: serde_json::json!({ "id": 42 }),
}).await?;
```

A handler is found by owner and name prefix: `"my-feature"` handles
`my-feature` and `my-feature:<anything>`. Kernel timers are not held to the
per-plugin limit.

## Administration

`/admin/reports/timers` (Reports, Timers) lists every timer: owner, name,
next run in the viewer's time zone and as a relative time, repeat, status,
when it last fired and how late, attempts and the last error. It filters by
owner, status and name and pages at 50.

`view timers` opens it. `administer timers` also offers, per row, Pause,
Resume, Run now and Cancel, and an edit form for the next run and the repeat
interval. No form creates a timer: what a timer does comes from code only.
The throttles form (`/admin/reports/timers/settings`) sets:

| Throttle | Default | Environment variable |
|---|---|---|
| Timers firing at once, per server | 4 | `TIMER_MAX_CONCURRENT` |
| Timers per plugin | 10,000 | `TIMER_MAX_PER_PLUGIN` |
| Shortest repeat interval (seconds) | 60 | `TIMER_MIN_REPEAT_SECS` |
| Largest payload (bytes), at most 65536 | 4096 | `TIMER_MAX_PAYLOAD_BYTES` |

A variable that is set overrides the form, which then shows the value read
only. Every change made on these pages is checked against the permission,
takes the page's form token and is written to the security log
(`timer.changed`, `timer.settings_changed`) with who made it and the timer
before and after.

## Measured

With 10,000 timers in the table the claim takes 0.29 ms at p50 and the wake
query 0.16 ms, each on its partial index; 1,000 timers due over ten seconds
fire 6 ms after their second at p50 and 8 ms at p95, each once, across two
servers. The method, the numbers and the plans are in
[the timers benchmark](benchmarks/timers.md).
