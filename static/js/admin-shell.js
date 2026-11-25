/**
 * The administration shell's progressive enhancement. Everything here is an
 * addition: with this file not loaded, every admin page still works, the
 * sidebar is simply always open and shown first on a narrow screen.
 *
 * - Marks <html> with `js` at once (this file is loaded in the head, not
 *   deferred), so the sidebar renders in its remembered state with no flash.
 * - Sidebar groups: each heading's disclosure button (aria-expanded)
 *   shows and hides its list; the collapsed set is saved for the viewer.
 * - Narrow screens: the Menu button (aria-expanded) shows and hides the
 *   sidebar. Escape closes it and returns focus to the button.
 *
 * Nothing opens on hover.
 */
(function () {
    'use strict';

    var root = document.documentElement;
    root.classList.remove('no-js');
    root.classList.add('js');

    function saveCollapsed(nav) {
        var collapsed = [];
        nav.querySelectorAll('.admin-nav__group').forEach(function (group) {
            var button = group.querySelector('.admin-nav__toggle');
            if (button && button.getAttribute('aria-expanded') === 'false') {
                collapsed.push(group.getAttribute('data-group'));
            }
        });
        // Cosmetic state only; a failure leaves the page as it is.
        try {
            fetch('/admin/preferences/nav', {
                method: 'POST',
                credentials: 'same-origin',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ collapsed: collapsed })
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
                saveCollapsed(nav);
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
