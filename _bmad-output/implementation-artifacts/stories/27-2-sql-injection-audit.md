# Story 27.2: SQL Injection Audit

Status: ready-for-dev

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

- [ ] Audit Gather query builder for `Expr::cust()` with interpolated values (AC: #2, #4)
  - [ ] Fix `jsonb_extract_expr()` — path components interpolated into SQL via `format!` (HIGH)
  - [ ] Fix `build_category_filter()` — JSONB path and UUID list interpolated (MEDIUM)
  - [ ] Verify `FullTextSearch` filter — base_table interpolated but value parameterized via `$1` (LOW)
- [ ] Audit `execute_gather()` outer query wrapping (AC: #1)
  - [ ] Fix `gather_service.rs:297` — `format!("SELECT row_to_json(t) FROM ({main_sql}) t")` wraps dynamic SQL (HIGH)
- [ ] Verify plugin DB API host functions (AC: #3)
  - [ ] Confirm `VALID_IDENTIFIER` regex validation on all table/column names in `host/db.rs`
  - [ ] Confirm DDL guard blocks CREATE/DROP/ALTER/TRUNCATE/GRANT/REVOKE
  - [ ] Confirm parameterized binding for all SQL parameters
- [ ] Verify all `sqlx::query` calls use bind parameters (AC: #1)
  - [ ] Grep for `format!` near `sqlx::query` contexts across all routes and models
- [ ] Verify migration/schema operations (AC: #5)
  - [ ] Confirm `plugin/migration.rs` only runs file-loaded SQL (not user input)
- [ ] Standardize identifier validation (AC: #4)
  - [ ] Ensure `is_safe_identifier()` from `gather/handlers.rs` is used consistently
  - [ ] Ensure `is_valid_field_name()` in `gather_service.rs` is strict enough
- [ ] Document all findings with severity ratings (AC: #6, #7)

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Codebase Research Findings

#### HIGH: Gather Query Builder — `Expr::cust()` with String Interpolation

**Location:** `crates/kernel/src/gather/query_builder.rs`

The gather query builder uses `Expr::cust()` (raw SQL strings) for JSONB operations, with `format!` for interpolation:

1. **`jsonb_extract_expr()` (lines 375-385)** — Path components interpolated into SQL quotes:
   ```rust
   expr = format!("({expr}->>'{part}')");  // part from user-defined field paths
   Expr::cust(expr)
   ```

2. **`build_category_filter()` (lines 407-423)** — JSONB path and UUID list interpolated:
   ```rust
   let expr = format!("{}.fields->>'{}' IN ({})", base_table, jsonb_path, uuid_list.join(", "));
   Some(Expr::cust(expr))
   ```

3. **`FullTextSearch` filter (lines 289-295)** — Base table interpolated, but search value properly parameterized via `Expr::cust_with_values`:
   ```rust
   Expr::cust_with_values(
       format!("{}.search_vector @@ to_tsquery('english', $1)", base_table),
       [tsquery],
   )
   ```

#### HIGH: Gather Service — Outer Query Wrapping

**Location:** `crates/kernel/src/gather/gather_service.rs:297`

```rust
let mut rows: Vec<serde_json::Value> =
    sqlx::query_scalar(&format!("SELECT row_to_json(t) FROM ({main_sql}) t"))
```

The entire dynamically-built `main_sql` query is interpolated into an outer `row_to_json` wrapper. While `main_sql` is built via SeaQuery (mostly safe), any `Expr::cust()` raw strings within it could carry injection.

#### PROTECTED: Plugin DB API Host Functions

**Location:** `crates/kernel/src/host/db.rs`

Well-protected with comprehensive `VALID_IDENTIFIER` regex (`^[a-zA-Z_][a-zA-Z0-9_]*$`) validation on all table/column names before any `format!` interpolation. All SQL parameters use `sqlx` bind parameters. DDL guard blocks dangerous statements.

#### PROTECTED: Gather Handlers Extension System

**Location:** `crates/kernel/src/gather/handlers.rs:21-31`

Uses strict `is_safe_identifier()` function that validates all identifier inputs. Properly designed.

#### SAFE: Plugin Migration System

**Location:** `crates/kernel/src/plugin/migration.rs`

Uses `sqlx::raw_sql(&sql)` where SQL is loaded from migration files, not user input. Intentional for multi-statement migration execution.

### Root Cause

The vulnerabilities stem from mixing SeaQuery's safe query builders with raw `Expr::cust()` strings. SeaQuery's `Expr::col()` and `Alias::new()` are type-safe, but `Expr::cust()` accepts raw SQL, and when combined with `format!`, bypasses parameterization.

### Recommended Fix Approach

1. Replace `format!` interpolation in `jsonb_extract_expr()` with `Expr::cust_with_values()` or validate path parts against `is_safe_identifier()`
2. Replace `build_category_filter()` with parameterized IN clause using SeaQuery's `Expr::col().is_in()`
3. For the outer `row_to_json` wrapper, verify that SeaQuery-built SQL cannot contain injected content (or restructure to avoid the wrapper)

### References

- [Source: crates/kernel/src/gather/query_builder.rs — JSONB extraction and category filters]
- [Source: crates/kernel/src/gather/gather_service.rs — execute_gather outer query]
- [Source: crates/kernel/src/host/db.rs — Plugin DB API with identifier validation]
- [Source: crates/kernel/src/gather/handlers.rs — is_safe_identifier validation]
