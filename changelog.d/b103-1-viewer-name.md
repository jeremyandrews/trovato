Every page template now gets `user_name`, the signed-in viewer's account name,
and the stock theme's header reads "Logged in as <name>". `inject_site_context`
built the viewer from the user record but kept only the id, the roles and the
permissions, so only `/user/profile` and the admin dashboard, which put the name
in their own context, could say who was signed in; a theme could print "Logged
in" and never whose session it was. The name now rides on the `UserContext`
built from that same record, so it costs no query, and it is set only for a
signed-in viewer: an anonymous visitor's context has no `user_name` key. It is a
key of its own beside `user_authenticated`, not a field of `viewer`, and it
carries the account name and nothing else.
