/**
 * Admin forms: progressive enhancement of the pattern in
 * docs/admin/design.md section 8. Every form works without this file: the
 * server validates, the error summary's links are plain anchors, and the
 * action bar is simply where the form ends.
 *
 * - A failed submit: the error summary takes focus, so a screen reader
 *   announces it first. Its links move focus to their field and scroll the
 *   field's label (or a group's legend) into view with it.
 * - A completed action: focus moves to the status message the page shows.
 * - A long form: its action bar stays in view (sticky), and the page's
 *   scroll padding is set to the bar's height so a focused field is never
 *   hidden behind it (WCAG 2.4.11).
 * - An unsaved changes warning, only while the form differs from what was
 *   loaded; never after the form is submitted.
 *
 * Nothing validates as you type or on blur.
 */
(function () {
    'use strict';

    var main = document.getElementById('main');
    if (!main) return;

    // --- Error summary -----------------------------------------------------
    var summary = main.querySelector('[data-error-summary]');
    if (summary) {
        summary.focus();
        summary.addEventListener('click', function (e) {
            var link = e.target.closest('a[href^="#"]');
            if (!link) return;
            var field = document.getElementById(link.getAttribute('href').slice(1));
            if (!field) return;
            e.preventDefault();
            var group = field.closest('fieldset');
            var label = document.querySelector('label[for="' + field.id + '"]');
            var anchor = (field.type === 'radio' || field.type === 'checkbox') && group
                ? group.querySelector('legend') || group
                : label || field;
            anchor.scrollIntoView({ block: 'start' });
            field.focus({ preventScroll: true });
        });
    } else {
        // --- A completed action: focus its status message ------------------
        var notice = main.querySelector('.admin-messages .notice');
        if (notice) {
            notice.setAttribute('tabindex', '-1');
            notice.focus();
        }
    }

    // --- Long forms: a sticky action bar -----------------------------------
    var root = document.documentElement;
    function measure(bar) {
        root.style.setProperty('--action-bar-height', bar.getBoundingClientRect().height + 'px');
    }
    main.querySelectorAll('form').forEach(function (form) {
        var bar = form.querySelector(':scope > .form-actions, :scope > div > .form-actions');
        if (!bar || bar.classList.contains('form-actions--sticky')) {
            if (bar) measure(bar);
            return;
        }
        if (form.getBoundingClientRect().height > window.innerHeight * 1.25) {
            bar.classList.add('form-actions--sticky');
            measure(bar);
        }
    });
    window.addEventListener('resize', function () {
        var bar = main.querySelector('.form-actions--sticky');
        if (bar) measure(bar);
    });

    // --- Unsaved changes ---------------------------------------------------
    // Only forms built from the field pattern (they hold .form-item), not a
    // listing's bulk checkboxes.
    function snapshot(form) {
        var parts = [];
        Array.prototype.forEach.call(form.elements, function (el) {
            if (!el.name || el.type === 'hidden' || el.type === 'submit' || el.type === 'button') return;
            if (el.type === 'checkbox' || el.type === 'radio') {
                parts.push(el.name + '=' + (el.checked ? el.value : ''));
            } else if (el.type === 'file') {
                parts.push(el.name + '=' + (el.files ? el.files.length : 0));
            } else {
                parts.push(el.name + '=' + el.value);
            }
        });
        return parts.join('&');
    }
    var watched = [];
    main.querySelectorAll('form[method="post"], form[method="POST"]').forEach(function (form) {
        if (!form.querySelector('.form-item') || form.hasAttribute('data-no-unsaved-warning')) return;
        var entry = { form: form, initial: snapshot(form), submitted: false };
        form.addEventListener('submit', function () { entry.submitted = true; });
        watched.push(entry);
    });
    if (watched.length) {
        window.addEventListener('beforeunload', function (e) {
            var dirty = watched.some(function (w) {
                return !w.submitted && snapshot(w.form) !== w.initial;
            });
            if (dirty) {
                e.preventDefault();
                // Older browsers show their own words; the text is not used.
                e.returnValue = '';
            }
        });
    }
})();
