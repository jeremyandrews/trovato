# Epic 4: From Demo to Data-Driven

**Tutorial Part:** 2
**Trovato Phase Dependency:** Phase 3 (Plugin SDK, Gather, Category, Search)
**BMAD Epic:** 33 (stories 33-1 through 33-4) + 30-1 (search)
**Status:** Not started

---

## Narrative

Part 1 ends with a working Trovato site and three hand-entered conferences. Part 2 transforms it into a live data feed. A WASM plugin fetches thousands of conferences from confs.tech, maps them to the `conference` item type, deduplicates, validates, and keeps them current via a daily cron job. A hierarchical topic taxonomy makes conferences browsable. Exposed and contextual Gather filters make them searchable by location, topic, and CFP deadline. PostgreSQL full-text search completes the picture.

The reader writes real plugin code in this part — not just configuration. They see how taps work, how to call host functions from inside a WASM sandbox, how the queue system absorbs bursty import jobs, and how a three-level taxonomy maps a flat list of filenames to a meaningful browsable tree.

By the end of Part 2, Ritrovo goes from "a CMS with three conferences" to "a live aggregator with hundreds of conferences, fully searchable and filterable."

---

## Stories

| Story | Title | BMAD | Status |
|---|---|---|---|
| 2.1 | Plugin Scaffold & SDK Basics | 33-1 | Not started |
| 2.2 | Cron-Driven Conference Import | 33-2 | Not started |
| 2.3 | Hierarchical Topic Taxonomy | 33-3 | Not started |
| 2.4 | Advanced Gathers with Exposed & Contextual Filters | 33-4 | Not started |
| 2.5 | Full-Text Search | 30-1 | Not started |

---

## Deferred to Later Parts

- Custom `item--conference.html` template (Part 3)
- Speaker Item Type and relationship (Part 3)
- File uploads / conference logos (Part 3)
- User accounts and auth-gated favourites (Part 4)
- Stage-scoped search indexing (Part 4)
- AI-powered semantic search (Part 9, foreshadowed in 2.5)
