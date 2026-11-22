A site plugin can give its groups, clubs or projects vocabularies of their own,
administered by that owner's managers with the kernel's term list and term form.

A migration adds two nullable columns to `category`, `owner_type` and
`owner_id`, set together or not at all. Every existing vocabulary has neither
and behaves exactly as before. A vocabulary with an owner is listed at
`/admin/structure/categories` with its owner, and keeps that owner through
config export and import; an import naming only half an owner is refused.

The plugin surface (additive; no existing signature changed):

- `[[vocabulary_owners]]` in a manifest declares an owner type, the route its
  term administration is served under (`/<prefix>/{owner}/<name>`, outside the
  kernel's own paths), and optionally a permission whose holders administer
  every owner's terms.
- The new `vocabulary` host interface: `create-owned` creates a vocabulary for
  one owner of a declared type, and `list-owned` lists an owner's vocabularies.
  The SDK has `host::create_owned_vocabulary`, `host::list_owned_vocabularies`,
  `OwnedVocabularyRequest` and `Vocabulary`, with new error codes `-90`
  (owner type not declared), `-91` (invalid) and `-92` (id taken).
- The new `tap_vocabulary_owner_access` resolves the route's `{owner}` segment
  to an owner id and grants, denies or leaves the decision to the declared
  permission (`VocabularyOwnerAccessRequest`, `VocabularyOwnerAccess`).

Under the declared route the kernel serves the owner's vocabulary list, the term
list, and the add, edit and delete term forms. An anonymous user is sent to sign
in, a refused user gets 403, and a vocabulary or term of another owner is 404.

A term reference field can be limited to the vocabularies owned by the item's
owner with the field setting
`{"vocabulary_owner": {"owner_type": …, "owner_field": …}}`. The owner is the
value of `owner_field` on the same item. Every save then refuses a term from
any other vocabulary: on a form at the field, and through the API with a 422.

The term form, at `/admin/structure/categories` as under an owner route, no
longer accepts a parent term from another vocabulary. Its add form no longer
fails with a 500 on a hierarchical vocabulary that already has a term.
