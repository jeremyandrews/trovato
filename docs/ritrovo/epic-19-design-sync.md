# Epic 19 (J): Design Doc Sync

**Tutorial Parts Affected:** None
**Trovato Phase Dependency:** All epics A–I landed
**BMAD Epic:** 49
**Status:** Partially complete. Individual design docs updated during feature implementation. Pending: cross-cutting sync pass to ensure all design docs reference current code, Phases overview update, architecture diagram update.
**Estimated Effort:** 1–2 weeks
**Dependencies:** Epics A–I (each ships its own design doc updates; this epic covers cross-cutting sync)
**Blocks:** None (terminal epic)

---

## Narrative

*Each epic A–H updated the design docs it touched. Epic I refreshed the tutorials. But design docs reference each other — Overview.md points to Phases.md, Content-Model.md references Plugin-SDK.md, Infrastructure.md links to Web-Layer.md. After eight epics modify different docs independently, the cross-references may be stale, the architecture overview may not reflect the new inclusivity-first positioning, and Phases.md is definitely wrong (it says "Not started" for every phase despite all being complete).*

*This epic is the final pass: update the documents that only make sense after all the pieces are in place.*

---

## BMAD Stories

### Story 49.1: Update Phases.md with Current Status

**As a** project stakeholder reviewing progress,
**I want** Phases.md to reflect reality,
**So that** the roadmap document is useful rather than misleading.

**Acceptance criteria:**

- [ ] All phases (0–6) marked with their actual status:
  - Phase 0 (Critical Spike): **Complete** — WASM benchmarks done, hybrid access mode chosen
  - Phase 1 (Skeleton): **Complete** — Axum server, Postgres, Redis, sessions, auth
  - Phase 2 (Plugin Kernel + SDK): **Complete** — WASM loader, tap dispatcher, 24 plugins, SDK
  - Phase 3 (Content, Fields, Stages): **Complete** — JSONB items, stages, revisions, search, admin forms
  - Phase 4 (Gather + Categories): **Complete** — SeaQuery Gather, categories with hierarchy, extensions
  - Phase 5 (Form API, Theming, Admin UI): **Complete** — Forms, CSRF, AJAX, Tera templates, theme engine
  - Phase 6 (Files, Search, Cron, Hardening): **Complete** — File uploads, tsvector search, cron, rate limiting
- [ ] Each phase entry updated with key implementation dates (from git history)
- [ ] "Not in Estimate" section updated: WASM tooling debugging done, Plugin SDK done, comprehensive tests done (145+ tests), plugin author docs partially done
- [ ] New section: "Phase 7: Inclusivity-First Foundation" summarizing Epics A–H with their status
- [ ] Time estimate updated to reflect actual effort vs. original estimate

**Implementation notes:**
- Use `git log --oneline --after="2026-02-01"` to find implementation dates for each phase
- The original estimate was "42-58 weeks, honestly 50-65 weeks." Compare to actual timeline.
- Phase 7 is the new work from the inclusivity-first epics — list the epics and their status

---

### Story 49.2: Update Overview.md with Inclusivity-First Positioning

**As a** reader discovering Trovato for the first time,
**I want** the Overview to reflect the inclusivity-first architecture,
**So that** I understand this is a CMS that bakes accessibility, i18n, security, and privacy into its foundation.

**Acceptance criteria:**

- [ ] Overview.md "What It Is" section updated to mention inclusivity-first as a design principle alongside WASM plugins, JSONB fields, etc.
- [ ] New "Design Principles" section (or expansion of "Key Design Decisions"):
  1. Plugins are untrusted (WASM boundary) ✅ existing
  2. No persistent state in binary ✅ existing
  3. **Accessibility by default** — semantic HTML, ARIA, skip links, required alt text
  4. **i18n from day one** — language column on all content, RTL support, locale-aware rendering
  5. **Security by design** — CSP, field-level access control, crypto host functions
  6. **Privacy by default** — consent tracking, PII markers, no external resource loading
  7. **Multi-tenancy as infrastructure** — tenant_id on all tables, like language column
  8. **API-first** — route metadata, versioning, deprecation headers
  9. **AI as a governed resource** — tap_ai_request, metadata audit trail, per-feature config
- [ ] "Design Documents" section updated to include any new design docs
- [ ] Wikilinks (`[[...]]`) verified to resolve correctly in both Obsidian and GitHub rendering

**Implementation notes:**
- Overview.md uses Obsidian wikilink syntax (`[[Projects/Trovato/...]]`) which doesn't render on GitHub. Consider adding standard markdown link alternatives.
- Keep the document concise — it's an overview, not a manifesto. One sentence per principle.

---

### Story 49.3: Cross-Reference Audit and Appendix Cleanup

**As a** reader navigating between design docs,
**I want** all cross-references between design docs to be correct,
**So that** I can follow links without hitting dead ends or stale content.

**Acceptance criteria:**

- [ ] All design docs (17 files in `docs/design/`) audited for internal cross-references
- [ ] Broken links fixed (file renames, moved sections, deleted content)
- [ ] Stale references updated (references to "planned" features that are now implemented)
- [ ] `Appendix-Deferred-Issues.md` updated:
  - Clear items that have been resolved by Epics A–H
  - Add new deferred items identified during epic work
  - Each entry has: issue, why deferred, when to revisit
- [ ] `docs/design/Terminology.md` updated with any new Trovato terminology introduced by the inclusivity-first work
- [ ] No design doc references features or APIs that don't exist (each claim verifiable against current code)
- [ ] Epic docs (`docs/ritrovo/epic-*.md`) cross-reference correctly to design docs and to each other

**Implementation notes:**
- Use `grep -rn '\[\[' docs/design/` to find all wikilinks
- Use `grep -rn '\](../' docs/` to find all relative links
- Check each link target exists
- For Appendix-Deferred-Issues.md: review the epic "What's Deferred" sections — aggregate the cross-epic deferrals into the appendix

---

## Plugin SDK Changes

None.

---

## Design Doc Updates

This IS the design doc epic. All 17 design docs are in scope for cross-reference verification. Specific content updates:

| Doc | Changes |
|---|---|
| `docs/design/Phases.md` | Full status update (Story 49.1) |
| `docs/design/Overview.md` | Inclusivity-first positioning (Story 49.2) |
| `docs/design/Appendix-Deferred-Issues.md` | Clear resolved, add new (Story 49.3) |
| `docs/design/Terminology.md` | Add any new terms from Epics A–H |
| All others | Cross-reference verification only (Story 49.3) |

---

## Tutorial Impact

None. This is a design doc epic.

---

## Recipe Impact

None.

---

## Screenshot Impact

None.

---

## Config Fixture Impact

None.

---

## Migration Notes

None. This is a documentation-only epic.

---

## What's Deferred

Nothing. This is the terminal epic — it closes out the inclusivity-first foundation work.

---

## Related

- All design docs in `docs/design/`
- [Epics A–H](epic-10-accessibility.md) — Each shipped its own design doc updates
- [Epic I (18): Tutorial & Recipe Refresh](epic-18-tutorial-refresh.md) — Covers tutorial coherence
