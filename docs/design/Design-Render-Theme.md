# Trovato Design: Render Tree & Forms

*Sections 5, 10 of the v2.1 Design Document*

---

## 5. The Render Tree (Theming Layer)

### Why No Opaque Strings

Drupal 6 had a long history of XSS vulnerabilities because plugins could return arbitrary HTML. Storing `field_body` with a `"format"` key but never filtering it means every item body is a stored XSS vector.

The solution: plugins **must** return Render Elements (JSON), not HTML strings. The Kernel assembles these into a tree, runs `alter` taps, sanitizes based on text format, and then renders to HTML.

### The Render Element

```json
{
    "#type": "container",
    "#weight": 0,
    "#attributes": {"class": ["item", "article"]},
    "title": {
        "#type": "markup",
        "#tag": "h2",
        "#value": "My Article"
    },
    "body": {
        "#type": "markup",
        "#value": "<p>User content...</p>",
        "#format": "filtered_html"
    }
}
```

### The Render Pipeline

1. **Build:** `tap_item_view` returns the JSON Render Element structure.
2. **Alter:** `tap_item_view_alter` allows other plugins to modify the JSON (e.g., add a class, remove a field, inject new elements). Plugins return additions/modifications, not a replacement of the entire tree — this prevents clobber bugs where one plugin overwrites another's changes.
3. **Sanitize:** The Kernel checks `#format`. If present (e.g., `filtered_html`), it runs the text through the security pipeline. The `TextFormat` system supports configurable filter chains: strip `<script>` tags, whitelist allowed HTML elements, convert URLs to links, etc. Text without a `#format` key is treated as plain text and HTML-escaped.
4. **Render:** The Kernel maps `#type` to a Tera template (e.g., `container` → `container.html`, `markup` → inline rendering) and produces final HTML.

### Template Resolution

We use Tera (a Jinja2-style Rust template engine) with a layered resolution strategy:

```rust
pub struct ThemeEngine {
    tera: tera::Tera,
    suggestion_cache: DashMap<String, String>,
}

impl ThemeEngine {
    pub fn resolve_template(
        &self, suggestions: &[String],
    ) -> String {
        for suggestion in suggestions {
            if self.tera.get_template(suggestion).is_ok() {
                return suggestion.clone();
            }
        }
        suggestions.last().unwrap().clone()
    }

    pub fn render_item(
        &self, item: &Item,
        context: &mut tera::Context,
    ) -> Result<String, ThemeError> {
        let suggestions = vec![
            format!("item--{}--{}.html",
                    item.r#type, item.id),
            format!("item--{}.html", item.r#type),
            "item.html".to_string(),
        ];
        let template =
            self.resolve_template(&suggestions);
        self.tera.render(&template, context)
            .map_err(ThemeError::from)
    }
}
```

### Preprocess Taps

Plugins can implement `tap_preprocess_item`. This tap receives the context variables (title, content, etc.) and returns a JSON object of additional template variables. It cannot mutate the HTML directly, only the variables passed to the template.

### Render Elements for Small Fragments

For small reusable output fragments (pagers, tables, status messages), we use Tera macros and a render element system. Each type has a default Tera macro. Themes override by providing their own macro with the same name.

### Template Directory Structure

```
templates/
  base.html
  page.html
  item.html
  item--article.html
  views/
    gather-view.html
    gather-view--frontpage.html
    gather-row.html
  macros/
    elements.html
  themes/
    mytheme/
      page.html
      macros/
        elements.html
```

### Language in the Render Context

The kernel resolves a language per request and hands the render context enough to
keep a reader inside it. Six variables, and one rule about who owns two of them.

| Variable | What it holds |
|---|---|
| `active_language` | The language this response was negotiated in. |
| `text_direction` | `ltr` or `rtl`, for the same language. |
| `default_language` | The site's default language code. |
| `available_languages` | Every language the site is configured for. |
| `available_translations` | On an item page or the front page: `{language, path}` for every language this page exists in, default included. What a language switcher is built from. |
| `hreflang_links` | The same facts as `<link rel="alternate">` data. Absent when the page exists in only one language. |

