The content list promotes a selection to the front page, removes it again,
and makes it sticky or not, and a content type can promote (and pin) its new
items by default. Items had the flags and the item forms set them since B107,
but `bulk_content_action` knew only publish, unpublish, delete and move, and a
new item was promoted only if its author ticked the box: the type form had no
default, where Drupal's Article and Blog types promote theirs, so a new post on
a migrated site never reached a front page or feed that lists promoted items.

The four bulk actions (`promote`, `demote`, `sticky`, `unsticky`) go through
`ItemService::update` like the others and are refused, before any write, to a
user holding neither `publish content` nor `administer content`, the rule
`gate_promotion` applies to one item. The type form's publishing options gain
"Promoted to front page by default" and "Sticky at top of lists by default",
stored as `promote_default` and `sticky_default` in the type's settings;
`ItemService::create` applies them when a write names no value, whoever makes
it, and the add form starts from them.
