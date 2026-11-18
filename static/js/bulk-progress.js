/**
 * Bulk action progress on the content list (B68).
 *
 * The list renders #bulk-progress with the batch id when a bulk action was
 * started as a queued batch. This polls GET /api/batch/{id} once a second and
 * shows how far it has got, then the finished message or the reason it
 * stopped. A file rather than an inline script, for the Content-Security-Policy.
 */
(function() {
    'use strict';

    var panel = document.getElementById('bulk-progress');
    if (!panel) { return; }
    var id = panel.getAttribute('data-batch');
    var label = panel.querySelector('.bulk-progress__label');
    var bar = panel.querySelector('.bulk-progress__bar');
    var done = panel.querySelector('.bulk-progress__done');

    function finish(text) {
        label.textContent = text;
        done.hidden = false;
    }

    function poll() {
        fetch('/api/batch/' + encodeURIComponent(id), {
            credentials: 'same-origin',
            headers: { 'Accept': 'application/json' }
        }).then(function(response) {
            if (!response.ok) { throw new Error('HTTP ' + response.status); }
            return response.json();
        }).then(function(batch) {
            var progress = batch.progress || {};
            bar.value = progress.percentage || 0;
            if (batch.status === 'complete') {
                bar.value = 100;
                finish((batch.result && batch.result.message) || 'Done.');
                return;
            }
            if (batch.status === 'failed') {
                finish(batch.error || 'The action stopped.');
                return;
            }
            if (batch.status === 'cancelled') {
                finish('The action was cancelled.');
                return;
            }
            label.textContent = progress.total
                ? 'Processed ' + progress.processed + ' of ' + progress.total + ' items (' + (progress.percentage || 0) + '%)'
                : 'Queued…';
            window.setTimeout(poll, 1000);
        }).catch(function() {
            finish('Progress could not be read. Refresh the list to see the result.');
        });
    }

    poll();
})();
