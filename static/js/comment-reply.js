/**
 * Comment reply handling.
 *
 * Manages reply-to links on comment threads: sets the parent ID,
 * shows the "replying to" indicator, and focuses the comment textarea.
 */
(function() {
    'use strict';

    document.querySelectorAll('.comment__reply-link').forEach(function(link) {
        link.addEventListener('click', function(e) {
            e.preventDefault();
            var parentId = this.getAttribute('data-parent-id');
            document.getElementById('comment-parent-id').value = parentId;
            document.getElementById('reply-parent-id').textContent = '#' + parentId.substring(0, 8);
            document.getElementById('replying-to').style.display = 'inline';
            document.getElementById('comment-body').focus();
        });
    });

    // Expose cancelReply globally for the inline onclick handler
    window.cancelReply = function() {
        document.getElementById('comment-parent-id').value = '';
        document.getElementById('replying-to').style.display = 'none';
    };
})();
