An editor who may edit an item, administrator or not, can now read its whole
history: the revision history page names the author of every revision, previews
any past revision at `/item/{id}/revisions/{revision}`, rendered by the same field
renderer as the item page, and compares any two at
`/item/{id}/revisions/compare?from=...&to=...`, field by field, with changed text
marked word by word and unchanged fields counted rather than listed. The compare
is computed from the two revisions' `fields` snapshots: `item_revision.change_summary`
and `ai_generated` are written by nothing, so there was no stored summary to show,
and their doc comments now say so. Revert, which no test had ever posted as a
non-administrator, now has one.

Every revision route answers to the same rule, through one `ItemService` method:
the item must be viewable (otherwise it is not found) and then editable
(otherwise the request is refused), every revision id is checked against the item
in the path with one query scoped to it, and every revision's fields are cut to
the ones the viewer may view, so a hidden field appears in neither the preview nor
the compare, not even as removed. The history, preview and compare each make a
fixed number of queries, the authors' names included, whatever the number of
revisions or authors.

The history page's 500 for any item with a revision, which Ritrovo met on 0.103.0
and 0.104.0, was fixed earlier by `c137f2b`: both revision queries selected eight
columns while `ItemRevision` derives `sqlx::FromRow` over ten, and `#[serde(default)]`
does nothing for `FromRow`, so every non-empty result failed to decode. Its entry
in `KNOWN-ISSUES.md` is gone and BL-91 is marked fixed.
