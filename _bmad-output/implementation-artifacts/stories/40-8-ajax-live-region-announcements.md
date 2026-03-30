# Story 40.8: AJAX Live Region Announcements

Status: ready-for-dev

## Story

As a **screen reader user** performing AJAX operations in the admin UI,
I want changes announced audibly when the page updates without a full reload,
so that I know my action succeeded without visually scanning the page.

## Acceptance Criteria

1. `base.html` includes `<div aria-live="polite" id="trovato-announcements" class="visually-hidden"></div>` before `</body>`
2. AJAX `executeCommand` function populates `#trovato-announcements` with a human-readable message after DOM changes
3. Announcement text is contextual: "Field added" after add-another, "Item saved" after inline save, "Removed" after remove, etc.
4. Announcement is cleared after 5 seconds (allows re-announcement of the same message on repeated actions)
5. The `replace` AJAX command announces "Content updated" by default if no custom message is provided
6. At least 1 integration test: trigger an AJAX add-another operation, verify `#trovato-announcements` text content is set

## Tasks / Subtasks

- [ ] Add `<div aria-live="polite" id="trovato-announcements" class="visually-hidden"></div>` to `base.html` (AC: #1)
- [ ] Add `Trovato.announce(message)` function to the AJAX framework JS (AC: #2)
  - [ ] Sets `#trovato-announcements` textContent
  - [ ] Clears after 5 seconds via setTimeout (AC: #4)
- [ ] Update `executeCommand` to call `Trovato.announce()` after each command type (AC: #2, #3)
  - [ ] `replace` → "Content updated" (or custom `cmd.announcement` if provided) (AC: #5)
  - [ ] `append` → "Item added"
  - [ ] `remove` → "Item removed"
  - [ ] `redirect` → no announcement (page is navigating away)
  - [ ] `alert` → no announcement (alert is already accessible)
- [ ] Add `announcement` optional field to AJAX command protocol for custom messages (AC: #3)
- [ ] Write integration test: trigger AJAX operation, verify announcement div content (AC: #6)

## Dev Notes

### Architecture
- Modify `templates/base.html` — add the live region div (requires `.visually-hidden` class from Story 40.7)
- Modify AJAX framework JS in `base.html` (or `static/js/trovato.js` after Story 42.1 extracts it)
- The `aria-live="polite"` attribute means screen readers wait until the user is idle before reading the announcement — this is the correct politeness level for non-urgent status updates

### Testing
- Screen reader testing (manual): trigger AJAX add-another in block editor, verify screen reader reads "Item added"
- DOM test: after AJAX operation, assert `document.getElementById('trovato-announcements').textContent` is non-empty
- Timing test: after 5 seconds, assert the content is cleared

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] — Epic 40 definition
- [Source: templates/base.html] — AJAX framework
- WAI-ARIA live regions specification
