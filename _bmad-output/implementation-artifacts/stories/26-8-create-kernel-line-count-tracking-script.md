# Story 26.8: Create Kernel Line-Count Tracking Script

Status: done

## Story

As a **maintainer**,
I want automated tracking of kernel lines of code,
so that I can detect kernel bloat trends before they become problems.

## Acceptance Criteria

1. Script counts lines of Rust code in `crates/kernel/src/` (excluding tests, blanks, comments)
2. Script counts lines in plugin SDK (`crates/plugin-sdk/src/`)
3. Script outputs kernel LOC, SDK LOC, and ratio
4. Baseline measurement recorded in `docs/kernel-minimality-audit.md`
5. Script can be run manually or in CI to compare against baseline

## Tasks / Subtasks

- [x] Create `scripts/kernel-loc.sh` using available tooling (AC: #1, #2, #3)
  - [x] Count kernel LOC excluding blanks and comment lines
  - [x] Count plugin SDK LOC
  - [x] Output kernel LOC, SDK LOC, and kernel:SDK ratio
- [x] Record baseline measurement in `docs/kernel-minimality-audit.md` (AC: #4)
- [x] Add usage instructions to audit doc (AC: #5)

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Created `scripts/kernel-loc.sh` — portable shell script using grep/wc/awk (no external tools needed)
- Baseline: Kernel 34,785 code lines | SDK 481 code lines | Ratio 72.3:1
- Added Section 7 (Kernel Line-Count Baseline) to `docs/kernel-minimality-audit.md`
- Script includes baseline in its output for quick comparison

### File List

- `+ scripts/kernel-loc.sh` (new, executable)
- `~ docs/kernel-minimality-audit.md` (added Section 7 baseline, Section 8 ongoing maintenance)
