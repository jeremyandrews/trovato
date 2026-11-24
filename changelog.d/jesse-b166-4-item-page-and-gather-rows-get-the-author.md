An item page and a gather listing row now carry their author's display name,
and a profile link when the viewer may see one. `view_item` put `item`
(carrying only `author_id`), `viewer` and the rest in the template context but
never a name, so a theme had to hardcode "by Jeremy" or print nothing at all;
`/api/item`'s `include=author` was the only read path that resolved one. Both
the item page and a gather row now resolve it through the same
`UserService::author_views`, one shared function: a single id reuses the
cached `find_by_id` lookup `/api/item` already pays for, so the item page adds
no query beyond a cold cache miss any author lookup would cost; a gather row
resolves every row's author in one batched query, never one per row — the
same shape `resolve_row_urls` (B74) already gives `url`. The shipped item and
gather-row templates print `by {{ author.name }}`, linked to `author.url` only
when it is set. A deleted account's content is reattributed to the kernel's
anonymous sentinel account, which has had its own row since
`20260212000001_create_users.sql`; that account, and any id matching no user
row at all, show the kernel's anonymous display name and are never linked,
permission or not. Only the display name and an access-checked URL reach a
template — never the mail address or anything else a `User` record carries.
