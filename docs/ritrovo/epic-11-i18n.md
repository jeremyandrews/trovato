# Epic 11 (B): i18n Infrastructure

**Tutorial Parts Affected:** 7 (going global), 3 (templates/render)
**Trovato Phase Dependency:** Phase 5 (Theming) — already complete
**BMAD Epic:** 41
**Status:** ~95% complete. Implemented: full language negotiation middleware (URL prefix, Accept-Language, session, default), RTL_LANGUAGES constant (15 codes), text_direction_for_language(), CLDR-based format_date with localized month names for 14 locales (en, de, fr, es, it, pt, nl, pl, ru, ar, he, ja, zh, ko), lang/dir attributes in base.html, hreflang links. Remaining: translation memory system.
**Estimated Effort:** 2–3 weeks
**Dependencies:** Epic A (10) — RTL `dir` attribute and logical CSS properties established there
**Blocks:** None

---

## Narrative

*Internationalization is like plumbing: invisible when done right, catastrophic when missing. You don't notice it until someone needs Arabic, or Japanese dates, or a URL that starts with `/fr/`. This epic makes the plumbing solid.*

Trovato already has substantial i18n infrastructure. The `language` table exists with a foreign key from `item.language`. The `LocaleService` handles contextual translations via PO import with a `trans` Tera filter. Language negotiation middleware is 703 lines of serious work: `UrlPrefixNegotiator` and `AcceptLanguageNegotiator` with a resolution chain and URI rewriting. `base.html` sets `<html lang="{{ active_language | default(value='en') }}">`. The `trovato_content_translation` and `trovato_config_translation` plugins exist and work.

What's incomplete is the connective tissue between these pieces:

1. **SDK blindness to language.** `Item.language` is explicitly excluded from serde serialization in `types.rs` — plugins literally cannot see what language an item is in. This was likely an oversight during early development when language wasn't wired up yet.

2. **Hardcoded English dates.** `format_date` renders `"%B %-d, %Y"` (March 30, 2026) regardless of locale. A German site should show "30. März 2026". A Japanese site should show "2026年3月30日".

3. **Incomplete RTL support.** Epic A establishes `dir="rtl"` on `<html>` and converts CSS to logical properties. This epic verifies that all render contexts (API responses, error pages, emails) consistently propagate `active_language` and `text_direction`.

4. **Language negotiation coverage gaps.** The middleware is thorough for page requests but needs verification on API routes, admin routes, and error pages.

**Before this epic:** Language exists in the schema but is invisible to plugins. Dates are always English. Language negotiation works for pages but may have gaps elsewhere.

**After this epic:** Plugins can read `Item.language`. Dates format per locale. Language negotiation covers all entry points. RTL direction propagates consistently. The i18n infrastructure is complete enough that the `trovato_locale`, `trovato_content_translation`, and `trovato_config_translation` plugins have a solid kernel to build on.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| Un-ignore `Item.language` in SDK serde | SDK defines the plugin data contract — this is the plugin API |
| Locale-aware `format_date` filter | Tera filters are kernel infrastructure; plugins can't add Tera filters |
| Verify `active_language` in all render contexts | Template context population is kernel middleware |
| Language negotiation coverage | Middleware is kernel — runs before any plugin or route code |

All changes are kernel infrastructure. The actual translation UI, PO management, and content translation workflows remain plugin territory.

---

## BMAD Stories

### Story 41.1: Un-ignore Item.language in Plugin SDK

**As a** plugin developer,
**I want** to read the `language` field on `Item` objects,
**So that** my plugin can behave differently based on content language (e.g., translation workflows, language-specific formatting).

**Acceptance criteria:**

- [ ] `Item` struct in `crates/plugin-sdk/src/types.rs` includes `language: Option<String>` in serde serialization (remove the `#[serde(skip)]` or equivalent exclusion)
- [ ] Items serialized to plugins via host functions include the `language` field
- [ ] Items deserialized from plugin responses accept the `language` field
- [ ] `language` field is `Option<String>` — `None` for items created before language support, `Some("en")` for items with language set
- [ ] Existing plugins that deserialize `Item` continue to work (serde `Option` defaults to `None` for missing fields — backward compatible)
- [ ] At least one integration test verifies language round-trips through the WASM boundary

