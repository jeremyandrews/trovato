# Story 49.2: Update Overview.md with Inclusivity-First Positioning

Status: ready-for-dev

## Story

As a reader discovering Trovato for the first time,
I want the Overview document to reflect the inclusivity-first architecture,
so that I immediately understand Trovato's core design philosophy and how it differentiates from other CMS platforms.

## Acceptance Criteria

1. The "What It Is" section is updated to mention inclusivity-first as a foundational architectural principle.
2. A new "Design Principles" section is added (or "Key Design Decisions" is expanded) covering: accessibility by default, i18n from day one, security by design, privacy by default, multi-tenancy as infrastructure, API-first, AI as governed resource.
3. The "Design Documents" section is updated to reference any new design docs added during Epics A-H.
4. All wikilinks in the document are verified and resolve correctly.

## Tasks / Subtasks

- [ ] Review current `docs/design/Overview.md` content and structure (AC: #1, #2, #3)
- [ ] Update "What It Is" section to incorporate inclusivity-first positioning (AC: #1)
- [ ] Add "Design Principles" section (or expand "Key Design Decisions") with the seven principles (AC: #2)
- [ ] Write concise descriptions for each principle: accessibility by default, i18n from day one, security by design, privacy by default, multi-tenancy as infrastructure, API-first, AI as governed resource (AC: #2)
- [ ] Inventory design docs added during Epics A-H and add them to the "Design Documents" section (AC: #3)
- [ ] Verify all wikilinks resolve to existing documents (AC: #4)
- [ ] Review the updated document for tone and consistency with existing prose style (AC: #1, #2)

## Dev Notes

### Architecture

The Overview is the entry point for anyone reading the design documentation. It should convey Trovato's identity quickly and accurately. The inclusivity-first framing is not a bolted-on concern but a foundational architectural decision that influenced the kernel design, plugin boundary, template engine, and data model.

Each design principle should be 2-3 sentences: what it means concretely in Trovato, not abstract platitudes. For example, "accessibility by default" means semantic HTML in the render pipeline, ARIA attributes in form generation, and skip links in the base template — not "we care about accessibility."

### Testing

- Verify the document renders correctly in a Markdown viewer.
- Verify all wikilinks resolve (check each `[[link]]` or `[text](path)` against the filesystem).
- Read the updated document from the perspective of someone who has never heard of Trovato — does it convey the key ideas clearly?

### References

- `docs/design/Overview.md`
- `docs/ritrovo/epic-*.md` — Epic A-H documentation for principle details
- `docs/design/*.md` — full design doc inventory
