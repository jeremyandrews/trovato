# Plugin item plumbing (B68)

Four decisions that B68's later parts implement: how a plugin's item write goes
through `ItemService` without unbounded re-entrancy, how a background write keeps
kernel authority, how two plugins share a queue, and how a batch and a bulk action
actually run. Line numbers are at `59f2825`.

## 1. Re-entrancy for plugin item writes

**Decision.** `save-item` and `delete-item` call `ItemService::create`, `update`
and `delete`, so `tap_item_presave`, `tap_item_insert`, `tap_item_update`,
`tap_item_update_index` and `tap_item_delete` fire for a plugin's write as for a
person's. The bound is a task-local **stack of the plugins whose tap is currently
handling an item write**:

- When `ItemService` dispatches one of those five taps, each handler runs with its
  plugin pushed on the stack.
- A write made while the stack is non-empty is a nested write. Its taps go to every
  plugin **except** those already on the stack, so a plugin never sees its own
  write echoed back into the handler that made it.
- A nested write when the stack already holds `MAX_ITEM_WRITE_DEPTH` (4) plugins
  fails closed before anything is written, with a new host error
  `ERR_ITEM_WRITE_DEPTH`. The service returns a typed error and the host maps it.

The exclusion alone terminates (each level removes at least one plugin from the
set a nested write can reach, and the set is finite); the depth bound makes the
worst case small and explicit instead of "the number of installed plugins".

**Why a task-local.** Same reason as the existing `DECIDING_ACCESS` marker
(`crates/kernel/src/host/item.rs:152-168`): `ItemService::tap_state` builds a fresh
`RequestState` for every dispatch, so a field on the state does not cross into the
handler, while tap dispatch awaits its handlers inline on the calling task
(`TapDispatcher::dispatch`, `crates/kernel/src/tap/dispatcher.rs:75-117`, a plain
`for` loop with `.await`; nothing under `tap/` spawns), so a task-local does. It is
per task, so two concurrent requests never see each other's stack.
`DECIDING_ACCESS` stays as it is: an item-api call inside an access decision is
still refused.

**Why a nested dispatch cannot deadlock.** The dispatcher instantiates per
dispatch: `instantiate_and_call_export` (`dispatcher.rs:342-461`) builds a fresh
`PluginState` and `Store` and calls `instantiate_async` on the plugin's own
compiled module and linker (`:437`). Walking every resource a nested write touches:

| Resource | Held across a nested dispatch? |
|---|---|
| `TapRegistry` | No lock: an immutable `HashMap<String, Vec<TapHandler>>` (`tap/registry.rs:25-27`), read by `get_handlers`. |
| Wasmtime pooling allocator | One slot per live instance (`plugin/runtime.rs:802-805`, 1000 by default). Exhaustion makes `instantiate_async` return an error, it never waits, so a deep chain fails one call rather than blocking. The depth bound caps a chain at 5 live instances. |
| Postgres pool | Not held. `Item::create` and `Item::update` open and commit their own transaction (`models/item.rs:243,330`) and no tap is dispatched inside it; presave runs before, insert/update/index after. The plugin `db` host functions acquire and release per call (`host/db.rs:206-260`). `sync_file_references` opens its own short transaction after the taps. |
| Moka caches (`cache`, `stage_cache`, field access) | Lock-free from the caller's view; no guard lives across an `.await`. |
| Store / epoch | Each level has its own `Store` and its own epoch deadline; host-call time is credited to the outer call, and the outer wall clock ceiling still bounds the whole chain. |
| Semaphores | None on this path. The only bounded concurrency is the queue drain's `JoinSet` width, which sits *above* the worker dispatch, not inside it. |

Part 2 carries a test: a fixture plugin whose `tap_item_update` calls `save-item`
on the same item terminates, with the outer write succeeding and the chain stopped
by exclusion, and a second test drives the depth bound directly.

## 2. Kernel authority

**Decision.** The background principal passes `ItemService::check_access`, the way
it already passes `gate_field_writes` (`content/item_service.rs:1555`) and
`gate_publish` (`:1651`). The host keeps `Acting::Kernel` only as "call the service
with `UserContext::background()`"; there is no second, direct write path.

**Evidence that no caller gains authority.** `UserContext::background()` is built
in three places, all in `crates/kernel/src/cron/mod.rs`: the queue worker state
(`:346`), the `tap_cron` state (`:1079`) and `background_state` (`:1801`, used for
`tap_queue_info`). No route, session, token or MCP path builds it
(`tap/request_state.rs:137`). The callers that would hand `ItemService` a
background principal after this change:

- `save-item` / `delete-item` / `get-item` / `query-items` with `Acting::Kernel`,
  reachable only from a plugin whose manifest declares `item_background`
  (`host/item.rs:140-150`). Without it the call still returns
  `ERR_ITEM_BACKGROUND_DENIED` before reaching the service. `get-item` and
  `query-items` already skip the decision under `Acting::Kernel`, so reads are
  unchanged.
