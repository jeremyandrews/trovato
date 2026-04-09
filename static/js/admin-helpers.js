/**
 * Admin helper behaviors — consolidates small inline scripts from admin templates.
 *
 * Provides:
 * - data-confirm click handler (delete confirmation dialogs)
 * - WAI-ARIA tab keyboard navigation
 * - Machine name auto-generation from label fields
 * - Select-all / bulk action form handling
 * - Tile form type field toggling
 */
(function() {
    'use strict';

    // -------------------------------------------------------------------------
    // data-confirm handler: shows a confirm() dialog before following the action
    // -------------------------------------------------------------------------
    document.addEventListener('click', function(e) {
        var btn = e.target.closest('[data-confirm]');
        if (btn) {
            var msg = btn.getAttribute('data-confirm');
            if (!confirm(msg)) {
                e.preventDefault();
            }
        }
    });

    // -------------------------------------------------------------------------
    // WAI-ARIA tab keyboard navigation (Left/Right arrows, Home/End)
    // -------------------------------------------------------------------------
    document.querySelectorAll('[role="tablist"]').forEach(function(tablist) {
        var tabs = Array.from(tablist.querySelectorAll('[role="tab"]'));
        if (tabs.length < 2) return;

        tablist.addEventListener('keydown', function(e) {
            var current = document.activeElement;
            var index = tabs.indexOf(current);
            if (index === -1) return;

            var newIndex = index;
            if (e.key === 'ArrowRight') {
                newIndex = (index + 1) % tabs.length;
            } else if (e.key === 'ArrowLeft') {
                newIndex = (index - 1 + tabs.length) % tabs.length;
            } else if (e.key === 'Home') {
                newIndex = 0;
            } else if (e.key === 'End') {
                newIndex = tabs.length - 1;
            } else {
                return;
            }

            e.preventDefault();
            tabs[index].setAttribute('tabindex', '-1');
            tabs[newIndex].setAttribute('tabindex', '0');
            tabs[newIndex].focus();
        });
    });

    // -------------------------------------------------------------------------
    // Machine name auto-generation from label inputs
    //
    // Attach to any element with data-machine-name-source="<target-id>".
    // The target input gets its value auto-generated until the user edits it
    // manually.  Optional data-machine-name-prefix adds a prefix (e.g. "field_").
    // Optional data-machine-name-max sets the max length (default 32).
    // -------------------------------------------------------------------------
    document.querySelectorAll('[data-machine-name-source]').forEach(function(source) {
        var targetId = source.getAttribute('data-machine-name-source');
        var target = document.getElementById(targetId);
        if (!source || !target) return;

        var prefix = source.getAttribute('data-machine-name-prefix') || '';
        var maxLen = parseInt(source.getAttribute('data-machine-name-max'), 10) || 32;

        source.addEventListener('input', function() {
            if (target.readOnly || target.dataset.userEdited) return;
            target.value = prefix + this.value
                .toLowerCase()
                .replace(/[^a-z0-9]+/g, '_')
                .replace(/^_+|_+$/g, '')
                .substring(0, maxLen - prefix.length);
        });

        target.addEventListener('input', function() {
            this.dataset.userEdited = 'true';
        });
    });

    // -------------------------------------------------------------------------
    // Select-all checkbox + bulk action confirmation
    // -------------------------------------------------------------------------
    var selectAll = document.getElementById('select-all');
    if (selectAll) {
        selectAll.addEventListener('change', function(e) {
            document.querySelectorAll('input[name="ids[]"]').forEach(function(cb) {
                cb.checked = e.target.checked;
            });
        });
    }

    var bulkForm = document.getElementById('bulk-form');
    if (bulkForm) {
        bulkForm.addEventListener('submit', function(e) {
            var action = this.querySelector('[name="action"]').value;
            if (!action) { e.preventDefault(); return; }
            if (action === 'delete' && !confirm('Delete selected items?')) { e.preventDefault(); }
        });
    }

    // -------------------------------------------------------------------------
    // Tile form: toggle visibility of type-specific field groups
    // -------------------------------------------------------------------------
    var tileTypeSelect = document.getElementById('tile_type');
    if (tileTypeSelect) {
        function toggleTypeFields(type) {
            document.querySelectorAll('.tile-type-fields').forEach(function(el) {
                el.style.display = 'none';
            });
            var target = document.getElementById('fields-' + type);
            if (target) target.style.display = 'block';
        }

        toggleTypeFields(tileTypeSelect.value);
        tileTypeSelect.addEventListener('change', function() {
            toggleTypeFields(this.value);
        });
    }
})();
