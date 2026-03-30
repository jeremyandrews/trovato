# Story 40.9: Admin Tab Arrow Key Navigation

Status: ready-for-dev

## Story

As a **keyboard user** navigating the admin UI,
I want arrow keys to move between tabs within a tab group,
so that I can switch tabs efficiently without tabbing through every tab link.

## Acceptance Criteria

1. Admin tab container (`templates/admin/macros/tabs.html`) has `role="tablist"` on the container element
2. Individual tab links have `role="tab"` and `aria-selected` reflecting active state
3. Left/Right arrow keys move focus between tabs within the tablist
4. Only the active tab is in the Tab order (`tabindex="0"`); inactive tabs have `tabindex="-1"`
5. Home key moves to first tab, End key moves to last tab
6. Modal-like dialogs (if any exist) trap focus within the dialog while open; if no modals exist, document as N/A
7. At least 1 integration test: verify ARIA roles present on admin tab markup

## Tasks / Subtasks

- [ ] Add `role="tablist"` to admin tab container (AC: #1)
- [ ] Add `role="tab"` and `aria-selected` to tab links (AC: #2)
- [ ] Implement arrow key JavaScript handler for admin tabs (AC: #3, #4, #5)
  - [ ] Left arrow: focus previous tab (wrap to last)
  - [ ] Right arrow: focus next tab (wrap to first)
  - [ ] Home: focus first tab
  - [ ] End: focus last tab
  - [ ] Manage `tabindex` so only focused tab has `tabindex="0"`
- [ ] Audit for modal-like dialogs and add focus trapping if found (AC: #6)
  - [ ] If modals exist: Tab key cycles within modal, Escape closes modal
  - [ ] If no modals: document as N/A
- [ ] Write integration test verifying ARIA roles on admin tab HTML (AC: #7)

## Dev Notes

### Architecture
- Modify `templates/admin/macros/tabs.html` — add ARIA roles and tabindex management
- Add tab navigation JavaScript — can be inline in the template or in `static/js/trovato.js` (if Story 42.1 has already extracted JS)
- The WAI-ARIA Authoring Practices tabs pattern is the reference implementation
- This is a small, focused change — only the admin tab macro is affected

### Testing
- Manual keyboard testing: use arrow keys to navigate admin tabs on `/admin/structure/types/{type}` (which has tabs for Fields, Display, etc.)
- Verify `role="tablist"` and `role="tab"` in rendered HTML
- Verify `aria-selected="true"` on active tab, `aria-selected="false"` on others
- Verify `tabindex="0"` on active tab, `tabindex="-1"` on others

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] — Epic 40 definition
- [Source: templates/admin/macros/tabs.html] — Admin tab navigation
- WAI-ARIA Authoring Practices: Tabs pattern
