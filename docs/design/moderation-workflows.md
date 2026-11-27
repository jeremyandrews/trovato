# Moderation workflows: a site's own states and transitions (B301)

Status: decided. Closes the part of B158 that left the Editorial workflow
compiled into the kernel.

Content moderation (B158) gave a moderated type's items three states, Draft,
Published and Archived, and five transitions between them, each a `const` in
`content/moderation.rs` and enforced in the database by a check constraint on
`item_revision.moderation_state`. A site that wanted a fourth state, a review
step before publishing, could not add one without a kernel change. This note
decides how a site defines its own states and transitions, how a type is
assigned one, and what stays true of the built-in workflow so an upgraded site
sees no change.

## 1. Where a workflow lives

**Decision.** One site configuration variable, `moderation.workflows`, holding
every workflow the site has:

```yaml
key: moderation.workflows
value:
  workflows:
    - id: review
      label: Review
      default_state: draft
      states:
        - {id: draft, label: Draft, published: false, default_revision: false}
        - {id: in_review, label: In review, published: false, default_revision: false}
        - {id: approved, label: Approved, published: false, default_revision: false}
        - {id: published, label: Published, published: true, default_revision: true}
        - {id: archived, label: Archived, published: false, default_revision: true}
      transitions:
        - {id: start_draft, label: Start Draft, from: [draft], to: draft}
        - {id: submit, label: Submit for review, from: [draft], to: in_review}
        - {id: approve, label: Approve, from: [in_review], to: approved}
        - {id: publish, label: Publish, from: [approved], to: published}
        - {id: archive, label: Archive, from: [published], to: archived}
        - {id: revise, label: Revise, from: [published], to: in_review}
```

A state:

- `id`: the machine name stored in `item_revision.moderation_state`. At most
  32 characters (the column is `VARCHAR(32)`), matching `^[a-z][a-z0-9_]*$`.
- `label`: what a person calls it, shown on the item form, the moderated list
  and the revision pages.
- `published`: whether an item in this state is what visitors with
  `access content` see. A published state must have `default_revision: true`:
  publishing always replaces what visitors see, never waits beside it.
