# Story 41.1: Un-ignore Item.language in Plugin SDK

Status: ready-for-dev

## Story

As a **plugin developer**,
I want to read the `language` field on `Item` objects,
So that my plugin can behave differently based on content language (e.g., translation workflows, language-specific formatting).

## Acceptance Criteria

1. `Item` struct in `crates/plugin-sdk/src/types.rs` includes `language: Option<String>` in serde serialization (remove the `#[serde(skip)]` or equivalent exclusion)
2. Items serialized to plugins via host functions include the `language` field
3. Items deserialized from plugin responses accept the `language` field
4. `language` field is `Option<String>` -- `None` for items created before language support, `Some("en")` for items with language set
5. Existing plugins that deserialize `Item` continue to work (serde `Option` defaults to `None` for missing fields -- backward compatible)
6. At least one integration test verifies language round-trips through the WASM boundary

## Tasks / Subtasks

- [ ] Remove `#[serde(skip)]` (or equivalent) from `language` field on `Item` in `crates/plugin-sdk/src/types.rs` (AC: #1)
- [ ] Remove or update the comment explaining the original exclusion if the reason no longer applies (AC: #1)
- [ ] Verify items serialized to plugin host functions include `language` (AC: #2)
- [ ] Verify items deserialized from plugin responses accept `language` (AC: #3)
- [ ] Verify `Option<String>` defaults to `None` when field is absent in older serialized data (AC: #4, #5)
- [ ] Add integration test: create item with `language = Some("en")`, serialize through WASM boundary, verify language round-trips (AC: #6)
- [ ] Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all` (AC: all)

## Dev Notes

### Architecture

This is a one-line change plus test. The `Item` struct in `crates/plugin-sdk/src/types.rs` has a `language: Option<String>` field with a serde skip annotation. Removing the skip makes the field visible to plugins.

Backward compatible: existing compiled WASM plugins that do not know about the `language` field will simply ignore it during deserialization (serde `Option` defaults to `None` for missing fields). No plugin recompilation required.

### Testing

- Add a unit test in the plugin-sdk crate verifying serde round-trip with and without `language`
- Add an integration test in `crates/kernel/tests/` verifying an item with `language` set survives serialization to a plugin and back

### References

- `crates/plugin-sdk/src/types.rs` -- Item struct with serde skip on language
- [Epic 41 source: docs/ritrovo/epic-11-i18n.md]
