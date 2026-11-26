/**
 * Admin listings, enhanced. Every listing works without this file: filtering,
 * sorting and paging are GET forms and links, and a destructive action asks
 * on its own confirmation page. This adds:
 *
 * - The select all checkbox in a bulk table: it checks or clears every row,
 *   and shows "mixed" when only some rows are checked.
 * - A polite live count of the selected rows ("Selected: 3").
 * - After a filter or sort, the result count is announced: the server writes
 *   it in the page, and this repeats it in a live region once the page has
 *   loaded, so a screen reader hears what the filter found.
 * - On a narrow screen a table can lay each row out as a block; each cell
 *   gets its column's name as `data-label`, which the stylesheet shows.
 *
 * Words come from the shell's translated table (window.trovatoText).
 */
(function () {
    'use strict';

    var tr = window.trovatoText || function (s, a) {
        return s.replace(/\{([a-z_]+)\}/g, function (m, n) { return a && n in a ? a[n] : m; });
    };

    // Select all, and the selected count.
    document.querySelectorAll('[data-select-all]').forEach(function (all) {
        var scope = all.closest('form') || document;
        var rows = function () {
            return Array.prototype.slice.call(scope.querySelectorAll('input[type=checkbox][data-select-row]'));
        };
        var status = scope.querySelector('[data-selected-count]');
        function update() {
            var boxes = rows();
            var checked = boxes.filter(function (b) { return b.checked; }).length;
            all.checked = boxes.length > 0 && checked === boxes.length;
            all.indeterminate = checked > 0 && checked < boxes.length;
            if (status) status.textContent = checked ? tr('Selected: {count}', { count: checked }) : '';
        }
        all.addEventListener('change', function () {
            rows().forEach(function (b) { b.checked = all.checked; });
            update();
        });
        scope.addEventListener('change', function (e) {
            if (e.target.matches && e.target.matches('[data-select-row]')) update();
        });
        update();
    });

    // Announce the result count after a filter or a sort.
    var count = document.querySelector('[data-list-count]');
    var live = document.getElementById('list-live');
    if (count && live && window.location.search) {
        // A live region announces a change, not what the page loaded with.
        window.setTimeout(function () { live.textContent = count.textContent.trim(); }, 400);
    }

    // Column names for the narrow layout.
    document.querySelectorAll('table.table--stack').forEach(function (table) {
        var heads = Array.prototype.map.call(table.querySelectorAll('thead th'), function (th) {
            // The select all column and the actions column need no name
            // before each cell.
            if (th.querySelector('input') || th.classList.contains('operations')) return '';
            return (th.getAttribute('data-label') || th.textContent || '').trim();
        });
        table.querySelectorAll('tbody tr').forEach(function (tr) {
            Array.prototype.forEach.call(tr.children, function (cell, i) {
                if (heads[i] && !cell.hasAttribute('data-label')) cell.setAttribute('data-label', heads[i]);
            });
        });
        table.classList.add('table--stacked');
    });
})();
