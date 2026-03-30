# Epic 10 (A): Accessibility Foundation

**Tutorial Parts Affected:** 3 (templates/render), 5 (forms/input), 1 (base install), 6 (community)
**Trovato Phase Dependency:** Phase 5 (Form API, Theming) — already complete
**BMAD Epic:** 40
**Status:** Not started
**Estimated Effort:** 3–4 weeks
**Dependencies:** None (this epic lands first)
**Blocks:** Epic B (i18n), Epic C (Security — form accessibility patterns)

---

## Narrative

*Accessibility is not a feature. It is a quality of the platform. When you add a feature later, it inherits whatever quality the kernel established. This epic establishes that quality.*

Trovato's template layer already has the bones of accessible markup. `base.html` uses `<header>`, `<main>`, `<footer>` semantic landmarks. `page.html` has `<nav>` and `<aside>`. The pager has `aria-label="Pagination"`. Admin tabs use `aria-current="page"`. Form labels use `<label for>`. These exist because they were the obvious right thing to do at the time.

What's missing is the connective tissue that makes these elements work together as an accessible experience: a skip link so keyboard users can bypass navigation, `<article>` wrappers on item content so screen readers announce content boundaries, `aria-describedby` linking form inputs to their validation errors so screen readers read the error when the field is focused, `dir="rtl"` support for right-to-left languages, and `loading="lazy"` as a render pipeline default (performance, but accessibility-adjacent — it prevents layout shift for users with motion sensitivity).

The most important kernel change is schema-level: making `alt` required in the image block JSON schema. Today `alt` is defined as a field but not in the `required` array — a plugin can submit an image block with no `alt` attribute at all. This epic adds `alt` to the required array. The value may be empty string (`alt=""` is correct for decorative images per WCAG 2.1 Success Criterion 1.1.1), but the field must *exist*. Whether the alt text is *good enough* is content policy — a plugin's job, not the kernel's.

Similarly, the SDK's `ElementBuilder` can produce any HTML tag via the `#tag` field, but it has no ARIA-aware helpers. A plugin author writing `ElementBuilder::container().attr("aria-label", "Search results")` is fine, but `ElementBuilder::container().aria_label("Search results")` is better — it signals intent, catches typos at compile time, and makes accessibility discoverable in the API. This epic adds ARIA helpers without changing the underlying mechanism.

**Before this epic:** Templates have semantic structure but gaps. Image blocks can omit alt text. Forms show errors but don't link them to inputs for screen readers. No skip link. Plugin SDK has no accessibility-specific API surface.

**After this epic:** Every page has a skip link. Image blocks always carry alt text (even if empty for decorative images). Form errors are programmatically associated with their inputs. Templates use `<article>` for item content. The Rust-side render fallback produces semantic HTML. Plugin authors get ARIA helpers in `ElementBuilder`. Admin UI has keyboard navigation baseline.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| Skip link in `base.html` | Part of the page shell — plugins can't modify `base.html` |
| `<article>` wrapper on item content | Template change in kernel-owned templates |
| `aria-describedby` on form errors | Form API is kernel infrastructure; plugins consume it |
| `alt` required in image block schema | Content model validation is kernel responsibility |
| `ElementBuilder` ARIA helpers | SDK is kernel — defines the plugin API contract |
| Render fallback semantic HTML | Kernel render pipeline — not pluggable |
| `dir="rtl"` on `<html>` | Base template owned by kernel |
| `loading="lazy"` on `<img>` | Render pipeline default — plugins shouldn't need to add this |
| Admin UI keyboard baseline | Admin UI is kernel-owned |

All changes are kernel infrastructure that plugins depend on or inherit. No item here is a user-facing feature — they are qualities of the platform.

---

## BMAD Stories

### Story 40.1: Skip Link and Page Landmarks

**As a** keyboard or screen reader user,
**I want** a skip link as the first focusable element on every page,
**So that** I can bypass navigation and jump directly to content.

**Acceptance criteria:**

- [ ] `base.html` includes `<a href="#main-content" class="skip-link">Skip to main content</a>` as the first child of `<body>` (inside `{% block body %}`)
- [ ] `<main>` element has `id="main-content"` to serve as the skip link target
- [ ] Skip link is visually hidden by default, visible on focus (CSS: `.skip-link` positioned off-screen, `.skip-link:focus` positioned on-screen)
- [ ] `<article>` wrapper added around item content in `templates/elements/item.html` (and any type-specific item templates)
- [ ] Gather listing items wrapped in `<article>` in default gather row templates
- [ ] All existing `aria-label`, `aria-current` usages verified still correct after template changes
- [ ] No visual regression in default theme (skip link hidden until focused)

