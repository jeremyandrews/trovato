A Redis that stops answering no longer hangs a cron run, or any other kernel
path, for ever, and a cron tick skipped behind a stuck run now says so.

The root cause was an incident on v0.102.0. A SYN flood on the site's port cut
the pod off from Redis and DNS for a minute or two, just as a cron run started.
The run never got past `process_queues`: its queue pop, like every Redis call
the kernel made, went through `get_multiplexed_async_connection()` with no
connection timeout and no response timeout, and the connection it was waiting
on never answered and never closed. The poker's 30-second request then hung up,
which in that release dropped the run part way through and orphaned its
heartbeat. The orphaned heartbeat went on renewing `cron:lock` for ten hours,
and its loop spun on the closed channel of the run it had lost: the two
runtime threads found busy in user space for those ten hours are, as far as
the evidence goes, that loop (the same loop burns more than a core when
reproduced on its own). Every cron tick in those ten hours was skipped with a
single `debug!` line, which production does not show.

The spin and the endless renewal were already gone (the heartbeat stops when its
run is gone and stops renewing after `MAX_LOCK_RENEWAL_SECS`, and the run no
longer dies with its request, all since v0.104.0). What was left is closed now.
Every async Redis connection the kernel opens goes through one constructor,
`redis_conn::BoundedRedis`, with a five-second connection timeout and a
five-second response timeout; a blocking `BLPOP` for N seconds gets N plus five.
The session pool gets the same two values. A unit test fails the build if any
kernel source opens a Redis connection another way. Each cron task is bounded
too: a task that has not finished in 150 seconds (the allowance `tap_cron`
already had) is logged at warn, recorded as `<task>:TIMEOUT` in the run's task
list, and the run moves on and releases the lock straight away; the queue
drains get their own budget plus the batch they may have in flight. And a run
that finds the lock held reads its age, which the lock value now records, and
logs at warn naming that age when the holder has had it longer than
`MAX_LOCK_RENEWAL_SECS`, or when the lock has no expiry at all. An ordinary
overlap stays at debug.