**Implementation notes:**
- Modify `crates/plugin-sdk/src/types.rs` — remove serde skip annotation on `language`
- This is a backward-compatible addition — existing plugin WASM binaries continue to work because `Option<String>` defaults to `None` when the field is absent in older serialized data
- Verify the comment explaining *why* it was excluded; if the reason no longer applies, remove the comment too

---

### Story 41.2: Locale-Aware Date Formatting

**As a** site visitor reading content in my language,
**I want** dates formatted according to my locale,
**So that** "March 30, 2026" appears as "30. März 2026" in German or "2026年3月30日" in Japanese.

**Acceptance criteria:**

- [ ] `format_date` Tera filter accepts an optional `trovato_locale` parameter: `{{ date | format_date(locale="de") }}`
- [ ] When `trovato_locale` is omitted, defaults to `active_language` from template context (not hardcoded "en")
- [ ] Supports at least: en, de, fr, es, ja, zh, ar, he, pt, it, nl, ko, ru, pl (covering major languages)
- [ ] Date format patterns per locale stored in a compile-time lookup (not a runtime config table — these are stable conventions)
- [ ] `format_date` also accepts an optional `format` parameter for custom patterns: `{{ date | format_date(format="%Y-%m-%d") }}`
- [ ] When both `trovato_locale` and `format` are provided, `format` takes precedence (explicit format overrides locale default)
- [ ] Existing tutorial code using `format_date` without parameters continues to work (defaults to active language)
- [ ] At least 3 locale formats tested (en, de, ja)

**Implementation notes:**
- Modify `crates/kernel/src/theme/engine.rs` — the `format_date` filter registration
- Consider using the `chrono` locale formatting or a simple pattern lookup table
- Keep it simple — a `HashMap<&str, &str>` mapping language codes to strftime patterns is sufficient. Full ICU/CLDR is plugin territory.
- Pattern examples: `en` → `"%B %-d, %Y"`, `de` → `"%-d. %B %Y"`, `ja` → `"%Y年%-m月%-d日"`

---

### Story 41.3: Language Context Propagation Audit

**As a** kernel maintainer,
**I want** `active_language` and `text_direction` available in all template render contexts,
**So that** every page, error response, and admin screen knows the current language.

**Acceptance criteria:**

- [ ] `active_language` is set in template context for: page renders, gather renders, admin pages, error pages (400, 403, 404, 500), installer pages
- [ ] `text_direction` (from Epic A) is set in the same contexts as `active_language`
- [ ] API JSON responses include `Content-Language` header matching the negotiated language
- [ ] Language negotiation middleware runs on all route groups: public pages, admin routes, API routes, file routes
- [ ] Negotiation chain order verified: URL prefix → cookie → Accept-Language header → site default
- [ ] If URL prefix negotiation activates a language, the rewritten URL is used for routing (already implemented — verify no regression)
- [ ] Error pages rendered by the kernel (not plugin) use the negotiated language for their UI strings (button text, "Page not found" message)

**Implementation notes:**
- Audit `crates/kernel/src/routes/` — verify every route handler that renders templates includes language context
- Audit `crates/kernel/src/middleware/language.rs` — verify it's applied to all router layers
- Check error handlers in `crates/kernel/src/routes/helpers.rs` (render_error, render_server_error, render_not_found) — they must pass `active_language` to templates
- This is primarily verification and gap-filling, not new development

---

### Story 41.4: Language Negotiation Configuration

**As a** site operator running a multilingual site,
**I want** the language negotiation chain to be configurable,
**So that** I can control which negotiation methods are active and in what order.

**Acceptance criteria:**

- [ ] Language negotiation methods are configurable via site config or environment variables
- [ ] Configurable options: `LANGUAGE_NEGOTIATION_METHODS` (comma-separated, ordered: e.g., `url_prefix,cookie,accept_header`)
- [ ] Each method can be enabled/disabled independently
- [ ] Default configuration: `url_prefix,accept_header` (cookie negotiation opt-in)
- [ ] URL prefix negotiator's prefix list derived from enabled languages in the `language` table
- [ ] Configuration changes take effect on next request (no server restart required — language config is not startup-only)
- [ ] Single-language sites (only one language in `language` table) skip negotiation entirely — no URL prefix rewriting, no Accept-Language parsing