**Implementation notes:**
- Modify `templates/base.html` — add skip link before `<header>`, add `id="main-content"` to `<main>`
- Modify `templates/elements/item.html` — wrap content in `<article>`
- Add `.skip-link` CSS to `base.html` `<style>` block
- Check `templates/page.html`, `templates/elements/item--*.html` for consistency

---

### Story 40.2: Image Block Alt Text Schema Enforcement

**As a** content platform,
**I want** the image block schema to require the `alt` field,
**So that** every image block in the system carries alt text metadata (even if empty for decorative images).

**Acceptance criteria:**

- [ ] Image block JSON schema adds `alt` to the `required` array alongside `file`
- [ ] Migration updates existing image blocks that lack `alt` field to add `alt: ""` (empty string — treats them as decorative until editors add real alt text)
- [ ] Block editor UI shows `alt` as a required field with helper text: "Describe the image for screen readers. Leave empty for decorative images."
- [ ] API validation rejects image blocks without `alt` field (not without alt *value* — empty string is valid)
- [ ] Existing `trovato-test` code blocks in tutorials that create image blocks updated to include `alt`
- [ ] `templates/elements/block--image.html` renders `alt="{{ block.alt | default(value='') }}"` (already close to this but verify)

**Implementation notes:**
- Modify `crates/kernel/src/content/block_types.rs` — add `"alt"` to image block's `required` vec
- Write migration to backfill `alt: ""` on existing image blocks in `item.fields` and `item_revision.fields` JSONB
- Verify block editor form in `crates/kernel/src/content/form.rs` renders alt field
- Update `docs/tutorial/part-05-forms-and-input.md` if image block examples omit alt

---

### Story 40.3: Form Accessibility — Error Association

**As a** screen reader user filling out a form,
**I want** validation errors programmatically associated with their input fields,
**So that** when I focus a field with an error, my screen reader reads the error message.

**Acceptance criteria:**

- [ ] Form API generates `id="error-{field_name}"` on error message elements
- [ ] Form API adds `aria-describedby="error-{field_name}"` on the corresponding input element when that field has a validation error
- [ ] `aria-invalid="true"` set on input elements that failed validation
- [ ] Error messages rendered in a `<div role="alert">` or equivalent live region so screen readers announce errors on form submission
- [ ] Form templates (`templates/form/form-element.html`, `templates/form/form.html`) updated
- [ ] Admin forms inherit these changes automatically (they use the Form API)
- [ ] Existing CSRF error display follows the same pattern

**Implementation notes:**
- Modify `crates/kernel/src/form/` render logic to inject `aria-describedby` and `aria-invalid` attributes
- Modify form templates in `templates/form/`
- The form API already tracks per-field errors — this connects them to the DOM

---

### Story 40.4: ElementBuilder ARIA Helpers

**As a** plugin developer building accessible UI components,
**I want** ARIA-specific helper methods on `ElementBuilder`,
**So that** I can add accessibility attributes with compile-time safety and discoverability.

**Acceptance criteria:**

- [ ] `ElementBuilder` gains methods: `.aria_label(s)`, `.aria_describedby(id)`, `.aria_hidden(bool)`, `.aria_current(s)`, `.aria_live(s)`, `.role(s)`, `.aria_expanded(bool)`, `.aria_controls(id)`
- [ ] Each method maps to the corresponding HTML attribute (e.g., `.aria_label("Search")` → `aria-label="Search"`)
- [ ] Methods are additive (can call multiple on the same builder)
- [ ] Existing `.attr("aria-label", "...")` usage continues to work (helpers are sugar, not replacement)
- [ ] Documentation with examples in doc comments
- [ ] At least one existing plugin or kernel usage updated to demonstrate (e.g., pager render element)

**Implementation notes:**
- Modify `crates/plugin-sdk/src/types.rs` — add methods to `ElementBuilder` impl
- These are pure convenience methods — they call `.attr()` internally
- No breaking change to existing plugins

---

### Story 40.5: Render Pipeline Semantic Fallback

