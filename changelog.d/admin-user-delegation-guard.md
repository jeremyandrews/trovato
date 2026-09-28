A holder of `administer users` who is not a superuser can now act only on
accounts, roles and permissions within their own authority, and the last active
superuser can no longer be demoted, blocked or deleted from the admin screens. The
delegation rule for `administer users`, that a non-superuser may hand out only
what they already hold, was enforced on the role checkboxes of the user form and
nowhere else. The user edit and delete routes, the permission grid and role
deletion trusted any holder of the permission with accounts and permissions
beyond their own, superusers' accounts included, and nothing kept the last
superuser in place. The rule now lives in one place in `routes/admin_user.rs` and
all four paths use it: an account is within reach only when the actor holds every
permission it holds, and never when it is a superuser's; a role can be deleted
only when the actor holds every permission it carries; and the grid neither
grants nor revokes a permission the actor lacks. The grid also ignores, for every
actor, any permission name it did not render. The edit and delete routes refuse
to take the last active superuser out of that role, using the same
`blocks_last_admin` rule self service account deletion already applies.
