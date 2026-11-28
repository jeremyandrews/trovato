A security event that records a change to stored state now commits in the
same database transaction as that change. Before, every such path (account
creation, update, deletion, blocking and role changes, password set, change
and removal, passkey registration, rename and revocation, API token issue and
revocation, role and permission changes, `trovato import` and the command
line user commands) committed its change first and wrote its event to
`security_audit_log` afterwards, on a separate connection. A failed event
write was only logged, and a crash between the two left the change in place
with no record of it, so the security log could not be cited as complete.

Now the change and its event persist together or not at all. When the event
cannot be written, the change rolls back and the request fails as a failed
change would: a registration creates no account, a password change keeps the
old password, a delete keeps the account, a token is not issued. Mail, Redis
session ends, cache invalidation and plugin taps still run only after the
commit. Events with no stored change behind them (sign in results, refusals,
rate limit hits, recovery steps before the password is set, and sessions kept
only in Redis) are written as before.

A submission that makes several changes now makes them in one transaction.
The administrator's account form used to save the account, then each role,
then the password, then delete a blocked account's API tokens, each on its
own, with a failed role change or token deletion only logged: an account
could be blocked and still hold working tokens, or keep a new address with
its role change lost, while the screen said it had saved. The permission grid
saved each role on its own the same way. Now either everything the form asked
for changes, with its events, or nothing does and the screen says so. A
completed recovery records `recovery.completed` with the password it sets.

Each event now describes what its own write changed, also when another
request changes the same account or role at the same moment: an account's
earlier values are read from the row as last committed, a role membership or
permission change is recorded when the statement making it changed a row,
and `account.deleted` counts the sessions it ended rather than the sessions
it found listed. The rows themselves are unchanged in shape. The writes cost no
extra statements: the admin user edit and the password change read their
earlier values in the same statement that updates them, and an API token is
written together with its event in one statement, one fewer than before.
