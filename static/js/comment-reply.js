/**
 * Comment reply handling.
 *
 * Manages reply-to links on comment threads: sets the parent ID,
 * shows the "replying to" indicator, and focuses the comment textarea.
 * The fields are found by name inside the comment form (`[data-comment-form]`),
 * which the kernel's form renderer draws.
 */
(function() {
    'use strict';

    var form = document.querySelector('[data-comment-form]');
    if (!form) {
        return;
    }
    var parent = form.querySelector('[name="parent_id"]');
    var body = form.querySelector('[name="body"]');
    var replying = document.getElementById('replying-to');
    var replyParent = document.getElementById('reply-parent-id');

    document.querySelectorAll('.comment__reply-link').forEach(function(link) {
        link.addEventListener('click', function(e) {
            e.preventDefault();
            var parentId = this.getAttribute('data-parent-id');
            parent.value = parentId;
            if (replyParent) {
                replyParent.textContent = '#' + parentId.substring(0, 8);
            }
            if (replying) {
                replying.hidden = false;
            }
            body.focus();
        });
    });

    // Bound here, not inline: the kernel's CSP blocks inline event handlers.
    document.querySelectorAll('.cancel-reply').forEach(function(button) {
        button.addEventListener('click', function() {
            parent.value = '';
            if (replying) {
                replying.hidden = true;
            }
        });
    });
})();
