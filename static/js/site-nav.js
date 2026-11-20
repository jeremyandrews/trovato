// The site navigation's menu button, below 768px.
//
// The button ships `hidden`, so with JavaScript off it never shows and the nav
// stays open. Here it is shown on narrow screens, starts closed, and toggles
// the nav it names in `aria-controls`, keeping `aria-expanded` true to what is
// on screen. No inline handler: the Content-Security-Policy forbids them.
(function () {
    'use strict';

    var toggle = document.querySelector('.site-nav__toggle');
    if (!toggle) {
        return;
    }
    var nav = document.getElementById(toggle.getAttribute('aria-controls'));
    if (!nav) {
        return;
    }
    var narrow = window.matchMedia('(max-width: 768px)');

    function setOpen(open) {
        toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
        nav.classList.toggle('site-nav--closed', !open);
    }

    function fit() {
        toggle.hidden = !narrow.matches;
        setOpen(!narrow.matches);
    }

    toggle.addEventListener('click', function () {
        setOpen(toggle.getAttribute('aria-expanded') !== 'true');
    });
    narrow.addEventListener('change', fit);
    fit();
})();
