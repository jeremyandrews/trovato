# Story 3.6: Active Menu Highlighting & Breadcrumbs

Status: ready-for-dev

## Story

As a site visitor,
I want the current page's menu item highlighted and breadcrumbs showing my location,
So that I always know where I am in the site.

## Acceptance Criteria

1. Active page menu item gets `active` CSS class
2. Parent menu item gets `active-trail` class when child is current
3. Breadcrumbs on conference detail: Home > Conferences > {title}
4. Breadcrumbs on topic pages reflect category hierarchy: Home > Topics > {parent} > {child}

## Tasks / Subtasks

- [ ] Add active/active-trail class logic to menu rendering macro (AC: #1, #2)
  - [ ] Compare current path against menu link paths
  - [ ] Mark parent as active-trail when child matches
- [ ] Create `templates/macros/breadcrumb.html` (AC: #3, #4)
  - [ ] Build from menu hierarchy for section pages
  - [ ] Build from category hierarchy for topic pages
- [ ] Integrate breadcrumb rendering into content area of page template

## Dev Notes

### Architecture

- Active trail: compare request path against each menu link's path
- Breadcrumbs: two sources — menu hierarchy and category hierarchy
- Category hierarchy: `crates/kernel/src/models/category.rs` — recursive CTE for ancestry
- Menu tree already has parent-child relationships

### References

- [Source: docs/design/Design-Web-Layer.md] — breadcrumb generation
- [Source: docs/tutorial/plan-parts-03-04.md#Step 5] — active trail and breadcrumbs
