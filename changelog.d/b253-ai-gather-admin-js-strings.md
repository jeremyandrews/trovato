The last English words three admin screens built outside the translation
function now go through it too, and a new test catches the next one.

`static/js/ai-admin.js` wrote "Testing connection...", a latency line and a
request failed message straight into the page, and duplicated the provider
form's "Select operation" placeholder as a second literal. `static/js/
gather-builder.js` built every filter, sort, relationship and include row
(operator and join type option lists, field placeholders, aria labels,
the remove buttons, and the preview panel's loading and result messages)
the same way. Both scripts now read every one of those words from a
`<script type="application/json">` data block the admin template renders
with `t()`, the mechanism the AI provider form already used for its
operation list, so a script sets nothing the translation catalogue cannot
reach. `AiProviderService::mask_key_ref` returned an English sentence
("OPENAI_API_KEY (set)", "(no env var configured)") built in the service;
it now returns the env var name and its state as a `KeyRefDisplay`, and the
provider list template turns that into a translated sentence, because
display text does not belong in a service.

The admin's marker mode crawl reads only the HTML a response renders, so a
word a script sets at runtime, inside a `<script>` block the crawl drops,
never reached it. A new test, `admin_js_i18n_test.rs`, reads the two
scripts' own source and fails if either assigns a literal word to a
property that puts it on the page, proven once on a reintroduced literal
before it was removed.
