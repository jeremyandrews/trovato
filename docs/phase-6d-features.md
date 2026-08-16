# Phase 6D Features Documentation

This document covers the features implemented in Phase 6D of Trovato CMS.

## Overview

Phase 6D adds the following capabilities:
1. Search field configuration UI - Admin interface to configure search field weights
2. Drag-and-drop file uploads - Enhanced UX for file fields
3. Batch API for long operations - Progress polling for background tasks

---

## 1. Search Field Configuration

### User Guide

The search field configuration UI allows administrators to control how content is indexed and ranked in search results.

#### Accessing the Configuration

1. Navigate to **Admin > Structure > Content Types**
2. Select a content type (e.g., "Page")
3. Click the **Search** tab or navigate to `/admin/structure/types/{type}/search`

#### Field Weights

PostgreSQL full-text search uses four weight levels:

| Weight | Meaning | Multiplier |
|--------|---------|------------|
| A | Highest priority | 1.0 |
| B | High priority | 0.4 |
| C | Normal priority | 0.2 |
| D | Low priority | 0.1 |

The **title** field is always indexed with weight A (highest priority).

#### Adding a Field to Search Index

1. On the search configuration page, locate the "Add search field" section
2. Select a field from the dropdown
3. Choose the appropriate weight (A-D)
4. Click "Add"

The field will now be included in the search vector when content is saved.

#### Removing a Field from Search Index

Click the "Delete" button next to any configured field to remove it from the search index.

#### Reindexing Content

After changing search configuration, you may want to reindex existing content:

1. Click the "Reindex" button on the search configuration page
2. This triggers a background reindex of all items of that content type

### API Reference

**Search Configuration Endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| GET | `/admin/structure/types/{type}/search` | View search config |
| POST | `/admin/structure/types/{type}/search/add` | Add field to index |
| POST | `/admin/structure/types/{type}/search/{field}/delete` | Remove field |
| POST | `/admin/structure/types/{type}/search/reindex` | Trigger reindex |

---

## 2. Drag-and-Drop File Uploads

### User Guide

File fields now support drag-and-drop uploads with visual feedback and progress indicators.

#### Using Drag-and-Drop

1. When editing content with a file field, you'll see a dropzone
2. Drag files from your computer onto the dropzone
3. The dropzone highlights when files are dragged over it
4. Drop the files to begin upload
5. A progress bar shows upload status
6. Once complete, thumbnails/previews appear below the dropzone

#### File Restrictions

- **Maximum file size:** 10MB per file
- **Allowed file types:** Images (jpg, png, gif, webp), documents (pdf), and text files

#### Traditional Upload

You can also click the dropzone to open a traditional file picker dialog.

### Developer Reference

The file upload component consists of:

- **Template:** `templates/form/file-upload.html`
- **JavaScript:** `static/js/file-upload.js`
- **Backend:** `POST /api/file/upload` endpoint

#### Including in Custom Templates

```html
{% include "form/file-upload.html" %}
```

The component expects these variables in context:
- `field.field_name` - Field machine name
- `field.cardinality` - Number of allowed files (-1 for unlimited)

---

## 3. Batch API

### Overview

The Batch API allows long-running operations to be executed asynchronously with progress tracking. Operations are stored in Redis with a 24-hour TTL.

### User Guide

Batch operations are used internally for:
- Content reindexing
- Bulk content updates
- Data migrations

Progress can be monitored through the API or admin interface.

### API Reference

#### Create Operation

```http
POST /api/batch
Content-Type: application/json

{
    "operation_type": "reindex",
    "params": {
        "bundle": "page"
    }
}
```

**Response (201 Created):**
```json
{
    "id": "01952a3c-4e5f-7b8a-9c0d-e1f2g3h4i5j6",
    "status": "pending"
}
```

#### Get Operation Status

```http
GET /api/batch/{id}
```

