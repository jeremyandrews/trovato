/**
 * RecordReference autocomplete — searches items by type and title,
 * sets the hidden UUID field when a result is selected.
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

  /** Fetch autocomplete results from the API. */
  function fetchResults(targetType, query, callback) {
    var url = '/api/v1/items/autocomplete?type=' +
      encodeURIComponent(targetType) +
      '&q=' + encodeURIComponent(query) +
      '&limit=10';

    fetch(url, { credentials: 'same-origin' })
      .then(function (r) { return r.json(); })
      .then(callback)
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
        fetchResults(targetType, q, function (items) {
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
