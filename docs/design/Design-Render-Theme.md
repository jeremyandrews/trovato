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

### Language-Aware Menus

When the active language is not the default, `main_menu` and `footer_menu` are
rewritten before they reach the template. A link whose target has a translation in
that language gets:

- its path prefixed with `/{lang}`, so the click stays in the language; and
- its title replaced by the translated item's title **when the link's title is the
  target's default-language title** — the common case of a menu label mirroring a
  page title. A label somebody wrote by hand is theirs, and is left alone.

A link whose target has no translation is untouched, address and label both. A
translated label on an untranslated page is a promise the click breaks, and a
prefixed address for a page that does not exist in that language is a 404 offered
as navigation.

The rewrite costs two queries for a whole menu, not two per link: one resolves
link paths back to the items behind them, one reads the translated and
default-language titles together. Nothing is cached, which is the same discipline
as the rest of menu building — the links themselves are read per render.

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

