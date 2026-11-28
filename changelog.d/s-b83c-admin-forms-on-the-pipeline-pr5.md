Every form the kernel and the in-tree plugins serve is now built by
`FormService::build`, drawn by the one form renderer and, when it posts,
checked by `FormService::process`. This covers the screens moved here:

- the config, people, reports, plugin and AI admin screens;
- the admin preferences and recovery settings, and an account's sessions;
- the installer;
- the GET forms: a listing's filter and sort, Gather's exposed filters, site
  search, the 404 page's search box, the admin header search and command
  palette, the admin search results page, and the revision compare form;
- the Log out buttons on the dashboard, on the preferences page and in the
  front-end header.

A GET form gets the same markup and the same way of showing errors as any
other, but no CSRF token and no saved state. The renderer marks every form it
draws with `data-form-pipeline`. The new `form_marker_test` opens every admin
page an administrator can reach, and the front end's pages signed in and not,
and fails on any `<form>` without that mark, so a form added later off the
pipeline fails CI.

On a POST to these screens, a hand-made post with a token and no
`_form_build_id` is refused as an expired form, and `tap_form_alter`,
`tap_form_validate` and `tap_form_submit` reach them for the first time. The
`list::delete_button`, `filter_start`, `filter_end` and `sort_form` macros
are gone. So are the `csrf`, `hidden_fields`, `actions`, `effect` and
`details_*` macros in `admin/macros/form.html`; that file now holds only the
field macros the renderer's admin templates map onto. The new
`admin_list::render_forms` builds a listing's filter and sort forms.

What an administrator or visitor sees change:

- **Row Delete** on users, roles, AI providers and timers (Cancel) is a link
  to a confirmation page of its own, with a red button and Cancel. It was a
  small form that posted to the generic confirmation page first. Users also
  get `/admin/people/{id}/cancel`, beside `/user/{id}/cancel`.
- **Errors shown on the form.** Mistakes that were a bare error page or a 422
  now show beside their field, with the form kept filled:
  - the chat and assistant settings (temperature, prompt length, minimum
    score; a whole number is asked for in words);
  - a budget's period or action;
  - the admin language;
  - the account cancel method;
  - every installer field. The installer reports all its problems at once,
    where it showed the first alone.
- **Filter forms** on every listing use the renderer's markup (`form
  filter-form`, a form-actions row). The media library's Type select has an
  empty "All files" choice; an old `?type=all` address still lists every
  type.
- **GET form addresses.** A named submit adds `op=` to the address: `op=Apply`
  on Gather filters, `op=Search` on `/search` and the admin search results
  page. Gather's pager links leave it out.
- **Gather filter selects** still start with "Any". A front-end select now
  shows its placeholder as the empty choice, as the admin select already did.
- **Field and form ids** follow the renderer, which names a field's id after
  the field:
  - form ids: `user_form`, `role_form`, `permissions_form`,
    `admin_preferences_form`;
  - field ids: `registration_mode`, `email_enabled`, `codes_enabled`,
    `language`, `method`, `q` (the 404, search and admin search boxes);
  - Gather filter ids lose their `filter-` prefix.
  - The admin search page's anchors follow the new ids.
- **Widths.** The renderer sets field widths, so a few fields are wider or
  narrower ("Items per page" is now the number width).
- **Required selects.** The AI protocol, budget period and action selects
  and the admin language select are marked "(required)" and have no empty
  choice. The language select's options lose their `lang` attribute.
- **One form per table.** The sessions list, the timers list, the plugin
  list, the AI providers' Test buttons and the assistant's Apply and Discard
  buttons each belong to one form for the table, posted to each row's own
  address. The AI providers page gives each form its own token, so running a
  Test no longer makes Save defaults fail.
- **Front-end header.** Log out posts a hidden form at the foot of the page.
  The button looks as before. Each signed-in front-end page view saves that
  form's state: one more query.
- **Wording that now differs:**
  - a few hints and messages that carry a value ("From {min} to {max}.",
    "Front page … does not resolve") are written out in the admin language
    when the page is drawn;
  - the timer edit form's repeat field no longer says "(optional)";
  - search setting bounds read "From 0 to 10.".
- **Assistant conversation page.** The message field's id is `message`, and
  the help line sits under Send.
