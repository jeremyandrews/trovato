Pressing Test on a provider row at `/admin/system/ai-providers` could show
"Latency: undefinedms" instead of the connection test's real result.

The root cause: `static/js/ai-admin.js` submitted the test form with
`fetch(form.action, { body: new FormData(form) })`, which a browser sends as
`multipart/form-data`, but `test_provider` in
`crates/kernel/src/routes/admin_ai_provider.rs` extracted
`Form(form): Form<CsrfOnlyForm>`, which only accepts urlencoded bodies. Axum
rejected the mismatched request before the handler ran, with a plain text
rejection body, so the handler's own doc comment promise of "JSON in all
cases" never held, and the script's `data.latency_ms` read as undefined.

The script now sends `new URLSearchParams(new FormData(form))`, matching what
the route accepts, and the route pulls `Form` out by hand so that a body it
still rejects (a wrong content type, a malformed field) answers with the same
`{success, message, latency_ms}` shape every other failure in the handler
already uses, rather than axum's bare rejection text. The script also only
renders a latency line when `latency_ms` is actually a number, so no response
shape can print "undefinedms" again.

A sweep of every script under `static/js/` that posts a `FormData` body found
one more mismatch of this class: none. `trovato.js` builds a `FormData` only
to read its values into a plain object before sending JSON; `file-upload.js`,
`admin-media-picker.js`, `media-picker.js`, and the block editor's image
uploader all post `FormData` to routes that take `Multipart`, which is
correct and unchanged.
