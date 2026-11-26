The account functions a Drupal site has now all exist on Trovato.

Signing in accepts the e-mail address as well as the username, as Drupal's
"Username or e-mail address" field does. The login looked the account up by
name only. A wrong password and an address nobody has still fail with the same
message, and a blocked account still cannot sign in by either.

Registration can now require administrator approval, the third of Drupal's
three registration modes; Trovato had only open and administrators-only. In
that mode an applicant is told the account is pending approval, and confirming
the e-mail address leaves the account blocked until an administrator activates
it. A name that is taken is now named ("The name … is already taken."), as
Drupal says it; a taken e-mail address is still answered with the generic
message, so registration never confirms that an address has an account.

An administrator adding a user can tick "Notify user of new account". The new
user is then mailed a one-time sign-in link, from the new
`admin_created_account` template. Before, the form had no such choice and the
person was never told.

`/user/{id}/cancel` is Drupal's cancel page, with its four methods: disable the
account and keep its content; disable it and unpublish its content; delete it
and give its content to Anonymous; delete it and its content. Before, an
administrator could only block an account or delete it outright. It is reached
from the user edit form and needs `administer users`. The guards that protect
an administrator and the last superuser apply here too, and every method ends
the account's sessions. One's own cancel page is the existing self-service
deletion.

Accounts have pages: `/user/{id}` and `/users/{name}` show the name and
"Member for" to anyone holding `access user profiles` (or `administer users`),
and `/user` is the signed-in user's own page, with the edit form as its tab.
Comment author names link to them for those viewers. Before, there was no
account page at all, so every Drupal profile URL was a 404. A viewer without
the permission is refused before any lookup, so the refusal is the same
whether or not the account exists, and a blocked account answers `404` to
everyone but its administrators.