- Taps fired by such a write receive the same background principal through
  `tap_state`. A second plugin handling them that calls `item-api` is judged by
  **its own** `item_background`, because `acting_for` reads the calling plugin's
  state, so authority does not pass from the first plugin to the second.
- `tap_item_access` dispatched by a background decision: any item-api call from
  its handler is refused by `DECIDING_ACCESS`.

Every other `check_access` caller (`routes/*`, `gather/gather_service.rs:992`,
`mcp-server`) gets its user from a session or token and so never holds the
background principal; there is no gather host function a background tap could
reach. Admin already short-circuits the same step, so the bypass sits beside it.

The create permission (`create {type} content`) is not decided by the service
(the item routes check it before calling `create`), so the host keeps that one
check for request-scoped creates. The edit and delete checks in the host are
duplicates of the service's and go.

## 3. Shared queues

**Decision.** The owning plugin declares a queue shared where queues are already
declared, in its `tap_queue_info` entry:

```json
[{ "name": "notify", "concurrency": 2, "shared_with": ["ritrovo_cfp", "ritrovo_importer"] }]
```

Deny by default: no `shared_with`, or a pusher not in it, refuses the push. A new
additive host function in the `queue` interface,

```wit
push-to: func(owner: string, queue-name: string, payload-json: string) -> result<_, string>;
```

reads the owner's declaration at push time by dispatching its `tap_queue_info`
(the drain already does this every cycle, `cron/mod.rs:1771-1793`) and refuses with
a new error code when the owner is not loaded, does not declare the queue, or does
not list the caller. The row is stored under the **owner's** `plugin_name`, so the
existing drain, claim (`:1698-1750`) and per queue width apply unchanged.

The pushing plugin is recorded in a new nullable column `plugin_queue.sender`,
written from the caller's `PluginState.plugin_name` (never from the guest). A row
with a sender is delivered to the owner's `tap_queue_worker` as

```json
{ "sender": "ritrovo_cfp", "queue": "notify", "payload": { ... } }
```

and a row without one exactly as today (the payload itself), so `push` and
`enqueue` keep their meaning and existing workers see no change. Another plugin
cannot make a row arrive with a sender it chose: only `push-to` writes the column,
and only with the caller's own name. (The owner can of course put anything in its
own payloads; that is not impersonation of anyone else.)

**Migration.** One additive migration: `ALTER TABLE plugin_queue ADD COLUMN sender
TEXT`. No backfill; existing rows have no sender and are delivered as before.

## 4. Batch execution and bulk actions

**Decision.** One registry of bulk actions, `BulkActionRegistry`, built at startup
and held in `AppState`. It holds the three kernel actions (`publish`, `unpublish`,
`delete`, with today's permissions from `routes/admin_content.rs:839-843`) and every
action an enabled plugin declares in its manifest:

```toml
[[bulk_actions]]
name = "archive"            # registered as "<plugin>.archive"
label = "Archive selected"
permissions = ["edit any content", "archive content"]
```

A plugin action must name at least one permission (deny by default); a
declaration without one fails the manifest parse. The `matches!` allowlist and
both `unreachable!()` arms go; the template renders the select from the registry,
filtered to the actions whose permissions the user holds. B69's stage move
registers as one more kernel entry in `BulkActionRegistry::kernel_actions`.

A plugin action runs through a new tap, `tap_bulk_action`, dispatched only to the
declaring plugin (`dispatch_to_plugin`) with one chunk:
`{ "action": "archive", "item_ids": [...] }`, returning `{ "succeeded": n,
"failed": n }`. The ids it receives are already filtered to the items the user may
`edit` at execution time; the tap's `RequestState` carries the submitting user, so
any `item-api` call it makes is judged as that user.

**One executor.** Submitting (the content list form, or `POST /api/batch`) checks
the action's permissions, creates the `BatchService` record (`operation_type` =
action name, `params.item_ids`), and enqueues a kernel job through
`enqueue_kernel_job` (`host/queue.rs:63`) under a reserved name, `__kernel_batch`,
then nudges a drain of that one kernel queue on a spawned task so a person does
not wait for cron; cron and the resident runner drain it too, and `SKIP LOCKED`
claims make the overlap safe. The executor works in chunks of 100 ids. Before
each chunk it re-reads the batch (a cancelled batch stops) and re-resolves the
submitting user's permissions from the database; a user who no longer holds the
action's permissions stops the run, which is marked failed. Kernel actions make
one `ItemService::update` or `delete` call per item as the form does today; plugin
actions load the chunk with one set query, decide `edit` per item through
`check_access`, and dispatch the tap once per chunk. After each chunk it calls
`BatchService::update_progress`; at the end `complete` with
`{ "succeeded", "failed", "message" }`, where `message` is today's flash text
("N item(s) published. M item(s) failed."). The executor never uses the background
principal's authority; it always acts as the re-resolved submitting user.

**`/api/batch`** accepts only registered action names and refuses anything else
with 400, so it no longer records intentions nothing will run. `/admin/content`
redirects to itself with the batch id, and the page polls `GET /api/batch/{id}`
(a script file, not inline, for the CSP) to show progress and the final message.
