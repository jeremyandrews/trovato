# Story 38.3: ritrovo_translate Plugin

Status: done

## Story

As a **content editor**,
I want automatic language detection for imported conferences and language badges on conference pages,
so that Italian-language conferences are identified for translation and visitors can switch between languages.

## Acceptance Criteria

1. `tap_item_insert` detects Italian content using marker-word heuristic and returns detected language
2. Italian detection uses a list of common Italian articles, prepositions, and function words
3. Detection requires at least 3 marker matches to avoid false positives on short text
4. Detection result includes `detected_language` and `translation_status` ("needs_translation" or "translated")
5. `tap_item_view` renders a language badge (e.g., "English" or "Italiano") on conference pages
6. `tap_item_view` renders a language switcher link to view the conference in the other language
7. Both taps only process conference items (empty return for other types)

## Tasks / Subtasks

- [x] Define ITALIAN_MARKERS constant with common Italian function words (AC: #2)
- [x] Implement is_likely_italian() heuristic with 3-match threshold (AC: #2, #3)
- [x] Implement tap_item_insert for language detection on conference creation (AC: #1, #4)
- [x] Return JSON with detected_language and translation_status (AC: #4)
- [x] Implement tap_item_view with language badge rendering (AC: #5)
- [x] Add language switcher links between /conferences/{id} and /it/conferences/{id} (AC: #6)
- [x] Early return for non-conference items in both taps (AC: #7)
- [x] Write unit tests for detection and rendering (AC: #1-#7)

## Dev Notes

### Architecture

The ritrovo_translate plugin (238 lines including tests) implements language detection and display:

- **Detection** (`tap_item_insert`): Checks title + description against `ITALIAN_MARKERS` (50+ common Italian words padded with spaces for whole-word matching). Threshold of 3 matches balances precision vs. recall. Returns JSON payload for the kernel to process.
- **Display** (`tap_item_view`): Renders a `<div class="lang-badge-switcher">` with a `<span class="lang-badge">` showing the detected language and an `<a class="lang-switcher__link">` pointing to the alternate-language URL.

The heuristic approach was chosen over a full NLP library to keep the WASM plugin small. It works well for the Italian/English binary classification needed for the conference use case. The combined text (title + description) gives enough signal for accurate detection.

### Testing

8 unit tests: Italian text detection, English text detection, short text handling, insert Italian detection, insert English detection, insert ignores non-conference, view shows language badge, view empty for non-conference.

### References

- `plugins/ritrovo_translate/src/lib.rs` (238 lines) -- Full plugin implementation with tests