- `default_revision`: whether a save into this state over a published item
  replaces the item row (what visitors see) rather than waiting beside it as
  a forward revision. The built-in workflow's Draft is the one state with
  `default_revision: false`; a site's own workflow can have more than one
  (`review`'s draft, in_review and approved are all "still being worked on").

A transition has an `id`, a `label`, the states it leaves `from` (one or
more) and the one state it arrives `to`. It grants its own permission, named
`use <workflow id> transition <id>` — Drupal's Editorial workflow names its
own transitions the same way, so a migrated role keeps its grants on the
built-in workflow, whose id is `editorial`.

Every workflow a type's items might move through must be in this one
variable; there is no per-type variable and no merge across workflows. A
site adding a workflow through `config import` writes the whole list, the
way `workflow.editorial`'s transition graph is one unit already
(`docs/design/stage-workflow.md`).

The kernel parses it into typed `ModerationWorkflow` / `State` / `Transition`
values (`content/moderation.rs`), read through `ItemService::moderation_workflow`
fresh on every use — one `SiteConfig::get`, never cached, the same way
`ItemService::stage_view` reads `workflow.editorial` — so a change to the
variable or to a type's assignment is seen at once by every server, with no
hook in the config storage, including a `config import` from another
process. `None`, an unparsable value or an empty list all fall back to
`builtin_editorial()`, a pure function returning today's three-state
workflow exactly: an upgraded site reads the same workflow it always had,
under the name `editorial`, and the migration (below) seeds exactly that
value so the fallback is never actually exercised on a migrated site.

## 2. Assigning a workflow to a type

A content type's settings carry two keys, unchanged in meaning from B158 and
additive beside it:

- `moderation: true` turns moderation on, as it always did. Kept as a plain
  boolean (not folded into the workflow name) so a previous release rolled
  back onto this data still reads every moderated type as moderated.
- `moderation_workflow: <id>` names the workflow; absent means `editorial`.
  A type naming a workflow that does not exist at run time falls back to
  `editorial`, logged, the same shape as an unusable stage workflow entry.

The content type form's "Moderate content of this type" checkbox is joined
by a workflow select, options the stored workflows, defaulting to `editorial`
(`routes/admin_content_type.rs`, `templates/admin/content-type-form.html`).
Creating or editing a workflow itself is not here: like the stage workflow,
workflows are managed by `config import`, and an admin screen to build one is
a later change.

## 3. The write path

`ItemService::create_moderated` and `update_moderated` read the type's
workflow and apply it exactly where the old code applied the three fixed
constants:

- a new item starts on `default_state`;
- the published status (`item.status`) is the target state's `published`
  flag;
- a save into a state with `default_revision: false` over a published item
  is a forward revision: recorded as a new `item_revision`, with the item
  row — what visitors see — left as it is, the way a Draft saved over a
  Published item always has been;
- a save naming no state maps a bare `status` change onto the workflow: `status:
  1` is the first published state (in the workflow's order) with a
  transition from the current state; `status: 0` from a published state is
  the first state with `published: false, default_revision: true` (an
  archive-like move) with a transition from it; anything else keeps the
  current state. `ItemService::gate_transition` then decides the move exactly
  as it always has: the workflow must have the transition, and the user (or
  the kernel's background principal) must hold its permission.

**Scheduled publishing** (`plugins/trovato_scheduled_publishing`) sends only
`{"id", "status"}` through `save-item`, never a moderation state, so it is
entirely a bare status change decided by the rule above. On the built-in
workflow, Draft has a direct transition to Published, so a scheduled publish
of a brand-new draft still works exactly as before. On a workflow like
`review`, where publishing only follows a review pipeline, the same
`status: 1` save on an item still in Draft finds no transition that reaches
a published state, and resolves back to the current state: whether that is
accepted as a no-op or refused depends on whether the workflow grants the
background principal a transition that leaves the item where it is (a
"start" transition most workflows need anyway, for the same reason a type
needs one for a human to create an item in the first place — see §5). Either
way the item is never published by that save.

## 4. A stage move and a forward revision together

A requested stage move and a save that resolves to a forward revision cannot
both take effect in one request: the forward revision branch returns before
`Item::update` runs, so a stage move asked for in the same save would be
silently accepted and then dropped. `update_moderated` now refuses the
combination with `WriteDenied::StageMoveWithForwardRevision` rather than
applying one and dropping the other; the message asks the caller to save the
stage move separately. This holds on every workflow, including the built-in
one: it is not new to a site with its own workflow.

## 5. Permissions

A permission is derived, never declared: one per transition of every
workflow `moderation.workflows` holds, named `use <workflow id> transition
<id>`. They are not in `KERNEL_PERMISSIONS`, the same way a text format's
`use <format>` permission is not — both are added to the permission grid and
to `config import`'s accepted permission set from what is actually stored
(`routes/admin_user.rs::grid_permissions`, `config_storage/yaml.rs::known_permissions`),
so a site's own workflow and a role file granting its transitions can arrive
in the same import. The built-in workflow's five transitions are named
exactly as Drupal's Editorial workflow names them
(`use editorial transition publish`, and so on), so a role migrated from
Drupal, or from a release before B301, keeps its grants unchanged.

A workflow that lets anyone create an item at all needs a transition from
its `default_state` to itself (the built-in workflow's `create_new_draft`),
because every write — including an implicit create that asks for no state —
is decided by `gate_transition`, which requires the workflow to have *some*
transition between the two states it is asked to move between, even when
they are the same state. A workflow with no such transition can never be
written to by anyone, administrators included: `may()` checks the transition
exists before it asks who holds its permission, the same rule the built-in
workflow's unit tests already prove.

## 6. What `config import` refuses

`validate_moderation_workflows` (`config_storage/yaml.rs`), called beside
`validate_workflow`:

- anything `content::moderation::problems` finds wrong with the
  `moderation.workflows` value itself: a transition naming a state that does
  not exist, a workflow with no published state, a `default_state` that is
  not one of its states, a state id longer than 32 characters or not
  matching `^[a-z][a-z0-9_]*$`, a published state with
  `default_revision: false`, or two workflows in the same value disagreeing
  about what a shared state id means (its `published` or `default_revision`
  flags);
- an item type (stored, or arriving in the same import) naming a workflow
  that is neither stored nor arriving in the same import;
- a workflow change that drops a state some stored `item_revision` of a type
  using that workflow still holds: that revision's state would no longer
  resolve to anything the workflow knows, and would read from its `status`
  as if moderation had never touched it (the same reading a revision saved
  before its type was moderated gets). One count query per removed state, so
  the failure names which revisions are at risk and by how many.

## 7. Out of scope

- An admin screen to create or edit a workflow: config import only, for now,
  like the stage workflow.
- Mapping a revision's state when its type switches workflow: an unknown
  value reads from its `status`, as it always has.
- Carrying a moderation state through `trovato import`
  (`docs/design/import.md`), a moderation state filter or column on
  `/admin/content` itself, and translating a workflow's labels through
  config translation.
- A `moderation.transition` rules event: `docs/design/rules.md` already
  names where one would go; this change does not add it.

## 8. Contract

- The variable, the type setting, the derived permissions and the new
  `WriteDenied` variant: additive.
- Dropping `item_revision_moderation_state_check` (the migration): the
  previous release only ever wrote `draft`, `published` or `archived`, and
  never relied on the database refusing anything else, so no released code
  reads or writes depend on the constraint existing. Marked
  `-- trovato:contract 0.105.0` in the migration.
- A site that keeps the three built-in states needs nothing: the permission
  names, the write-path behaviour and the screens are unchanged.
