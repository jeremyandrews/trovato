# Versioning

Trovato uses a decoupled versioning model: the **kernel version** tracks the overall CMS release, while the **plugin API version** tracks the contract between the kernel and WASM plugins. These version numbers are independent — not every kernel release changes the plugin API.

## Kernel Version (SemVer)

The kernel follows [Semantic Versioning 2.0.0](https://semver.org/):

- **MAJOR**: breaking changes to user-facing behavior, database schema, or configuration format
- **MINOR**: new features, admin pages, config entities (backward-compatible)
- **PATCH**: bug fixes, security fixes, performance improvements

Pre-release versions use the `-beta.N` suffix (e.g., `0.2.0-beta.1`).

The kernel version is defined in `Cargo.toml` at the workspace level and inherited by all crates:

```toml
[workspace.package]
version = "0.2.0"
```

## Plugin API Version (two-part: MAJOR.MINOR)

The plugin API version tracks the WASM host function contract:

- **MAJOR**: host functions removed or signatures changed (breaking)
- **MINOR**: new host functions or taps added (backward-compatible)

The kernel declares its API version as a constant:

```rust
// crates/kernel/src/plugin/mod.rs
pub const KERNEL_API_VERSION: (u32, u32) = (0, 2);
```

## Compatibility Rule

At plugin install and enable time, the kernel enforces:

```
Plugin API MAJOR == Kernel API MAJOR
Plugin API MINOR <= Kernel API MINOR
```

Examples with kernel API 0.2:

| Plugin API | Compatible? | Reason |
|------------|-------------|--------|
| 0.1 | Yes | Older minor, same major |
| 0.2 | Yes | Exact match |
| 0.3 | No | Plugin needs newer kernel |
| 1.0 | No | Major version mismatch |

This check runs before expensive operations (WASM compilation, migrations) and produces a clear error message explaining the incompatibility.

## Plugin Manifest

Plugins declare their API target in `.info.toml`:

```toml
name = "my_plugin"
description = "Example plugin"
version = "1.0.0"
api_version = "0.2"
```

- `version` — the plugin's own semantic version (independent of the kernel)
- `api_version` — the minimum kernel API version required

**Best practice:** Target the lowest API version your plugin needs. This maximizes compatibility across kernel versions.

If `api_version` is omitted, it defaults to `"0.2"` (the current API version), ensuring all existing plugins work without manifest changes.

## Host Function Lifecycle

Host functions and taps follow four states:

1. **experimental** — available but may change without notice
2. **stable** — committed contract; changes only via deprecation
3. **deprecated** — still works, logs a warning with replacement and removal version
4. **removed** — gone in next major API version

Deprecation lasts at least one minor API version before removal.

## Docker Image Versioning

Docker images follow these conventions:

- **Release tags** (`v0.2.0-beta.1`): multi-platform (amd64 + arm64), tagged with full semver, major.minor, and `latest`
- **Nightly builds** (every push to main): amd64 only, tagged as `nightly`, `nightly-<sha>`, and an auto-incrementing version (`0.2.1`, `0.2.2`, ...)

The nightly version increments by counting commits since the latest release tag:

```
version = BASE_VERSION + commits_since_tag
```

This ensures each nightly image has a unique, monotonically increasing version.

## Support Policy

At most two active major kernel versions:

- **Current major**: bug fixes, security fixes, new features
- **Previous major**: security fixes (12 months), critical bug fixes (6 months)

## Current Versions

| Component | Version |
|-----------|---------|
| Kernel | 0.2.0 |
| Plugin API | 0.2 |
| Docker base | 0.2.x |