`active_language` and `text_direction` belong to the **route**, which is the only
thing that knows the language the response was actually negotiated in.
`inject_site_context` supplies the site default only when the context does not
already carry one. Inserting them unconditionally used to mean the order of two
lines decided whether a translated page announced itself correctly.

**A route sets them before calling `inject_site_context`.** The fallback rule
makes the *value* order-independent, which is not the same as making the *call*
order-independent: the helper reads the language back out of the context and
builds the localized menus from it. A language inserted afterwards still reaches
`<html lang>` and reaches nothing derived from it, which is a page announcing one
language over navigation built for another.

`available_translations` gives the default language's canonical alias verbatim and
every other language that same address behind a `/{lang}` prefix, which is how the
prefix negotiator reads it back. Only languages the page actually exists in are
listed: an `hreflang` naming an address that 404s is worse than no tag at all,
because a crawler acts on it.

On the front page the addresses are the front page's own, `/` and `/{lang}/`, not
the configured item's alias. The reader asked for the front page; a switcher that
lands them on `/it/whatever-the-item-is-called` has moved them somewhere else.

### Paths in the Render Context

Two paths, and they are not the same path.

- `current_path` is what the request was rewritten *to*. On an item reached
  through an alias it is `/item/{uuid}`. Routes need it; it is the address the
  handler is serving.
- `requested_path` is the address as asked for, before a language prefix was
  stripped and before an alias was resolved. `/it/why` stays `/it/why`.

Match a menu link against `requested_path`. A menu links to `/why`, and by the
time the page renders `current_path` is `/item/{uuid}`, so the two can never be
equal and the active trail is dead on every aliased page. `requested_path` falls
back to `current_path` where a route does not know better, so a template can read
it unconditionally.

### The Viewer in the Render Context

Every page template and every item template gets `viewer`, built by one function
(`theme::Viewer::of`) from the request's `UserContext`:

| Key | What it holds |
|---|---|
| `viewer.id` | The user's id as a string; `null` for an anonymous visitor. |
| `viewer.authenticated` | Whether the viewer is signed in. |
| `viewer.roles` | The names of the viewer's roles, sorted: `anonymous user` for a visitor; every assigned role plus `authenticated user` for a member. |

```jinja
{% if viewer.authenticated %}
  <a href="/user/{{ viewer.id }}/subscriptions">My subscriptions</a>
{% endif %}
```

It is the only viewer data a template gets through `viewer`, on purpose. No email,
no display name, no permission list, no session or token: what a template never
receives it cannot print, and a theme is not where access is decided. A per-viewer
control drawn from `viewer.roles` is a convenience; the page it leads to still
checks for itself. The role names come from the permission service's cache,
loaded with the permissions, so `viewer` costs no query on a warm cache. The item
route builds it from the viewer it already loaded, so one request loads its
viewer once.

`user_authenticated`, `user_can_access_admin` and `csrf_token` are still there,
unchanged, beside one more page template key:

| Key | What it holds |
|---|---|
| `user_authenticated` | Whether the viewer is signed in. |
| `user_name` | The signed-in viewer's account name (`users.name`, the name `/user/profile` shows). Set only when the viewer is signed in; an anonymous visitor's context has no `user_name` key at all, so test it with `is defined`. |
| `user_can_access_admin` | Whether the viewer may open `/admin`. |
| `csrf_token` | The session's form token, for the logout form; signed-in only. |

```jinja
{% if user_name is defined %}Logged in as {{ user_name }}{% endif %}
```

`user_name` is a key of its own and stays one: it is not a field of `viewer`. It
comes from the user record the viewer was built from, so it costs no query, and it
is the account name and nothing else; no other account field reaches a template
this way. It is user input, so print it with autoescape on, never with `| safe`.
It is set wherever `user_authenticated` is, which is every page template rendered
through `inject_site_context`; an item template gets neither.

### The Author on the Item Template, and on a Gather Row

