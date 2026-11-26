/**
 * Reference autocomplete — one script for both reference widgets: a
 * RecordReference field searches items by type and title, or category terms
 * by name for a `category_term` field; a CategoryReference field (the tag
 * picker) searches the tags of one category. The input names its endpoint in
 * `data-autocomplete-url` (the target type or category is appended), shows the
 * chosen result's label, and writes its uuid into the hidden field the form
 * posts.
 *
 * Listeners are delegated from the document, so a slot added by "Add another"
 * after the page loaded works the same as the ones it was rendered with.
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
   *
   * `owner` is sent whenever the field has an owner field at all, blank or
   * not: its absence and its emptiness mean different things to the server,
   * which cannot otherwise tell "the owner field is empty" from "this
   * request says nothing about an owner."
   */
  function context(input) {
    var params = '';
    var add = function (key, value) {
      if (value) params += '&' + key + '=' + encodeURIComponent(value);
    };
    add('item_type', input.getAttribute('data-item-type'));
    add('field', input.getAttribute('data-field'));
    add('item', input.getAttribute('data-item'));
    var ownerField = input.getAttribute('data-owner-field');
    if (ownerField) {
      params += '&owner=' + encodeURIComponent(formValue(input, ownerField));
    }
    return params;
  }

  /** Whether `input` is limited to an owner field that is currently empty. */
  function ownerFieldEmpty(input) {
    var ownerField = input.getAttribute('data-owner-field');
    return !!ownerField && !formValue(input, ownerField);
  }

  var DEFAULT_URL = '/api/v1/items/autocomplete?type=';
  var SELECTOR = '.record-ref-autocomplete';

  /** Fetch autocomplete results from the input's endpoint: the item
   * endpoint answers a page (`{data: [...]}`), the tag endpoint a list. */
  function fetchResults(targetType, query, input, callback) {
    var url = (input.getAttribute('data-autocomplete-url') || DEFAULT_URL) +
      encodeURIComponent(targetType) +
      '&q=' + encodeURIComponent(query) +
      '&limit=10';
    if (targetType === 'category_term') url += context(input);

    fetch(url, { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(function (page) {
        callback(Array.isArray(page) ? page : ((page && page.data) || []));
      })
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

  /** Show a short hint in place of results, e.g. while an owner field a term
   * search depends on is empty. */
  function showHint(resultsEl, text) {
    resultsEl.innerHTML = '';
    var div = document.createElement('div');
    div.className = 'record-ref-hint';
    div.textContent = text;
    resultsEl.appendChild(div);
    resultsEl.style.display = 'block';
  }

  /** The pieces an input works with, or null when it is not complete. */
  function partsOf(input) {
    var targetType = input.getAttribute('data-target-type');
    var hiddenFieldId = input.getAttribute('data-hidden-field');
    var resultsEl = document.getElementById(hiddenFieldId + '_results');
    if (!targetType || !resultsEl) return null;
    return { targetType: targetType, hiddenFieldId: hiddenFieldId, resultsEl: resultsEl };
  }

  function search(input) {
    var parts = partsOf(input);
    if (!parts) return;
    if (ownerFieldEmpty(input)) {
      showHint(parts.resultsEl, input.getAttribute('data-owner-hint') || 'Choose the owner first');
      return;
    }
    var q = input.value.trim();
    if (q.length < MIN_CHARS) {
      parts.resultsEl.style.display = 'none';
      return;
    }
    fetchResults(parts.targetType, q, input, function (items) {
      // A slower answer for an older query must not replace a newer one.
      if (input.value.trim() === q) {
        showResults(parts.resultsEl, items, parts.hiddenFieldId, input);
      }
    });
  }

  /** `search`, debounced per input. */
  function debounced(input) {
    if (!input._recordRefSearch) {
      input._recordRefSearch = debounce(function () { search(input); }, DEBOUNCE_MS);
    }
    input._recordRefSearch();
  }

  function target(e) {
    return e.target && e.target.closest ? e.target.closest(SELECTOR) : null;
  }

  document.addEventListener('input', function (e) {
    var input = target(e);
    if (!input) return;
    // Clear the hidden field when user edits (forces re-selection)
    var hidden = document.getElementById(input.getAttribute('data-hidden-field'));
    if (hidden) hidden.value = '';
    debounced(input);
  });

  document.addEventListener('focusin', function (e) {
    var input = target(e);
    if (input && (ownerFieldEmpty(input) || input.value.trim().length >= MIN_CHARS)) {
      debounced(input);
    }
  });

  document.addEventListener('focusout', function (e) {
    var input = target(e);
    var parts = input && partsOf(input);
    if (!parts) return;
    // Delay hide to allow mousedown on results
    setTimeout(function () { parts.resultsEl.style.display = 'none'; }, 200);
  });
})();
