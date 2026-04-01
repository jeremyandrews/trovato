# Story 31.5: Content Enrichment Field Rules

Status: ready-for-dev

## Story

As a **content editor**,
I want AI to automatically enrich content fields when I save an item (e.g., generate a summary from a description),
so that tedious metadata tasks are handled by AI without manual effort.

## Acceptance Criteria

1. **AC1: Field Rule Schema** — Field rules are stored in `site_config` under a dedicated key (e.g., `"ai_field_rules"`). Each rule specifies: `item_type`, `source_field`, `target_field`, `trigger` (on_change, on_create, always), `operation` (chat), `prompt` template with `{field_name}` placeholders, `behavior` (fill_if_empty, overwrite, append), and `weight` (execution order).

2. **AC2: tap_item_presave Dispatch** — `tap_item_presave` is listed in `KNOWN_TAPS` and wired into the item create path in `item_service.rs`. When an item is saved, the kernel dispatches `tap_item_presave` with the item JSON, allowing plugins to modify fields before persistence.

3. **AC3: Rule Evaluation in trovato_ai** — The `trovato_ai` plugin's `tap_item_presave` handler loads field rules from site config, evaluates trigger conditions against the current item type and changed fields, and calls `ai_request()` for each matching rule.

4. **AC4: Prompt Template Expansion** — Rule prompts support `{field_name}` placeholders that are resolved from the item's field values at runtime. Example: `"Summarize this in 2 sentences: {field_description}"` becomes `"Summarize this in 2 sentences: A three-day conference on Rust..."`.

5. **AC5: Target Field Application** — After AI response, the result is applied to the target field according to the rule's behavior: `fill_if_empty` (only if target is empty/null), `overwrite` (always replace), `append` (add to existing content).

6. **AC6: Error Resilience** — If an AI request fails for any rule, the item save continues with the remaining rules and the original field values. Failures are logged but do not block content creation.

7. **AC7: Admin Configuration** — Field rules are configurable via site config (database). A visual admin UI for rule management is a future enhancement. For v1, rules are managed via config import or direct database manipulation.

8. **AC8: Integration Tests** — Tests verify: (a) `tap_item_presave` fires during item creation; (b) field rules schema validates correctly; (c) prompt template expansion produces correct output; (d) `fill_if_empty` skips non-empty targets; (e) failed AI requests do not block item save.

## Tasks / Subtasks

- [x] Task 1: Add `tap_item_presave` to KNOWN_TAPS (AC: #2)
  - [x] 1.1 Add `"tap_item_presave"` to `KNOWN_TAPS` in `crates/kernel/src/plugin/info_parser.rs`
- [x] Task 2: Wire `tap_item_presave` into item create path (AC: #2)
  - [x] 2.1 Add dispatch call in `ItemService::create()` in `crates/kernel/src/content/item_service.rs`
- [x] Task 3: Scaffold `tap_item_presave` in trovato_ai plugin (AC: #3)
  - [x] 3.1 Add `tap_item_presave` handler in `plugins/trovato_ai/src/lib.rs` (returns item unchanged)
- [ ] Task 4: Define field rule schema (AC: #1)
  - [ ] 4.1 Define `FieldRule` struct with all required fields
  - [ ] 4.2 Define `FieldRuleConfig` wrapper for site_config storage
  - [ ] 4.3 Add validation for rule schema (valid field names, trigger types, behaviors)
- [ ] Task 5: Implement rule evaluation in tap_item_presave (AC: #3, #4, #5)
  - [ ] 5.1 Load rules from site config via `ai_request()` host function context
  - [ ] 5.2 Filter rules by item_type and trigger condition
  - [ ] 5.3 Expand prompt templates with field values
  - [ ] 5.4 Call `ai_request()` for each matching rule
  - [ ] 5.5 Apply results to target fields based on behavior
- [ ] Task 6: Add error resilience (AC: #6)
  - [ ] 6.1 Wrap each rule execution in error handling
  - [ ] 6.2 Log failures with rule details and continue
- [ ] Task 7: Integration tests (AC: #8)

## Dev Notes

### Current State

The infrastructure for field rules exists:
- `tap_item_presave` is in `KNOWN_TAPS` (line 93 of `info_parser.rs`)
- `ItemService::create()` dispatches `tap_item_presave` before saving (line 125-139 of `item_service.rs`)
- `trovato_ai` plugin has a scaffold `tap_item_presave` that returns the item unchanged (line 68-79 of `plugins/trovato_ai/src/lib.rs`)

What remains:
- Field rule schema definition and site_config storage
- Runtime rule evaluation logic in the trovato_ai plugin
- Prompt template expansion with field value substitution
- Target field application with behavior modes
- Integration tests for the full field rules pipeline

### Field Rule JSON Schema (Planned)

```json
{
  "item_type": "conference",
  "source_field": "field_description",
  "target_field": "field_summary",
  "trigger": "on_change",
  "operation": "chat",
  "prompt": "Summarize this conference description in 2 sentences: {field_description}",
  "behavior": "fill_if_empty",
  "weight": 0
}
```

### Key Files

- `crates/kernel/src/plugin/info_parser.rs` — KNOWN_TAPS (tap_item_presave at line 93)
- `crates/kernel/src/content/item_service.rs` — tap_item_presave dispatch (lines 119-139)
- `plugins/trovato_ai/src/lib.rs` — tap_item_presave scaffold (lines 67-80)
- `crates/plugin-sdk/src/types.rs` — AiRequest, AiResponse types for ai_request() calls
