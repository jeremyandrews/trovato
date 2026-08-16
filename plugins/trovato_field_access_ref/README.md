# trovato_field_access_ref — reference `tap_field_access` plugin

The canonical implementation of the FR-8 **`tap_field_access`** tap. It is a
real, enableable plugin (not a test-only fixture) that downstream products —
**Cairn**, **Argus**, **Ritrovo** — copy as the starting template for
field-level access control.

## What it demonstrates

`tap_field_access` is **type-level, batched, deny-wins, fail-open** (design
`fr-8-field-access-and-retrieval-layer.md` §2). One dispatch carries a
`FieldAccessBatchInput { user{user_id,authenticated,permissions}, item_type,
operation, fields[] }` and returns a `FieldAccessBatchResult { decisions:
map<field, "Allow"|"Deny"|"NoOpinion"> }`. The kernel aggregates `Deny`-wins
across all implementing plugins; an absent/`NoOpinion` field is visible
(fail-open).

This plugin implements the two downstream patterns, both **type-level** (a
decision is a pure function of `(permissions, item_type, field, operation)` — no
per-item data):

| Pattern | Rule kind | Example (default rules) |
|---|---|---|
| **Ritrovo role** | field on a type requires a permission | `person.ssn` needs `"view pii"`, `person.salary` needs `"view salary"` |
| **Cairn encryption-tier** | field on a type has a sensitivity tier; viewer needs clearance ≥ tier | `record.secret_notes` = tier 3, `record.top_secret` = tier 5; clearance is `max N` over `"clearance N"` permissions |

A governed field the viewer may see returns `Allow`; one they may not returns
`Deny`; an ungoverned field is omitted (`NoOpinion`). If a field is governed by
both patterns, `Deny` wins.

## Rules come from `variables` config

Rules are read from the plugin's `field_rules` variable (JSON), falling back to
the baked-in `DEFAULT_RULES` in `src/lib.rs`. Because the kernel flushes the
shared field-access cache on any plugin `variables` write (design amendment α),
an admin editing `field_rules` takes effect on the **next request** — there is
no ≤5-minute staleness window.

`field_rules` shape:

```json
{
  "role_rules": { "<item_type>": { "<field>": "<required permission>" } },
  "tier_rules": { "<item_type>": { "<field>": <minimum clearance tier> } }
}
```

## Building

Built like every plugin cdylib, for `wasm32-wasip1`:

```bash
cargo build -p trovato_field_access_ref --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/trovato_field_access_ref.wasm plugins/trovato_field_access_ref/
```

CI builds and copies it before the test job (the `.wasm` is gitignored). The
freeze-supporting integration test `crates/kernel/tests/field_access_plugin_test.rs`
drives this plugin through the real kernel dispatch, validating the frozen batch
schema end-to-end before PF-5.

## Capabilities

`host_interfaces = ["logging", "variables"]` — `logging` is referenced by the
`#[plugin_tap]` macro; `variables` is used to read `field_rules`. Declared
exactly (WASM-1 deny-unless-declared), derived from the compiled `.wasm` imports.
`default_enabled = false` — enable it explicitly to opt a site into the example
rules.
