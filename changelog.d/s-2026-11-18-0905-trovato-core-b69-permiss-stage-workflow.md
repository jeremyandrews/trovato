Items move between stages by the editorial workflow, and new content lands on
the stage marked default.

Nothing moved one item from one stage to another: the forms carried no stage,
the bulk actions set only `status`, `save-item` ignored a stage, and the
tutorial's `workflow.editorial` variable was read by nothing. The kernel now
reads it, as a typed `Workflow` (`models/workflow.rs`) cached beside the stages
in `ItemService`. A user moves an item from stage S to stage T only if they may
edit the item and a transition names exactly S to T with a permission they
hold; no transition is implied by another, an administrator may move any item,
and with no workflow configured only an administrator moves items.
`UpdateItem` gains `stage_id`, and `ItemService::update` and `create` apply the
rule (a create on a stage other than the default is a move from the default).
The admin and front-end edit forms offer a "Move to stage" selector listing
only the stages the user may reach, the content list has a "Move selected to
stage" bulk action that decides each item, and `save-item` accepts a `stage`
machine name (an unknown one is `ERR_PARAM_DESERIALIZE`, a refused move
`ERR_ITEM_ACCESS_DENIED`). A move takes the item's URL alias and menu links with
it, through the same `move_dependents` the stage publish uses. `config import`
refuses a workflow transition that names a stage or a permission it cannot
find, with the same permission rule role files use.

`/admin/structure/stages` said the default stage is "where new content lands",
and nothing read the flag: creation bound Live. `Item::create` now resolves a
missing stage to the default stage in the INSERT itself, Live when none is
marked, so every caller (forms, JSON, `save-item`, MCP) gets it.

The content list's bulk form could not be submitted from a browser: each row's
Delete button sat in its own `<form>` inside `#bulk-form`, so the browser closed
the bulk form after the first row and sent `_token` twice, and every bulk action
(publish, unpublish, delete, and the new move) answered 400 "The form could not
be read." The Delete buttons now submit forms rendered after the table through
the `form` attribute.

`ItemService::check_access` treated an item whose stage row could not be found
as visible; it now denies, and logs.

Contract: additive for `UpdateItem::stage_id`, the selectors, the bulk action
and the import check. Frozen for two observable changes to existing behaviour:
`save-item` honours a `stage` field it used to ignore, and a create with no
stage lands on the default stage rather than Live (the same thing on a site
that never marked another stage default). No WIT or SDK type changed. See
`docs/design/stage-workflow.md` and `UPGRADING.md`.