**As a** theme developer,
**I want** the Rust-side render fallback to produce semantic HTML,
**So that** pages without custom templates are still accessible.

**Acceptance criteria:**

- [ ] When no template matches a render element, the Rust fallback uses semantic tags: `<article>` for item-type elements, `<section>` for container elements with headings, `<nav>` for navigation elements, `<div>` only for generic containers
- [ ] Heading elements use the correct `<h1>`–`<h6>` tag based on `heading_level` in block data (not generic `<div>`)
- [ ] Image elements in fallback output include `alt` attribute (Note: `loading="lazy"` is handled by Story 44.4 in Epic E)
- [ ] Link elements use `<a>` with proper `href` (not `<div>` with click handler)
- [ ] Fallback output tested with at least 3 render element types (item, heading block, image block)

**Implementation notes:**
- Modify `crates/kernel/src/theme/render.rs` — the `render_element()` fallback path
- Map RenderElement `#type` values to semantic HTML tags
- This is the path used when no Tera template matches the suggestion chain

---

### Story 40.6: RTL Direction Support

**As a** site serving content in right-to-left languages (Arabic, Hebrew, Farsi),
**I want** the `dir` attribute set correctly on `<html>`,
**So that** text direction renders correctly throughout the page.

**Acceptance criteria:**

- [ ] `base.html` sets `dir="{{ text_direction | default(value='ltr') }}"` on the `<html>` element
- [ ] Kernel middleware populates `text_direction` in the template context based on active language
- [ ] A lookup table of RTL language codes exists in the language middleware (ar, he, fa, ur, ps, yi, etc.)
- [ ] Default CSS in `base.html` uses logical properties where physical direction was previously used: `margin-inline-start/end` instead of `margin-left/right`, `padding-inline-start/end` instead of `padding-left/right`, `text-align: start` instead of `text-align: left`
- [ ] Admin UI CSS also uses logical properties (admin templates extend `base.html`)
- [ ] No visual regression for LTR sites (logical properties resolve identically to physical for LTR)

**Implementation notes:**
- Modify `templates/base.html` — add `dir` attribute, convert CSS to logical properties
- Modify language middleware (`crates/kernel/src/middleware/language.rs`) to set `text_direction` in template context
- Add `RTL_LANGUAGES` constant with ISO 639-1 codes
- This story prepares the foundation that Epic B (i18n) builds on

---

### Story 40.7: Focus Indicators and Tab Order Audit

**As a** keyboard-only user administering a Trovato site,
**I want** visible focus indicators and correct tab order on all interactive elements,
**So that** I can see where I am and reach every control via keyboard.

**Acceptance criteria:**

- [ ] CSS `:focus-visible` outline is visible on all focusable elements — not suppressed
- [ ] All interactive admin elements (buttons, links, form controls) are reachable via Tab key — no `<div onclick>` patterns
- [ ] Admin list tables: rows are not interactive — action buttons/links within rows are focusable
- [ ] Delete confirmation actions require explicit activation (not triggered by focus alone)
- [ ] `.visually-hidden` CSS utility class added for screen-reader-only content

**Implementation notes:**
- Add `:focus-visible` styles and `.visually-hidden` class to `base.html` CSS
- Audit admin templates for non-focusable interactive elements

---

### Story 40.8: AJAX Live Region Announcements

**As a** screen reader user performing AJAX operations in the admin UI,
**I want** changes announced audibly when the page updates,
**So that** I know my action succeeded without visually scanning the page.

**Acceptance criteria:**

- [ ] `base.html` includes `<div aria-live="polite" id="trovato-announcements" class="visually-hidden"></div>`
- [ ] AJAX `executeCommand` populates `#trovato-announcements` with a contextual message after DOM changes
- [ ] Announcement cleared after 5 seconds (allows re-announcement on repeated actions)

**Implementation notes:**
- Modify AJAX framework JS to call `Trovato.announce()` after each command type
- Requires `.visually-hidden` class from Story 40.7

---

### Story 40.9: Admin Tab Arrow Key Navigation

**As a** keyboard user navigating the admin UI,
**I want** arrow keys to move between tabs within a tab group,
**So that** I can switch tabs efficiently without tabbing through every tab link.

**Acceptance criteria:**

- [ ] Admin tab container has `role="tablist"`, tab links have `role="tab"` and `aria-selected`
- [ ] Left/Right arrow keys move focus between tabs; Home/End move to first/last
- [ ] Only active tab is in Tab order (`tabindex="0"`); inactive tabs have `tabindex="-1"`
- [ ] Modal-like dialogs (if any) trap focus within the dialog while open

