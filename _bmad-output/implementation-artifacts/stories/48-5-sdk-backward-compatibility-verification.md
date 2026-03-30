# Story 48.5: SDK Backward Compatibility Verification

Status: ready-for-dev

## Story

As a plugin author with compiled WASM plugins,
I want verified evidence that existing plugins continue to work after SDK changes,
so that I don't need to recompile every plugin when the kernel is updated.

## Acceptance Criteria

1. All 21 fully-implemented WASM plugins compiled against the pre-change SDK load successfully on the updated kernel
2. All plugin tap handlers (tap_item_info, tap_menu, tap_perm, tap_install, etc.) execute without deserialization errors
3. Plugins that handle `Item` objects continue to work — the new `language`, `tenant_id`, and `retention_days` fields default to `None`/`null` when absent from old serialized data
4. Plugins that define `FieldDefinition` continue to work — the new `personal_data` field defaults to `false` when absent
5. New host functions (crypto_*, register_route_metadata) do not interfere with existing host function bindings
6. At least 1 integration test per verification: load old plugin binary, invoke a tap, verify correct behavior
7. Test methodology documented: how to reproduce the backward compatibility check for future SDK changes

## Tasks / Subtasks

- [ ] Capture pre-change WASM binaries for at least 3 representative plugins: blog, categories, ritrovo_importer (AC: #1)
  - [ ] These binaries serve as "old SDK" test fixtures
- [ ] After all SDK changes land, attempt to load each pre-change binary (AC: #1)
  - [ ] Verify Module compilation succeeds
  - [ ] Verify Store instantiation succeeds
  - [ ] Verify tap function exports are found
- [ ] Invoke tap_item_info on old binaries, verify content type definitions returned correctly (AC: #2)
- [ ] Invoke tap_menu on old binaries, verify menu definitions returned correctly (AC: #2)
- [ ] Create an Item with new fields (language, tenant_id), serialize to old plugin, verify no crash (AC: #3)
- [ ] Create a FieldDefinition with personal_data=true, serialize to old plugin context, verify no crash (AC: #4)
- [ ] Verify new host function exports don't collide with existing ones (AC: #5)
- [ ] Write integration test: load pre-change blog plugin binary, call tap_item_info, assert expected result (AC: #6)
- [ ] Document the backward compatibility testing methodology (AC: #7)

## Dev Notes

### Architecture

The key insight is that WASM plugin binaries are compiled against a specific version of `crates/plugin-sdk`. The SDK changes in Epics B, C, D, and G add new fields with `#[serde(default)]` or `Option<T>`. When the kernel serializes data with these new fields and passes it to an old plugin binary, the old plugin's deserialization code should silently ignore unknown fields (serde's default behavior for structs without `#[serde(deny_unknown_fields)]`).

This works because:
1. `serde_json` ignores unknown keys by default when deserializing
2. `Option<T>` fields missing from JSON deserialize as `None`
3. `bool` fields with `#[serde(default)]` missing from JSON deserialize as `false`

The risk is if any SDK type uses `#[serde(deny_unknown_fields)]` — this would break backward compatibility. This story verifies that no such annotation exists.

### Testing

- **Fixture approach:** Before merging any SDK changes, compile the blog, categories, and ritrovo_importer plugins and save the `.wasm` binaries as test fixtures in `crates/kernel/tests/fixtures/`. After SDK changes, load these fixtures and exercise them.
- **Alternative:** If the git history is accessible, check out the pre-change commit, compile plugins, then switch back and test. The fixture approach is simpler and more reproducible.

### References

- `crates/plugin-sdk/src/types.rs` — SDK type definitions
- `crates/kernel/src/plugin/` — plugin loader
- `plugins/blog/`, `plugins/categories/`, `plugins/ritrovo_importer/` — test subjects
- [Source: docs/ritrovo/epic-10-19-summary.md] — "No hard breaking changes" claim to verify
