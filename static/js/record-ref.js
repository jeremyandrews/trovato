/**
 * RecordReference autocomplete — searches items by type and title, or
 * category terms by name for a `category_term` field, and sets the hidden
 * UUID field when a result is selected.
 */
(function () {
  'use strict';

  var DEBOUNCE_MS = 250;
  var MIN_CHARS = 2;

  /** Debounce helper. */
  function debounce(fn, ms) {
    var timer;
    return function () {
      var args = arguments;
      var ctx = this;
      clearTimeout(timer);
      timer = setTimeout(function () { fn.apply(ctx, args); }, ms);
    };
  }

  /**
   * The value of the form's `name` input, for a single or multi value field:
   * `name`, else its first value `name[0]`. A disabled (read-only) input
   * still has its value in the page.
   */
  function formValue(input, name) {
    var form = input.form;
    if (!form || !name) return '';
    var el = form.elements[name] || form.elements[name + '[0]'];
    if (!el) return '';
    // A RadioNodeList or several same-named inputs: the first value.
    if (typeof el.value !== 'string' && el.length) el = el[0];
    return (el && typeof el.value === 'string') ? el.value.trim() : '';
  }

  /**
   * The field context a category term search needs: which field of which
   * type is being edited, the item when there is one, and the current value
   * of the owner field when the field is limited to the item's owner. The
   * server decides from these whether this editor may be offered terms, and
   * which.
   */
  function context(input) {
    var params = '';
    var add = function (key, value) {
      if (value) params += '&' + key + '=' + encodeURIComponent(value);
    };
    add('item_type', input.getAttribute('data-item-type'));
    add('field', input.getAttribute('data-field'));
    add('item', input.getAttribute('data-item'));
    add('owner', formValue(input, input.getAttribute('data-owner-field')));
    return params;
  }

  /** Fetch autocomplete results from the API. */
  function fetchResults(targetType, query, input, callback) {
    var url = '/api/v1/items/autocomplete?type=' +
      encodeURIComponent(targetType) +
      '&q=' + encodeURIComponent(query) +
      '&limit=10';
    if (targetType === 'category_term') url += context(input);

    fetch(url, { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (page) { callback((page && page.data) || []); })
      .catch(function () { callback([]); });
  }

  /** Show results dropdown. */
  function showResults(resultsEl, items, hiddenFieldId, inputEl) {
    resultsEl.innerHTML = '';
    if (items.length === 0) {
      resultsEl.style.display = 'none';
      return;
    }

    items.forEach(function (item) {
      var div = document.createElement('div');
      div.className = 'record-ref-result';
      div.textContent = item.title;
      div.setAttribute('data-id', item.id);
      div.addEventListener('mousedown', function (e) {
        e.preventDefault();
        var hidden = document.getElementById(hiddenFieldId);
        if (hidden) hidden.value = item.id;
        inputEl.value = item.title;
        resultsEl.style.display = 'none';
      });
      resultsEl.appendChild(div);
    });

    resultsEl.style.display = 'block';
  }

  /** Initialize autocomplete on all record reference fields. */
  function init() {
    var fields = document.querySelectorAll('.record-ref-autocomplete');
    fields.forEach(function (input) {
      var targetType = input.getAttribute('data-target-type');
      var hiddenFieldId = input.getAttribute('data-hidden-field');
      var resultsEl = document.getElementById(hiddenFieldId + '_results');
      if (!targetType || !resultsEl) return;

      var search = debounce(function () {
        var q = input.value.trim();
        if (q.length < MIN_CHARS) {
          resultsEl.style.display = 'none';
          return;
        }
        fetchResults(targetType, q, input, function (items) {
          showResults(resultsEl, items, hiddenFieldId, input);
        });
      }, DEBOUNCE_MS);

      input.addEventListener('input', function () {
        // Clear the hidden field when user edits (forces re-selection)
        var hidden = document.getElementById(hiddenFieldId);
        if (hidden) hidden.value = '';
        search();
      });

      input.addEventListener('blur', function () {
        // Delay hide to allow mousedown on results
        setTimeout(function () { resultsEl.style.display = 'none'; }, 200);
      });

      input.addEventListener('focus', function () {
        if (input.value.trim().length >= MIN_CHARS) {
          search();
        }
      });
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
