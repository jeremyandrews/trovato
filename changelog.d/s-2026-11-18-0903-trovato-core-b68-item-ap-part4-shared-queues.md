Two plugins can now share a queue. The queue host inserted every job under the
caller's own plugin name and the drain handed a job only to that plugin's
worker, so a queue was private to the plugin that pushed into it and there was
no way for one plugin to hand work to another's worker.

An owner shares a queue by listing plugins under `shared_with` in that queue's
`tap_queue_info` entry, and a listed plugin pushes with the new `push-to` host
function (`trovato_sdk::host::queue_push_to`). The kernel checks the push against
the owner's declaration at push time and refuses anything else with the new
`ERR_QUEUE_PUSH_REFUSED` (-64). An accepted job is stored under the owner, so the
owner's worker runs it at the owner's width for that queue with the usual retry
and dead-letter rules, and the worker receives `{"sender", "queue", "payload"}`,
with the sender recorded by the kernel in a new nullable `plugin_queue.sender`
column. Jobs a plugin pushes to its own queues reach its worker unchanged.

`docs/plugin-queue.md` now says that concurrency is per queue, which it has been
since #101, and that `max_retries` and `retry_delay_seconds` are not read.

Contract: additive. `push-to`, its SDK binding and the error code are new; `push`,
`enqueue` and what an existing worker receives are unchanged.
