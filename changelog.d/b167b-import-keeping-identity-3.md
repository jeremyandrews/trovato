`trovato import` now imports users and tiles, completing the record types a
site migration needs: users, items and their revisions, comments, URL aliases,
redirects and tiles, each keeping its id and its dates.

A user record is checked as the "Add user" form and `trovato user create`
check an account (a name and an address, neither held by another account
ignoring case) and keeps its sign-up date. A password arrives only as a hash,
and only as an Argon2 PHC string, the one format `User::verify_password`
accepts: a hash from another system is refused rather than stored as an
account that looks like it has a password and cannot sign in, and a refused
hash is never repeated in the error. Without one the account has no password
until a reset sets it, and a password set that way survives later runs. Roles
are granted by name, never taken away, and the superuser flag is not
something a file can set. No `tap_user_register` fires, so migrated accounts
get no welcome mail. A tile is checked as the tile form checks one: its
machine name, a type the kernel renders, a custom HTML format the site has, an
audience the form offers, and a machine name free on the live stage. The tile
form's list of audiences is now shared with the importer instead of being
private to the form.
