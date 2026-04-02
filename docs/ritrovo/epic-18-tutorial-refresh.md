# Epic 18 (I): Tutorial & Recipe Refresh

**Tutorial Parts Affected:** All 7 parts
**Trovato Phase Dependency:** All epics A–H landed
**BMAD Epic:** 48
**Status:** Partially complete. All 8 tutorial parts exist with recipes and screenshots. Inclusivity-First narrative integrated into Parts 1 and 8. Sync-check script maintained. Pending: full narrative pass incorporating all Epics A-H feature changes, screenshot refresh for any UI changes from beta prep.
**Estimated Effort:** 2–3 weeks
**Dependencies:** Epics A–H (each ships its own tutorial updates; this epic covers net-new content and systemic issues)
**Blocks:** Epic J (19)

---

## Narrative

*Each epic A–H shipped its own tutorial updates — that's a non-negotiable requirement of the "no deferred debt" principle. But individual epic updates are surgical: fix the broken code block, add the missing field, note the new capability. They don't step back and ask "does the tutorial still flow well as a whole?"*

*This epic does.*

After eight epics modify templates, schemas, forms, APIs, and infrastructure, the tutorial needs a systemic pass. Not to redo the epic-specific work (that's done), but to:

1. **Weave inclusivity-first concepts naturally into the narrative.** The research said: don't add "Chapter 8: Accessibility" — instead, when you first show a template in Part 3, naturally demonstrate that it uses semantic HTML and why. When you first show a form in Part 5, naturally demonstrate that errors are linked to inputs. The concepts should be *ambient*, not *separate*.

2. **Verify the tutorial still reads as a coherent progression.** Epic updates may have introduced repetition, inconsistency, or pacing issues. A reader going through Parts 1–7 sequentially should have a smooth experience.

3. **Refresh all recipes against the updated kernel.** The sync-check script verifies hash alignment, but recipes also need to work against the actual running system.

4. **Recapture screenshots.** Any UI change from Epics A–H needs fresh screenshots.

**What this epic does NOT do:** Fix tutorial content for individual epics. If Epic A broke a Part 3 code block and didn't fix it, that's an Epic A bug, not an Epic I task.

---

## BMAD Stories

### Story 48.1: Inclusivity-First Narrative Integration

**As a** tutorial reader learning Trovato,
**I want** foundational concepts (accessibility, i18n, security, privacy, performance) woven naturally into the tutorial narrative,
**So that** I learn these as "how Trovato works" rather than "extra things to think about."

**Acceptance criteria:**

- [ ] Part 3 (Look and Feel): When introducing templates, show that `base.html` has semantic landmarks and a skip link. Explain *why* briefly: "Trovato's base template uses semantic HTML — `<header>`, `<main>`, `<footer>`, `<nav>` — so that screen readers and search engines understand the page structure. The skip link lets keyboard users bypass navigation." Don't belabor it; make it part of the template explanation.
- [ ] Part 3: When showing the render pipeline, note that the fallback uses semantic tags and that images get `loading="lazy"` by default. Frame as performance and quality defaults, not a separate accessibility lesson.
- [ ] Part 5 (Forms): When showing form validation, demonstrate `aria-describedby` on error messages. Frame as "Trovato forms are accessible by default — screen readers read the error message when you focus the field."
- [ ] Part 5: When showing image blocks, note that `alt` is required. Bridge to Drupal: "Like Drupal's image field requiring alt text, Trovato enforces this at the schema level."
- [ ] Part 7 (Going Global): When showing language configuration, demonstrate `dir="rtl"` and locale-aware dates. Frame as "Trovato's i18n is infrastructure, not a bolt-on."
- [ ] No separate "accessibility chapter" or "security chapter." These concepts live where they naturally arise.
- [ ] Tutorial bridge language used where introducing new concepts: "Taps serve the same role as Drupal hooks", "Tiles are Trovato's equivalent of Drupal blocks", etc. Use sparingly — only when introducing a concept for the first time.

**Implementation notes:**
- Read each tutorial part end-to-end looking for natural integration points
- Add 1-3 sentences at each point, not entire sections
- The goal is that a reader finishing the tutorial *already knows* that Trovato is accessible, internationalized, and secure — without ever reading a dedicated chapter on any of those topics

---

### Story 48.2: Tutorial Coherence Pass

**As a** tutorial reader going through Parts 1–7 sequentially,
**I want** the tutorial to read as a coherent narrative,
**So that** concepts build on each other without repetition, gaps, or pacing issues.

**Acceptance criteria:**

- [ ] No concept explained twice across different parts (unless intentional progressive disclosure)
- [ ] Terminology consistent across all parts (use Trovato terminology per CLAUDE.md: category not taxonomy, item not node, tap not hook, etc.)
- [ ] Forward references ("we'll cover this in Part N") verified accurate after epic updates
- [ ] Backward references ("as we saw in Part N") verified accurate
- [ ] Each part's introduction and conclusion still set up the next part correctly
- [ ] Code blocks tested sequentially (Part 1 setup → Part 2 builds on Part 1 → etc.)
- [ ] `trovato-test` blocks in all parts pass against the updated kernel
- [ ] No orphaned instructions (referencing UI elements that no longer exist or fields that moved)

**Implementation notes:**
- Read all 7 parts in order, taking notes on inconsistencies
- Run `grep -n "Part " docs/tutorial/part-*.md` to find all cross-references
- Run the trovato-test extraction to verify all code blocks execute

---

### Story 48.3: Recipe Verification Pass

**As a** agent following a recipe to work through the tutorial,
**I want** all recipes tested against the updated kernel,
**So that** every recipe step produces the expected result.

**Acceptance criteria:**

- [ ] Run `bash docs/tutorial/recipes/sync-check.sh` — all hashes match (if not, recipes are stale)
- [ ] For each recipe (1–7), verify that the command sequences in the recipe produce the expected outcomes against a fresh Trovato install
- [ ] Recipe steps that reference config fixtures in `docs/tutorial/config/` verified against actual fixture files
- [ ] Recipe steps that reference admin UI paths verified against current route structure
- [ ] Any recipe step that fails is fixed (recipe bug) or noted (kernel bug to file separately)
- [ ] All sync hashes updated after any recipe modifications

**Implementation notes:**
- This is a testing story, not a writing story
- Start from a fresh database (use the backup/restore process documented in CLAUDE.md)
- Work through each recipe sequentially
- This catches drift between recipes and the running system that hash checks can't detect (a recipe can have the right hash but wrong commands if both the tutorial and recipe were updated to the same incorrect state)

---

### Story 48.5: SDK Backward Compatibility Verification

**As a** plugin author with compiled WASM plugins,
**I want** verified evidence that existing plugins continue to work after SDK changes,
**So that** I don't need to recompile every plugin when the kernel is updated.

**Acceptance criteria:**

- [ ] All 21 fully-implemented WASM plugins compiled against the pre-change SDK load successfully on the updated kernel
- [ ] Plugins that handle `Item` objects continue to work — new `language`, `tenant_id`, `retention_days` fields default to `None`/`null`
- [ ] Plugins that define `FieldDefinition` continue to work — new `personal_data` field defaults to `false`
- [ ] New host functions (crypto_*, register_route_metadata) do not interfere with existing bindings
- [ ] Test methodology documented for future SDK changes

**Implementation notes:**
- Capture pre-change WASM binaries as test fixtures before SDK changes merge
- After all SDK changes land, load fixtures and exercise tap handlers
- Verifies the "no hard breaking changes" claim from the summary document

---

### Story 48.4: Screenshot Refresh

**As a** tutorial reader,
**I want** screenshots to match the current UI,
**So that** I can visually verify I'm on the right track.

**Acceptance criteria:**

- [ ] All screenshots in `docs/tutorial/images/part-{01-07}/` verified against current UI
- [ ] Screenshots that no longer match recaptured using `docs/tutorial/images/screenshot.mjs`
- [ ] New screenshots needed for UI elements added by Epics A–H (e.g., alt field on image blocks, consent fields on user admin, AI config page)
- [ ] Screenshot dimensions and format consistent across all parts
- [ ] Screenshots use the tutorial's standard database state (created by following the tutorial, or restored from a backup)
- [ ] No screenshots reference features that don't exist yet (e.g., no screenshots of webhook admin if webhook plugin is still a stub)

**Implementation notes:**
- Use `docs/tutorial/images/screenshot.mjs` to capture
- Review each screenshot in each part directory
- Some screenshots from Epics A–H may already be correct (captured during those epics). Verify, don't re-do unnecessarily.
- Use the thumbnail format established by commit `e0fcef9` (convert tutorial images to thumbnail format)

---

## Plugin SDK Changes

None.

---

## Design Doc Updates

None (each epic shipped its own; Epic J handles cross-cutting sync).

---

## Tutorial Impact

This IS the tutorial epic. All 7 parts are in scope.

---

## Recipe Impact

All 7 recipes are in scope (Story 48.3).

---

## Screenshot Impact

All screenshot directories are in scope (Story 48.4).

---

## Config Fixture Impact

Config fixtures verified as part of recipe verification (Story 48.3). No new fixtures expected.

---

## Migration Notes

None. This is a documentation-only epic.

---

## What's Deferred

- **New tutorial parts** (multi-tenancy tutorial, AI tutorial, webhook tutorial) — future epics. This epic refreshes existing Parts 1–7.
- **Tutorial translations** — future. Would require the locale plugin to be fully implemented.
- **Interactive tutorial** (run-in-browser tutorial environment) — future infrastructure project.
- **Tutorial video recordings** — different medium, different project.

---

## Related

- [Epics A–H](epic-10-accessibility.md) — Each shipped its own tutorial updates; this epic does the systemic pass
- [Epic J (19): Design Doc Sync](epic-19-design-sync.md) — Follows this epic
- [docs/tutorial/recipes/sync-check.sh](../tutorial/recipes/sync-check.sh) — Hash verification tool
- [docs/tutorial/images/screenshot.mjs](../tutorial/images/screenshot.mjs) — Screenshot capture tool