An item template gets `author`, resolved through `UserService::author_views` —
the same cached lookup `/api/item`'s `include=author` uses — so a theme prints
who wrote the page without hardcoding a name or printing `item.author_id`:

| Key | What it holds |
|---|---|
| `author.name` | The author's display name (`users.name`). A deleted account's content is reattributed to the kernel's anonymous sentinel account, which has its own row and its own name; an id matching no user row at all gets the kernel's anonymous display name. Never empty. |
| `author.url` | `/user/{id}`, present only when this viewer holds `access user profiles`. Absent — not an empty string — otherwise, and always absent for the anonymous sentinel: there is no profile to send a visitor to. |

```jinja
{% if author %}
  <span class="item__author">by
    {% if author.url %}<a href="{{ author.url }}">{{ author.name }}</a>
    {% else %}{{ author.name }}{% endif %}
  </span>
{% endif %}
```

A gather row carries the same shape under `row.author`, resolved for the whole
page in one query (never one per row), the way `resolve_row_urls` already
resolves `row.url` — see `gather/row.html`. Only the display name and this
access-checked URL ever reach either template: no mail address, no other
`User` field, and no distinction between a login name and a display name (the
kernel has one name, not two).

### Plugin Output on the Item Template

An item template gets two strings of rendered HTML:

- `children`: the generic field dump, followed by every plugin's `tap_item_view`
  output. Unchanged; a template that prints only `children` prints what it always
  did.
- `plugin_output`: the plugin output alone, the same string `children` ends with.

A theme that lays out its own fields prints `plugin_output` and not `children`,
and keeps every plugin's contribution (a series navigation, a CFP badge, JSON-LD)
without printing each field twice. Both are kernel-rendered and already
sanitized, so both are printed with `| safe` and a `{# SAFE: ... #}` comment.

A text field's format, stored beside it as `<name>_format`, is part of that field
and is not printed as a field of its own when the type declares `<name>` as text.

### The Comment Thread in the Render Context

`routes::comment::render_thread` builds `elements/comments.html`'s context; the
reply, edit and delete pages (`comment-page.html`) build the same shape for one
comment through `comment_for_page`. Each comment in `comments` (and the single
`comment` the other pages carry) is:

| Key | What it holds |
|---|---|
| `comment.id` | The comment's id. |
| `comment.depth` | Thread depth, for indentation; absent on the single-comment pages. |
| `comment.created` | Unix timestamp. |
| `comment.author_name` | The author's account name, or absent for one the kernel could not look up. |
| `comment.subject` | The comment's optional plain text subject, or absent when it has none. It is user input and is never passed through a text format, so it is printed with autoescape on (`{{ comment.subject }}`), never `| safe` — the one field here that is the exception `body_html` already is for the opposite reason. |
| `comment.body_html` | The body, already rendered and sanitized by its text format. Printed with `| safe`. |
| `comment.can_edit` / `comment.can_delete` | Whether an Edit/Delete link should be shown; absent on the single-comment pages. |

```jinja
{% if comment.subject %}<h3 class="comment__subject">{{ comment.subject }}</h3>{% endif %}
```

### Menus in the Render Context

Every config menu reaches templates, by name, as a tree:

| Variable | What it holds |
|---|---|
| `menus_by_name` | Menu name to the menu's links as a tree. Each link is `{title, path, active, children}`, weight ordered, filtered for the viewer, localized. Every menu the stage has is present, empty when the viewer may follow none of its links. |
| `menus_by_parent` | Parent path to the plugin entries registered under it that this viewer may see, as trees of the same shape. `menus_by_parent["/user"]` is the account menu. |
| `menus_tree` | The plugin root entries as a tree of the same shape. |
| `main_menu`, `footer_menu`, `main_menu_owned`, `menus` | Unchanged: the `main` and `footer` menus flat, whether a `main` menu exists, and the plugin root entries flat. |

**One active rule.** `active` is computed in Rust (`menu::navigation::is_active`):
the requested path is the link path, or starts with the link path followed by
`/`; `/` matches only itself. A template prints it and never recomputes it.

