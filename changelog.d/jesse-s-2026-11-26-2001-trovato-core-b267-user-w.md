`UserService::create` and `update` wrote a user without telling the other
caches in `UserService::invalidate` and `clear_cache` about it: creating or
renaming an account bumped nothing in `CacheKind::Users`, so a second server
kept serving a cached copy of the old row (or nothing, for a brand new row's
id reused by another cache) until its entry's TTL expired on its own. Both
methods now bump `CacheKind::Users` once after their write commits, as every
other user write already did. The CLI's own `CacheKind::Users` bump after
`trovato user create` is gone with it, since the service now does that; the
CLI still bumps `CacheKind::Permissions` for the roles it grants after the
account exists. The B167 importer writes users with its own SQL inside the
run's transaction and was never part of this gap; it still bumps every kind
once at commit, not once per record.

Separately, `validate_username` gated registration, the installer, and a
self-service rename, but not the admin "Add user" form or its rename on the
edit form, so an administrator could create or rename an account to a name
registration would refuse. Both admin forms now run the same check and show
the same message a rejected registration would.
