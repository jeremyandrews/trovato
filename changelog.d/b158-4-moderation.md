Content moderation: a content type can be moderated, and its items then move
between Draft, Published and Archived by transitions each role is granted,
with a draft of a published item waiting beside it while visitors keep the
published version, and a moderated content list at `/admin/content/moderated`.
There was no per-item workflow at all. An item was published or not, the
editorial workflow in `models::workflow` moves items between site-wide stages
rather than through states of one item, an author who could publish published
on save, an edit of a published item went live at once, and there was nothing
for an editor to review.

A type is moderated when its settings say `moderation: true`, a checkbox on
the content type form, off by default. The workflow is Drupal's Editorial one,
fixed: Create New Draft (Draft or Published to Draft), Publish (Draft or
Published to Published), Archive (Published to Archived), Restore to Draft and
Restore (Archived to Draft or Published), each granted by its own permission
with Drupal's name (`use editorial transition publish`), so a migrated role
keeps its grants; `view latest version` is new too. `content::moderation` is
the pure rule; `ItemService::create_moderated` and `update_moderated` apply it
on every write path, the item forms, `/item` JSON, the admin forms, bulk
actions, revert and plugins alike, and the state decides the published status,
so on a moderated type the transition permission replaces `publish content`.
A refused transition is `WriteDenied::Transition`, a 403.

Each revision records its state in a new nullable column,
`item_revision.moderation_state`; nothing is backfilled, and a revision saved
before its type was moderated reads as Published or Draft from its status. The
item row stays the revision visitors see: a Draft saved over a published item
writes only a revision, so the read path is unchanged and asks nothing more.
The edit form works on the latest revision and offers a "Change to" select of
the states the user may reach in place of the Published checkbox;
`/item/{id}/latest` shows a pending draft to a user with edit access, `view
revisions` or `view latest version`; the revision list shows each revision's
state; and the moderated content list, for `view latest version` or `edit any
content`, lists every moderated item whose latest revision is a Draft or
Archived.

A draft over a published item does not yet move the item's file references,
alias, promote or sticky flags or stage: those follow when the draft is
published.

The revision history listed revisions by `created` alone, which holds whole
seconds, so a draft saved in the same second as the published revision before
it could be listed below it while `/item/{id}/latest` (ordered by `created`
then id) showed it as the latest. Both now break the tie by the later id.
