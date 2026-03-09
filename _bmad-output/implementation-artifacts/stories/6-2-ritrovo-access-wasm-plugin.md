# Story 6.2: ritrovo_access WASM Plugin

Status: ready-for-dev

## Story

As a site administrator,
I want the ritrovo_access plugin to declare custom permissions and control item-level access,
So that content visibility is enforced by role.

## Acceptance Criteria

1. Plugin declares permissions via `tap_perm`: "edit any conference", "view internal content", etc.
2. `tap_item_access` returns Grant for Live/Public items (anonymous)
3. `tap_item_access` returns Neutral for Internal items (anonymous)
4. `tap_item_access` returns Grant for editors viewing internal items
5. WASM sandbox enforced: 5s DB, 30s HTTP, 10 ticks

## Tasks / Subtasks

- [ ] Scaffold plugin: `cargo run --release --bin trovato -- plugin new ritrovo_access`
- [ ] Implement `tap_perm` — declare permissions (AC: #1)
- [ ] Implement `tap_item_access` — Grant/Neutral based on stage and role (AC: #2, #3, #4)
- [ ] Build: `cargo build --target wasm32-wasip1 -p ritrovo_access --release`
- [ ] Install: `cargo run --release --bin trovato -- plugin install ritrovo_access`
- [ ] Test access control per role

## Dev Notes

- Plugin scaffold: creates `plugins/ritrovo_access/` with Cargo.toml, src/lib.rs, info.toml
- Plugin SDK: `crates/plugin-sdk/src/` — tap macros, types, host functions
- Access result: Grant/Deny/Neutral enum from plugin-sdk
- Stage visibility: check item's stage tag visibility (Internal vs Public)
- WASM constraints: `statement_timeout` 5s, epoch interrupt 10 ticks

### References

- [Source: docs/design/Design-Plugin-SDK.md]
- [Source: docs/design/Design-Plugin-System.md]
- [Source: crates/plugin-sdk/src/host_errors.rs] — host function error constants
