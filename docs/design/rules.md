# Rules: events, conditions and actions with no code

Status: proposed. Reverses [gap-analysis.md](../alignment/gap-analysis.md) #11
and [roadmap.md](../alignment/roadmap.md)'s "Actions/Trigger system" row, both of
which said webhooks plus the tap system cover the same ground. They do, for a
plugin author who writes Rust. They do nothing for a blog owner on
trovato.blog who writes no code at all, and that is the audience this note is
for.

Verified against `main` at `0833f5e` (plugin API `(0, 107)`) on 2026-11-23.

## Contents

1. [Why this, why now](#1-why-this-why-now)
2. [What other systems teach](#2-what-other-systems-teach)
3. [Shape of the system](#3-shape-of-the-system)
4. [Events](#4-events)
5. [Conditions](#5-conditions)
6. [Actions](#6-actions)
7. [Execution](#7-execution)
8. [Tenancy and security](#8-tenancy-and-security)
9. [Storage, upgrades and portability](#9-storage-upgrades-and-portability)
10. [Debugging and trust](#10-debugging-and-trust)
11. [UI](#11-ui)
12. [Performance](#12-performance)
13. [Where it lives](#13-where-it-lives)
14. [Ten worked examples](#14-ten-worked-examples)
15. [What this does not build](#15-what-this-does-not-build)
16. [Build plan](#16-build-plan)

---

## 1. Why this, why now

trovato.blog hosts blogs for strangers. A stranger who wants "when a comment
needs approval, email me" or "when someone pays, give them the supporter role"
today has one path: write a WASM plugin in Rust, implementing a tap, uploading
it to a kernel that treats every plugin as code the operator vetted. That is
the right model for a developer extending the kernel. It is no model at all
for the blog owner the hosted network exists to serve, and it is the gap
Drupal filled with Rules and later ECA, and WordPress hosts fill with
Automator-class plugins. The skip in the gap analysis was reasoning about a
single-operator install, where whoever can write a tap already has shell
access to add one. A hosted network inverts that: the person who wants
automation and the person trusted to deploy code are no longer the same
person, so "write a plugin" stops being an answer at all for nearly everyone
who will ask for this.

This note designs the smallest events/conditions/actions layer that covers the
cases a blog owner or small business actually brings — [section 14](#14-ten-worked-examples)
lists ten — without becoming a second programming language, a second
permission system, or a second place the plugin API can break.

## 2. What other systems teach

**Drupal 7 Rules.** The most-used automation in the Drupal ecosystem: triggers
(events), conditions and actions, configured through a multi-step admin UI,
exported as code or Features. What site builders used: content publish
notifications, role grants on registration, scheduled unpublish, simple
field-to-field copies. What went wrong: performance (a "react on save" rule
that itself saved the same entity re-triggered the same rule — unbounded
recursion was common enough that Rules shipped its own recursion-prevention
flag), debugging (a rule that silently did nothing because a condition typo'd
a machine name, with no run log to point at), and the PHP-eval-style "execute
custom PHP" action, which was both a known security liability (arbitrary PHP
as a rule action is arbitrary code execution gated only by whoever could edit
a rule) and the one escape hatch every nontrivial site ended up relying on
because the fixed condition set did not cover their case. Upgrades were
brittle: a renamed or removed entity property broke a rule silently, visible
only when the action stopped firing.

**ECA (Drupal 9/10+).** Replaces Rules' flat condition/action lists with a
BPMN-style model (start event, gateways, end event), aimed at modeling
multi-step business processes rather than single react-to-event automations.
Site builders who need "if A then B else C then D" branching like it; site
builders who need "when X happens, do Y" find it heavier than the job, and the
visual modeler is itself a maintenance surface (importing/exporting BPMN XML,
keeping the diagram and the generated plugin config in sync). What to copy:
ECA's explicit gateway nodes make branching legible in a way Rules' nested
condition groups do not. What to avoid: a visual canvas is a second UI to keep
accessible, keyboard-navigable and working without JavaScript, for a job a
form can do.

**WordPress automation (Uncanny Automator, AutomatorWP).** Pure
trigger-plus-action, no conditions beyond "this trigger's own filters" in the
most common configurations, aimed at the same non-technical site owner this
plugin targets. Wide adoption confirms the form is right: pick a trigger from
a list, pick an action from a list, fill in a few fields, done. What goes
wrong at scale: these plugins integrate dozens of third-party plugins each
with their own trigger/action vocabulary and no shared event schema, so the
same logical event ("a product sold") is a different trigger per integration,
and an update to the integrated plugin silently stops firing the trigger with
no record of why. The lesson for a kernel that owns both the event source and
the automation layer: define the event catalogue once, in one place, typed,
rather than letting each integration invent its own.

**n8n / Zapier.** General-purpose workflow canvases: any trigger app, any
number of chained steps, arbitrary data transforms (often literally a
JavaScript or Python code node). Powerful, and the code node is exactly the
"no code" promise's escape hatch undone — a Zapier "Code by Zapier" step or an
n8n Function node is arbitrary code running with the workflow's credentials,
which is also the thing this project's own "no plugin runs arbitrary user
code" boundary rules out for a hosted network of untrusted blog owners. What
to copy: a visual chain of discrete steps is a legible mental model. What to
avoid: once a code step exists, the no-code promise is cosmetic, because
anything a condition or action can't do gets done in the code step instead,
including anything the host would have refused. For a single untrusted tenant
this is a feature; for thousands of untrusted strangers sharing a kernel, each
code step is a sandbox escape waiting for a bug in the sandbox.

**What this design takes from the survey:** a flat trigger/condition/action
list (not a BPMN canvas) because the ten worked examples are all single-chain
automations, not multi-branch processes; one typed event catalogue the kernel
owns (not per-integration ad hoc triggers); a fixed condition and action
vocabulary with no code escape hatch, because the hosted-network threat model
is exactly the one n8n's and Rules' code steps were never built to survive;
an explicit run log and a recursion guard from day one, because both were
bolted onto Rules years after launch in response to real incidents, and
retrofitting either into a stored rule format is harder than shipping them
from the start.

## 3. Shape of the system

A **rule** fires on one **event**, checks a tree of **conditions**, and on a
pass, runs an ordered list of **actions**. Events come from a kernel-owned
**event catalogue** that every existing tap (and every tap added later)
populates automatically. Conditions are a fixed, closed, side-effect-free set
(field comparisons, role/type/stage checks, Gather counts, combined with
and/or/not) — no expression language. Actions are a fixed set that each call
exactly the kernel service a human or a normal plugin would call for the same
effect (`ItemService`, `RoleService`, the moderation transition table, the
mail and `http` host interfaces, `CacheLayer`), through the same permission
checks, as a per-blog, least-privilege service account that is never the
kernel's own background authority.

```
Event (catalogue, typed payload)
   -> Condition tree (and/or/not over fixed primitives)
        -> Action list (fixed set, same kernel services as any write)
             -> Run log entry (always written, success or failure)
```

## 4. Events

### The mechanism

Every tap dispatch in the kernel already goes through one chokepoint:
`TapDispatcher::dispatch(tap_name, input_json, state)` in
`crates/kernel/src/tap/dispatcher.rs`. It looks up the plugins that declared
`implements` for `tap_name` in their manifest and calls each in weight order;
if none did, it returns immediately (`handlers.is_empty()`, already the
fast-exit path today).

The additive kernel mechanism is one more lookup at that same chokepoint,
before the handler loop: a static table, the **event catalogue**, maps a
subset of tap names to a stable event name and a payload schema version:

```rust
// crates/kernel/src/rules_event.rs (new, kernel)
pub struct EventDescriptor {
    pub event: &'static str,       // "item.update" — the stable name a rule stores
    pub tap: &'static str,         // "tap_item_update" — the existing dispatch point it rides
    pub schema_version: u32,       // bumped only when the payload below gains a field
}

pub const EVENT_CATALOGUE: &[EventDescriptor] = &[
    EventDescriptor { event: "item.insert", tap: "tap_item_insert", schema_version: 1 },
    EventDescriptor { event: "item.update", tap: "tap_item_update", schema_version: 1 },
    // ... one row per tap below, see "Initial catalogue"
];
```

`TapDispatcher::dispatch` adds one check at the top, before the handler loop:
a lookup in a single precomputed `HashSet<&'static str>` — call it
`FORWARD_TAPS` — holding exactly the tap names that are (a) in
`EVENT_CATALOGUE` and (b) whose event currently has at least one enabled rule
attached. The two facts (catalogue membership, listener state) are collapsed
into this one set at build time, not checked as two separate steps on the hot
path, so the dispatcher does one `HashSet::contains(tap_name)` and nothing
else when the answer is no. That check runs on **every** tap dispatch —
`tap_menu`, `tap_theme`, `tap_preprocess_item` and the other taps that will
never be Rules events included, since `dispatch()` has no cheaper way to know
in advance which call this is — but a `HashSet::contains` on a short string is
tens of nanoseconds, negligible next to the WASM instantiate-and-call cost
`dispatch()` already pays for every handler it invokes. When `tap_name` is in
`FORWARD_TAPS`, the dispatcher forwards the same `input_json` (wrapped in the
envelope below) to every plugin that implements the one new tap
`tap_rules_event`, alongside (not instead of) the normal handler loop. The set
is rebuilt the same way `GatherService` already refreshes its query cache
(`gather_service.rs` `reload_from_db` on a `CacheKind` signal): a new
`CacheKind::Rules`, bumped whenever a rule is saved, enabled, disabled or
deleted, read by a background task on the existing interval, so a new or
disabled rule is live across every server within one cache cycle without a
restart — this reuses the exact mechanism BL-121 shows already exists for
Gather, sidestepping that row's bug by not reinventing the reload path.

A tap not yet in the catalogue is simply not a Rules event yet; adding one
later is one line in `EVENT_CATALOGUE` wherever the tap itself is declared
(`crates/kernel/src/plugin/info_parser.rs`'s `KNOWN_TAPS`), not a change to
the Rules plugin. That is the whole claim in the Goal — "every event later
runs add becomes a Rules event automatically" — made true by routing through
the one chokepoint every tap already uses, rather than by the Rules plugin
declaring `implements` for forty-five tap names and needing a new release
every time a forty-sixth is added.

Two events are not already tap-dispatched and need one new call each, both
small:

- **`moderation.transition`**: `content/moderation.rs`'s `TRANSITIONS` table
  (Draft/Published/Archived, B158) is consulted by whatever write path moves
  `item_revision.moderation_state`, which today fires no tap at all. That call
  site gains one `dispatcher.dispatch("tap_item_update", envelope)` call,
  enriched with `moderation_from`/`moderation_to`, mirroring exactly how the
  stage selector already rides `Item::update`'s own transaction and its
  existing tap dispatch (`docs/design/stage-workflow.md` section 6).
- **`import.completed`**: `config_storage/yaml.rs`'s `import_config` gains one
  dispatch call after a successful import, carrying entity counts by type —
  nothing upstream of it needs to change.

`stage.transition` needs no new call site at all: it already rides
`tap_item_update` (stage-workflow.md section 6); the payload only needs
`stage_before`/`stage_after` added to what is already serialized there.

### The envelope

Every event, regardless of source, is wrapped the same way before it reaches
`tap_rules_event`:

```json
{
  "event": "comment.insert",
  "schema_version": 1,
  "tenant_id": "<uuid, DEFAULT_TENANT_ID until BL-129 lands>",
  "occurred_at": "2026-11-23T10:04:00Z",
  "actor": { "user_id": "<uuid|null>", "is_background": false },
  "rules_depth": 0,
  "payload": { "...": "event-specific, see table below" }
}
```

`rules_depth` is the recursion counter from [section 7](#7-execution); it
travels on every envelope, including ones caused by a plain write a human
made (depth 0), so the depth check has no special case to get wrong.

### Initial catalogue

| Event | Rides tap | Payload fields | Area |
|---|---|---|---|
| `item.insert` | `tap_item_insert` | `item_id, item_type, stage_id, author_id, title` | items |
| `item.update` | `tap_item_update` | above + `changed_fields: [str], stage_before, stage_after, moderation_from, moderation_to` | items, stages, moderation |
| `item.delete` | `tap_item_delete` | `item_id, item_type, author_id` | items |
| `item.presave` | `tap_item_presave` (sync) | same as `item.update`, before commit | items |
| `item.access` | `tap_item_access` (sync) | `item_id, item_type, operation, user_id` | items |
| `field.access` | `tap_field_access` (sync) | `item_id, field_name, operation, user_id` | items |
| `category.term_insert`/`_update`/`_delete` | `tap_categories_term_*` | `term_id, vocabulary, name, parent_id` | categories |
| `form.submit` | `tap_form_submit` | `form_id, item_id (optional), values: {field: value}` | forms, items |
| `user.register` | `tap_user_register` | `user_id, name, mail` | users |
| `user.login`/`logout` | `tap_user_login`/`logout` | `user_id` | users |
| `user.update`/`delete` | `tap_user_update`/`delete` | `user_id, changed_fields` | users |
| `comment.insert`/`update`/`delete` | `tap_comment_*` | `comment_id, item_id, author_id, status` | comments |
| `comment.access` | `tap_comment_access` (sync) | `comment_id, operation, user_id` | comments |
| `cron.tick` | `tap_cron` | `run_at` | cron |
| `queue.worker` | `tap_queue_worker` | `queue_name, payload` (the owning plugin's own job — rules do not listen on their own jobs, see depth guard) | cron |
| `stage.transition` | `tap_item_update` (enriched) | `item_id, stage_before, stage_after` | workflow stages |
| `moderation.transition` | `tap_item_update` (enriched, new call site) | `item_id, revision_id, moderation_from, moderation_to` | moderation |
| `import.completed` | new call site in `config_storage/yaml.rs` | `entity_counts: {type: count}, item_ids: [uuid]` | imports |
| `assistant.tool_call` | `tap_assistant_tool` | `tool_name, scope` | AI/assistant |

**Events with no single item** (`cron.tick` above all) still need to hand an
action an item to act on, for cases like "publish whatever is due." The fix
is not a new event per item; it is the "item matches a named, saved Gather
view" condition from [section 5](#5-conditions) acting as a fan-out when the
triggering event itself names no item: the Rules queue worker evaluates the
named view once per `cron.tick`, bounded by the view's own pager (the same
Gather caps every query already has), and runs the rest of the condition
tree and the action list once per item the view currently returns, each
logged as its own run-log entry. `import.completed` does not need this
fan-out — its payload carries `item_ids` directly, since the kernel's import
path already knows every id it just wrote.

Two areas named in the Goal have no event row yet because the kernel has no
source for them to ride, and the dependency is stated rather than invented:

- **Billing** depends on BG5, which does not exist in any form today (no
  model, no doc, no stub — confirmed by search). Once BG5 ships a
  `payment_succeeded`-shaped write, it gets one catalogue row the same way
  every other row was added; nothing about this design needs to change to
  accommodate it. Worked example 4 is written against this future event and
  marked accordingly.
- **Webhooks in** has no receiver anywhere in the kernel or in
  `plugins/trovato_webhooks` (whose entire implementation today is one
  permission and one menu entry; the `webhook`/`webhook_delivery` tables its
  migrations create are read and written by nothing). An inbound event needs
  a route that verifies and accepts an external POST before it can become a
  catalogue row; that route is not Rules' job to build on spec, and worked
  example 10 is written against it as a stated dependency, not a built
  feature.

**Webhooks out** is not a dependency of this design — see
[section 6](#6-actions): the action calls the kernel's existing hardened
`http` host interface directly, so Rules does not wait on
`trovato_webhooks` to implement the delivery logic its schema has promised
since before this note existed.

### Catalogue versioning

A payload gains fields only (additive, matching the plugin-API freeze
policy in [Versioning.md](Versioning.md)); a saved rule records the
`schema_version` it was authored against, and an older rule reading a newer,
field-superset envelope sees exactly the fields it expects, nothing missing.
If a tap is ever removed outright (pre-1.0, under the same "deprecated at
least one MINOR before removal" policy as any other tap), every rule whose
event rode that tap is disabled automatically with a notice to its owner —
the same mechanism [section 10](#10-debugging-and-trust) already needs for a
rule that errors out N times in a row, so catalogue removal is not a second
code path, just a second reason the first one fires.

## 5. Conditions

A condition tree combines these primitives with `and`, `or` and `not` (no
other combinator; no operator precedence to get wrong because there is no
syntax, just a nested structure the builder UI assembles):

- **Field comparison** on any item kind or type: equals, not-equals, contains,
  starts-with, ends-with, greater/less (-or-equal), in, not-in, is-null,
  is-not-null, has-tag, has-any-tag, has-all-tags, has-tag-or-descendants —
  the subset of `gather::types::FilterOperator` that is total and cheap per
  row. Excluded: `FullTextSearch`, `SemanticSimilarity` and `Custom(String)` —
  the first two are search-index operations with their own latency budget,
  wrong for a per-write condition; `Custom` is literal SQL fragment injection
  by another name and is exactly the "small expression language" this design
  declines to add (see justification below).
- **Link count in a text field**: one narrow, additive, bounded primitive,
  `LinkCount(field)` — a deterministic linear scan counting `href`-like
  substrings in a text field's rendered value, no backtracking, no
  user-supplied pattern. This exists for worked example 5 ("more than N
  links from a new author") and is not a precedent for a general pattern
  operator: it is justified the same way every other addition to this list
  is, by a real worked case, and it is total and side-effect-free like every
  primitive beside it. A case this narrow set still cannot cover gets its own
  equally narrow primitive later, the same additive way, never a pattern
  language that could express it and everything else besides.
- **Item matches a named, saved Gather view**: a boolean condition, not a
  count — "is this item currently in view V", where V is picked from the
  list of Gather views the blog already has, the same picker an editor
  already uses to choose a view for a page. Distinct from the count
  condition below: this one hands the matching item to the rest of the rule,
  which a count cannot. Subject to the same async-only restriction given
  with the count condition below, for the same reason.
- **Role and blog membership**: checked through the same
  `PermissionService`/`user_has_permission` path as any access check — no
  separate Rules permission model. "Blog membership" is written against the
  tenant model BL-129 has not yet decided; until it lands, every condition
  implicitly scopes to `DEFAULT_TENANT_ID`, and the field exists in the
  condition schema now so no rule needs rewriting once tenancy is real.
- **Item kind and type**: `item_type`, directly.
- **Stage and moderation state**: both, named separately, because they are
  different mechanisms (`stage_id` vs `item_revision.moderation_state` —
  confirmed distinct in `content/moderation.rs` and
  `docs/design/stage-workflow.md`).
- **Time and schedule**: compare `occurred_at` or a field's timestamp against
  now, a fixed instant, or a relative duration ("more than 3 days old").
  Recurring schedules are not a condition at all; they are `cron.tick` fired
  on the kernel's own schedule, with a condition narrowing which tick matters
  (see worked example 2).
- **Counts from a Gather**: a condition compares a count to a number — "how
  many of this author's previous comments were held as spam", "how many
  items has this author published" — through `GatherService::execute_for_plugin`
  and its `Count` aggregate, the same entry point and the same caps
  (`MAX_FILTERS = 32`, 10-second plugin statement timeout) every plugin's
  programmatic query already has. **The builder never asks a blogger to
  author a query for this.** The Rules plugin ships a short, fixed list of
  named, parameterized count templates — "number of this [author]'s previous
  [published items / held comments / spam comments], [ever / in the last
  ___]" — and the per-rule configuration is picking one template and filling
  in a number and, where it applies, a duration (or "ever," for an all-time
  count), the same shape as every other condition's form. A template
  this list does not yet have is added to the plugin the same additive way a
  `FilterOperator` primitive is, never opened up as free-form query
  authoring. **Restriction:** permitted only on queue-processed (async)
  events, never on `item.presave`, `item.access`, `field.access` or
  `comment.access` — those are synchronous, inline, request-blocking taps,
  and a Gather call's latency (bounded at ten seconds per the existing cap)
  has no business sitting in a write's transaction. A rule that tries to
  attach a Gather condition to a sync event is refused at save time with that
  reason given, not discovered at run time under load.
- **Combinations**: `and`, `or`, `not`, nested without a depth limit beyond
  the rule-size limit the builder UI enforces (a flat list is also just a
  one-level `and`).

**No general expression language**, and the survey argues why directly:
Drupal 7 Rules' "execute custom PHP" action was the admission that the fixed
set didn't cover every case, and it was also the one feature nobody could
secure, review or upgrade — exactly the risk a hosted network with untrusted
blog owners cannot carry (it is also explicitly out of scope: "Rules that run
arbitrary user code"). The fixed set above is chosen to cover the comparisons
the ten worked examples need and the comparisons Drupal Rules' and ECA's most
common condition usage shows site builders actually reach for (field compare,
role, bundle/type, simple counts) — not a subset arrived at by guessing. If a
future case needs a comparison this list does not have, the answer is one
more `FilterOperator`-shaped primitive added the same additive way events are,
not a mini-language that can express it and everything else besides.

## 6. Actions

Every action is a thin caller of an existing kernel service, nothing new
underneath it:

| Action | Calls | Inline-safe (sync events)? |
|---|---|---|
| Set a field | `ItemService::update` | yes from presave, no otherwise |
| Publish / unpublish | the moderation transition table (`content/moderation.rs`) | no (queued) |
| Move a stage | `ItemService::update` with `stage_id` | yes from presave, no otherwise |
| Create an item (incl. from a form submission) | `ItemService::create` | no |
| Send an email | `mail.send-to-user` / `send-to-site-contacts` host functions | no |
| Call a webhook | the `http` host interface (hardened client) | no |
| Add / remove a role within a blog *(withheld until BL-132, see below)* | `RoleService::assign_to_user` / `remove_from_user` | no |
| Hold for moderation | set `CommentStatus::Pending` (comments) or a moderation/stage move (items) | no — `comment.insert` and `item.insert` are both post-commit, queued events, same as every event but the four named above |
| Refresh cached pages (a path or tag) | `CacheLayer::invalidate` / `invalidate_tag` | no |
| Start another rule set | re-enters [section 7](#7-execution)'s dispatch with `rules_depth + 1` | no |
| Deny access | returns the refusal `tap_item_access`/`tap_field_access`/`tap_comment_access` already expect, no write at all | yes — this is what those three taps exist for |

"Refresh cached pages" is this action's name in the builder; `invalidate_tag`
is what it calls underneath, named here for the implementer, not the blogger
(worked example 8 shows the blogger-facing phrasing). "Start another rule
set" lets one rule's action list call a second, named rule set — the same
"call a shared sub-routine" shape a weight-ordered chain of separate rules on
one event cannot give, since those are independent and unordered-by-intent
beyond their weight; it is not needed by any of the ten worked examples and
is justified on its own terms, guarded by the same depth counter and cycle
detection as every other write a rule's action can cause. The picker only
ever lists rule sets carrying the caller's own `tenant_id` — the same
server-side scoping [section 8](#8-tenancy-and-security) gives every other
condition and action, named here explicitly because this action, uniquely,
references another stored rule by id rather than calling a kernel service
directly.

**Least privilege, concretely.** A rule is never executed with the kernel's
`item_background` capability — the authority `stage-workflow.md` section 7
describes cron and import writes carrying, which bypasses the normal
permission check by design for trusted internal callers. Rules is not a
trusted internal caller; it is configuration a blog owner wrote, and it must
be checked exactly like a human. Each blog that enables Rules gets one
non-interactive service-account user (cannot authenticate interactively,
flagged the same way the anonymous user already is), and every action a
rule on that blog takes is attributed to and permission-checked against that
account through the unmodified `user_has_permission` / `check_access` /
`tap_field_access` paths — the same gates a human edit goes through.

That leaves exactly one place a rule could still escalate: role grants.
`RoleService::assign_to_user` (`crates/kernel/src/services/role.rs`), the
function both a human grant and this design's own "add a role" action call,
enforces no delegation rule today — it assigns whatever role id it is given,
to whatever user it is given, checking nothing about who is asking. The one
place Trovato checks "can the granting user grant this" is
`crates/kernel/src/routes/admin_user.rs`'s `Authority::of(...).covers(...)`
(which compares permission sets, not role names), specific to the
interactive admin user-save screen, which the Rules service-account's own
screens do not call and cannot inherit for free. So this design does not
claim an existing kernel guarantee it would only be reusing; it states the
requirement plainly, at **both** places that call `assign_to_user`:

- The account-configuration screen (who the blog owner lets the service
  account impersonate, permission-wise) only offers, and on every submit
  re-checks, a role whose permissions are all already covered by permissions
  the granting owner holds at that moment — modeled on
  `Authority::of(...).covers(...)` but written fresh, because nothing lower
  in the stack enforces it.
- The "add/remove a role" **action itself**, at the moment it runs (inside
  the queue worker, immediately before the `assign_to_user`/`remove_from_user`
  call — not at enqueue time or condition-evaluation time, so a job that sat
  in the queue is checked against the account's permissions as they are now,
  not as they were when the rule fired), re-checks the same thing a second
  time: before *granting* a role to the action's target user, it checks that
  the service account's own *current* permissions cover the role being
  granted. This is not redundant with the first check — the first bounds what
  the account may be configured to hold; this one bounds what the action may
  hand to someone else, which `assign_to_user` itself does nothing to stop,
  and a rule granting a role to a target user is a different call than a
  human granting the account a role, so it needs its own check rather than
  inheriting the first one. Removing a role needs no equivalent check — it
  can only narrow a target user's access, never grant more than the account
  itself holds, so there is nothing for it to escalate.

Demoting the account later immediately constrains every rule on that blog,
because the account's runtime permissions (not a one-time grant record) are
what both checks above and every other action are checked against.

**The add/remove-a-role action is withheld until roles are tenant-scoped.**
Per `docs/BACKLOG.md` BL-132, `roles`/`user_roles`/`role_permissions` carry no
tenant column today — a role grant is install-wide, full stop, regardless of
which blog's screen made it. Gating only *network* rules on tenancy
([section 8](#8-tenancy-and-security)) is not enough on its own: an ordinary
per-blog rule's "add a role" action would reach every blog the moment a
second one exists on the install, which contradicts this section's own "a
per-blog rule literally cannot touch another blog's data" claim for exactly
this one action. The fix is not a documentation caveat; it is a capability
check in the action itself: "add/remove a role" is refused at save time and
at run time once more than one row exists in the `tenant` table
(`crates/kernel/src/models/tenant.rs`, counting real blogs only — the
network sentinel tenant below is not a row there, so it never trips this
guard on its own) — a count the Rules plugin can check cheaply without
waiting on BL-129/130 to decide anything else — until BL-132 ships
tenant-scoped roles. It ships, guarded this way, with Part 4
([section 16](#16-build-plan)) — the same part as every other queued action,
since this one is never inline-safe either — and works today's single-blog
reality; the guard is lifted at the same time network rules ship, in Part 8,
not before.

**Network rules** (a rule whose scope is the whole network rather than one
blog — e.g., "feature this post on the network front page") exist because
the goal this design was set names "a network administrator" as a distinct
actor from a blog owner, independent of whether one of the ten worked
examples needs the category — it is a separate, more tightly gated category:
visible only to users
holding a new `administer network rules` permission, stored with a sentinel
tenant ("network", not a blog id) so a per-blog rule's condition and action
evaluation is *always* scoped to its own blog's `tenant_id` server-side, not
merely hidden from the per-blog builder UI. A per-blog rule literally cannot
construct a condition or action that reads or writes another blog's data
through any action whose underlying kernel call is already tenant-scoped,
because the query and write calls it goes through are given the configuring
blog's tenant id, not one a plugin could forge — the role-grant action above
is called out as today's one exception, and withheld for exactly that
reason until it is not.

**Outgoing webhooks** are a thin caller of the kernel's existing hardened
outbound client (`net_policy::is_disallowed_ip`, the validating DNS resolver
and the ten-hop revalidating redirect policy already shared by every
plugin's `http` calls and the AI provider client, per `net_policy.rs`'s own
stated purpose of being the *one* copy of this fence) — Rules inherits the
SSRF fence by construction, the same way any other plugin's `http` call does,
rather than opening its own client. HMAC request signing and delivery
retries are Rules' own responsibility (reusing the plugin queue's existing
retry/backoff/DLQ semantics — see [section 7](#7-execution)), independent of
`plugins/trovato_webhooks`, whose own tables exist but are read by nothing
today; this design does not depend on that plugin being finished, and a note
belongs in its backlog row that Rules has since built equivalent delivery, so
whoever next touches that plugin does not duplicate it.

**Secrets** an action needs (a webhook's bearer token, an HMAC signing key)
are encrypted at rest using the kernel's existing `crypto-api` host interface
(already in `KNOWN_HOST_INTERFACES`, so no new capability is added), decrypted
only inside the queue worker at send time, and never returned by any read
path — the edit form shows a placeholder and a "replace" control, never the
stored value, the same pattern config export already uses for plugin secrets
(`env:TROVATO_SECRET_...` indirection in `config_storage/yaml.rs`).

**Nothing a rule does bypasses field access, moderation holds or billing
state**, because nothing above is a new write path — every action calls the
exact function a human write or a normal plugin would, so `tap_field_access`,
the moderation transition table's permission gate, and (once BG5 exists)
whatever access check reflects billing state all run exactly as they do for
any other caller. Rules adds no shortcut around any of them to add a
shortcut to.

## 7. Execution

**Synchronous only where the event demands it.** `item.presave`,
`item.access`, `field.access` and `comment.access` are synchronous, in-request
taps today, and Rules runs inline there — but only the actions marked
inline-safe in the table above: set a field or move a stage from
`item.presave`, or deny access from `item.access`/`field.access`/
`comment.access`. Any action that is not inline-safe, attached to a sync event, is
automatically deferred: the rule enqueues it (via the plugin queue, naming the
just-committed write's id) so it runs after the transaction commits, never
inside it. Every other event is queued always, never run inline in the
request path — a slow SMTP server or an unreachable webhook endpoint never
adds latency to a page view or an API response.

**Recursion depth and loop detection.** The `rules_depth` counter in the
envelope ([section 4](#4-events)) increments by one every time a rule's own
action causes a write that could itself dispatch an event (directly via
"start another rule set", or indirectly via "create an item" or "set a
field"). Dispatch refuses to fire a Rules-originated event past depth 10,
logging the refusal to the run log of the rule that would have fired. A cycle
detected twice in a row (rule A's action reliably causes rule B's event, whose
action reliably causes rule A's event again) auto-disables the later rule in
the cycle, with the same owner notice [section 10](#10-debugging-and-trust)
uses for repeated errors — recursion is treated as a particular, diagnosable
kind of repeated failure, not a separate mechanism.

**Budgets**, each independently enforced: per-rule runs per minute (default
60), per-blog outgoing emails per hour, per-blog outgoing webhook calls per
minute (a new `rules_webhook` rate bucket, configured the same way every
other bucket already is — `TROVATO_RATE_LIMIT_RULES_WEBHOOK` /
`rate_limit.rules_webhook`), and one network-wide ceiling on total queued
Rules jobs, past which new jobs are refused and logged rather than queued,
while jobs already queued keep draining under the existing per-plugin
fairness cap (`MAX_QUEUE_ITEMS_PER_CYCLE`) — so one blog's bug cannot starve
every other blog's rules.

The email budget needs its own honest accounting. `mail.send-to-user`'s
`mail_users`/`mail_recipient` rate buckets (`crates/kernel/src/host/mail.rs`)
are keyed by `plugin:{plugin_name}` — one shared ceiling for the whole Rules
plugin across every blog on the install, not per blog. Rules' own per-blog
cap is a fair-share allocation *inside* that shared ceiling (each blog is
given a fraction of it and refused, with a notice, once its own fraction is
spent), which stops one blog's rules from starving another's — but it
cannot raise the shared ceiling itself, so a large number of active blogs
sharing one install can still collectively hit the kernel's per-plugin bound
even when every individual blog stays inside its fair share. That is a real,
known limitation of a plugin-wide bucket serving many tenants, not something
this design solves; the fix, a per-tenant mail bucket in the kernel, is the
kernel's to build and is out of this plan's scope.

**Idempotency for retried queue jobs.** `plugin_queue` is at-least-once by
design (a crashed worker's claim lease expires and the job is retried) with
no dedupe key in its ABI — the kernel's own documentation of this exact
tradeoff points at the fix as a consumer-side concern
(`cron/mod.rs`: "Queue workers must therefore be idempotent", citing
`ritrovo_importer`'s dedup migration as the precedent). Rules follows that
precedent rather than inventing a new one: before performing a non-idempotent
action (send mail, call a webhook, create an item), the worker records
`(rule_id, trigger_event_instance_id, action_index)` in its own run-log
table and checks for that key first on every delivery attempt, skipping with
a logged "already executed" if it is already there.

**Ordering and conflicts** between rules on the same event: deterministic, by
an explicit per-rule `weight: i32` (the same field and the same meaning
`TapDeclaration::weight` already gives tap ordering), ties broken by creation
order. No implicit dependency inference between rules — Drupal Rules'
"mysterious firing order" was a direct consequence of implicit ordering, and
an explicit, builder-visible weight is the fix the survey already points to.

## 8. Tenancy and security

Rules is per blog by construction: every rule's conditions and actions are
evaluated against the configuring blog's `tenant_id`, server-side, not as a
UI-level restriction. Network rules are a distinct, separately-permissioned
category ([section 6](#6-actions)). A blog owner's rule can touch only that
blog's data because every service call it makes — `ItemService`, Gather
queries — is already tenant-scoped wherever the kernel's tenancy model is
wired, and Rules adds no path around that scoping. Role grants are the one
named exception: roles carry no tenant column at all today (BL-132), so the
add/remove-a-role action is withheld until that changes, rather than claimed
as scoped when it is not — see [section 6](#6-actions).

**This is a real, open dependency, stated rather than hidden**: per
`docs/BACKLOG.md` BL-129, the tenant layer is only half built today — the
tables and a `tenant_id` column exist on ten kernel tables, but
`resolve_tenant` is never mounted, no host function is tenant-scoped, and
roles are install-wide (BL-132). `docs/design/hosted-tenancy.md` (BG4) does
not exist yet. Until BL-129 is decided and BL-130/132 build it, Rules runs
against the one `DEFAULT_TENANT_ID` every other kernel subsystem already
assumes, which is safe (there is exactly one "blog" that exists today) but
not yet the multi-tenant isolation this section promises. Every place this
design reads or writes `tenant_id` is written to need zero rework once BL-129
lands — the field is in the schema, the condition set and the action
dispatch from day one — but the isolation itself is only as real as BL-129
makes the rest of the kernel. The Rules build plan ([section 16](#16-build-plan))
puts both network-scoped rules and general availability of the role action
explicitly after BL-129/130/132, for exactly this reason.

Outgoing webhooks, secrets, least privilege and the no-item_background rule
are covered in [section 6](#6-actions); they hold regardless of the tenancy
dependency above, because they are about what a rule's configured actor may
do, not about which blog's data it may see.

## 9. Storage, upgrades and portability

**Decision: a config-shaped plugin table**, not an item, and not a hand-rolled
`ENTITY_TYPE_ORDER` entry either. Rules describes site behavior — "when X, if
Y, do Z" — the same kind of thing a Gather query or a `workflow` variable
already is, not content an author writes or a visitor reads, which is what
the everything-is-an-item model (BG11) is unifying. `gather_query` is *not*
usable as the precedent for *how* to export it, though: it is a kernel
migration with bespoke, hand-written kernel CRUD
(`config_storage/direct.rs`'s `load_gather_query`/`save_gather_query`/
`delete_gather_query`/`list_gather_queries`) backing its `ENTITY_TYPE_ORDER`
entry — there is no existing case of a *plugin-owned* table reaching
`ENTITY_TYPE_ORDER` directly, and following that path for `rule` would mean
the kernel growing bespoke per-entity CRUD for a plugin's own table, which is
the "second storage model" this decision is trying to avoid, not dodge.

The mechanism that actually fits is already built for exactly this case:
`[config] tables` in a plugin manifest (`PluginConfigDecl`,
`crates/kernel/src/plugin/info_parser.rs`) plus
`crates/kernel/src/config_storage/plugin_config.rs`, which exports and
re-imports any plugin-declared table generically through
`to_jsonb`/`jsonb_populate_record`, with no kernel code written per entity.
Rules declares its `rule` table (one row per rule: event name,
`schema_version` it was authored against, condition tree, action list,
weight, enabled flag, owner tenant_id) under `[config] tables` the way any
other plugin's config-shaped table already can, and gets YAML export,
re-import and diffability for free, with zero kernel changes beyond the ones
already listed in [section 13](#13-where-it-lives). This both corrects the
precedent and keeps the claim it was meant to support: a blog's automation
ships in the same config bundle as its content types and Gather queries,
without the kernel growing a second storage model or bespoke CRUD to carry
it. The honest limitation is unchanged: config export today has no tenant
scoping at all (confirmed: `export_config`/`import_config` take no tenant
parameter), so a multi-blog export is exactly as unfinished for Rules as for
every other config-shaped table, pending the BL-129 decision.

**Catalogue versioning** is covered in [section 4](#4-events): additive-only
payloads, and automatic disable-with-notice for a rule whose event or field
disappears outright, rather than a silent stop.

**A field a rule references going away** (a custom field deleted from an item
type, say) is not a special upgrade case either: the next run logs "field not
found" as an ordinary action error, and the same N-consecutive-errors
auto-disable in [section 10](#10-debugging-and-trust) takes it from there —
one mechanism for every way a rule can go stale, not one per cause.

## 10. Debugging and trust

**Per-rule run log**: every run (whether conditions passed or not) writes one
entry — the triggering event and its id, each condition evaluated and its
result, each action attempted and its outcome (success, error, or
skipped-as-already-executed), duration, and the queue job id where
applicable — retained for a bounded window (default 30 days or the most
recent N runs per rule, whichever is smaller, configurable per blog to bound
storage). The run-log **screen** shows this in plain language first, success
or not — "Ran 2 minutes ago on 'Welcome post', sent one email" when it
worked, "Ran 2 minutes ago on 'Welcome post', couldn't send the email (mail
server unavailable) — will try again" for an error, "Ran 2 minutes ago,
already sent — skipped" for an idempotent skip — with the queue job id,
schema version and other internal fields folded under an optional "technical
details" twisty, for the blog owner who wants them and invisible to the one
who does not.

**Dry run**: pick an existing item (or, for events without a natural item —
`user.login`, say — a synthesized minimal payload), evaluate the condition
tree against it for real, and render each action in the same plain-language
sentence form the builder itself uses — "Would email the post's author" —
not a function-call trace, without executing it. This asks every action
implementation for a `describe` alongside its `execute`, a small, fixed cost
paid once per action type, not per rule.

**Auto-disable**: a rule that errors on N consecutive runs (default 5) is
disabled automatically, and its owner (the blog's configured contact, via the
existing mail path) gets one notice naming the rule and the last error. The
same path fires for a removed catalogue event or field (section 9) and for a
detected recursion cycle (section 7) — three triggers, one consequence, one
notice format.

## 11. UI

A server-rendered, form-based builder — **when** (pick an event from the
catalogue, grouped by area), **if** (build a condition tree from the fixed
primitives, nested and/or/not groups as repeating form sections), **then**
(an ordered list of actions, each contributing its own sub-form the same way
a field widget already does) — built on the existing `FormService` /
`FormBuilder` pipeline BG11's custom forms already use, so it gets the same
accessibility and no-JavaScript operation as every other content form for
free, rather than a second rendering path to keep working. A visual flow
(BPMN-style) editor is explicitly out of scope — the survey's ECA finding is
why: a canvas is a second UI surface to keep accessible and keep in sync with
its underlying config, for a job every one of the ten worked examples does
with a flat when/if/then.

Two rules the builder follows everywhere, stated once rather than repeated
per condition and action: **every value that names an existing thing is a
dropdown of that blog's own named things** — a form by its title, a
vocabulary by its name, a Gather view by its name, a configured plan or
webhook partner by its label — never a typed machine name, which is exactly
the "typo'd machine name" failure mode the survey's Drupal 7 Rules finding
calls out. And **when more than one rule listens on the same event, the list
is reorderable** — a plain "move up / move down" control per rule, the
builder-visible form of the `weight` ordering [section 7](#7-execution)
already defines, not a number a blogger is asked to assign by hand.

**Onboarding for "matches a saved view".** A condition that names a Gather
view is only as usable as getting one to exist in the first place, and this
design does not ask a blogger to go find the Gather admin unguided to build
it. When a blogger picks "item matches a saved view" and the blog has none
yet (or none that fit), the condition's own picker offers "create one now,"
opening Gather's existing filter-based builder (already a no-code screen,
per [section 2](#2-what-other-systems-teach)) pre-seeded from a short list
of starter templates for the common cases — "items due to publish" among
them — so the first-run path is "fill in the one filter this template
leaves blank," not "learn Gather from nothing." The blank itself follows the
same rule as everything else in this section: it is a pick from a list (a
field, a value from that field's own options), never typed syntax — "filter"
names an existing, pre-built screen a blogger is being pointed to, not a new
kind of authoring Rules introduces.

## 12. Performance

**Zero-rule cost**: one `HashSet::contains` call, on every tap dispatch — the
roughly forty-five taps that will never be a Rules event included, since
`dispatch()` has no cheaper way to know which call this is in advance — at a
cost of tens of nanoseconds, negligible next to the per-call WASM
instantiate-and-call cost `dispatch()` already pays for every handler it
invokes. This is the same shape as `TapDispatcher::dispatch`'s existing early
return when no plugin implements a tap (`handlers.is_empty()`); Rules adds a
second, equally cheap check alongside it, not instead of it.

**Target cost per event dispatch**, past the zero-rule case:

| Rules listening | Sync event (inline-safe actions only) | Async event (queued) |
|---|---|---|
| 1 | < 1 ms added to the request | one `INSERT` into `plugin_queue`, sub-millisecond |
| 10 | < 5 ms added (worst case, all ten inline-safe) | one batched insert of up to 10 rows, < 10 ms |
| 100 | discouraged by the builder UI past a documented ceiling for sync events (see below) | one batched insert of up to 100 rows, < 50 ms, bounded by the existing per-plugin fairness cap in the queue drain |

A hundred rules on one synchronous, in-request event (`item.presave`, most
plausibly) is the case the table calls out rather than papers over: the
builder UI warns past a configurable soft ceiling (default 20 inline-safe
actions across all rules on one sync event), and the cumulative time those
actions added to the request is surfaced only in a technical/ops view gated
behind a separate permission, not the ordinary per-rule run log a blog owner
sees — a millisecond figure is an operator's signal, not a blogger's,
because a hosted network's sync-path latency budget belongs to every tenant
sharing the server, not to one blog's rule count.

## 13. Where it lives

**The Rules plugin** (new, `plugins/trovato_rules`): the `rule` table and its
`[config] tables` export, the builder UI, the run log, condition/action
evaluation, the per-blog service account, queue worker, dry run, and
auto-disable. Everything in [sections 5–11](#5-conditions) that is not
explicitly called out below as a kernel change lives here.

**Kernel changes, named exactly, all additive (MINOR, not MAJOR, under
[Versioning.md](Versioning.md))**:

- `crates/kernel/src/rules_event.rs` (new): `EventDescriptor`, `EVENT_CATALOGUE`,
  and the `CacheKind::Rules`-backed listener-set lookup.
- `crates/kernel/src/tap/dispatcher.rs`: `TapDispatcher::dispatch` gains the
  catalogue check and the conditional forward to `tap_rules_event` handlers.
- `crates/kernel/src/plugin/info_parser.rs`: one new entry in `KNOWN_TAPS`,
  `"tap_rules_event"`.
- `crates/wit/kernel.wit`: one new tap signature,
  `tap-rules-event: func(event-name: string, payload: string) -> result<string, string>`.
- `crates/kernel/src/content/moderation.rs`'s transition write path and
  `crates/kernel/src/config_storage/yaml.rs`'s `import_config`: one new
  dispatch call each, per [section 4](#4-events); `import_config` additionally
  carries the written ids on the event it now dispatches.
- A new `rules_webhook` rate-limit bucket, declared the same way every
  existing bucket already is (`crates/kernel/src/middleware/rate_limit.rs`).

No kernel change registers the `rule` table itself — it is declared under
`[config] tables` in the Rules plugin's own manifest, the generic mechanism
[section 9](#9-storage-upgrades-and-portability) uses, which needs no kernel
change per plugin table by design.

**SDK changes**: `trovato-sdk` gains the typed wrapper for `tap_rules_event`
(mirroring every other `#[plugin_tap]` signature) and the typed envelope
struct from [section 4](#4-events), so a plugin author who ever writes a
second automation-style plugin gets the same typed payload Rules does — this
is the only SDK-visible surface the whole design adds.

Nothing else in the kernel, the WIT, or the SDK changes. The capability model
needs no new entry: `implements = ["tap_rules_event", "tap_item_presave",
"tap_item_access", "tap_field_access", "tap_comment_access"]` is five
manifest lines, not forty-five, and the five are exactly the synchronous taps
[section 7](#7-execution) names, declared the normal way because they need an
inline answer (set a field or move a stage from `tap_item_presave`; deny
access from the other three) before the dispatcher's generic forward path
(built for the asynchronous majority) would reach them in time.
`tap_form_validate` is not among them — no condition or action in this design
needs a synchronous validation verdict, so it is not declared, and is added
later the same additive way if a future case needs it.

## 14. Ten worked examples

Each as event / conditions / actions, in the words the builder would actually
show a blogger — a picked value is always a name from an existing list on
that blog (a form's title, a vocabulary's name, a configured plan, a
configured partner), never a typed machine name, per [section 11](#11-ui).

1. **Notify the author when a comment is held.**
   When: *a comment is posted*. If: *the comment is waiting for approval*.
   Then: *email the post's author* that a comment is waiting.

2. **Publish a scheduled post and post it to a webhook.**
   When: *every few minutes* (`cron.tick`). If: *the item matches the saved
   view* "Posts due to publish" — a Gather view listing drafts whose publish
   date has passed, picked from the blog's list of views or, if none fits
   yet, created on the spot from a one-click starter the "matches a saved
   view" condition itself offers for this exact case (see
   [section 11](#11-ui)'s onboarding note), rather than sending the blogger
   to an unrelated screen to build one unguided. See [section 4](#4-events)'s
   fan-out note for how this supplies the item. Then: *publish it*; *call
   the configured webhook* with the item's title and link.

3. **Form submission creates an item and emails a team.**
   When: *the "Contact us" form is submitted*. Then: *create a Lead* from
   the submitted answers; *email the configured team address* with a link to
   the new lead.

4. **Add a role when a member pays.** *(depends on BG5, not built today —
   see [section 4](#4-events).)*
   When: *a payment succeeds*. If: *the plan is "Supporter"*. Then: *give the
   paying member the Supporter role*.

5. **Hold any post with more than N links from a new author.**
   When: *a post is created* (not while it is being saved — a check against
   the author's history needs the queued path per [section 5](#5-conditions);
   the worked example accepts the resulting brief visibility window rather
   than allowing this kind of condition on in-the-moment events generally).
   If: *the post's body has more than [N] links*, and *this author has fewer
   than [M] previously published items, ever* (the canned count template
   from [section 5](#5-conditions), its duration blank left as "ever" rather
   than filled in — the template supports both). Then: *hold it for
   moderation*.

6. **Welcome new users.**
   When: *someone registers*. Then: *email them* a welcome message.

7. **Notify an editor when a term is added under a given vocabulary.**
   When: *a term is added*. If: *the vocabulary is "Press Releases"*. Then:
   *email the configured editor address*.

8. **Refresh cached pages and promote on publish.**
   When: *a post is published*. Then: *mark it promoted to the front page*;
   *refresh the cached pages that show it* (the home page and the post's own
   page — [section 6](#6-actions)'s plain-language framing of a cache-tag
   purge).

9. **Auto-categorize untagged imported content.**
   When: *an import finishes* (the event hands Rules the ids it just wrote,
   [section 4](#4-events)). If: *the item has no category*. Then: *set its
   category* to a configured default.

10. **Create a comment from a verified inbound webhook.** *(depends on an
    inbound webhook receiver that does not exist in the kernel or in
    `plugins/trovato_webhooks` today — see [section 4](#4-events); written
    against that seam, not built by this plan.)*
    When: *a webhook arrives*. If: *it's from the configured partner*
    "Review Syndicate" (a label the blog owner typed when they set up that
    partner, not a slug). Then: *post the payload's text as a comment* on the
    named item.

## 15. What this does not build

- A visual flow (BPMN) editor — [section 11](#11-ui).
- Rules that run arbitrary user code — no expression language, no code-step
  action, matching the fixed, closed, total condition and action sets in
  [sections 5](#5-conditions) and [6](#6-actions).
- Outbound webhook delivery for any plugin other than Rules itself — Rules
  builds its own HMAC signing and retry on the hardened `http` interface
  because nothing else has built it yet; it does not also fix
  `plugins/trovato_webhooks`.
- An inbound webhook receiver, multi-tenant isolation (BL-129/130/132), or
  billing (BG5) — all stated dependencies of specific worked examples, not
  things this plan builds.
- Building any of the above. This note is architecture.

## 16. Build plan

Each run below ends at one green, merged pull request, in dependency order.
Matching rows are in [BACKLOG.md](../BACKLOG.md).

**Part 1 — event catalogue and dispatch hook.** The kernel-only change in
[section 13](#13-where-it-lives): `rules_event.rs`, the `TapDispatcher`
forward, the new tap name and WIT signature, and the two new dispatch call
sites in `moderation.rs` and `yaml.rs`'s import path. No Rules plugin exists
yet; this lands with a trivial test plugin implementing `tap_rules_event` to
prove the forward fires and the zero-listener path stays a no-op, and is
reviewable and mergeable entirely on its own.

**Part 2 — Rules plugin skeleton.** The `rule` table declared under
`[config] tables` with its YAML export/import, the per-blog service account
and its least-privilege role-grant screen (built fresh, per
[section 6](#6-actions) — nothing lower in the stack enforces this for
Rules), and bare CRUD (list, enable, disable, delete) with no builder UI yet
— enough to prove storage, tenancy scoping against `DEFAULT_TENANT_ID`, and
the account model end to end.

**Part 3 — when/if/then builder for the core set.** The form-based builder
for field comparisons (including `LinkCount`), role/type/stage/moderation
conditions, and the inline-safe actions (set a field, move a stage, deny
access) evaluated and executed *inline*, which in practice means on
`item.presave`/`item.access`/`field.access`/`comment.access` only — none of
the ten worked examples trigger there, so this part proves the builder and
the core condition set end to end (every condition a rule on
`comment.insert`, `user.register`, `category.term_insert`,
`moderation.transition` or `import.completed` needs) with queued actions
still stubbed to the run log rather than executed, not a working example on
its own. Add/remove a role is never inline-safe and is not part of this
part; it ships with Part 4, guarded per [section 6](#6-actions).

**Part 4 — queue-backed action execution, run log, idempotency and
budgets.** Every action this design has, run for real through the queue —
which in practice is every action except the Part 3 inline-safe set, since
none of the ten examples' triggering events is one of the four synchronous
ones and [section 7](#7-execution) queues everything else always — plus the
per-rule run log, the idempotency key check, and the
per-rule/per-blog/network budgets, including the per-blog fair-share mail
accounting inside the shared plugin-wide bucket. This is the point at which
worked examples 1, 3, 6, 7, 8 and 9 fully work — each needs only a core-set
condition (or none) and a queued action, nothing from Parts 5 or 6.

**Part 5 — outgoing webhooks and secrets.** HMAC signing, retry via the
existing queue semantics, the `rules_webhook` rate bucket, and `crypto-api`-
backed secret storage.

**Part 6 — Gather-backed conditions, recursion and "start another rule set".**
The canned count templates and the "item matches a saved view" fan-out from
[section 5](#5-conditions), both restricted to async events, the
`rules_depth` guard and cycle auto-disable, and the "start another rule set"
action (justified on its own terms, per [section 6](#6-actions), not because
an example needs it). Combined with Part 5's webhook action, worked example
2 fully works; combined with Part 3's `LinkCount` and Part 4's queued
execution, worked example 5 fully works.

**Part 7 — dry run and the admin debugging UI.** The `describe`/`execute`
split on every action, the dry-run picker, and the run-log viewer screen.

**Part 8 — network-scoped rules, and general availability of the role
action.** Gated on BL-129/130/132 landing first: the `administer network
rules` permission, the network sentinel tenant, the builder UI's
blog-vs-network mode switch, and lifting the single-blog restriction on
add/remove-a-role from [section 6](#6-actions) now that roles carry a
tenant.

**Part 9 — inbound webhook event source.** Gated on an inbound receiver
existing anywhere in the kernel or a plugin; adds the `webhook.received`
catalogue row and worked example 10 once that lands.

**Part 10 — billing event integration.** Gated on BG5 existing in any form;
adds the `billing.payment_succeeded` catalogue row and worked example 4 once
that lands.
