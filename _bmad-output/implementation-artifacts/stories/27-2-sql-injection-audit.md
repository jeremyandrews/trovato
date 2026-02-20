# Story 27.2: SQL Injection Audit

Status: review

## Story

As a **security reviewer**,
I want every SQL query in the kernel audited for injection vulnerabilities,
So that no user input can be used to construct arbitrary SQL.

## Acceptance Criteria

1. Every SQL query in kernel verified as parameterized (no string interpolation)
2. SeaQuery usage audited — no `Expr::cust()` or raw expressions with user input
3. Plugin DB API host functions verified — plugins cannot inject through field/table/column names
4. Gather engine verified — filter values, sort directions, contextual arguments all parameterized
5. Migration/schema operations verified — no DDL with interpolated user input
6. All findings documented with severity ratings
7. All Critical/High findings fixed

## Tasks / Subtasks

- [x] Audit Gather query builder for `Expr::cust()` with interpolated values (AC: #2, #4)
  - [x] Fix `jsonb_extract_expr()` — path components interpolated into SQL via `format!` (HIGH)
  - [x] Fix `build_category_filter()` — JSONB path and UUID list interpolated (MEDIUM)
  - [x] Verify `FullTextSearch` filter — base_table interpolated but value parameterized via `$1` (LOW)
- [x] Audit `execute_gather()` outer query wrapping (AC: #1)
  - [x] Verify `gather_service.rs:297` — wraps SeaQuery-built SQL (SAFE after Expr::cust fixes)
- [x] Verify plugin DB API host functions (AC: #3)
  - [x] Confirm `VALID_IDENTIFIER` regex validation on all table/column names in `host/db.rs`
  - [x] Confirm DDL guard blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE
  - [x] Confirm parameterized binding for all SQL parameters
- [x] Verify all `sqlx::query` calls use bind parameters (AC: #1)
  - [x] Grep for `format!` near `sqlx::query` contexts across all routes and models
- [x] Verify migration/schema operations (AC: #5)
  - [x] Confirm `plugin/migration.rs` only runs file-loaded SQL (not user input)
- [x] Standardize identifier validation (AC: #4)
  - [x] Consolidated on `is_safe_identifier()` from `handlers.rs` (removed duplicate `is_safe_sql_ident`)
  - [x] Added `is_safe_table_name()` to `gather_service.rs` for service-layer validation
  - [x] Added `QueryField` field_name/table_alias validation to `validate_definition()`
  - [x] Added `QueryRelationship.name` validation to `validate_definition()`
  - [x] Fixed `base_table` validation: added `starts_with` check, ASCII-only, 63-char limit
  - [x] Fixed `target_table`/`table_alias` validation: ASCII-only, 63-char limit
  - [x] Verified `is_valid_field_name()` in `gather_service.rs` is strict enough
- [x] Document all findings with severity ratings (AC: #6, #7)

## Audit Findings

### FIXED: jsonb_extract_expr() — Unvalidated Path Interpolation (HIGH)

**Location:** `crates/kernel/src/gather/query_builder.rs`

Path components from query definitions were directly interpolated into SQL strings via `format!` + `Expr::cust()`. While the service layer's `validate_definition()` checks field names, `jsonb_extract_expr()` itself had no defense-in-depth validation.

**Fix:** Added `is_safe_identifier()` validation for both table name and each path component before interpolation. Returns `NULL` expression if validation fails.

### FIXED: build_category_filter() — String-Interpolated UUID Values (MEDIUM)

**Location:** `crates/kernel/src/gather/query_builder.rs`

UUID values were manually formatted into SQL strings (`format!("'{u}'")`), and the JSONB path was also interpolated. Both code branches (include_descendants/not) were identical (dead code duplication).

**Fix:** Replaced with `jsonb_extract_expr()` (now validated) + SeaQuery's `is_in()` for parameterized UUID values. Eliminated dead code duplication.

**Known limitation:** `include_descendants` parameter is accepted but not yet implemented — `HasTagOrDescendants` currently behaves identically to `HasTag`. This is a pre-existing gap (the original code had identical branches). A TODO and runtime warning have been added.

### FIXED: FullTextSearch Filter — Unvalidated base_table (LOW)

**Location:** `crates/kernel/src/gather/query_builder.rs`

The search value was properly parameterized via `Expr::cust_with_values("$1")`, but `base_table` was interpolated without validation.

**Fix:** Added `is_safe_identifier()` check on `base_table` before interpolation. Returns `FALSE` expression if validation fails.

### FIXED: CategoryHierarchyQuery::in_descendants_expr() — Unvalidated field_path (LOW)

**Location:** `crates/kernel/src/gather/query_builder.rs`

`field_path` parameter interpolated into SQL without validation. Currently dead code (`#[allow(dead_code)]`) but fixed for defense-in-depth.

**Fix:** Added `is_safe_identifier()` validation. Returns `Expr::cust("FALSE")` if validation fails (consistent with other defense expressions).

### FIXED: validate_definition() — Missing QueryField and Relationship Validation (MEDIUM)

**Location:** `crates/kernel/src/gather/gather_service.rs`

The `validate_definition()` function validated filter/sort field names but did not validate `QueryField.field_name`, `QueryField.table_alias`, or `QueryRelationship.name`. Additionally, `base_table`, `target_table`, and `table_alias` validation used `.is_alphanumeric()` (which accepts Unicode) instead of `.is_ascii_alphanumeric()`, and `base_table` lacked a `starts_with` check.

**Fix:** Added `is_safe_table_name()` function with ASCII-only, starts-with, and 63-char limit checks. Applied to `base_table`, `target_table`, `table_alias`, and `rel.name`. Added validation for `QueryField.field_name` and `QueryField.table_alias`.

### SAFE: execute_gather() Outer Query Wrapping

**Location:** `crates/kernel/src/gather/gather_service.rs:297`

The `format!("SELECT row_to_json(t) FROM ({main_sql}) t")` wrapper interpolates `main_sql`, which is built entirely by SeaQuery. Now that all `Expr::cust()` calls within the query builder validate their inputs, this wrapper is safe — `main_sql` cannot contain injected content.

### PROTECTED: Plugin DB API Host Functions

**Location:** `crates/kernel/src/host/db.rs`

Comprehensive `VALID_IDENTIFIER` regex (`^[a-zA-Z_][a-zA-Z0-9_]*$`) validation on all table/column names (13 validation points). DDL guard blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE. All SQL parameters use `sqlx::query().bind()` parameterization.

### PROTECTED: All sqlx::query Calls Across Routes and Models

Audited all `sqlx::query` usage outside gather/host/migration modules. All dynamic SQL construction uses parameterized placeholders ($N) with `.bind()` — no user input enters SQL strings directly. Notable patterns:
- `models/user.rs`: Conditional SET clause with $N placeholders, values via `.bind()`
- `models/item.rs`: Conditional WHERE clauses with $N placeholders, values via `.bind()`

### SAFE: Plugin Migration System

**Location:** `crates/kernel/src/plugin/migration.rs`

Uses `sqlx::raw_sql(&sql)` where SQL is loaded from migration files on disk, not from user input. Tracking queries use parameterized bindings.

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Key Files Modified

- `crates/kernel/src/gather/query_builder.rs` — Consolidated on `is_safe_identifier()` from `handlers.rs`, hardened `jsonb_extract_expr()`, refactored `build_category_filter()`, added FTS validation, changed `in_descendants_expr()` to return `SimpleExpr`, truncated error log payloads, added 11 security regression tests
- `crates/kernel/src/gather/gather_service.rs` — Added `is_safe_table_name()`, hardened `validate_definition()` (ASCII-only, starts_with, length limits, rel.name), added 7 regression tests
- `crates/kernel/src/gather/handlers.rs` — Made `is_safe_identifier()` `pub(super)` for reuse

### Adversarial Review Fixes

Addressed 12 findings from adversarial review:
1. Consolidated duplicate `is_safe_sql_ident()` → reuse `is_safe_identifier()` from handlers.rs
2. Fixed Unicode vs ASCII validation gap in `validate_definition()`
3. Documented `include_descendants` as not-yet-implemented with TODO and runtime warning
4. Added safety comment for `descendants_cte()` UUID interpolation
5. Added `rel.name` validation in `validate_definition()`
6. Added length limit and starts_with check for table_alias/base_table validation
7. Added sort field injection test
8. Changed `in_descendants_expr()` return type from `String` to `SimpleExpr`
9. Truncated attacker-controlled values in error log messages
10. Strengthened test assertions to check SQL structure not just substrings
11. Added `starts_with` check for base_table validation
12. Updated story documentation with accurate coverage claims

### Test Coverage

- 11 security regression tests in `query_builder::tests`
- 7 validation tests in `gather_service::tests`
- All test suites pass, cargo clippy clean, cargo fmt clean
