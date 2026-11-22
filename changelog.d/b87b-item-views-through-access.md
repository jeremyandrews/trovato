The front page's promoted listing is now the viewer's. It rendered `list_promoted` with no viewer, so a visitor without `access content`, or one a plugin denied an item to, was shown every promoted item with its body teaser. The listing now passes its candidates through `ItemService::filter_page_for_view` for the viewer, over-fetching so a few hidden items do not leave it short, which also drops any field the viewer may not see before the teaser reads it.

The teaser cut the filtered HTML of the body at 200 characters, which could end inside a tag or an entity and leave an element open across the rest of the page. It now takes the body's plain text, cuts that, and escapes it.

An item set as the front page rendered its fields with a renderer of its own, which knew nothing of Blocks fields, compound sections or a text field's `_format` key. It now uses the item page's (`routes::item::render_item_fields`), so the item reads the same at `/` as at its own address.

The front-end edit form (`GET /item/{id}/edit`) checked `edit` only, so a user who may not see an item got a 403 that confirmed it exists, where the item page and the revision pages answer 404. A user it refuses now gets 404 when they may not view the item and 403 when they may; a user who may edit still gets the form without a view grant, as an author with `edit own content` does on their own unpublished item.