**Implementation notes:**
- Modify `crates/kernel/src/config.rs` — add `LANGUAGE_NEGOTIATION_METHODS` env var
- Modify `crates/kernel/src/middleware/language.rs` — make negotiator chain configurable
- The middleware already has `UrlPrefixNegotiator` and `AcceptLanguageNegotiator` — this wires them to configuration instead of hardcoding the chain
- Single-language optimization is important: most Trovato sites are monolingual; they shouldn't pay the cost of language negotiation

---

### Story 41.5: Tutorial Part 7 Verification

**As a** tutorial reader working through Part 7 (Going Global),
**I want** the tutorial content to reflect the current i18n infrastructure accurately,
**So that** the tutorial teaches real, working features without misleading about what's implemented vs. stubbed.

**Acceptance criteria:**

- [ ] Part 7 tutorial verified against actual kernel behavior: every code example runs, every screenshot matches current UI
- [ ] Tutorial notes where the `trovato_locale` plugin is a stub (permissions + menu only, no translation UI) — sets expectations correctly
- [ ] Tutorial demonstrates `format_date` with locale parameter (new capability from Story 41.2)
- [ ] Tutorial demonstrates `Item.language` visibility to plugins (new capability from Story 41.1)
- [ ] Recipe `recipe-part-07.md` updated to match any tutorial changes
- [ ] Sync hash updated in `docs/tutorial/recipes/sync-check.sh`
- [ ] `trovato-test` blocks in Part 7 pass against updated kernel

**Implementation notes:**
- Read `docs/tutorial/part-07-going-global.md` end-to-end
- Verify each code block and CLI command against the running system
- Update recipe to match
- This is the tutorial-ships-with-epic requirement — not deferred to Epic I

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| Un-ignore `Item.language` serde | `crates/plugin-sdk/src/types.rs` | No (additive `Option<String>`) | All plugins that handle Items gain a new field; existing code ignores it via `Option::None` default |

**Migration guide:** No action required. Plugins that want to read `Item.language` can now access `item.language` as `Option<String>`. Plugins that don't care about language continue to work unchanged — `None` is the default for items without language set.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Web-Layer.md` | Update language negotiation section to reflect configurable chain, single-language optimization |
| `docs/design/Design-Render-Theme.md` | Update `format_date` filter documentation with locale parameter. Add note about `active_language` and `text_direction` template context variables. |
| `docs/design/Design-Plugin-SDK.md` | Update `Item` type documentation to include `language` field |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| `part-07-going-global.md` | i18n sections, format_date examples | Update format_date usage to show locale parameter. Verify all code blocks. Note locale plugin stub status. |
| `part-03-look-and-feel.md` | Template variables section | Minor: note `active_language` and `text_direction` as available template context variables |

---

## Recipe Impact

Recipe for Part 7 needs updates matching tutorial changes. Run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

| Part | Screenshots | Reason |
|---|---|---|
| Part 7 | Date formatting screenshots | Dates now format per locale instead of always English |

---

## Config Fixture Impact

`docs/tutorial/config/locales/` YAML files may need updates if language configuration format changes (Story 41.4).

---

## Migration Notes

**Database migrations:** None. The `language` table and `item.language` column already exist.

**Breaking changes:** None. All changes are additive.

**Upgrade path:** No action required. Existing sites continue to work. Monolingual sites see no behavioral change (language negotiation skipped when only one language exists).

---

## What's Deferred

- **Locale plugin implementation** (translation UI, string management, PO import UI) — plugin territory, existing epic 7
- **Content Translation plugin enhancements** — plugin territory
- **ICU/CLDR full formatting** (number formatting, currency, plurals) — future plugin. This epic provides date formatting only; full ICU is a heavy dependency.
- **RTL-specific template overrides** — theme territory. The kernel provides `dir="rtl"`; themes adapt their layout.
- **Locale-aware sorting** (e.g., German ß sorting, Chinese stroke-order) — database collation, deferred to when a real need arises
- **URL transliteration** (e.g., `/конференции/` → transliterated URL aliases) — plugin territory

---

## Related

- [Epic A (10): Accessibility Foundation](epic-10-accessibility.md) — Establishes `dir="rtl"` and logical CSS properties that this epic builds on
- [Design-Web-Layer.md](../design/Design-Web-Layer.md) — Language negotiation middleware
- [Design-Render-Theme.md](../design/Design-Render-Theme.md) — Template system and Tera filters
- [part-07-going-global.md](../tutorial/part-07-going-global.md) — i18n tutorial
