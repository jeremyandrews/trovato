Bulk publish, unpublish and delete on `/admin/content` now work. The bulk form
declared its selection as a sequence of item ids read by `axum::Form`, whose
`serde_urlencoded` deserializer cannot build a sequence from the repeated
`ids[]` keys the list screen posts, so every request that selected anything was
refused with a 422 before the handler ran; only an empty selection ever got
through. The handler now reads the body with `url::form_urlencoded`: `_token`
and `action` must each appear exactly once (a 400 otherwise), every selected id
must be a valid item id or nothing is written, and a repeated id is acted on
once. A selected id that names no item is now reported as failed by bulk
publish and unpublish, as bulk delete already reported it, instead of being
counted as done. The permission checks for each action and the CSRF check are unchanged,
and every selected item is still decided by the item service as the acting
user, so an item that user may not edit, publish or delete is counted as failed
rather than changed.