**Response:**
```json
{
    "id": "01952a3c-4e5f-7b8a-9c0d-e1f2g3h4i5j6",
    "operation_type": "reindex",
    "status": "running",
    "progress": {
        "total": 100,
        "processed": 45,
        "percentage": 45,
        "current_operation": "Indexing item 45 of 100"
    },
    "created": 1739000000,
    "updated": 1739000045
}
```

#### Cancel Operation

```http
POST /api/batch/{id}/cancel
```

**Response:** Returns updated operation with `status: "cancelled"`

Only `pending` or `running` operations can be cancelled.

#### Delete Operation

```http
DELETE /api/batch/{id}
```

**Response:** 204 No Content

### Status Values

| Status | Description |
|--------|-------------|
| `pending` | Operation queued, not yet started |
| `running` | Operation in progress |
| `complete` | Operation finished successfully |
| `failed` | Operation encountered an error |
| `cancelled` | Operation was cancelled by user |

### Progress Polling

For UI integration, poll the status endpoint:

```javascript
async function pollBatch(batchId) {
    const response = await fetch(`/api/batch/${batchId}`);
    const data = await response.json();

    if (data.status === 'running' || data.status === 'pending') {
        updateProgressBar(data.progress.percentage);
        setTimeout(() => pollBatch(batchId), 1000);
    } else if (data.status === 'complete') {
        showSuccess(data.result);
    } else if (data.status === 'failed') {
        showError(data.error);
    }
}
```

### Developer Reference

#### Using BatchService

```rust
use trovato_kernel::batch::{BatchService, CreateBatch};

// Create operation
let batch = state.batch().create(CreateBatch {
    operation_type: "my_operation".to_string(),
    params: serde_json::json!({"key": "value"}),
}).await?;

// Update progress
state.batch().update_progress(
    batch.id,
    processed,
    total,
    Some("Processing item X".to_string()),
).await?;

// Complete operation
state.batch().complete(
    batch.id,
    Some(serde_json::json!({"items_processed": 100})),
).await?;

// Or fail operation
state.batch().fail(
    batch.id,
    "Error message",
).await?;
```

#### Error Handling

| HTTP Status | Meaning |
|-------------|---------|
| 201 | Operation created |
| 200 | Operation retrieved/updated |
| 204 | Operation deleted |
| 404 | Operation not found |
| 409 | Cannot cancel (wrong status) |
| 500 | Internal error |

---

## Testing

### Integration Tests

All Phase 6D features have integration tests:

```bash
# Run all Phase 6D tests
cargo test --test integration_test e2e_batch
cargo test --test integration_test e2e_admin_search
cargo test --test integration_test e2e_static

# Run all tests
cargo test --test integration_test
```

### Manual Testing

1. **Search Configuration:**
   - Login as admin
   - Navigate to `/admin/structure/types/page/search`
   - Add/remove fields, verify search results change

2. **File Upload:**
   - Create/edit content with a file field
   - Test drag-and-drop and click-to-upload
   - Verify progress bar and previews

3. **Batch API:**
   - Use curl or browser dev tools to create operations
   - Monitor progress through status endpoint
   - Test cancel and delete

---

## Architecture Notes

### Search Field Configuration

The search configuration is stored in the `search_field_config` table:

```sql
CREATE TABLE search_field_config (
    id UUID PRIMARY KEY,
    bundle VARCHAR(32) NOT NULL,
    field_name VARCHAR(32) NOT NULL,
    weight CHAR(1) NOT NULL DEFAULT 'C',
    UNIQUE (bundle, field_name)
);
```

A PostgreSQL trigger (`item_search_update`) rebuilds the `search_vector` column whenever an item is inserted or updated.

### File Upload

Files are uploaded via multipart form to `/api/file/upload`, stored via the `FileStorage` trait (local or S3), and tracked in the `file_managed` table.

### Batch Operations

Batch operations are stored in Redis with keys prefixed `batch:` and a 24-hour TTL. The `BatchService` provides atomic operations for status updates.

---

## Future Enhancements

- Batch operation UI in admin dashboard
- Websocket notifications for batch progress
- Bulk file upload with queue processing
- Advanced search syntax (field-specific queries)
