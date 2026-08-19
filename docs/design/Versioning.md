# Versioning

Trovato has **one version number**. The kernel, the SDK crates, every plugin,
every plugin manifest, the plugin API tuple and the Docker tags all carry it,
and they all move together.

At 0.100.0 the plugin API is `(0, 100)` and every manifest declares
`api_version = "0.100"`. At 1.0.0 the API becomes `(1, 0)` and every manifest
declares `"1.0"`. There is no case where one of these numbers moves and the
others do not.

This is a deliberate simplification. Earlier in development there were four
independent tracks (a kernel version, a plugin API version, an SDK crate version
and a plugin version) and keeping them straight cost more than the flexibility
was worth. A reader who knows a site runs Trovato 0.100.0 now knows exactly which
plugin API it serves and which SDK its plugins were built against.

## Where the number lives

The single source is `[workspace.package]` in the root `Cargo.toml`:

```toml
[workspace.package]
version = "0.100.0"
```

Every in-tree crate inherits it with `version.workspace = true`. No crate in the
workspace declares its own version. Everything else derives from it, and the
full list of places that move together is in [version-map.md](version-map.md).

## Semantics

The project version follows [Semantic Versioning 2.0.0](https://semver.org/):

- **MAJOR**: breaking changes to the plugin contract, user-facing behaviour, the
  database schema, or the configuration format
- **MINOR**: new features, admin pages, config entities, new host functions and
  taps (backward compatible)
- **PATCH**: bug fixes, security fixes, performance improvements

The plugin API tuple is the same version with the patch component dropped:
`0.100.0` gives `(0, 100)`, declared as `KERNEL_API_VERSION` in
`crates/kernel/src/plugin/mod.rs`.

## Compatibility rule

At plugin install and enable time the kernel enforces:

```
Plugin API MAJOR == Kernel API MAJOR
Plugin API MINOR <= Kernel API MINOR
```

With a kernel at API 0.100:

| Plugin API | Compatible? | Reason |
|------------|-------------|--------|
| 0.100 | Yes | Exact match |
| 0.42 | Yes | Same major, older minor: the kernel provides everything it asks for |
| 0.101 | No | Needs host functions this kernel may not export |
| 1.0 | No | Major version mismatch |

The check runs before any expensive work (WASM compilation, migrations) and
produces an error naming both versions.

This is a compatibility gate, not a provenance check. It answers "does this
kernel provide everything the plugin declared it needs", and nothing more.

## The contract is frozen; the number is pre-1.0

The plugin boundary (the WIT surface, the `trovato-sdk` crate, the manifest
semantics, the error vocabularies) was frozen before the first public release
and does not change before 1.0.

Read that alongside how SemVer treats 0.x versions, because the two do not line
up. Under the 0.x rules a breaking change is permitted by a MINOR bump, so
`cargo-semver-checks` in the `SDK Semver Gate` CI job **cannot** fail a break
that moves 0.100 to 0.101. Before 1.0 the freeze is therefore policy, held by
review, not by the tool. At 1.0.0 the tooling and the policy agree again, and a
break requires a MAJOR bump plus a written justification.

The short version: do not break the plugin contract. That the version number
starts with a zero does not make it negotiable.

## Plugin manifests

Plugins declare both numbers in `.info.toml`:

```toml
name = "my_plugin"
description = "Example plugin"
version = "0.100.0"
api_version = "0.100"
```

- `version` is the plugin's own version. In-tree plugins carry the project
  version, because they are released as part of Trovato. An out-of-tree plugin
  is free to version itself however it likes.
- `api_version` is the kernel API it targets. Every plugin, in-tree or not,
  declares the API it was built against.

If `api_version` is omitted it defaults to the current kernel API.

## Host function lifecycle

Host functions and taps follow four states:

1. **experimental** — available but may change without notice
2. **stable** — committed contract; changes only via deprecation
3. **deprecated** — still works, logs a warning naming the replacement and the
   removal version
4. **removed** — gone in the next MAJOR

Deprecation lasts at least one MINOR before removal.

## Docker images

- **Release tags** (`v0.100.0`): multi-platform (amd64 + arm64), tagged with the
  full version, with major.minor, and with `latest`
- **Nightly builds** (every push to `main`): amd64 only, tagged `nightly`,
  `nightly-<sha>`, and an auto-incrementing version

The nightly version increments by counting commits since the latest release tag:

```
version = BASE_VERSION + commits_since_tag
```

`BASE_VERSION` lives in `.github/workflows/docker-publish.yml` and is the
major.minor of the project version. It moves with every release; see
[version-map.md](version-map.md).

## Support policy

At most two active MAJOR versions:

- **Current major**: bug fixes, security fixes, new features
- **Previous major**: security fixes (12 months), critical bug fixes (6 months)

Before 1.0 there is one active line and no back-porting.
