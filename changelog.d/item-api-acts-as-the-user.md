The four `item-api` host functions decide now, as the user the plugin is
acting for.

They decided nothing before. `get-item`, `query-items`, `save-item` and
`delete-item` read and wrote through the `Item` model directly and used the
requesting user only as an author id. The module uses the model on purpose, to
avoid dispatching a tap from inside a tap, and the cost of that was supposed to
be the taps; it was also the access decision, because the decision lives in
`ItemService` beside them. So a plugin handling an anonymous visitor's request
could read an unpublished item and its restricted fields by id, list them, and
rewrite or delete any item on the site. Nothing about the request reached the
item: the user was an author id and the id was a lookup key.

A request-scoped call now gets that user's own answer. `get-item` requires
`view` and drops the fields the user may not see; `query-items` returns what
`filter_page_for_view` keeps, the same seam the REST, SSR, gather and search
read paths use; `save-item` requires `edit` on the item that is there for an
update and `create {type} content` for a create, the permission the item routes
check for the same act; `delete-item` requires `delete`. A denied read is
indistinguishable from a missing item and writes `null`, so the answer cannot
be used to confirm that a draft exists. A denied write returns the new
`ERR_ITEM_ACCESS_DENIED`.

Writes still go straight to the model. The insert, update and delete taps do
not fire from `item-api`, which is the contract the module exists for; only the
decision is new, and only the decision reaches `ItemService`.

Background callers need a capability. Cron and the queue worker run under the
kernel-internal background principal, which carries no identity and holds no
permissions, so there is no user whose authority a call could act with. Rather
than fall back to kernel authority silently — which is what every call did
before — a plugin that needs it declares `item_background = true`, on the same
manifest plane as `ai_background` and for the same reason. Without it, an
`item-api` call from a background context returns `ERR_ITEM_BACKGROUND_DENIED`.
No in-tree plugin imports the interface; the SDK has no binding for it, so this
reaches external plugins that hand-roll the imports.

Two pieces of plumbing come with it, both shaped by things that would otherwise
break.

The access decision needs an `ItemService`, and `ItemService` already holds a
`RequestServices`, so putting one inside `RequestServices` is a reference cycle
that never frees. The services hold a `Weak` handle instead, in a shared cell
set once by `AppState` after the service is built. The cell is shared rather
than copied so the clone the service itself took a moment earlier sees the
binding, and weak so neither side keeps the other alive. A handle that cannot
be upgraded reports no services rather than deciding without one.

And the decision dispatches `tap_item_access`, so a plugin handling that tap
could call `get-item`, ask for another decision, and dispatch the tap again
without end. An `item-api` call made while a decision is in progress fails
closed. The marker is a task-local rather than a field on the request state
because the dispatch does not carry the caller's state: `ItemService::tap_state`
builds a fresh `RequestState` for the handler, so the invocation depth
`plugin-api` carries does not cross that boundary and neither would a flag
beside it. Tap dispatch awaits its handlers inline, so a task-local does.

Separately, a plugin call has a wall clock ceiling. Guest CPU was already
bounded by epoch interruption, and the deadline callback extended that budget by
however long a call had spent parked inside host functions, so that waiting on
a slow provider was not billed as computing. That extension had no ceiling: a
guest looping over slow host calls bought another second of deadline for every
second it waited and was never interrupted, and request-scoped dispatch has no
other clock, so one plugin could hold a request open for as long as it liked.
Total elapsed time is bounded now as well, 120 seconds for a request-scoped
call and 900 for a background one, both far above the epoch budgets beside them
and both overridable. A call stopped by this bound is recorded as its own
outcome, distinct from CPU exhaustion, because the two say different things
about what the plugin was doing; a queue job stopped by it is dead-lettered
with its own reason rather than retried.
