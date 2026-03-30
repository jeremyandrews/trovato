# Story 44.4: Default Lazy Loading on Images

Status: ready-for-dev

## Story

As a **site visitor on a slow connection**,
I want images below the fold to load lazily,
so that initial page load is fast and bandwidth is conserved.

## Acceptance Criteria

1. The render pipeline adds `loading="lazy"` as the default attribute on all `<img>` tags
2. The `block--image` template includes `loading="lazy"` on its `<img>` element
3. Image render fallback paths include `loading="lazy"`
4. The first `<img>` on a page gets `loading="eager"` to avoid delaying the Largest Contentful Paint (LCP)
5. Heuristic: first `<img>` in rendered output is eager, all subsequent images are lazy
6. Plugin `ElementBuilder` images default to `loading="lazy"` (overridable by the plugin)
7. Existing templates (e.g., ritrovo `all_speakers`) verified to not duplicate the `loading` attribute
8. At least 1 integration test verifying lazy/eager attribute assignment

## Tasks / Subtasks

- [ ] Update render pipeline in `crates/kernel/src/theme/render.rs` to inject `loading="lazy"` on `<img>` tags that lack a `loading` attribute (AC: #1)
- [ ] Implement first-image heuristic: track whether the first `<img>` has been emitted and set `loading="eager"` on it (AC: #4, #5)
- [ ] Update `templates/elements/block--image.html` to include `loading="lazy"` (AC: #2)
- [ ] Audit image render fallback paths for missing `loading` attribute (AC: #3)
- [ ] Update `ElementBuilder` to set `loading="lazy"` as default for image elements, allowing plugin override (AC: #6)
- [ ] Audit `all_speakers` and other existing templates — verify no duplicate `loading` attributes (AC: #7)
- [ ] Write integration test: render a page with multiple images, verify first is eager and rest are lazy (AC: #8)

## Dev Notes

### Architecture

The first-image heuristic operates at the page render level. A render context flag (`first_image_emitted: bool`) tracks whether the LCP-candidate image has been rendered. The render pipeline checks this flag when processing `<img>` elements:

- If `first_image_emitted` is false and the image has no explicit `loading` attribute, set `loading="eager"` and flip the flag.
- For all subsequent images without an explicit `loading` attribute, set `loading="lazy"`.

This keeps the logic centralized so individual templates and plugins get correct behavior automatically.

### Testing

- Render a page containing 3+ images. Assert the first `<img>` has `loading="eager"` and the remaining have `loading="lazy"`.
- Test that a plugin-provided image element with an explicit `loading="eager"` is not overridden.
- Verify the `block--image` template output includes the attribute.

### References

- `crates/kernel/src/theme/render.rs` — render pipeline
- `templates/elements/block--image.html` — image block template
- Web Vitals LCP guidance on `loading="eager"` for above-the-fold images
