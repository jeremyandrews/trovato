# Story 36.2: WYSIWYG Editor for Rich Text Fields

Status: done

## Story

As a **content editor**,
I want a block-based WYSIWYG editor for rich text fields,
so that I can compose structured content with paragraphs, headings, images, lists, and code blocks without writing raw HTML.

## Acceptance Criteria

1. `trovato_block_editor` plugin provides the `"use block editor"` permission and gates editor routes
2. `BlockTypeRegistry` registers 8 standard block types: paragraph, heading, image, list, quote, code, delimiter, embed
3. Each block type has a JSON Schema definition and allowed text formats
4. `render_blocks()` converts Editor.js JSON arrays into semantic HTML
5. Block text content is sanitized via ammonia before rendering
6. Code blocks use `syntect` for server-side syntax highlighting
7. Embed blocks validate URLs against a whitelist before rendering iframes
8. `FieldType::Blocks` fields use the block editor widget in content forms

## Tasks / Subtasks

- [x] Create block_editor plugin with tap_perm for "use block editor" permission (AC: #1)
- [x] Define BlockTypeDefinition and BlockTypeRegistry in `content/block_types.rs` (AC: #2, #3)
- [x] Register 8 standard block types with schemas (AC: #2)
- [x] Implement render_blocks() dispatcher in `content/block_render.rs` (AC: #4)
- [x] Implement per-block renderers: paragraph, heading, image, list, quote, code, delimiter, embed (AC: #4)
- [x] Add ammonia sanitization for text content in blocks (AC: #5)
- [x] Integrate syntect for code block syntax highlighting with lazy-loaded resources (AC: #6)
- [x] Implement URL whitelist validation for embed blocks (AC: #7)
- [x] Wire Blocks field type to block editor widget in form builder (AC: #8)

## Dev Notes

### Architecture

The block editor is split between a minimal WASM plugin (feature flag + permission) and kernel infrastructure:
- **Plugin** (`plugins/block_editor/src/lib.rs`): Only 23 lines. Provides the `"use block editor"` permission via `tap_perm`. All heavy lifting is in the kernel.
- **Registry** (`content/block_types.rs`): `BlockTypeRegistry` with `with_standard_types()` factory. Each `BlockTypeDefinition` carries a JSON Schema for client-side validation and `allowed_formats` for sanitization.
- **Renderer** (`content/block_render.rs`): `render_blocks()` dispatches to per-type renderers. Uses `ammonia::clean()` for HTML sanitization. `syntect` SyntaxSet and ThemeSet are loaded via `LazyLock` statics to avoid per-call overhead. Embed URLs validated with `is_safe_url()`.

The `render_blocks` function is also exposed as a Tera filter for use in templates.

### Testing

- Block rendering tested via unit tests in block_render.rs
- BlockTypeRegistry tested via unit tests in block_types.rs
- Plugin permission count verified in plugin unit tests

### References

- `plugins/block_editor/src/lib.rs` (23 lines) -- Plugin permission definition
- `crates/kernel/src/content/block_types.rs` -- BlockTypeRegistry and definitions
- `crates/kernel/src/content/block_render.rs` -- Server-side block-to-HTML rendering
