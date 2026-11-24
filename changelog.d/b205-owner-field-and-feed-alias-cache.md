A category term box limited to the item's owner now searches when the owner
field is one this editor may not see. The edit form leaves such a field out,
and the widget read the missing control as an empty owner field: it showed
"Choose the owner first" and never asked the server, though the server would
have offered the stored owner's terms, the ones the save accepts. The form now
names the owner field to the widget only when the field is on the form.

A feed's channel link no longer sticks to the raw `/gather/{query_id}` path
for five minutes after one failed `url_alias` lookup: a lookup that fails is
answered for that request only and is not cached. A lookup that read
`url_alias` before an alias write cleared the cache can no longer put the old
alias back into it afterwards; it drops its answer when the cache was cleared
while it ran.
