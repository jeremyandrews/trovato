# Story 44.1: Gather Relationship Depth Limiting

Status: ready-for-dev

## Story

As a **kernel maintainer**,
I want Gather queries limited to an explicit relationship depth,
so that unbounded recursive joins cannot degrade query performance or exhaust memory.

## Acceptance Criteria

1. Gather queries accept a `max_depth: u8` parameter (default 1)
2. Depth 0 means no relationships are loaded
3. Depth 1 means only direct relationships are loaded
4. Depth 2 means nested relationships (one level deep) are loaded
5. Hard limit of 3 is enforced — configurable via `GATHER_MAX_RELATIONSHIP_DEPTH` env var (default 3)
6. Queries requesting depth exceeding the hard limit are silently truncated to the max (no error returned)
7. Gather admin UI displays and allows editing of `max_depth` on query configurations
8. At least 2 integration tests covering depth limiting behavior

## Tasks / Subtasks

- [ ] Add `max_depth: u8` field to `GatherQuery` struct with default of 1 (AC: #1)
- [ ] Read `GATHER_MAX_RELATIONSHIP_DEPTH` env var in config, default to 3 (AC: #5)
- [ ] Update query builder to respect `max_depth` — stop joining relationships beyond the configured depth (AC: #2, #3, #4)
- [ ] Clamp requested depth to the hard limit at query build time (AC: #6)
- [ ] Add `max_depth` field to Gather admin form and query YAML schema (AC: #7)
- [ ] Add migration for `max_depth` column if gather queries are DB-stored, or update YAML schema (AC: #1)
- [ ] Write integration test: depth 0 returns no relationships (AC: #8)
- [ ] Write integration test: depth exceeding hard limit is silently clamped (AC: #8)

## Dev Notes

### Architecture

The query builder in `crates/kernel/src/gather/` constructs SQL joins for related content. The depth parameter controls how many levels of relationship joins are emitted. At each recursion level, the builder checks `current_depth < max_depth` before adding another join layer. The extension registry may reference relationship fields — depth limiting applies uniformly regardless of how the relationship was registered.

### Testing

- Test with depth 0: verify the result set contains items but no populated relationship fields.
- Test with depth > hard limit: verify the effective depth equals the hard limit (check that deeply nested relationships are absent).
- Consider a test with depth 1 vs depth 2 on content that has nested references to confirm the boundary.

### References

- `crates/kernel/src/gather/` — query builder and extension registry
- Gather query YAML configs in `config/` or database
