Item writes now honour field-level edit access, and whether an item is published
is a permission rather than a value the client picks.

Field access was built and tested for reading. The decision has always taken an
operation and the design has always named two, `view` and `edit`, but every
caller asked for `view` and no write path asked at all — so a user who could
edit an item could overwrite a field they were not even allowed to see, and the
edit form showed them its current value to do it with. `create`, `update`,
`revert_to_revision` and `save_translation` now run the `edit` decision on the
client's own submission, before the presave tap, and refuse a change to a field
the user may not edit, naming the field. A denied field the submission leaves out
is copied back from the stored item rather than erased, which is what makes a
partial JSON update safe. The item forms render a field the viewer may not see
not at all, and one they may see but not edit as a disabled control, so a
refusal only happens on a request built by hand.

Whether an item is published came straight from the client — a `status` key in a
JSON body, a checkbox on the form — with no permission behind it, so anyone who
could create content could put it on the live site. The new `publish content`
permission is checked on every item write: without it a create asking to publish
is refused, a create that says nothing stores a draft, and an update or revert
that changes the published state is refused in either direction. Bulk publish and
unpublish on the admin content screen ask for it too. The published checkbox is
rendered only for a user who may publish, and travels with a hidden marker
saying it was rendered, because an unchecked checkbox posts nothing and the
handler read its absence as "unpublish". A forward-only migration grants
`publish content` once to every role that already held `create content`,
`edit own content`, `edit any content` or a per-type `create … content`, so no
existing site loses the ability to publish on upgrade; nothing implies it
afterwards.

The field-access decision cache is keyed on the viewer now — user id and
authenticated flag alongside the permission hash — not on the permission hash
alone. The payload a plugin receives carries the viewer's identity, and the
contract asking a plugin to decide from permissions alone was advice, not
enforcement: one plugin author taking the payload at face value meant one user's
decision was served to every other user holding the same permissions for up to
five minutes, an anonymous visitor and a logged-in user with the same effective
set included. Anonymous viewers all carry the nil id, so they still share
entries with each other.
