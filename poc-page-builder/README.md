# Trovato Page Builder — Proof of Concept

Validates the **Puck JSON → Tera template** round-trip before committing to full kernel integration.

## Quick Start

### Rust renderer
```bash
cd renderer
cargo build --release
cd ..
./renderer/target/release/pb-renderer fixtures/simple-page.json > output.html
open output.html
```

### React editor
```bash
cd editor
npm install
npm run dev
# Open http://localhost:5173
```

### Run all tests
```bash
./compare.sh
```

## Architecture

```
Puck (React editor)          Tera (Rust renderer)
        │                           │
        ▼                           ▼
   Puck JSON  ──── same JSON ────►  Parse
        │                           │
   React render                 Tera templates
   (live preview)               + pulldown-cmark
        │                       + Ammonia
        ▼                           ▼
   Same HTML structure         Same HTML structure
   Same CSS classes            Same CSS classes
```

## Components

| Component | Pattern | Key Test |
|-----------|---------|----------|
| **Hero** | Standalone, variant-based | Optional props, variant CSS classes |
| **TextBlock** | Content, Markdown | pulldown-cmark ↔ react-markdown parity |
| **Columns** | Layout with zones | Recursive child rendering via DropZone |

## Fixtures

| File | Tests |
|------|-------|
| `simple-page.json` | Basic Hero + TextBlock, no zones |
| `with-columns.json` | Zone recursion: Columns with TextBlock children |
| `edge-cases.json` | XSS injection, empty props, empty zones, Unicode, special chars |

## How the Data Flows

Puck's DropZone-based output puts child components in a `zones` map:

```json
{
  "type": "Columns",
  "props": { "layout": "2/3+1/3" },
  "zones": {
    "zone-0": [{ "type": "TextBlock", "props": { "content": "..." } }],
    "zone-1": [{ "type": "TextBlock", "props": { "content": "..." } }]
  }
}
```

The Tera renderer recursively renders children for each zone and passes the
rendered HTML into the parent template as a `zones` array.

## Notes for Kernel Integration

1. **Sanitize components, not pages.** Ammonia strips `<html>`, `<head>`, `<style>` etc.
   In production, the kernel page wrapper is trusted; only component bodies get sanitized.

2. **Ammonia configuration.** Must allow `class` and `style` generic attributes, and the
   `<section>` tag. The `style` attribute is needed for `background-image` (Hero) and
   `gap` (Columns). Consider restricting allowed CSS properties in production.

3. **Ammonia adds `rel="noopener noreferrer"` to links.** This is correct security behavior
   but means React and Tera output will differ on `<a>` tags. Not a structural issue.

4. **Puck v0.20 still supports DropZone.** The newer Slot-based API is also available but
   DropZone works well for this use case and produces a clean `zones` map in the JSON.

5. **Performance.** ~0.8ms per page (3 components with zone recursion) in release mode.
   Tera template compilation is the bottleneck on first render; subsequent renders are
   effectively instant. No performance concerns for production use.