**Plugin entries.** One rule decides whether a plugin entry is a link for a
viewer (`menu::nav_entry_for`): it is visible, the viewer holds its permission
(or it has none, or the viewer is an administrator), and its path can be
followed. A `{uid}` segment becomes the signed-in viewer's id; `{uid}` for an
anonymous viewer, or any other `{...}` segment, leaves the entry out. Root
entries and children go through the same rule.

**A menu tile** (`tile_type: menu`) renders `menus_by_name[menu_name]` through
`elements/tile--menu.html`, overridable per tile as
`elements/tile--menu--{machine_name}.html`. Tile rendering takes one
`TileRenderContext` (the viewer, the language, the role names tile visibility
matches, and the menus already built), so a tile never reloads what the page
has.

### Gather Tiles

**A gather tile** (`tile_type: gather_query`) lists the first rows of the
gather its config names, rendered on the server through
`elements/tile--gather_query.html` and overridable per tile as
`elements/tile--gather_query--{machine_name}.html`. Its config is
`query_id` and `items`, the number of rows, from 1 to 50 and 5 when absent.
The tile form and `config import` both refuse any other count.

The gather runs once per tile, through `routes::gather::execute_query_only`,
the same path its JSON response and feed take: authorized for the viewer,
filtered by item access, on the viewer's stages, in the page's language, with
`current_user` meaning the viewer. Only the page size changes: the tile asks
for page 1 with `items` rows. It is not the full gather page. A tile has no
pager, no exposed filters and no URL arguments; a gather that needs them is
better linked than placed.

The template gets `tile`, `rows` (each item row has `title` and `url`, its
alias when it has one), `empty_text` and `more_url`, the gather's own page
(its canonical URL, else its first route without arguments, else
`/gather/{query_id}`).

A tile with nothing it may show is left off the page, not drawn empty: when
the viewer may not run the gather, silently; when the gather does not exist,
the config is invalid or the query fails, with a warning naming the tile.

### Language-Aware Menus

When the active language is not the default, every config menu is rewritten
before it reaches the template. A link whose target has a translation in that
language gets:

- its path prefixed with `/{lang}`, so the click stays in the language; and
- its title replaced by the translated item's title **when the link's title is the
  target's default-language title** — the common case of a menu label mirroring a
  page title. A label somebody wrote by hand is theirs, and is left alone.

A link whose target has no translation is untouched, address and label both. A
translated label on an untranslated page is a promise the click breaks, and a
prefixed address for a page that does not exist in that language is a 404 offered
as navigation.

### The Menu Cache (B62)

Menus render on every page, so they are cached. On a miss, one query reads every
`menu_link` row of the stage, one resolves every link path's alias source, and
for a non-default language one more reads the translated and default titles. The
result is held in a `moka` cache with a five-minute TTL, keyed by stage and
language. A warm render runs no `menu_link`, `url_alias` or `item_translation`
query; the per-viewer filter and the active flag are computed in memory.

Every in-process write that changes the answer clears it: menu link create,
update and delete (the model functions, so the admin screens and anything else
that calls them), URL alias create, update and delete, config storage saves and
deletes of menu links, aliases, items and item translations, item translation
save and delete, item update and delete (a label mirrors its target's title),
and stage publish. `trovato config import` runs as its own process, so a running
server sees an import when the TTL runs out.

### What This Doesn't Cover

Deliberately simpler than Drupal 6's theme layer. Omitted: render arrays with `#weight` ordering (elements are sorted by `#weight` in the Render Tree, but the ordering algorithm is not specified), theme registry caching (Tera compiles at load time), theme inheritance chains (use Tera's `extends` directly), CSS/JS aggregation.

---

## 10. The Form API

