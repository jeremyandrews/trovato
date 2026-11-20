/**
 * Conditional fields: show a field only while another field of the same form
 * has a given value.
 *
 * The kernel form builder wraps a field whose definition has `visible_when`
 * in an element carrying `data-visible-when-field` (the controlling field's
 * name) and `data-visible-when-equals` (the value that shows it), already
 * `hidden` when the stored values make the condition false. This keeps that
 * up to date as the editor types. The server applies the same rule on save:
 * a hidden field is neither saved nor required.
 */
(function () {
  'use strict';

  /** The controlling field's value, as the form would post it. */
  function valueOf(form, name) {
    var inputs = form.querySelectorAll('[name="' + name + '"]');
    for (var i = 0; i < inputs.length; i++) {
      var input = inputs[i];
      if (input.type === 'checkbox') {
        return input.checked ? input.value : '';
      }
      if (input.type === 'radio') {
        if (input.checked) return input.value;
        continue;
      }
      return input.value;
    }
    return '';
  }

  function update(form) {
    var wrappers = form.querySelectorAll('[data-visible-when-field]');
    for (var i = 0; i < wrappers.length; i++) {
      var wrapper = wrappers[i];
      var shown = valueOf(form, wrapper.getAttribute('data-visible-when-field')) ===
        wrapper.getAttribute('data-visible-when-equals');
      wrapper.hidden = !shown;
    }
  }

  function init() {
    var forms = document.querySelectorAll('form');
    for (var i = 0; i < forms.length; i++) {
      var form = forms[i];
      if (!form.querySelector('[data-visible-when-field]')) continue;
      update(form);
      form.addEventListener('change', update.bind(null, form));
      form.addEventListener('input', update.bind(null, form));
    }
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
