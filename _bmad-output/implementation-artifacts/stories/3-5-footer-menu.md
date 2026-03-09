# Story 3.5: Footer Menu

Status: ready-for-dev

## Story

As a site visitor,
I want a footer menu with About and Contact links,
So that I can find site information and contact details.

## Acceptance Criteria

1. Footer slot contains menu with About and Contact links
2. Links point to placeholder pages (or anchors)

## Tasks / Subtasks

- [ ] Create `menu.footer.yml` config
- [ ] Create `menu_link.footer_about.yml` — /about
- [ ] Create `menu_link.footer_contact.yml` — /contact
- [ ] Import config
- [ ] Render footer menu in Footer slot

## Dev Notes

### Architecture

- Same menu system as main nav, different menu machine name
- Footer slot rendering in `page.html`

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Step 5] — footer menu
