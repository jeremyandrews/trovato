# Story 38.4: Italian Content Seed Data

Status: done

## Story

As a **tutorial reader**,
I want seed data with Italian conference translations,
so that the multilingual features can be demonstrated with realistic bilingual content.

## Acceptance Criteria

1. 15 Italian seed conference YAML files in `docs/tutorial/config/seed-italian/`
2. Each file follows the item config-import schema with id, type, title, language, status, timestamps, and fields
3. Items have `language: it` to indicate Italian source language
4. Multilingual fields use per-language overlay structure (e.g., `field_description.it.value` and `field_description.en.value`)
5. Field values include both Italian and English translations for bilingual display
6. Seed data covers real Italian tech conferences (e.g., Codemotion Roma, PyCon Italia)
7. Files are named with UUID-based item IDs matching the config-import convention

## Tasks / Subtasks

- [x] Create 15 Italian conference seed YAML files (AC: #1)
- [x] Structure each file with item config-import schema (AC: #2)
- [x] Set language: it on all seed items (AC: #3)
- [x] Add per-language field_description with it/en translations (AC: #4, #5)
- [x] Include standard conference fields: dates, city, country, topics (AC: #2)
- [x] Use real Italian conference names and cities (AC: #6)
- [x] Name files as item.{uuid}.yml (AC: #7)

## Dev Notes

### Architecture

The 15 seed files follow the standard Trovato config-import YAML schema used throughout the tutorial. Each file represents a real Italian tech conference with bilingual field values.

The multilingual overlay structure for fields:
```yaml
fields:
  field_description:
    it:
      value: "<p>Italian description...</p>"
      format: filtered_html
    en:
      value: "<p>English description...</p>"
      format: filtered_html
  field_city: Roma
  field_country: IT
```

Non-translatable fields (dates, city, country) store plain values. Translatable fields (description) use the per-language nested structure that the language middleware resolves at render time.

### Testing

- Seed data validated by config-import during tutorial walkthrough
- YAML schema compliance verified by import pipeline

### References

- `docs/tutorial/config/seed-italian/` -- 15 Italian conference seed YAML files
