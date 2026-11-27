`UserService::create` and `update` wrote a user without bumping
`CacheKind::Users`: every other write to a user (a password change, a
delete, an admin cache clear) already told the generation counter that
other servers read before serving a cached user, but creating or
renaming an account did not. A second server kept serving a stale or
missing copy of the account until its own cache entry's TTL expired on
its own. Both methods now bump `CacheKind::Users` once after their
write commits. The CLI's own bump of `CacheKind::Users` after
`trovato user create` is gone with it, since the service now covers
that; the CLI still bumps `CacheKind::Permissions` for the roles it
grants once the account exists. The B167 importer writes users with
its own SQL inside the run's transaction, outside `UserService`
entirely, and was never part of this gap; it still bumps every kind
once at commit, not once per record.

Separately, `validate_username` gated registration, the installer, and
a self service rename, but not the admin "Add user" form or a rename
on the admin edit form, so an administrator could create or rename an
account to a name registration would refuse. Both admin forms now run
the same check and show the same message a rejected registration
gives. `UserService::create` and `update` (for a rename) now also run
`validate_username` themselves, so a caller that reaches the service
directly, such as the command line, refuses the same names rather than
relying on each caller to check first.
