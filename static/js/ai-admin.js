/**
 * AI admin page behaviors.
 *
 * Provides:
 * - Provider form: dynamic model row management (add/remove operation+model pairs)
 * - Provider list: connection test via fetch
 *
 * For the provider form, operations are passed via a <script type="application/json">
 * element with id="ai-operations". Every word this script puts on the page
 * (both sections) comes from the translated <script type="application/json">
 * element with id="ai-i18n" (B253): none of it is a literal here, so the
 * admin JS i18n guard has nothing to catch on this file.
 */
(function() {
    'use strict';

    function readJson(id) {
        var el = document.getElementById(id);
        if (!el) return null;
        try { return JSON.parse(el.textContent); } catch (e) { return null; }
    }

    var i18n = readJson('ai-i18n') || {};

    /** Replace each `{name}` in `template` with `values[name]`. */
    function fillTemplate(template, values) {
        return template.replace(/\{(\w+)\}/g, function(match, name) {
            return Object.prototype.hasOwnProperty.call(values, name) ? values[name] : match;
        });
    }

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
                '<select name="op_' + idx + '" style="flex: 1;"><option value="">' + i18n.select_operation + '</option>' + optionsHtml + '</select>' +
                '<input type="text" name="model_' + idx + '" value="" placeholder="' + i18n.model_id_placeholder + '" style="flex: 2;">' +
                '<button type="button" class="button button--small button--danger js-remove-row" title="' + i18n.remove_title + '">&times;</button>';
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
    // Each Test button belongs to the list's one test form (form=) and posts
    // to its own provider's address (formaction).
    document.querySelectorAll('.js-test-connection').forEach(function(button) {
        button.addEventListener('click', function(e) {
            var form = button.form;
            if (!form) return;
            e.preventDefault();
            var resultDiv = document.getElementById('test-result');
            var msgEl = document.getElementById('test-message');
            var latEl = document.getElementById('test-latency');
            msgEl.textContent = i18n.testing_connection;
            latEl.textContent = '';
            resultDiv.style.display = 'block';

            fetch(button.formAction, {
                method: 'POST',
                body: new FormData(form)
            })
            .then(function(r) { return r.json(); })
            .then(function(data) {
                msgEl.textContent = (data.success ? '\u2705 ' : '\u274c ') + data.message;
                latEl.textContent = fillTemplate(i18n.latency_template, { ms: data.latency_ms });
                msgEl.style.color = data.success ? 'var(--success, green)' : 'var(--danger, red)';
            })
            .catch(function(err) {
                msgEl.textContent = '\u274c ' + fillTemplate(i18n.request_failed_template, { error: err });
                msgEl.style.color = 'var(--danger, red)';
            });
        });
    });
})();
