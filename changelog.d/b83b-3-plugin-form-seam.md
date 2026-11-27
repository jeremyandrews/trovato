A plugin page can hand the kernel a form, and the contact form and the Book
admin screens now do, so every form a plugin serves goes through the one form
pipeline: built by `FormService::build`, drawn by the site's form templates,
checked by `FormService::process`.

The plugin API gains three optional parts, each added without changing
anything that exists:

- `ApiResponse::form` (and `ApiResponse::with_form`): the form, in the shape
  `tap_form_alter` already sees. The kernel builds it for this visitor, so
  `tap_form_alter` reaches it, and draws it after the response's body. Its id
  must start with the plugin's name.
- `ApiRequest::form_values` (and `ApiRequest::form_value`): on a POST of that
  form, the values of its inputs, set only once the kernel has checked the
  token, that the form was served to this visitor by this plugin, every
  required input, and `tap_form_validate`. `tap_form_submit` follows a
  callback that answers below 400. A submission that fails a check never
  reaches the plugin: the kernel shows the page the form came from again, with
  the errors and what was typed, under 422. A POST without a form build id is
  handled exactly as before.
- `render::escape_html`, also `trovato_sdk::escape_html`: the five plugins that
  each carried their own copy (contact, Book, SEO, series and the
  `test_plugin_api` fixture) use it instead. The contact, Book and fixture
  copies of a form body decoder are gone with the hand written forms.

A themed `tap_api` body and `tap_item_view` output that are a render tree (a
JSON object with `#type`) are now drawn by the theme's element templates; they
were shown as their JSON text before.

What a visitor or an administrator sees change:

- The contact form is drawn by the site's form templates. An empty name,
  address or message is reported as "… is required." beside its field, and
  the plugin's own checks (an address that does not look right, a field over
  its length) are shown beside their fields too, not in a list above the form.
- The Book admin screens render inside the admin theme with its sidebar,
  instead of as a bare page. The forms to create a book and to place a page
  are the kernel's, and a refused one (an item that does not exist, a page in
  another book, a placement that would make a cycle) is shown again on the
  form with the reason, where it was a plain-text 400 before. Removing a page
  now goes through a page that says what removing it does (its children move
  up, or the whole book goes with its root) and asks to confirm, in place of a
  Remove button on every row.
- The Book POST routes act only on their kernel-built forms. A hand written
  post with a token and no form build id changes nothing and gets the screen
  back.

The workspace version is already ahead of the newest release tag, so it is not
raised; the additions are recorded in `docs/design/Versioning.md` and
documented in `docs/plugin-development.md`.
