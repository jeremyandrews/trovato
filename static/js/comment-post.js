/**
 * Comment posting, as a progressive enhancement.
 *
 * The form works without this file: it posts form-encoded with the CSRF token in
 * a hidden `_csrf` input, and the route redirects back to the item. What this
 * adds is posting JSON with the `X-CSRF-Token` header instead, so the page does
 * not reload and the reader is told what happened to their comment in place —
 * including the case where the site holds comments for review.
 *
 * On any failure it falls back to submitting the form normally, so a reader is
 * never left with a comment that went nowhere.
 */
(function () {
    'use strict';

    var form = document.querySelector('[data-comment-form]');
    if (!form || !window.fetch) {
        return;
    }

    var status = form.querySelector('[data-comment-status]');
    var meta = document.querySelector('meta[name="csrf-token"]');
    var token = meta ? meta.getAttribute('content') : null;

    // Without a token there is nothing to send in the header, so leave the plain
    // form submission alone.
    if (!token) {
        return;
    }

    function say(message) {
        if (status) {
            status.textContent = message;
        }
    }

    form.addEventListener('submit', function (event) {
        var body = form.querySelector('#comment-body');
        var parent = form.querySelector('#comment-parent-id');
        if (!body || !body.value.trim()) {
            return; // Let the browser's own required-field handling run.
        }

        event.preventDefault();

        var payload = { body: body.value };
        if (parent && parent.value) {
            payload.parent_id = parent.value;
        }

        var button = form.querySelector('button[type="submit"]');
        if (button) {
            button.disabled = true;
        }
        say('Posting…');

        fetch(form.getAttribute('action'), {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'X-CSRF-Token': token
            },
            body: JSON.stringify(payload),
            credentials: 'same-origin'
        })
            .then(function (response) {
                if (!response.ok) {
                    throw new Error('request failed with ' + response.status);
                }
                return response.json();
            })
            .then(function (comment) {
                // 2 is CommentStatus::Pending — the comment is stored but held.
                if (comment && comment.status === 2) {
                    form.reset();
                    say('Your comment was submitted and is awaiting review.');
                    return;
                }
                // Published: reload so the new comment appears in thread order
                // rather than being positioned by guesswork here.
                window.location.reload();
            })
            .catch(function () {
                // Hand the submission back to the browser rather than losing it.
                say('');
                if (button) {
                    button.disabled = false;
                }
                form.submit();
            });
    });
})();
