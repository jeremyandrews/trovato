# Story 38.1: Multilingual Content Model

Status: done

## Story

As a **site builder**,
I want the content model to support multiple languages with per-language field translations,
so that conferences can be displayed in the visitor's preferred language.

## Acceptance Criteria

1. Language model with fields: id (BCP 47 code), label, weight, is_default, direction (ltr/rtl)
2. Language CRUD operations with validation: ID format (2-3 char primary subtag with optional hyphen-separated subtags), label length (1-255), direction ("ltr" or "rtl")
3. `item.language` column tracks the source language of each content item
4. Field translation overlay: multilingual fields store per-language values (e.g., `field_description.it.value`)
5. Default language is "en" for monolingual sites; additional languages added by plugins
6. Language list/find operations for runtime negotiation
7. Direction support for RTL scripts (Arabic, Hebrew, etc.)

## Tasks / Subtasks

- [x] Define Language and CreateLanguage models (AC: #1)
- [x] Implement BCP 47 language ID validation (2-12 chars, lowercase alpha primary subtag) (AC: #2)
- [x] Implement label validation (non-empty, max 255 chars) (AC: #2)
- [x] Implement direction validation ("ltr" or "rtl" only) (AC: #2)
- [x] Implement Language CRUD: create, find_by_id, list, update, delete (AC: #1, #6)
- [x] Add language column to item table (AC: #3)
- [x] Support per-language field values in JSONB fields (AC: #4)
- [x] Set "en" as default language (AC: #5)
- [x] Add language table migration

## Dev Notes

### Architecture

The language model (`models/language.rs`, 424 lines) provides the foundation for multilingual content:

- **Validation**: Three-layer validation before any DB operation: `validate_language_id()` (BCP 47 format), `validate_label()` (non-empty, max 255 chars), `validate_direction()` ("ltr" or "rtl").
- **Default language**: The `is_default` flag ensures exactly one language is the site default. Setting a new default automatically clears the flag on all other languages.
- **Content overlay**: Multilingual fields use a nested JSONB structure where each language code maps to a value object. For example, `field_description: { "it": { "value": "...", "format": "..." }, "en": { "value": "...", "format": "..." } }`. Non-multilingual fields store values directly.
- **Item.language**: Nullable column. When set, indicates the source language. When null, the item is in the default language.

The model uses `sqlx::FromRow` for direct mapping and standard async CRUD patterns with `PgPool`.

### Testing

- Language CRUD tested via model integration tests
- Validation tested for all error cases (invalid ID, empty label, bad direction)
- Multilingual field overlay tested through content rendering

### References

- `crates/kernel/src/models/language.rs` (424 lines) -- Language model with validation and CRUD
