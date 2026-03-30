# Story 40.3: Form Accessibility -- Error Association

Status: ready-for-dev

## Story

As a **screen reader user** filling out a form,
I want validation errors programmatically associated with their input fields,
so that when I focus a field with an error, my screen reader reads the error message.

## Acceptance Criteria

1. Form API generates `id="error-{field_name}"` on error message elements
2. Form API adds `aria-describedby="error-{field_name}"` on the corresponding input element when that field has a validation error
3. `aria-invalid="true"` set on input elements that failed validation
4. Error messages rendered in a `<div role="alert">` or equivalent live region so screen readers announce errors on form submission
5. Form templates (`templates/form/form-element.html`, `templates/form/form.html`) updated
6. Admin forms inherit these changes automatically (they use the Form API)
7. Existing CSRF error display follows the same pattern

## Tasks / Subtasks

- [ ] Add `id="error-{field_name}"` to error message elements in form templates (AC: #1)
  - [ ] Update `templates/form/form-element.html` error rendering
- [ ] Add `aria-describedby="error-{field_name}"` to input elements with errors (AC: #2)
  - [ ] Modify form render logic in `crates/kernel/src/form/` to inject attribute
  - [ ] Ensure attribute only added when field has a validation error
- [ ] Add `aria-invalid="true"` to failed input elements (AC: #3)
  - [ ] Modify form element rendering to conditionally add `aria-invalid`
- [ ] Wrap error messages in `<div role="alert">` (AC: #4)
  - [ ] Update `templates/form/form-element.html` error wrapper
  - [ ] Update `templates/form/form.html` for form-level errors
- [ ] Update form templates (AC: #5)
  - [ ] `templates/form/form-element.html` -- per-field error association
  - [ ] `templates/form/form.html` -- form-level error display
- [ ] Verify admin forms inherit changes automatically (AC: #6)
  - [ ] Admin forms use the Form API -- confirm no template overrides break the pattern
- [ ] Update CSRF error display to follow the same pattern (AC: #7)
  - [ ] Verify CSRF error rendering in form templates uses `role="alert"`

## Dev Notes

### Architecture
- `crates/kernel/src/form/` -- form render logic that generates HTML attributes on elements
- `templates/form/form-element.html` -- per-field template with label, input, and error display
- `templates/form/form.html` -- form wrapper template with form-level errors
- The Form API already tracks per-field errors -- this story connects them to the DOM via ARIA attributes

### Security
- Field names used in `id` and `aria-describedby` attributes must be safe for HTML attribute values
- Use `html_escape()` on field names if they could contain special characters (they should already be machine names)

### Testing
- Render a form with validation errors, verify `aria-describedby` and `aria-invalid` present in HTML output
- Verify `id="error-{field_name}"` matches `aria-describedby` value
- Verify `role="alert"` on error message containers
- Screen reader testing: focus a field with an error, confirm error message is announced

### References
- [Source: docs/ritrovo/epic-10-accessibility.md] -- Epic 40 definition
- [Source: crates/kernel/src/form/] -- Form API render logic
- [Source: templates/form/form-element.html] -- Form element template
- [Source: templates/form/form.html] -- Form wrapper template
