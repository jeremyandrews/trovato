An account may now have no email address. An administrator creating or
editing an account, `trovato user create` and a `trovato import` user record
all accept a blank address; such an account signs in with its name and
password, and anything that would mail it (a welcome message, a farewell on
deletion, a recovery code or notice, a comment notification) simply does not,
with no error and nothing logged about it. Self registration is unchanged and
still requires an address.

An empty address is stored as an empty string, the representation the
database already treated as "no address" for uniqueness: the existing unique
index on `mail` already excludes it, so no migration is needed. What changed
is that `User::find_by_mail` now refuses to match an empty or whitespace-only
argument, so a login or a password reset request that names no address can
never land on one of these accounts, and a new `User::mail_address` helper
replaces the ad hoc emptiness checks scattered across the mail-sending call
sites, closing two that were missing it (the admin "notify user of new
account" mail and the account-deletion farewell).
