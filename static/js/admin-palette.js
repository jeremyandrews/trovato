/**
 * The administration's command palette. Without this file the banner's
 * search form leads to /admin/search, which lists the same results; this
 * adds a quicker way to reach them:
 *
 * - The banner's Quick search button opens a modal dialog (<dialog>, shown
 *   with showModal, so the rest of the page is inert while it is open).
 * - The viewer's shortcuts open it too (Your preferences): "/" when focus
 *   is not in a text field, unless they turned it off, and optionally
 *   Control+K (Command+K) or Alt+K (Option+K), which work anywhere.
 * - The dialog holds an editable combobox (ARIA 1.2): typing asks
 *   /admin/search/suggest, which runs the same search as the results page,
 *   and fills a listbox of options grouped under their headings. The input
 *   keeps focus; aria-activedescendant names the active option.
 * - Up and Down move through the options (wrapping), Enter opens the
 *   active one, or the full results page when none is active. Escape
 *   closes the dialog and focus returns to whatever opened it.
 * - The number of results is announced in a polite live region.
 *
 * Words come from the shell's translated table (window.trovatoText); the
 * results arrive already in the viewer's language.
 */
(function () {
    'use strict';

    var dialog = document.getElementById('admin-palette');
    var opener = document.querySelector('.admin-palette-open');
    if (!dialog || !opener || typeof dialog.showModal !== 'function' || !window.fetch) return;

    var tr = window.trovatoText || function (s) { return s; };
    var form = dialog.querySelector('form');
    var input = document.getElementById('admin-palette-q');
    var listbox = document.getElementById('admin-palette-results');
    var status = document.getElementById('admin-palette-count');

    var options = [];
    var active = -1;
    var timer = null;
    var asked = 0;
    var returnTo = null;

    opener.hidden = false;

    function open(from) {
        if (dialog.open) {
            input.focus();
            return;
        }
        returnTo = from || document.activeElement;
        dialog.showModal();
        input.focus();
        input.select();
        if (input.value.trim() && !options.length) ask();
    }

    dialog.addEventListener('close', function () {
        clearTimeout(timer);
        if (returnTo && typeof returnTo.focus === 'function' && document.contains(returnTo)) {
            returnTo.focus();
        } else {
            opener.focus();
        }
        returnTo = null;
    });

    opener.addEventListener('click', function () { open(opener); });

    dialog.querySelectorAll('[data-palette-close]').forEach(function (button) {
        button.addEventListener('click', function () { dialog.close(); });
    });

    // A click on the backdrop (outside the dialog's box) closes it.
    dialog.addEventListener('click', function (event) {
        if (event.target === dialog) dialog.close();
    });

    // Shortcuts.
    var slash = opener.getAttribute('data-search-slash') !== 'off';
    var key = opener.getAttribute('data-search-key') || '';

    function typing(el) {
        if (!el || el === document.body) return false;
        if (el.isContentEditable) return true;
        var tag = el.tagName;
        if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
        if (tag !== 'INPUT') return false;
        var type = (el.getAttribute('type') || 'text').toLowerCase();
        return ['checkbox', 'radio', 'button', 'submit', 'reset', 'image', 'file', 'range', 'color'].indexOf(type) === -1;
    }

    document.addEventListener('keydown', function (event) {
        if (event.defaultPrevented || event.isComposing) return;
        var isK = event.code === 'KeyK' || (event.key && event.key.toLowerCase() === 'k');
        if (key === 'ctrl-k' && isK && (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey) {
            event.preventDefault();
            open();
            return;
        }
        if (key === 'alt-k' && isK && event.code === 'KeyK' && event.altKey && !event.ctrlKey && !event.metaKey) {
            event.preventDefault();
            open();
            return;
        }
        if (slash && event.key === '/' && !event.ctrlKey && !event.metaKey && !event.altKey
            && !dialog.open && !typing(event.target)) {
            event.preventDefault();
            open();
        }
    });

    // The combobox.
    function setActive(index) {
        if (active >= 0 && options[active]) options[active].setAttribute('aria-selected', 'false');
        active = index;
        if (active >= 0 && options[active]) {
            var option = options[active];
            option.setAttribute('aria-selected', 'true');
            input.setAttribute('aria-activedescendant', option.id);
            if (typeof option.scrollIntoView === 'function') option.scrollIntoView({ block: 'nearest' });
        } else {
            input.removeAttribute('aria-activedescendant');
        }
    }

    function clear() {
        listbox.textContent = '';
        listbox.hidden = true;
        options = [];
        setActive(-1);
        input.setAttribute('aria-expanded', 'false');
    }

    function go(option) {
        var href = option && option.getAttribute('data-href');
        if (href) window.location.assign(href);
    }

    function render(data) {
        clear();
        var n = 0;
        (data.groups || []).forEach(function (group) {
            if (!group.hits || !group.hits.length) return;
            var box = document.createElement('div');
            box.setAttribute('role', 'group');
            box.className = 'admin-palette__group';
            var heading = document.createElement('div');
            heading.setAttribute('role', 'presentation');
            heading.className = 'admin-palette__heading';
            heading.id = 'admin-palette-group-' + group.key;
            heading.textContent = group.title;
            box.setAttribute('aria-labelledby', heading.id);
            box.appendChild(heading);
            group.hits.forEach(function (hit) {
                var option = document.createElement('div');
                option.setAttribute('role', 'option');
                option.setAttribute('aria-selected', 'false');
                option.id = 'admin-palette-option-' + n;
                option.className = 'admin-palette__option';
                option.setAttribute('data-href', hit.href);
                var label = document.createElement('span');
                label.className = 'admin-palette__option-label';
                label.textContent = hit.label;
                option.appendChild(label);
                if (hit.context) {
                    var context = document.createElement('span');
                    context.className = 'admin-palette__option-context';
                    context.textContent = hit.context;
                    option.appendChild(document.createTextNode(' '));
                    option.appendChild(context);
                }
                box.appendChild(option);
                options.push(option);
                n += 1;
            });
            listbox.appendChild(box);
        });
        if (options.length) {
            listbox.hidden = false;
            input.setAttribute('aria-expanded', 'true');
            status.textContent = data.summary || '';
        } else {
            status.textContent = tr('No results. Press Enter to search every page.');
        }
    }

    function ask() {
        clearTimeout(timer);
        var q = input.value.trim();
        if (!q) {
            asked += 1;
            clear();
            status.textContent = '';
            return;
        }
        timer = setTimeout(function () {
            asked += 1;
            var mine = asked;
            fetch('/admin/search/suggest?q=' + encodeURIComponent(q), {
                credentials: 'same-origin',
                headers: { Accept: 'application/json' }
            }).then(function (response) {
                if (!response.ok) throw new Error(String(response.status));
                return response.json();
            }).then(function (data) {
                if (mine === asked && dialog.open) render(data);
            }).catch(function () {
                if (mine !== asked) return;
                clear();
                status.textContent = tr('The search could not be run.');
            });
        }, 150);
    }

    input.addEventListener('input', ask);

    input.addEventListener('keydown', function (event) {
        if (event.isComposing) return;
        switch (event.key) {
            case 'ArrowDown':
                if (!options.length) return;
                event.preventDefault();
                setActive(active + 1 >= options.length ? 0 : active + 1);
                break;
            case 'ArrowUp':
                if (!options.length) return;
                event.preventDefault();
                setActive(active - 1 < 0 ? options.length - 1 : active - 1);
                break;
            case 'Enter':
                if (active >= 0 && options[active]) {
                    event.preventDefault();
                    go(options[active]);
                }
                // Otherwise the form submits: the full results page.
                break;
            default:
                break;
        }
    });

    // A pointer chooses an option without taking focus from the input.
    listbox.addEventListener('mousedown', function (event) { event.preventDefault(); });
    listbox.addEventListener('click', function (event) {
        var option = event.target.closest('[role=option]');
        if (option) go(option);
    });

})();
