An item page and a gather listing row now carry their author's display name,
and a profile link when the viewer may see one. `view_item` put `item`
(carrying only `author_id`), `viewer` and the rest in the template context but
never a name, so a theme had to hardcode "by Jeremy" or print nothing at all;
`/api/item`'s `include=author` was the only read path that resolved one. The
item page and a gather row now resolve it through one function,
`UserService::author_views`, which reads the same user cache `/api/item`
uses: an author the cache holds costs no query, a cold item page pays the one
`find_by_id` any author lookup would, and a cold gather loads all of its
uncached authors in one query, never one per row. Warm pages run no more
queries than before; cold ones run one more. The shipped `item.html`,
`item--blog.html`, `gather/row.html` and blog listing templates print
`by {{ author.name }}`, linked to `author.url` only when it is set. A deleted
account's content is reattributed to the kernel's anonymous account, which
shows its own name and is never linked; an id matching no user row at all
shows the kernel's anonymous display name. Only the display name and an
access-checked URL reach a template, never the mail address or anything else
a `User` record carries. A gather that defines its own include named `author` keeps
that include's result; the byline never replaces it.
