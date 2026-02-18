# Block Editor Configuration

## Content Type Field Configuration

To use the block editor on a content type field, set the widget to `block_editor` in your content type definition:

```yaml
fields:
  body:
    type: compound
    widget: block_editor
    label: Body
    allowed_block_types:
      - paragraph
      - heading
      - image
      - list
      - quote
      - code
      - delimiter
      - embed
```

An empty `allowed_block_types` list permits all registered block types.

## Editor.js Assets

The block editor requires Editor.js and its tool plugins. These are loaded automatically when a field uses `widget: "block_editor"`. The JavaScript is in `static/js/block-editor.js`.

If Editor.js is not available (e.g., no network for CDN, or vendor files missing), the widget falls back to a raw JSON textarea editor.

## HTML Attributes

The block editor auto-initializes on elements with `data-block-editor`:

```html
<input type="hidden" name="field_body" value='[]'>
<div data-block-editor
     data-block-editor-input="field_body"
     data-block-types="paragraph,heading,image,list"
     data-read-only="false">
</div>
```

| Attribute | Description |
|-----------|-------------|
| `data-block-editor` | Marks the container for auto-initialization |
| `data-block-editor-input` | Name of the hidden input holding serialized JSON |
| `data-block-types` | Comma-separated list of allowed block types (empty = all) |
| `data-read-only` | Set to `"true"` for read-only mode |

## Upload Endpoint

Image blocks use `POST /api/block-editor/upload` for file uploads. The endpoint:
- Requires authentication (returns 403 otherwise)
- Accepts `multipart/form-data` with field name `image` or `file`
- Validates MIME type (JPEG, PNG, GIF, WebP only)
- Enforces the global `MAX_FILE_SIZE` limit
- Returns Editor.js format: `{ "success": 1, "file": { "url": "..." } }`

## Preview Endpoint

Content preview uses `POST /api/block-editor/preview`:
- Requires authentication
- Accepts `{ "blocks": [...] }` JSON body
- Returns `{ "html": "..." }` with server-rendered HTML

## Embed Whitelist

The embed block only renders iframes for whitelisted sources:
- `youtube.com/watch`, `youtube.com/embed/`, `youtu.be/`
- `vimeo.com/`, `player.vimeo.com/`

Non-whitelisted URLs render as safe anchor links.

## Syntax Highlighting

Code blocks with a `language` field use syntect for server-side syntax highlighting. Supported languages include all syntect defaults (Rust, Python, JavaScript, Go, C, C++, Java, Ruby, and many more). Unknown languages fall back to plain text rendering.
