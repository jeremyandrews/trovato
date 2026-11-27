The account and comment forms now go through the one form pipeline, so a
plugin's `tap_form_alter`, `tap_form_validate` and `tap_form_submit` reach them
as they reach the content and profile forms.

Each is built by `FormService::build`, drawn by the shared form templates and
checked by `FormService::process` with its CSRF token in `_token`, and its
errors are shown on the form. The form ids a plugin sees are `user_login` (the
login page and the login form on the access-denied page), `user_register`,
`user_password_change`, `user_mail_resubscribe`, `user_mail_unsubscribe`,
`user_delete_confirm` (the password step before deleting an account),
`user_delete_final`, `comment_form` (under an item and on the reply page),
`comment_edit`, `comment_delete`, `user_password_reset_request`,
`user_password_reset`, `user_recover` (the first step of account recovery) and
`user_api_token_create`. A submission now needs the `_form_build_id` the form
was served with; one without it is refused as an expired form.

The JSON endpoints are unchanged: `/user/login/json`, `/user/register/json`,
the comment route's JSON posts, the recovery steps and the API token endpoints.
A mail client's one-click unsubscribe still needs no form token.

What a visitor sees changes in a few places. The fields use the shared form
templates' markup, so a field's label is always shown, including on the comment
form under an item. Errors are listed at the top of the form they belong to,
not above the page. The login form on the access-denied page links to "Forgot
password?" from inside the form. Without JavaScript, the API token page now
creates a token, shown once on the page that answers, and the recovery page's
first step says that recovery needs JavaScript instead of doing nothing.
