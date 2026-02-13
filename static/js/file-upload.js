/**
 * Drag-and-drop file upload handler.
 *
 * Usage: Include this script and it will automatically enhance
 * all .file-upload__dropzone elements on the page.
 */
(function() {
    'use strict';

    // Maximum file size (10MB default)
    const MAX_FILE_SIZE = 10 * 1024 * 1024;

    // Initialize all file upload components
    function initFileUploads() {
        const dropzones = document.querySelectorAll('.file-upload__dropzone');
        dropzones.forEach(initDropzone);
    }

    function initDropzone(dropzone) {
        const fieldName = dropzone.dataset.field;
        const wrapper = dropzone.closest('.file-upload');
        const input = dropzone.querySelector('input[type="file"]');
        const progress = wrapper.querySelector('.file-upload__progress');
        const progressBar = wrapper.querySelector('.file-upload__progress-fill');
        const progressText = wrapper.querySelector('.file-upload__progress-text');
        const filesList = wrapper.querySelector('.file-upload__files');

        // Drag events
        ['dragenter', 'dragover'].forEach(eventName => {
            dropzone.addEventListener(eventName, (e) => {
                e.preventDefault();
                e.stopPropagation();
                dropzone.classList.add('dragover');
            });
        });

        ['dragleave', 'drop'].forEach(eventName => {
            dropzone.addEventListener(eventName, (e) => {
                e.preventDefault();
                e.stopPropagation();
                dropzone.classList.remove('dragover');
            });
        });

        // Handle file drop
        dropzone.addEventListener('drop', (e) => {
            const files = e.dataTransfer.files;
            handleFiles(files);
        });

        // Handle file selection via input
        input.addEventListener('change', (e) => {
            handleFiles(e.target.files);
            input.value = ''; // Reset input to allow re-selecting same file
        });

        // Handle file removal
        filesList.addEventListener('click', (e) => {
            if (e.target.classList.contains('file-upload__remove')) {
                const fileId = e.target.dataset.fileId;
                const fileElement = e.target.closest('.file-upload__file');
                if (fileElement) {
                    fileElement.remove();
                }
            }
        });

        function handleFiles(files) {
            Array.from(files).forEach(uploadFile);
        }

        function uploadFile(file) {
            // Validate file size
            if (file.size > MAX_FILE_SIZE) {
                showError(`File "${file.name}" is too large. Maximum size is ${formatBytes(MAX_FILE_SIZE)}.`);
                return;
            }

            // Show progress
            progress.style.display = 'block';
            progressBar.style.width = '0%';
            progressText.textContent = `Uploading ${file.name}...`;

            // Create FormData
            const formData = new FormData();
            formData.append('file', file);

            // Upload via XHR (for progress tracking)
            const xhr = new XMLHttpRequest();

            xhr.upload.addEventListener('progress', (e) => {
                if (e.lengthComputable) {
                    const percent = Math.round((e.loaded / e.total) * 100);
                    progressBar.style.width = percent + '%';
                    progressText.textContent = `Uploading ${file.name}... ${percent}%`;
                }
            });

            xhr.addEventListener('load', () => {
                progress.style.display = 'none';

                if (xhr.status === 200) {
                    try {
                        const response = JSON.parse(xhr.responseText);
                        if (response.success && response.file) {
                            addFileToList(response.file);
                        } else {
                            showError(response.error || 'Upload failed');
                        }
                    } catch (e) {
                        showError('Invalid server response');
                    }
                } else {
                    try {
                        const response = JSON.parse(xhr.responseText);
                        showError(response.error || `Upload failed (${xhr.status})`);
                    } catch (e) {
                        showError(`Upload failed (${xhr.status})`);
                    }
                }
            });

            xhr.addEventListener('error', () => {
                progress.style.display = 'none';
                showError('Upload failed. Please check your connection.');
            });

            xhr.open('POST', '/file/upload');
            xhr.send(formData);
        }

        function addFileToList(file) {
            const isImage = file.mime_type && file.mime_type.startsWith('image/');

            const fileElement = document.createElement('div');
            fileElement.className = 'file-upload__file';
            fileElement.dataset.fileId = file.id;

            fileElement.innerHTML = `
                <input type="hidden" name="${fieldName}_files[]" value="${file.id}">
                ${isImage ?
                    `<img src="${file.url}" alt="${escapeHtml(file.filename)}" class="file-upload__preview">` :
                    `<div class="file-upload__file-icon">
                        <svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5">
                            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/>
                            <polyline points="14 2 14 8 20 8"/>
                        </svg>
                    </div>`
                }
                <div class="file-upload__file-info">
                    <span class="file-upload__file-name">${escapeHtml(file.filename)}</span>
                    <span class="file-upload__file-size">${formatBytes(file.size)}</span>
                </div>
                <button type="button" class="file-upload__remove" data-file-id="${file.id}" title="Remove file">&times;</button>
            `;

            filesList.appendChild(fileElement);
        }

        function showError(message) {
            // Remove any existing error
            const existingError = wrapper.querySelector('.file-upload__error');
            if (existingError) {
                existingError.remove();
            }

            const errorElement = document.createElement('div');
            errorElement.className = 'file-upload__error';
            errorElement.textContent = message;

            // Insert after dropzone
            dropzone.insertAdjacentElement('afterend', errorElement);

            // Auto-remove after 5 seconds
            setTimeout(() => {
                errorElement.remove();
            }, 5000);
        }
    }

    function formatBytes(bytes) {
        if (bytes === 0) return '0 B';
        const k = 1024;
        const sizes = ['B', 'KB', 'MB', 'GB'];
        const i = Math.floor(Math.log(bytes) / Math.log(k));
        return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
    }

    function escapeHtml(text) {
        const div = document.createElement('div');
        div.textContent = text;
        return div.innerHTML;
    }

    // Initialize on DOM ready
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initFileUploads);
    } else {
        initFileUploads();
    }

    // Re-initialize when new content is added (for AJAX)
    window.initFileUploads = initFileUploads;
})();