In Drupal 6, you never write raw HTML forms. You define a form as a nested array, and the Form API handles rendering, validation, submission, CSRF protection, and multi-step state.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Form {
    pub form_id: String,
    pub action: String,
    pub method: String,
    pub elements: BTreeMap<String, FormElement>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormElement {
    pub element_type: ElementType,
    pub title: Option<String>,
    pub description: Option<String>,
    pub default_value: Option<serde_json::Value>,
    pub required: bool,
    pub weight: i32,
    pub children: BTreeMap<String, FormElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ElementType {
    Textfield { max_length: Option<usize> },
    Textarea { rows: u32 },
    Select { options: Vec<(String, String)>, multiple: bool },
    Checkbox,
    Checkboxes { options: Vec<(String, String)> },
    Radio { options: Vec<(String, String)> },
    Hidden, Password, File,
    Submit { value: String },
    Fieldset { collapsible: bool, collapsed: bool },
    Markup { value: String },
}
```

### Form State Cache

To support multi-step forms and AJAX (e.g., "Add another item") without keeping state in the stateless WASM plugin, we store form state in Postgres.

```sql
CREATE TABLE form_state_cache (
    form_build_id VARCHAR(64) PRIMARY KEY,
    state JSONB NOT NULL,
    updated BIGINT NOT NULL
);
```

### Form Processing Pipeline

> **Note:** The form processing pipeline passes forms as full JSON to plugins (`tap_form_alter`, `tap_form_validate`, `tap_form_submit`). This is an intentional use of **Mode 2 (Full Serialization)** — form structures are complex nested objects that plugins routinely need to restructure (add fields, reorder elements, inject validation). Handle-based access is not practical for form alter taps. This is acceptable because form builds are infrequent compared to item views.

```rust
pub async fn process_form_submission(
    state: &mut AppState, form_id: &str,
    submitted_values: &HashMap<String, String>,
    session: &Session,
) -> Result<FormResult, FormError> {
    // 1. CSRF check
    let token = submitted_values.get("form_token")
        .ok_or(FormError::MissingToken)?;
    if !verify_csrf_token(session, token) {
        return Err(FormError::InvalidToken);
    }

    // 2. Rebuild the form definition
    let form = build_form(state, form_id).await?;

    // 3. tap_form_alter
    let form_json = serde_json::to_string(&form)?;
    let mut altered_form = form;
    for result in state.plugin_registry
        .invoke_all("tap_form_alter", &form_json)
    {
        if let Ok(modified) = result {
            altered_form = serde_json::from_str(&modified)?;
        }
    }

    // 4. Validate
    let mut errors: Vec<FormError> = Vec::new();
    for (name, element) in &altered_form.elements {
        if element.required {
            let value = submitted_values.get(name)
                .map(|s| s.as_str()).unwrap_or("");
            if value.is_empty() {
                errors.push(FormError::FieldRequired(name.clone()));
            }
        }
    }

    if errors.is_empty() {
        let payload = serde_json::json!({
            "form_id": form_id,
            "values": submitted_values
        });
        for result in state.plugin_registry.invoke_all(
            "tap_form_validate",
            &serde_json::to_string(&payload)?,
        ) {
            if let Ok(r) = result {
                let parsed: serde_json::Value = serde_json::from_str(&r)?;
                if let Some(errs) = parsed.get("errors").and_then(|e| e.as_array()) {
                    for err in errs {
                        if let Some(msg) = err.as_str() {
                            errors.push(FormError::ValidationError(msg.to_string()));
                        }
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(FormError::Multiple(errors));
    }

    // 5. Submit
    let payload = serde_json::json!({
        "form_id": form_id,
        "values": submitted_values
    });
    state.plugin_registry.invoke_all(
        "tap_form_submit",
        &serde_json::to_string(&payload)?,
    );

    Ok(FormResult::Redirect("/".to_string()))
}
```

### The AJAX Flow

1. User clicks "Add Item".
2. JS sends POST to `/system/ajax`.
3. Kernel loads form state from DB using `form_build_id`.
4. Kernel invokes `tap_form_alter` (WASM) to modify the form structure.
5. Kernel renders only the changed element via the Render Tree.
6. Returns HTML fragment to client.

---

