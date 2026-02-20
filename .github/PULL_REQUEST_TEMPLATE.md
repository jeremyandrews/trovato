## Summary

<!-- Brief description of what this PR does and why. -->

## Changes

<!-- What was modified, added, or removed. -->

## Test Plan

<!-- How were the changes tested? -->

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all`
- [ ] `cargo doc --no-deps --document-private-items` (no warnings)

## Kernel Boundary

<!-- Complete this section if this PR modifies files in `crates/kernel/` or `crates/plugin-sdk/`. Skip for plugin-only, docs, or CI changes. -->
<!-- See CLAUDE.md (Kernel Minimality Rules) and docs/kernel-minimality-audit.md for classification reference. -->

- [ ] **Why can't this be a plugin?** No existing Tap or trait could provide this functionality from plugin space.
- [ ] **Does this contain CMS-specific business logic?** The kernel must not assume specific content types, field names, or domain concepts.
- [ ] **Could a plugin provide this through an existing Tap or trait?** If yes, extract it.
- [ ] **Does removing this break the plugin contract?** At least one plugin or kernel subsystem depends on this.
- [ ] **Is there hardcoded behavior that should be configurable via Tap?** Behavior that varies by use case should be configurable, not hardcoded.