**Implementation notes:**
- Modify `templates/admin/macros/tabs.html`
- Add arrow key JavaScript handler following WAI-ARIA Authoring Practices tabs pattern
- This is baseline — advanced keyboard UX (drag-and-drop reordering, tree navigation) is deferred

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| Add ARIA helper methods to `ElementBuilder` | `crates/plugin-sdk/src/types.rs` | No (additive) | None — existing code unchanged |

**Migration guide:** No action required for existing plugins. New methods are available for plugins that want to use them.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Render-Theme.md` | Add "Accessibility Defaults" section: skip link, `<article>` wrapper, semantic fallback, `dir` attribute, `loading="lazy"`, form error association. Document the render fallback tag mapping. |
| `docs/design/Design-Plugin-SDK.md` | Add `ElementBuilder` ARIA helpers to the API reference section. Add "Accessibility" subsection under plugin development guidelines. |
| `docs/design/Design-Content-Model.md` | Note `alt` required on image blocks. Document the `alt=""` convention for decorative images. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| `part-03-look-and-feel.md` | Template examples, render pipeline explanation | Update code blocks showing `base.html` structure (now includes skip link, `dir` attribute, logical CSS). Update render pipeline section to mention semantic fallback. |
| `part-05-forms-and-input.md` | Form API section, block editor section | Update form examples to show `aria-describedby` on errors. Update image block examples to include required `alt` field. |
| `part-01-hello-trovato.md` | Viewing items section | Note that default item display uses `<article>` wrapper. Minor mention, not structural change. |
| `part-06-community.md` | Admin UI section | Note keyboard navigability as a property of the admin UI. |

**`trovato-test` blocks affected:** Any test blocks that create image blocks without `alt`. Grep for `trovato-test` blocks containing "image" in parts 3 and 5.

---

## Recipe Impact

Recipes for parts 1, 3, 5, 6 need updates matching the tutorial changes above. After updates, run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

| Part | Screenshots | Reason |
|---|---|---|
| Part 3 | Template output screenshots | Skip link visible on focus; `<article>` wrapper changes DOM inspector view |
| Part 5 | Form screenshots, block editor screenshots | Alt field now required and visible; error messages linked to inputs |

---

## Config Fixture Impact

None. Content type definitions, gather queries, and roles are unchanged.

---

## Migration Notes

**Database migrations:**
1. `YYYYMMDD000001_require_image_block_alt.sql` — UPDATE `item.fields` and `item_revision.fields` to add `"alt": ""` to any image blocks missing the `alt` key. This is a JSONB transformation on existing data.

**Breaking changes:** Image blocks submitted without `alt` field will be rejected by validation. All 21 existing plugins that create image blocks must include `alt` in the block data.

**Upgrade path:** The migration backfills `alt: ""` on existing data. Plugins that create image blocks need a one-line addition (`"alt": "..."`) to their block construction code. This is a compile-time-visible change if using typed structs, or a runtime validation failure if building JSON manually.

---

## What's Deferred

- **AI-generated alt text** — Plugin territory (Epic 3/Story 31.5 field rules). The kernel just ensures the field exists.
- **Accessibility auditing/scoring** — Plugin. The kernel provides the structure; a plugin can scan it.
- **Color contrast checking** — Plugin territory.
- **Complex keyboard interactions** (drag-and-drop reordering, tree view keyboard nav) — Future epic. This epic covers baseline Tab/Enter/Space navigation.
- **ARIA landmark validation** — A linter plugin could verify templates use landmarks correctly.
- **Content readability analysis** — Plugin territory.
- **Focus management for client-side routing** — Trovato is server-rendered; not applicable.

---

## Related

- [Design-Render-Theme.md](../design/Design-Render-Theme.md) — Render pipeline and template system
- [Design-Plugin-SDK.md](../design/Design-Plugin-SDK.md) — ElementBuilder API
- [Design-Content-Model.md](../design/Design-Content-Model.md) — Block types and field definitions
- [Epic B (11): i18n Infrastructure](epic-11-i18n.md) — Builds on RTL direction support from this epic
- [Epic C (12): Security Hardening](epic-12-security.md) — Form accessibility patterns referenced in field_access stories
