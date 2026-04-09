/**
 * Search page initialization.
 *
 * Reads Scolta configuration from a <script type="application/json"> element
 * with id="scolta-config", then initializes Scolta and hides the server-side
 * fallback.
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
        if (fallback && typeof Scolta !== 'undefined') {
            fallback.style.display = 'none';
            Scolta.init('#scolta-search');
        }
    });
})();
