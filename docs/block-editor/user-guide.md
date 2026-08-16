# Block Editor User Guide

## Overview

The block editor provides a structured content editing experience where content is composed of discrete blocks (paragraphs, headings, images, etc.) rather than a single rich text area. Each block has its own type and configuration.

## Adding Content

Click the **+** button or start typing in the placeholder area to add your first block. The default block type is a paragraph.

To add a different block type, click **+** and select from the available types in the toolbar menu.

## Block Types

### Paragraph
Standard text content. Supports inline formatting (bold, italic, links).

### Heading
Section headings at levels 2, 3, or 4. Use headings to structure your content hierarchically.

### Image
Upload an image or provide a URL. Add a caption for accessibility and context. Supported formats: JPEG, PNG, GIF, WebP.

### List
Ordered (numbered) or unordered (bulleted) lists. Each item supports inline formatting.

### Quote
Block quotation with optional attribution/caption.

### Code
Code blocks with optional language specification for syntax highlighting. Specify the language (e.g., "rust", "python", "javascript") for colored syntax output.

### Delimiter
A horizontal rule to visually separate content sections.

### Embed
Embed videos from YouTube or Vimeo by pasting the video URL. Other URLs are rendered as links for safety.

## Reordering Blocks

Drag blocks up or down to reorder them. The weight (position) is automatically recalculated on save.

## Previewing Content

Click the **Preview** button to see a server-rendered preview of your content. The preview uses the same rendering pipeline as the published page.

## Read-Only Mode

Fields can be set to read-only mode via `data-read-only="true"`. In this mode, content is displayed but cannot be edited — useful for revision history views.

## Data Format

Block content is stored as a JSON array in the hidden form input. Each block has:
- `type`: The block type name
- `weight`: Sort order (integer)
- `data`: Block-specific content

Example:
```json
[
    { "type": "heading", "weight": 0, "data": { "text": "Welcome", "level": 2 } },
    { "type": "paragraph", "weight": 1, "data": { "text": "Hello, world!" } },
    { "type": "image", "weight": 2, "data": { "file": { "url": "/files/photo.jpg" }, "caption": "A photo" } }
]
```

## Fallback Mode

If the Editor.js library is not loaded, the block editor falls back to a raw JSON textarea. You can edit the JSON directly — invalid JSON is indicated with a red border.
