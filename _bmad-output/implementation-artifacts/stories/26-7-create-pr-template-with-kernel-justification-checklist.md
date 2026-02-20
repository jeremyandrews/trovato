# Story 26.7: Create PR Template with Kernel Justification Checklist

Status: done

## Story

As a **maintainer**,
I want every PR that touches kernel code to include a kernel justification,
so that feature logic doesn't creep into the kernel during code review.

## Acceptance Criteria

1. `.github/PULL_REQUEST_TEMPLATE.md` exists with standard PR sections (summary, changes, test plan)
2. Template includes a "Kernel Boundary" checklist section with guidance for kernel PRs
3. Checklist asks: "Why can't this be a plugin?", "Does this contain CMS-specific business logic?", "Could a plugin provide this through an existing Tap or trait?"
4. Template renders correctly on GitHub (verified by visual inspection of raw markdown)

## Tasks / Subtasks

- [x] Create `.github/PULL_REQUEST_TEMPLATE.md` (AC: #1)
  - [x] Add Summary section (brief description of changes)
  - [x] Add Changes section (what was modified)
  - [x] Add Test Plan section (how changes were tested)
- [x] Add Kernel Boundary checklist section (AC: #2, #3)
  - [x] Include "Why can't this be a plugin?" question
  - [x] Include "Does this contain CMS-specific business logic?" question
  - [x] Include "Could a plugin provide this through an existing Tap or trait?" question
  - [x] Include "Does removing this break the plugin contract?" question
  - [x] Include guidance note: only applies when `crates/kernel/` files are modified
- [x] Verify template markdown renders correctly (AC: #4)

## Dev Notes

### What Already Exists

- `.github/workflows/ci.yml` — 7-job CI pipeline (fmt, clippy, test, coverage, build, doc, terminology). The PR template complements this by adding human review gates.
- `CLAUDE.md` — Already has Kernel Minimality Rules section with the governing principle and decision framework. The PR template should reference this.
- `docs/kernel-minimality-audit.md` — Full audit with classification reasoning. The PR template should reference this for context.
- `docs/coding-standards.md` — Comprehensive coding standards. The PR template test plan section should reference the before-committing checklist.

### File to Create

```
.github/PULL_REQUEST_TEMPLATE.md    # New file — GitHub auto-applies to all PRs
```

### Template Design Guidance

GitHub PR templates are plain markdown. GitHub does not support conditional sections (showing/hiding based on files changed). The kernel boundary checklist should be included with a note saying "Complete if this PR modifies `crates/kernel/`" so contributors can skip it for plugin-only or docs-only PRs.

Keep the template concise — overly long templates get ignored. The kernel boundary section should be a checklist (GitHub renders `- [ ]` as interactive checkboxes) with 4-5 items max.

### Key Checklist Questions (from the audit doc)

These come directly from the audit checklist in the epic documentation:

1. **Is this infrastructure or feature?** Infrastructure stays. Features become plugins.
2. **Does this contain CMS-specific business logic?** E.g., if the Kernel knows what a "blog" is, that's wrong.
3. **Could a plugin provide this through an existing Tap or trait?** If yes, extract it.
4. **Does removing this break the plugin contract?** If yes, it stays.
5. **Is there hardcoded behavior that should be configurable via Tap?**

### Patterns from Existing Project

- The project uses conventional commits (e.g., `feat:`, `fix:`, `refactor:`, `docs:`)
- CI runs on push to `main` and pull requests to `main`
- The before-committing checklist is: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all`

### Project Structure Notes

- `.github/` directory exists, contains only `workflows/`
- No existing PR template — this is net-new
- No `CONTRIBUTING.md` exists yet (not in scope for this story)

### References

- [Source: _bmad-output/planning-artifacts/epics.md — Story 26.7]
- [Source: docs/kernel-minimality-audit.md — Audit Checklist section]
- [Source: CLAUDE.md — Kernel Minimality Rules]
- [Source: docs/coding-standards.md — Quick Start, Before Committing]

## Senior Developer Review (AI)

**Date:** 2026-02-20
**Outcome:** Approved (with fixes applied)
**Issues Found:** 2 Medium, 3 Low — all fixed

### Action Items

- [x] [M1] Add `cargo doc --no-deps --document-private-items` to Test Plan checklist
- [x] [M2] Expand scope guidance to include `crates/plugin-sdk/` alongside `crates/kernel/`
- [x] [L1] Remove bare dash placeholder from Changes section
- [x] [L2] Add CLAUDE.md reference to Kernel Boundary guidance comment
- [x] [L3] Rephrase checklist items as questions matching AC wording

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Debug Log References

### Completion Notes List

- Created `.github/PULL_REQUEST_TEMPLATE.md` with 4 sections: Summary, Changes, Test Plan, Kernel Boundary
- Test Plan section includes the 4-step before-committing checklist (fmt, clippy, test, doc) as interactive checkboxes
- Kernel Boundary section has 5 checklist items phrased as questions from the kernel minimality audit
- HTML comments provide guidance: skip for plugin-only/docs/CI changes, reference both CLAUDE.md and docs/kernel-minimality-audit.md
- Scope includes both `crates/kernel/` and `crates/plugin-sdk/` (SDK is the contract surface)
- Code review fixes applied: doc check added, SDK scope added, bare dash removed, CLAUDE.md referenced, question phrasing aligned

### File List

- `+ .github/PULL_REQUEST_TEMPLATE.md` (new)
