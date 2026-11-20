/**
 * Reference autocomplete — one script for both reference widgets: a
 * RecordReference field searches items by type and title, a
 * CategoryReference field (the tag picker) searches the tags of one category.
 * The input names its endpoint in `data-autocomplete-url` (the target type or
 * category is appended), shows the chosen result's label, and writes its uuid
 * into the hidden field the form posts.
 *
 * Listeners are delegated from the document, so a slot added by "Add another"
 * after the page loaded works the same as the ones it was rendered with.
 */
(function () {
  'use strict';

  var DEBOUNCE_MS = 250;
  var MIN_CHARS = 2;
  var DEFAULT_URL = '/api/v1/items/autocomplete?type=';
  var SELECTOR = '.record-ref-autocomplete';

  /** Fetch autocomplete results from the input's endpoint. */
  function fetchResults(input, query, callback) {
    var base = input.getAttribute('data-autocomplete-url') || DEFAULT_URL;
    var url = base +
      encodeURIComponent(input.getAttribute('data-target-type') || '') +
      '&q=' + encodeURIComponent(query) +
      '&limit=10';

    fetch(url, { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(callback)
      .catch(function () { callback([]); });
  }

  /** The results box and hidden field an input belongs to. */
  function partsOf(input) {
    var hiddenFieldId = input.getAttribute('data-hidden-field');
    return {
      hidden: document.getElementById(hiddenFieldId),
      results: document.getElementById(hiddenFieldId + '_results')
    };
  }

  /** Show results dropdown. */
  function showResults(input, items) {
    var parts = partsOf(input);
    var resultsEl = parts.results;
    if (!resultsEl) return;
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
        var hidden = partsOf(input).hidden;
        if (hidden) hidden.value = item.id;
        input.value = item.title;
        resultsEl.style.display = 'none';
      });
      resultsEl.appendChild(div);
    });

    resultsEl.style.display = 'block';
  }

  function search(input) {
    var resultsEl = partsOf(input).results;
    if (!resultsEl) return;
    var q = input.value.trim();
    if (q.length < MIN_CHARS) {
      resultsEl.style.display = 'none';
      return;
    }
    fetchResults(input, q, function (items) {
      // A slower answer for an older query must not replace a newer one.
      if (input.value.trim() === q) showResults(input, items);
    });
  }

  function debounced(input) {
    clearTimeout(input._recordRefTimer);
    input._recordRefTimer = setTimeout(function () { search(input); }, DEBOUNCE_MS);
  }

  function target(e) {
    return e.target && e.target.closest ? e.target.closest(SELECTOR) : null;
  }

  document.addEventListener('input', function (e) {
    var input = target(e);
    if (!input) return;
    // Clear the hidden field when the user edits (forces re-selection).
    var hidden = partsOf(input).hidden;
    if (hidden) hidden.value = '';
    debounced(input);
  });

  document.addEventListener('focusin', function (e) {
    var input = target(e);
    if (input && input.value.trim().length >= MIN_CHARS) debounced(input);
  });

  document.addEventListener('focusout', function (e) {
    var input = target(e);
    if (!input) return;
    // Delay hide to allow mousedown on results.
    var resultsEl = partsOf(input).results;
    if (resultsEl) setTimeout(function () { resultsEl.style.display = 'none'; }, 200);
  });
})();
