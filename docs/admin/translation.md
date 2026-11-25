# Translating the administration

Every word the administration shows goes through one function, `t`. It is
registered on every theme engine, whether or not the `trovato_locale` plugin
is enabled, so a template can use it unconditionally: with no translation, or
no locale service, it returns the English source with its placeholders
filled. The code is `crates/kernel/src/i18n.rs`.

## In a template

```text
{{ t(s="Save configuration") }}
{{ t(s="Delete the {label} stage?", label=stage.label) }}
{{ t(s="{count} item", plural="{count} items", count=total) }}
{{ t(s="Default", ctx="text format") }}
```

- **Whole sentences.** One string per sentence, with named placeholders.
  Never join translated fragments (`t(s="Edit") ~ " " ~ name`): word order
  differs between languages.
- **Placeholders** are `{name}`, filled from the call's other named
  arguments. Their values are data and are never translated. An unknown
  placeholder is left as written.
- **Plurals** take `plural` and `count`. The source has English's two forms;
  a translation has one form per CLDR plural category of its language (one,
  two, few, many, other; `i18n::plural_category` holds the integer rules for
  the languages Trovato ships and those like them). `count` is also the
  `{count}` placeholder.
- **Context** (`ctx`) tells apart the same English word used for different
  things, as gettext's `msgctxt` does. Use it only where a word really is
  ambiguous.
- **No markup inside a sentence.** A link goes after its sentence, as its own
  whole link text ("… <a>Edit the text formats</a>"); emphasis is dropped or
  becomes its own sentence. A code word is a placeholder value.
- **Data is printed as is**: titles, names, machine names, paths, numbers and
  dates are values, not interface text. A fixed English label that comes from
  Rust (a status word) may be passed through `t(s=row.status)`.
- `t` returns a string and is escaped like any value; never pipe it to
  `safe`.

## Messages built in Rust

A flash message, shown on the page after a redirect, is built in a handler
but must be translated when it is shown, in the language of the page that
shows it. Store an `i18n::UiMessage` in the session instead of a sentence:

```rust
UiMessage::new("{type} {title} has been created.")
    .arg("type", label)
    .arg("title", title)
    .link(format!("/admin/content/{id}/edit"), "Edit {title}");

UiMessage::new("{count} item published.").plural("{count} items published.", n);
```

The admin shell renders it with `{{ t_message(m=flash) }}`, which translates,
escapes, and adds the link only when it is a path on this site. A plain string
in the same session key still works.

Other words a handler writes into HTML (a button in a form built in Rust) use
`routes::admin_nav::AdminText`: `AdminText::for_page(state, viewer, path)`
gives the viewer's admin language on an `/admin` page and the English source
anywhere else, `text.html("Preview", &[])` the escaped, translated string, and
`text.scope(|| render)` runs a form render so the form templates' own `t`
calls translate too.

## The admin language

The viewer chooses it under Your preferences (`/admin/preferences`); without
a choice it is the site's default language. It sets `lang` and `dir` on
`<html>`: `dir="rtl"` for a right-to-left language, so the layout mirrors
(the stylesheet uses logical properties only; a test enforces it).

The language is held for the length of a render by `i18n::scope`, because a
Tera function cannot see the template's context. Outside a scope `t` uses
English, which is what keeps front end output unchanged for the form
templates the front end shares with the admin.

Translations come from the locale service (`trovato_locale`): import a `.po`
file as for any interface string. Plural forms are stored under the singular
and plural joined by the ASCII unit separator, U+001F
(`i18n::plural_source_key`), and its translation is the language's forms joined
the same way, one per CLDR category in the order zero, one, two, few, many,
other. Not the NUL a compiled gettext catalogue uses: PostgreSQL text cannot
hold one.

## Pseudo locales

With `TROVATO_PSEUDO_LOCALES=1` two extra languages appear under Your
preferences. They need no translation files:

- **en-XA**, accented: every letter accented and each string about 30%
  longer, in brackets. Clipped text, a string that never went through `t`
  (it stays plain), and a layout that cannot take longer words show at once.
- **ar-XB**, right to left: the same text, wrapped in right-to-left marks,
  with `dir="rtl"`. The whole layout must mirror.

The browser checks (`tests/admin-a11y`) run every admin page in ar-XB.

## Marker mode: the untranslated-string test

With `TROVATO_I18N_MARKERS=1` (tests only) every string `t` returns is wrapped
in one pair of private use characters, and every value a template prints in
another. Letters left outside both on a rendered page reached it without
passing through `t`.

`crates/kernel/tests/admin_i18n_test.rs` turns marker mode on, starts at the
Dashboard and the seven landing pages, follows every admin link (one page per
route in `docs/admin/inventory.md`), and fails naming the page and the text
for any text node, `aria-label`, `title`, `placeholder`, `alt` or button value
that is untranslated. It reached 74 page types on the test fixtures.

`tests/admin-a11y/scripts/check-template-strings.mjs` is the same check done
statically on template source, quicker while editing:

```sh
node tests/admin-a11y/scripts/check-template-strings.mjs templates/admin/*.html
```

## What marker mode cannot see

A word built in Rust and printed as a value (a form field's title from the
form builder, an English status word) is data to marker mode, so the test
cannot catch it. The form builder's labels are the known case; Part 2
(forms) moves them onto `t`.
