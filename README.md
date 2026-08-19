<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/png/trovato-lockup-white-2000w.png">
    <img src="assets/brand/png/trovato-lockup-2000w.png" alt="Trovato" width="440">
  </picture>
</p>

<p align="center">A content management system written in Rust.</p>

I have worked with Drupal since 2002. Drupal 6 got the mental model right: everything is a node, you add fields to any content type, you query it through Views without writing SQL, and you extend it by implementing a few named functions. It ran on PHP and MySQL, and loading a node with twenty fields meant twenty joins, because CCK gave every field its own table. Trovato keeps the model and replaces the implementation.

Plugins are WebAssembly modules. Each one runs in its own sandbox for a single request, and it reaches nothing it has not declared: the manifest lists the host interfaces the plugin may import, and the kernel builds a linker that exposes those and no others. A plugin that declares nothing gets nothing. There is no filesystem at all: the runtime hand-writes five WASI stubs and opens no directories, so a plugin that asks to write a file gets `ENOSYS`. There is no path from one plugin's memory into another's.

Content is stored in PostgreSQL as JSONB, so a content type with twenty fields is one row with one JSONB column rather than twenty tables. Queries are built with Gather, a type-safe query engine with a visual builder, filling the role Views did. Content staging and revisions belong to the migration that creates content rather than being added on top of it: every item carries a stage, and every save writes a revision.

## Why "Trovato"

Trovato is Italian for "found." The mark is a lowercase t with a terracotta dot resting in the curve of its foot.

## Status

Trovato is at 0.100.0, working toward 1.0. The plugin contract is frozen; the version number is pre-1.0 because the CMS around it is not finished yet.

The remaining work before 1.0 is reviewing the security findings from private development, triaging dependency advisories, and finishing the admin interface. Some things are deliberately not done: there is no plugin registry and no package format, so a plugin is built and installed from a directory on disk; role, permission, stage and system-configuration screens are driven by config import rather than by forms; semantic search compares vectors exactly, with no approximate index; and migrations only move forward.

[KNOWN-ISSUES.md](KNOWN-ISSUES.md) is the full list, and [ROADMAP.md](ROADMAP.md) says what happens to it.

## Quick start

Docker starts the kernel, PostgreSQL and Redis together. No Rust toolchain needed.

```
git clone https://github.com/jeremyandrews/trovato
cd trovato
cp .env.example .env
docker compose --profile full up
```

Open http://localhost:3001 and follow the installer. The `full` profile is what starts the Trovato container; without it, `docker compose up` brings up PostgreSQL and Redis alone, which is what you want when running the server natively on port 3000.

To build from source, see [INSTALL.md](INSTALL.md).

## Documentation

Start with [Building Your First Site](docs/building-your-first-site.md), then the [Plugin Development Guide](docs/plugin-development.md). The [docs/design](docs/design/) directory covers how each part works, and [Terminology](docs/design/Terminology.md) maps the Drupal words onto the Trovato ones.

## License

Dual licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.

## How it's built

Trovato is written with substantial AI assistance, under my direction and review. Contributions are welcome on the same basis; see [CONTRIBUTING.md](CONTRIBUTING.md).
