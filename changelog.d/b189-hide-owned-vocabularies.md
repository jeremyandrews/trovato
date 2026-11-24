A vocabulary owned by a group, club or project, and its terms, can now be read
only by someone who may see that owner. Owned vocabularies (B108) stored their
owner on the `category` row, but every category read path ignored it: the
category and tag API (`/api/v1/categories`, `/api/v1/category/{id}` with its
`tags` and `roots`, `/api/v1/tag/{id}` with its `parents`, `children`,
`ancestors`, `descendants` and `breadcrumb`, and the same routes under the
unversioned `/api` alias) needed no login and listed every owner's
vocabularies and terms to anyone, and so did the MCP `list_categories` and
`list_tags` tools, a gather term page or feed argument bound to an owned
vocabulary, a gather's exposed category filter and `/topics`. The term
autocomplete already refused owned terms to anyone who could not edit the
field, so the read rule and the edit rule disagreed.

The rule is decided once, in `content::owned_vocabulary`, and every one of
those paths uses it. A site-wide vocabulary reads exactly as before, for every
caller. An owned one is visible to a site administrator or a holder of
`administer categories`, to a holder of the permission the owner's
declaration names, and to a user the owner's plugin grants a new read
operation, `view`, of `tap_vocabulary_owner_access`, asked with the stored
owner id as the key and once per owner rather than once per row. A plugin that
does not answer `view`, or answers anything but `grant` for that very owner,
leaves the vocabulary hidden. To anyone else an owned vocabulary or term is a
404, the same 404 a vocabulary or term that does not exist now gives on every
read route (the tag list, roots and hierarchy routes used to answer an unknown
id with an empty 200), so the answer does not reveal that it exists. The
category list filters in SQL, so its pager counts what the caller can see.
Writes, the save check and the term autocomplete are unchanged.
