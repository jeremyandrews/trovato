Access rules turn away registrations and sign ins by username, email address or
client address, the way Drupal sites have long stopped spam registrations. A
rule is a type, a mask in which `%` matches any run of characters (`spam%`,
`%@example.com`, `203.0.113.%`), and allow or deny; masks match the whole value
and ignore case, and an allow rule overrides any deny rule of the same type, so
a site can deny a domain and still admit one address in it. Rules live in a new
`access_rule` table, which starts empty: an existing site blocks nothing until
it adds one.

Registration, from the form and from `/user/register/json`, checks the
username, the email address and the client address, and says which one is not
allowed. Sign in checks the username and the client address before the password
is verified, after the lockout check, and answers a refused attempt with a 403.
An administrator creating an account on `/admin/people/add` that a rule would
block is warned on the user list rather than refused.

`/admin/people/rules` lists, adds, edits and deletes rules, and has a box that
tests a value against them without registering or signing in. Every form on it
goes through the forms pipeline with its CSRF token. It takes the new
`administer access rules` permission, which no migration grants; an
administrator holds it implicitly. It sits under People in the admin sidebar and
on the dashboard.
