/**
 * Search page initialization.
 *
 * Reads Scolta configuration from a <script type="application/json"> element
 * with id="scolta-config", then initializes Scolta. The server-rendered
 * results (#search-fallback) are hidden only once Scolta reports that the
 * Pagefind index has loaded: with no index built, or Pagefind failing to
 * load, the visitor keeps the results the server already found (B160).
 */
(function() {
    'use strict';

    var configEl = document.getElementById('scolta-config');
    if (configEl) {
        try {
            window.scolta = JSON.parse(configEl.textContent);
        } catch(e) {
            // Fall back to server-side search if config is invalid
        }
    }

    document.addEventListener('DOMContentLoaded', function() {
        var fallback = document.getElementById('search-fallback');
        if (!fallback || typeof Scolta === 'undefined') {
            return;
        }
        var ready = Scolta.init('#scolta-search');
        if (ready && typeof ready.then === 'function') {
            ready.then(function() {
                fallback.hidden = true;
            }, function() {
                // Pagefind is unavailable: the server results stay the page.
            });
        }
    });
})();
