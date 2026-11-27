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
rate limit hits, recovery steps and sessions kept only in Redis) are written
as before. The rows themselves are unchanged in shape. The writes cost no
extra statements: the admin user edit and the password change read their
earlier values in the same statement that updates them, and an API token is
written together with its event in one statement, one fewer than before.
