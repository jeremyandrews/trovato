/**
 * File upload widget for admin content forms.
 *
 * Handles AJAX upload to /file/upload with CSRF token,
 * stores returned file UUID in hidden input for form submission.
 */
(function() {
    'use strict';

    var MAX_FILE_SIZE = 10 * 1024 * 1024;

    function initFileUploadWidgets() {
        var widgets = document.querySelectorAll('.file-upload-widget');
        widgets.forEach(initWidget);
    }

    function initWidget(widget) {
        var fieldName = widget.dataset.field;
        var hiddenInput = widget.querySelector('input[type="hidden"]');
        var previewDiv = widget.querySelector('.file-upload-widget__preview');
        var inputDiv = widget.querySelector('.file-upload-widget__input');
        var fileInput = widget.querySelector('.file-upload-widget__file');
        var statusSpan = widget.querySelector('.file-upload-widget__status');
        var removeBtn = widget.querySelector('.file-upload-widget__remove');

        // Get CSRF token from the form
        var form = widget.closest('form');
        var csrfToken = form ? form.querySelector('input[name="_token"]') : null;

        fileInput.addEventListener('change', function(e) {
            if (e.target.files.length === 0) return;
            uploadFile(e.target.files[0]);
            e.target.value = '';
        });

        removeBtn.addEventListener('click', function() {
            hiddenInput.value = '';
            previewDiv.style.display = 'none';
            inputDiv.style.display = '';
            var nameSpan = previewDiv.querySelector('.file-upload-widget__filename');
            if (nameSpan) nameSpan.textContent = '';
            var img = previewDiv.querySelector('.file-upload-widget__image');
            if (img) img.remove();
        });

        function uploadFile(file) {
            if (file.size > MAX_FILE_SIZE) {
                statusSpan.textContent = 'File too large (max 10 MB)';
                statusSpan.className = 'file-upload-widget__status file-upload-widget__status--error';
                return;
            }

            statusSpan.textContent = 'Uploading...';
            statusSpan.className = 'file-upload-widget__status';

            var formData = new FormData();
            formData.append('file', file);

            var xhr = new XMLHttpRequest();
            xhr.open('POST', '/file/upload');
            if (csrfToken) {
                xhr.setRequestHeader('X-CSRF-Token', csrfToken.value);
            }

            xhr.addEventListener('load', function() {
                if (xhr.status === 200) {
                    try {
                        var response = JSON.parse(xhr.responseText);
                        if (response.success && response.file) {
                            onUploadSuccess(response.file);
                        } else {
                            onUploadError(response.error || 'Upload failed');
                        }
                    } catch (e) {
                        onUploadError('Invalid server response');
                    }
                } else {
                    try {
                        var response = JSON.parse(xhr.responseText);
                        onUploadError(response.error || 'Upload failed (' + xhr.status + ')');
                    } catch (e) {
                        onUploadError('Upload failed (' + xhr.status + ')');
                    }
                }
            });

            xhr.addEventListener('error', function() {
                onUploadError('Upload failed. Check your connection.');
            });

            xhr.send(formData);
        }

        function onUploadSuccess(file) {
            hiddenInput.value = file.id;
            statusSpan.textContent = '';

            var nameSpan = previewDiv.querySelector('.file-upload-widget__filename');
            if (nameSpan) {
                nameSpan.textContent = file.filename + ' (' + formatBytes(file.size) + ')';
            }

            // Show image preview for image types
            var existingImg = previewDiv.querySelector('.file-upload-widget__image');
            if (existingImg) existingImg.remove();
            if (file.mime_type && file.mime_type.indexOf('image/') === 0) {
                var img = document.createElement('img');
                img.src = file.url;
                img.alt = file.filename;
                img.className = 'file-upload-widget__image';
                previewDiv.insertBefore(img, previewDiv.firstChild);
            }

            previewDiv.style.display = '';
            inputDiv.style.display = 'none';
        }

        function onUploadError(message) {
            statusSpan.textContent = message;
            statusSpan.className = 'file-upload-widget__status file-upload-widget__status--error';
        }
    }

    function formatBytes(bytes) {
        if (bytes === 0) return '0 B';
        var k = 1024;
        var sizes = ['B', 'KB', 'MB', 'GB'];
        var i = Math.floor(Math.log(bytes) / Math.log(k));
        return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initFileUploadWidgets);
    } else {
        initFileUploadWidgets();
    }
})();
