# Custom Block Types

Trovato's block editor ships with 8 standard block types and supports registering custom types via the `BlockTypeRegistry`.

## Standard Block Types

| Type | Label | Description |
|------|-------|-------------|
| `paragraph` | Paragraph | Rich text paragraph with filtered HTML |
| `heading` | Heading | Section heading, levels 1-6 |
| `image` | Image | Image with caption and alt text |
| `list` | List | Ordered or unordered list |
| `quote` | Quote | Block quote with optional attribution |
| `code` | Code | Code block with syntax highlighting via syntect |
| `delimiter` | Delimiter | Horizontal rule separator |
| `embed` | Embed | Embeddable media (YouTube, Vimeo) |

## Registering a Custom Block Type

Custom block types are registered in plugin startup code or kernel initialization:

```rust
use trovato_kernel::content::{BlockTypeDefinition, BlockTypeRegistry};

let mut registry = BlockTypeRegistry::with_standard_types();

registry.register(BlockTypeDefinition {
    type_name: "callout".to_string(),
    label: "Callout Box".to_string(),
    schema: serde_json::json!({
        "type": "object",
        "properties": {
            "text": { "type": "string" },
            "style": { "type": "string", "enum": ["info", "warning", "error"] }
        },
        "required": ["text", "style"]
    }),
    allowed_formats: vec!["filtered_html".to_string()],
    plugin: "my_plugin".to_string(),
});
```

## Block Data Format

Each block in the Trovato storage format has three fields:

```json
{
    "type": "paragraph",
    "weight": 0,
    "data": {
        "text": "Content goes here."
    }
}
```

- **type**: Machine name matching a registered `BlockTypeDefinition`
- **weight**: Integer for ordering (0, 1, 2, ...)
- **data**: JSON object validated against the block type's schema

## Validation

Call `BlockTypeRegistry::validate_block()` to check block data:

```rust
let errors = registry.validate_block("callout", &data);
if errors.is_empty() {
    // Block is valid
}
```

Validation checks:
- Block type must be registered
- Text fields are checked for disallowed HTML (via ammonia)
- Required fields must be present
- Type-specific rules (e.g., heading level 1-6, image URL non-empty)

## HTML Sanitization

Text-bearing blocks (paragraph, heading, quote, list) are sanitized via `ammonia::clean()` which strips:
- `<script>` tags and event handlers (`onclick`, etc.)
- XSS vectors and dangerous attributes
- Preserves safe formatting: `<b>`, `<i>`, `<a>`, `<p>`, `<strong>`, `<em>`, etc.

Use `BlockTypeRegistry::sanitize_blocks()` for in-place sanitization of an entire block array:

```rust
registry.sanitize_blocks(&mut blocks);
```

## Server-Side Rendering

Custom block types can extend `render_blocks()` in `block_render.rs` by adding a match arm for the custom type name. The renderer converts block JSON to semantic HTML.

## Editor.js Integration

On the client side, register an Editor.js tool class for your custom block in `block-editor.js`'s `buildToolConfig()` function. The JavaScript widget maps between Trovato's `{type, weight, data}` format and Editor.js's `{id, type, data}` format automatically.
