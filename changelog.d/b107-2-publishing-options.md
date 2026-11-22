The item add and edit forms, both the admin ones and `/item/add/{type}` and
`/item/{id}/edit`, have a "Publishing options" section: Promoted to front page,
Sticky at top of lists, Authored by (a username, checked against the users) and
Authored on (a date and time, read as UTC). Both forms used to send
`promote: None, sticky: None`, and `UpdateItem` had no author or date. So an
editor could not put an item on the front page or pin it, the author was always
whoever created the item, and the date was always when they saved it.

`UpdateItem` gains `author_id` and `created`, and `ItemService::create_authored`
takes an authored on time for a new item. `ItemService` decides all four on
every write path, as it decides the published state: `gate_promotion` admits a
holder of `publish content` or the new `administer content`, and
`gate_authoring` admits only `administer content`. A refused write is
`WriteDenied::Promotion` or `WriteDenied::Authoring`, a 403 on `/item` and an
error on the admin form. Asking for the value already stored is no change, so
an ordinary save by anyone still goes through. The form shows only the options
the user may set. `administer content` is granted by no migration; an
administrator holds it implicitly. Every save records a revision as before.

Gathers already accepted `promote` and `sticky` as filter and sort fields, and
the gather builder now suggests them with the other item columns, so a front
page gather can list promoted items with the sticky ones first.
