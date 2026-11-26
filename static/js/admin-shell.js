/**
 * The administration shell's progressive enhancement. Everything here is an
 * addition: with this file not loaded, every admin page still works, the
 * sidebar is simply always open and shown first on a narrow screen.
 *
 * - Marks <html> with `js` at once (this file is loaded in the head, not
 *   deferred), so the sidebar renders as the server drew it with no flash:
 *   the current page's group open, and any the viewer opened by hand.
 * - Sidebar groups: each heading's disclosure button (aria-expanded)
 *   shows and hides its list. Opening or closing a group other than the
 *   current page's is saved for the viewer, one group per request.
 * - Narrow screens: the Menu button (aria-expanded) shows and hides the
 *   sidebar. Escape closes it and returns focus to the button.
 *
 * - window.trovatoText: the admin's script words in the viewer's language.
 *
 * Nothing opens on hover.
 */
(function () {
    'use strict';

    var root = document.documentElement;
    root.classList.remove('no-js');
    root.classList.add('js');

    // The admin's script words, translated by the shell (<template
    // id="admin-text">): trovatoText('Close') is the viewer's language, and
    // the English source where there is no table. {name} placeholders are
    // filled from args.
    var table = null;
    window.trovatoText = function (source, args) {
        if (table === null) {
            table = {};
            var tpl = document.getElementById('admin-text');
            if (tpl && tpl.content) {
                tpl.content.querySelectorAll('[data-s]').forEach(function (el) {
                    table[el.getAttribute('data-s')] = el.textContent;
                });
            }
        }
        var text = Object.prototype.hasOwnProperty.call(table, source) ? table[source] : source;
        return text.replace(/\{([a-z_]+)\}/g, function (m, name) {
            return args && Object.prototype.hasOwnProperty.call(args, name) ? String(args[name]) : m;
        });
    };

    // Remember one group the viewer opened or closed by hand. The group
    // holding the current page (data-current) is open on every page, so its
    // state is never saved. Cosmetic state only; a failure leaves the page
    // as it is.
    function saveGroup(group, open) {
        if (!group || group.hasAttribute('data-current')) return;
        try {
            fetch('/admin/preferences/nav', {
                method: 'POST',
                credentials: 'same-origin',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ group: group.getAttribute('data-group'), open: open })
            }).catch(function () {});
        } catch (e) { /* older browser: not remembered */ }
    }

    function initSidebar() {
        var nav = document.getElementById('admin-nav');
        if (!nav) return;

        nav.querySelectorAll('.admin-nav__toggle').forEach(function (button) {
            var list = document.getElementById(button.getAttribute('aria-controls'));
            if (!list) return;
            button.hidden = false;
            button.addEventListener('click', function () {
                var open = button.getAttribute('aria-expanded') === 'true';
                button.setAttribute('aria-expanded', open ? 'false' : 'true');
                if (open) {
                    list.setAttribute('data-collapsed', '');
                } else {
                    list.removeAttribute('data-collapsed');
                }
                saveGroup(button.closest('.admin-nav__group'), !open);
            });
        });

        var toggle = document.querySelector('.admin-menu-toggle');
        if (!toggle) return;
        toggle.hidden = false;
        function setOpen(open) {
            toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
            nav.classList.toggle('is-open', open);
        }
        toggle.addEventListener('click', function () {
            setOpen(toggle.getAttribute('aria-expanded') !== 'true');
        });
        nav.addEventListener('keydown', function (e) {
            if (e.key === 'Escape' && toggle.getAttribute('aria-expanded') === 'true'
                && window.getComputedStyle(toggle).display !== 'none') {
                setOpen(false);
                toggle.focus();
            }
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initSidebar);
    } else {
        initSidebar();
    }
})();
