# Stage workflow: moving an item between stages

Status: decided (B69 Part 2). Closes BL-92 and BL-93.

An item's `stage_id` was set once, on create, to Live, and nothing moved it
afterwards: no route, no host function, no bulk action. `StageService::publish`
moved a whole stage and no route called it. The tutorial shipped a transition
graph (`variable.workflow.editorial.yml`) that nothing read, and the stage
marked default was stored and never consulted. Sites worked around it by
writing `item.stage_id` with raw SQL, which skips revisions, taps, the search
index and the cache. This note decides how an item moves, who may move it, and
where new content lands.

## 1. Where transitions live

**Decision.** One site configuration variable, `workflow.editorial`, in the
shape the tutorial file already has:

```yaml
key: workflow.editorial
value:
  transitions:
    - {from: incoming, to: curated, permission: "edit any content", label: "Promote to Curated"}
```

Stages are named by machine name; the permission is a permission name. The key
stays `workflow.editorial` because that is what the tutorial ships and what a
site that followed it already has stored; renaming it would silently disable
every such site's workflow.

The kernel parses it into a typed `Workflow` (`models/workflow.rs`, beside the
stage model): a list of `Transition { from, to, permission, label }`, each
stage resolved to its id. It is loaded through `ItemService`, cached with the
stage cache (same capacity bound and TTL, cleared by `clear_stage_cache`), so a
change to the variable is seen within the stage cache's TTL with no hook in
the config storage, and a `config import` from another process is seen the
same way stages are.

Validation, twice:

- **At `config import`**, a `workflow.editorial` variable whose value does not
  parse, names a stage that neither exists nor is in the import set, or names a
  permission that is not a kernel permission, a declared plugin permission or
  one already granted, fails the import with a message naming the transition.
  The permission rule is the one role imports use (`validate_role_permissions`),
  so the two cannot disagree.
- **At run time**, an entry that does not resolve (a stage deleted since, an
  empty permission) is dropped with a warning and is never usable. A value that
  does not parse at all is an empty workflow, and is logged.

## 2. Who may move an item: default deny

A user moves an item from stage S to stage T only if

1. `ItemService::check_access(item, "edit", user)` allows it, and
2. the workflow has a transition from S to T whose permission the user holds.

An administrator may move any item to any stage, workflow or not. With no
workflow configured (no variable, or one with no usable transitions), only an
administrator moves items. No transition is implied by another: `a -> b` and
`b -> c` do not allow `a -> c`, and `a -> b` does not allow `b -> a`.
A "move" to the stage the item is already on is not a move and needs nothing.

The rule is one function, `ItemService::may_move`, called by `create` and
`update` and by the code that lists the stages a user may choose (the form
selector), so what the form offers and what the service accepts cannot drift.

## 3. Stage and status are independent

A transition changes `stage_id` and nothing else; the published flag is
`status`, gated as before by `publish content`. "Publish" on Live therefore
means two separate things a site may want together: the item is on the Live
stage (visible to anonymous visitors at all) and its status is published
(visible to those with `access content`). The tutorial's labels ("Publish",
"Unpublish to Curated") describe stage moves and still read correctly: moving
to Live is what makes curated content public; the item keeps its status. The
tutorial chapter says so.

## 4. What moves with the item

An item's URL aliases and menu links on the source stage move to the target in
the same transaction as the item row. `stage/mod.rs` already moves a whole
stage's aliases and menu links (`publish_dependents_to_target`); that SQL is
narrowed by an optional item, and both the whole-stage publish and the
one-item move call it, so there is one statement per table rather than a
second copy beside it. An item's dependents are the aliases whose `source` is
`/item/{id}` and the menu links whose `path` is `/item/{id}`, on the source
stage. A conflict (the target already has an alias with that path) fails the
move and rolls back the item, rather than overwriting the target's row.

## 5. Creation and the default stage (BL-93)

`Item::create` binds the stage with one expression inside the one `INSERT`:

```sql
COALESCE($11, (SELECT tag_id FROM stage_config WHERE is_default), <live>)
```

No extra round trip, and every create path reaches it (routes, `save-item`,
the admin form, config import). Config import keeps binding Live explicitly.
On a site whose default is Live (the migration seeds it) nothing changes.

Creating directly on a stage other than the default counts as a transition
from the default stage to that stage and is checked as one. Creating with no
stage is not a move and needs no transition: content lands where the site said
it lands.

An author whose new item lands on an internal stage can still open and edit
it with no plugin enabled: `check_access` refuses anonymous users on an
internal stage, skips the published fast path, and then falls back to the
role permissions, where `edit own content` / `view own content` (or the "any"
forms) decide as for any other stage. The tests show it with a user holding
only `create page content`, `edit own content` and `access content`.

## 6. The one write path, and its three callers

`UpdateItem` gains `stage_id: Option<Uuid>`. `ItemService::create` and
`update` check the rule in section 2 and refuse with the existing item-level
denial (`access denied`, a 403 on every route), before the presave tap. The
stage change then rides `Item::update`'s own transaction with the new revision,
so revisions, `tap_item_update`, `tap_item_update_index` (and the search
vector, which a trigger maintains), file references and cache invalidation
happen exactly as for any other edit.

Every way in is a thin caller of that:

- **The edit forms.** A stage selector on the admin edit form and on the front
  end edit form, listing only the stages this user may move this item to (plus
  its current stage), absent when there are none. The submitted value is a
  stage id; anything else is refused.
- **A bulk action.** "Move to stage", with the target as its parameter, on the
  content list. The bulk action registry B68's design note describes
  (`BulkActionRegistry`) has not merged; the content list's existing action
  dispatch (`routes/admin_content.rs`) is today's registry, so the action is
  added there, to move into the registry with the others when it lands. It
  requires `edit any content` to submit and decides each item at execution
  time through `ItemService::update`, like the other actions.
- **`save-item`.** The JSON a plugin passes may carry `"stage": "<machine
  name>"` on create and on update; it is resolved through the stage cache and
  decided by the same rule as the acting user. An unknown machine name is
  `ERR_PARAM_DESERIALIZE`; a refused move is `ERR_ITEM_ACCESS_DENIED`. A
  background write with the `item_background` capability carries kernel
  authority, as for every other item decision.

## 7. Contract

- The workflow variable, the form selector, the bulk action and
  `UpdateItem::stage_id`: additive.
- `save-item` honouring a `stage` field it used to ignore: an existing host
  function's observable behaviour changes, so it is **frozen** by the version
  map, as BL-92's row says. The WIT signature and the SDK types do not change
  (the item JSON is untyped at the boundary), so `KERNEL_API_VERSION` does not
  move outside a release: it follows the next minor, and the change ships in
  that minor's notes. A plugin that never sends `stage` sees no difference
  except that its new items land on the default stage, which on an untouched
  site is Live.
- New content landing on the default stage: a behaviour change for a site that
  marked a stage other than Live default, recorded in `UPGRADING.md`.
