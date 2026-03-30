# Inclusivity-First Foundation: Epic Dependency Graph

**Epics:** 10–19 (A–J)
**Scope:** Kernel infrastructure, Plugin SDK, design docs, tutorials, recipes
**Governing Principle:** The core kernel enables. Plugins implement.

---

## Naming Convention

Each epic has three identifiers. Use this table to translate between them:

| Letter | File (docs/ritrovo/) | BMAD # (stories) | Title |
|---|---|---|---|
| A | `epic-10-accessibility.md` | 40 | Accessibility Foundation |
| B | `epic-11-i18n.md` | 41 | i18n Infrastructure |
| C | `epic-12-security.md` | 42 | Security Hardening |
| D | `epic-13-privacy.md` | 43 | Privacy Infrastructure |
| E | `epic-14-performance.md` | 44 | Performance Verification |
| F | `epic-15-versioning.md` | 45 | Versioning & Audit |
| G | `epic-16-multi-tenancy.md` | 46 | Multi-Tenancy Foundation |
| H | `epic-17-external.md` | 47 | External Interface Infra |
| I | `epic-18-tutorial-refresh.md` | 48 | Tutorial & Recipe Refresh |
| J | `epic-19-design-sync.md` | 49 | Design Doc Sync |

**Convention:** Epic docs use file number (10-19). Stories use BMAD number (40-49). Prose uses letter (A-J). Story `42-4` = Epic C = `epic-12-security.md`, Story 42.4.

---

## Dependency Graph

```
                    ┌─────────────────────────────────┐
                    │  Epic A (10): Accessibility      │
                    │  FIRST — extends base templates  │
                    └────────┬──────────┬──────────────┘
                             │          │
              ┌──────────────┘          └──────────────┐
              ▼                                        ▼
    ┌─────────────────┐                    ┌───────────────────┐
    │ Epic B (11):    │                    │ Epic C (12):      │
    │ i18n Infra      │                    │ Security          │
    │ (builds on A's  │                    │ Hardening         │
    │ RTL + logical   │                    │                   │
    │ CSS foundation) │                    │                   │
    └─────────────────┘                    └───────────────────┘

    ┌─────────────────┐     ┌──────────────────┐
    │ Epic D (13):    │     │ Epic E (14):     │
    │ Privacy/GDPR    │     │ Performance      │
    │ (independent)   │     │ (independent)    │
    └─────────────────┘     └──────────────────┘

    ┌─────────────────┐
    │ Epic F (15):    │
    │ Versioning &    │──────────────┐
    │ Audit           │              │
    └─────────────────┘              │
                                     ▼
    ┌─────────────────┐     ┌──────────────────┐
    │ Epic G (16):    │     │ Epic H (17):     │
    │ Multi-Tenancy   │     │ External Iface   │
    │ (independent    │     │ (needs F's       │
    │ but largest)    │     │ ai_generated)    │
    └─────────────────┘     └──────────────────┘

              │                        │
              ▼                        ▼
    ┌───────────────────────────────────────────┐
    │ Epic I (18): Tutorial & Recipe Refresh    │
    │ (after all epic-specific updates land)    │
    └───────────────────────────────────────────┘
              │
              ▼
    ┌───────────────────────────────────────────┐
    │ Epic J (19): Design Doc Sync              │
    │ (after all epic-specific updates land)    │
    └───────────────────────────────────────────┘
```

## Sequencing Rules

| Constraint | Reason |
|---|---|
| **A before B** | A adds `dir="rtl"` support and converts CSS to logical properties (Story 40.6); B verifies language context propagation across all render contexts and builds on A's RTL foundation |
| **A before C** | C's `field_access` tap stories reference A's form accessibility patterns |
| **F before H** | H uses `ai_generated` flag on `item_revision` that F creates |
| **A–H before I** | I covers only net-new tutorial content; each epic ships its own updates |
| **A–I before J** | J does the final cross-cutting design doc sync |

## Parallelization Opportunities

These groups can be worked on concurrently:

| Group | Epics | Notes |
|---|---|---|
| **Wave 1** | A | Must land first — template foundation |
| **Wave 2** | B, C, D, E, F | All independent of each other; B and C depend on A |
| **Wave 3** | G, H | G independent; H depends on F from Wave 2 |
| **Wave 4** | I, J | Sequential: I then J |

Within Wave 2, all five epics can be developed in parallel by different contributors. The only constraint is that B and C should not merge before A.

**`types.rs` coordination:** Epics B, C, D, and G all add types or fields to `crates/plugin-sdk/src/types.rs`. All changes are additive (new types, new `Option<T>` fields with `#[serde(default)]`, new methods), so merge conflicts are structural (adjacent lines), not semantic. **Mitigation:** Each epic appends to different sections of the file — B modifies `Item`, C adds `FieldAccessResult`, D adds `personal_data` on `FieldDefinition` and `UserExportData`, G adds `TenantContext`. Developers should merge `main` into their branch before opening a PR. The additive-only constraint means merge resolution is always "keep both additions."

## Epic Summary

| Epic | Title | BMAD # | Stories | Migrations | SDK Changes |
|---|---|---|---|---|---|
| A (10) | Accessibility Foundation | 40 | 9 | 1 | Yes (ElementBuilder ARIA helpers) |
| B (11) | i18n Infrastructure | 41 | 5 | 0 | Yes (Item.language serde) |
| C (12) | Security Hardening | 42 | 8 | 0 | Yes (crypto host functions) |
| D (13) | Privacy Infrastructure | 43 | 5 | 3 | Yes (personal_data on FieldDefinition) |
| E (14) | Performance Verification | 44 | 5 | 0 | No |
| F (15) | Versioning & Audit | 45 | 4 | 2 | No |
| G (16) | Multi-Tenancy Foundation | 46 | 9 | 5+ | Yes (tenant types, config) |
| H (17) | External Interface Infra | 47 | 6 | 1 | Yes (route metadata, AI types) |
| I (18) | Tutorial & Recipe Refresh | 48 | 5 | 0 | No |
| J (19) | Design Doc Sync | 49 | 3 | 0 | No |
