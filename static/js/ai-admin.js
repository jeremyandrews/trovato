/**
 * AI admin page behaviors.
 *
 * Provides:
 * - Provider form: dynamic model row management (add/remove operation+model pairs)
 * - Provider list: connection test via fetch
 *
 * For the provider form, operations are passed via a <script type="application/json">
 * element with id="ai-operations".
 */
(function() {
    'use strict';

    // -------------------------------------------------------------------------
    // AI provider form: dynamic model rows
    // -------------------------------------------------------------------------
    var container = document.getElementById('model-rows');
    var addBtn = document.getElementById('add-model-row');
    var opsEl = document.getElementById('ai-operations');

    if (container && addBtn && opsEl) {
        var operations = [];
        try { operations = JSON.parse(opsEl.textContent); } catch(e) { /* ignore */ }

        var optionsHtml = '';
        operations.forEach(function(op) {
            optionsHtml += '<option value="' + op.key + '">' + op.label + '</option>';
        });

        function getNextIndex() {
            return container.querySelectorAll('.form-item--inline').length;
        }

        addBtn.addEventListener('click', function() {
            var idx = getNextIndex();
            var row = document.createElement('div');
            row.className = 'form-item form-item--inline';
            row.style.cssText = 'display: flex; gap: 1rem; align-items: center; margin-bottom: 0.5rem;';
            row.innerHTML =
                '<select name="op_' + idx + '" style="flex: 1;"><option value="">\u2014 Select operation \u2014</option>' + optionsHtml + '</select>' +
                '<input type="text" name="model_' + idx + '" value="" placeholder="Model ID (e.g. gpt-4o)" style="flex: 2;">' +
                '<button type="button" class="button button--small button--danger js-remove-row" title="Remove">&times;</button>';
            container.appendChild(row);
        });

        container.addEventListener('click', function(e) {
            if (e.target.classList.contains('js-remove-row')) {
                var row = e.target.closest('.form-item--inline');
                if (container.querySelectorAll('.form-item--inline').length > 1) {
                    row.remove();
                }
            }
        });
    }

    // -------------------------------------------------------------------------
    // AI provider list: connection test
    // -------------------------------------------------------------------------
    document.querySelectorAll('.js-test-connection').forEach(function(form) {
        form.addEventListener('submit', function(e) {
            e.preventDefault();
            var resultDiv = document.getElementById('test-result');
            var msgEl = document.getElementById('test-message');
            var latEl = document.getElementById('test-latency');
            msgEl.textContent = 'Testing connection...';
            latEl.textContent = '';
            resultDiv.style.display = 'block';

            fetch(form.action, {
                method: 'POST',
                body: new FormData(form)
            })
            .then(function(r) { return r.json(); })
            .then(function(data) {
                msgEl.textContent = (data.success ? '\u2705 ' : '\u274c ') + data.message;
                latEl.textContent = 'Latency: ' + data.latency_ms + 'ms';
                msgEl.style.color = data.success ? 'var(--success, green)' : 'var(--danger, red)';
            })
            .catch(function(err) {
                msgEl.textContent = '\u274c Request failed: ' + err;
                msgEl.style.color = 'var(--danger, red)';
            });
        });
    });
})();
