/**
 * Media Picker — modal component for browsing and selecting media files.
 *
 * Usage:
 *   MediaPicker.open({
 *     onSelect: function(media) { console.log(media.id, media.url); },
 *     type: 'image',  // optional: filter by type
 *   });
 *
 * Requires authentication (uses session cookies automatically).
 */
(function() {
  'use strict';

  var modal = null;
  var callback = null;
  var currentPage = 1;
  var currentType = '';
  var currentSearch = '';

  function getCSRF() {
    var meta = document.querySelector('meta[name="csrf-token"]');
    if (meta) return meta.content;
    var input = document.querySelector('input[name="_token"]');
    if (input) return input.value;
    return '';
  }

  function createModal() {
    if (modal) return;

    var overlay = document.createElement('div');
    overlay.className = 'media-picker-overlay';
    overlay.innerHTML = [
      '<div class="media-picker-modal">',
      '  <div class="media-picker-header">',
      '    <h3>Select Media</h3>',
      '    <button class="media-picker-close" aria-label="Close">&times;</button>',
      '  </div>',
      '  <div class="media-picker-tabs">',
      '    <button class="media-picker-tab media-picker-tab--active" data-tab="browse">Browse Existing</button>',
      '    <button class="media-picker-tab" data-tab="upload">Upload New</button>',
      '  </div>',
      '  <div class="media-picker-toolbar">',
      '    <select class="media-picker-type-filter">',
      '      <option value="">All types</option>',
      '      <option value="image">Images</option>',
      '      <option value="document">Documents</option>',
      '    </select>',
      '    <input type="text" class="media-picker-search" placeholder="Search files...">',
      '  </div>',
      '  <div class="media-picker-content">',
      '    <div class="media-picker-grid" id="media-picker-grid"></div>',
      '    <div class="media-picker-upload-zone" id="media-picker-upload" style="display:none;">',
      '      <div class="media-picker-dropzone">',
      '        <p>Drag files here or click to upload</p>',
      '        <input type="file" class="media-picker-file-input" multiple>',
      '      </div>',
      '    </div>',
      '  </div>',
      '  <div class="media-picker-footer">',
      '    <button class="media-picker-load-more" style="display:none;">Load more</button>',
      '    <div class="media-picker-selected-info"></div>',
      '    <button class="media-picker-insert button button--primary" disabled>Insert</button>',
      '  </div>',
      '</div>'
    ].join('\n');

    document.body.appendChild(overlay);
    modal = overlay;

    // Close button
    overlay.querySelector('.media-picker-close').addEventListener('click', close);

    // Click outside modal to close
    overlay.addEventListener('click', function(e) {
      if (e.target === overlay) close();
    });

    // Escape key to close
    document.addEventListener('keydown', function(e) {
      if (e.key === 'Escape' && modal && modal.style.display !== 'none') {
        close();
      }
    });

    // Tab switching
    overlay.querySelectorAll('.media-picker-tab').forEach(function(tab) {
      tab.addEventListener('click', function() {
        overlay.querySelectorAll('.media-picker-tab').forEach(function(t) {
          t.classList.remove('media-picker-tab--active');
        });
        tab.classList.add('media-picker-tab--active');
        var tabName = tab.getAttribute('data-tab');
        document.getElementById('media-picker-grid').style.display =
          tabName === 'browse' ? '' : 'none';
        document.getElementById('media-picker-upload').style.display =
          tabName === 'upload' ? '' : 'none';
        overlay.querySelector('.media-picker-toolbar').style.display =
          tabName === 'browse' ? '' : 'none';
      });
    });

    // Type filter
    overlay.querySelector('.media-picker-type-filter').addEventListener('change', function() {
      currentType = this.value;
      currentPage = 1;
      loadMedia(true);
    });

    // Search with debounce
    var searchTimeout;
    overlay.querySelector('.media-picker-search').addEventListener('input', function() {
      clearTimeout(searchTimeout);
      var val = this.value;
      searchTimeout = setTimeout(function() {
        currentSearch = val;
        currentPage = 1;
        loadMedia(true);
      }, 300);
    });

    // Load more
    overlay.querySelector('.media-picker-load-more').addEventListener('click', function() {
      currentPage++;
      loadMedia(false);
    });

    // Insert selected
    overlay.querySelector('.media-picker-insert').addEventListener('click', function() {
      var selected = overlay.querySelector('.media-picker-item--selected');
      if (selected && callback) {
        callback({
          id: selected.dataset.id,
          url: selected.dataset.url,
          filename: selected.dataset.filename,
          thumbnail_url: selected.dataset.thumbnail || selected.dataset.url,
          mime_type: selected.dataset.mime,
        });
      }
      close();
    });

    // Upload: dropzone and file input
    var dropzone = overlay.querySelector('.media-picker-dropzone');
    var fileInput = overlay.querySelector('.media-picker-file-input');

    dropzone.addEventListener('click', function() { fileInput.click(); });
    dropzone.addEventListener('dragover', function(e) {
      e.preventDefault();
      dropzone.classList.add('media-picker-dropzone--active');
    });
    dropzone.addEventListener('dragleave', function() {
      dropzone.classList.remove('media-picker-dropzone--active');
    });
    dropzone.addEventListener('drop', function(e) {
      e.preventDefault();
      dropzone.classList.remove('media-picker-dropzone--active');
      uploadFiles(e.dataTransfer.files);
    });
    fileInput.addEventListener('change', function() {
      uploadFiles(this.files);
      this.value = '';
    });
  }

  function loadMedia(replace) {
    var grid = document.getElementById('media-picker-grid');
    if (replace) {
      grid.innerHTML =
        '<p style="text-align:center;padding:2rem;color:#999;">Loading...</p>';
    }

    var params = new URLSearchParams();
    params.set('page', currentPage);
    params.set('page_size', '24');
    if (currentType) params.set('type', currentType);
    if (currentSearch) params.set('q', currentSearch);

    fetch('/api/v1/media/browse?' + params.toString())
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (replace) grid.innerHTML = '';

        if (data.items.length === 0 && currentPage === 1) {
          grid.innerHTML =
            '<p style="text-align:center;padding:2rem;color:#999;">No media found.</p>';
          return;
        }

        data.items.forEach(function(item) {
          var card = document.createElement('div');
          card.className = 'media-picker-item';
          card.dataset.id = item.id;
          card.dataset.url = item.url;
          card.dataset.filename = item.filename;
          card.dataset.thumbnail = item.thumbnail_url || '';
          card.dataset.mime = item.mime_type;

          if (item.mime_type && item.mime_type.indexOf('image/') === 0 && item.thumbnail_url) {
            card.innerHTML =
              '<img src="' + escapeAttr(item.thumbnail_url) + '" alt="' +
              escapeAttr(item.filename) + '" loading="lazy">' +
              '<div class="media-picker-item__name">' +
              escapeHtml(item.filename) + '</div>';
          } else {
            var ext = item.filename.split('.').pop().toUpperCase();
            card.innerHTML =
              '<div class="media-picker-item__icon">' + escapeHtml(ext) + '</div>' +
              '<div class="media-picker-item__name">' + escapeHtml(item.filename) + '</div>';
          }

          card.addEventListener('click', function() {
            grid.querySelectorAll('.media-picker-item--selected').forEach(function(s) {
              s.classList.remove('media-picker-item--selected');
            });
            card.classList.add('media-picker-item--selected');
            modal.querySelector('.media-picker-insert').disabled = false;
            modal.querySelector('.media-picker-selected-info').textContent = item.filename;
          });

          grid.appendChild(card);
        });

        // Show/hide "Load more" button
        var loadMore = modal.querySelector('.media-picker-load-more');
        loadMore.style.display = data.items.length >= 24 ? '' : 'none';
      })
      .catch(function() {
        if (replace) {
          grid.innerHTML =
            '<p style="color:red;text-align:center;">Failed to load media.</p>';
        }
      });
  }

  function uploadFiles(files) {
    if (!files || files.length === 0) return;
    var csrf = getCSRF();

    Array.from(files).forEach(function(file) {
      var formData = new FormData();
      formData.append('file', file);

      var headers = {};
      if (csrf) headers['X-CSRF-Token'] = csrf;

      fetch('/file/upload', {
        method: 'POST',
        headers: headers,
        body: formData,
      })
      .then(function(r) { return r.json(); })
      .then(function(data) {
        if (data.success && data.file) {
          // Switch to browse tab and reload
          modal.querySelectorAll('.media-picker-tab').forEach(function(t) {
            t.classList.remove('media-picker-tab--active');
          });
          modal.querySelector('[data-tab="browse"]').classList.add('media-picker-tab--active');
          document.getElementById('media-picker-grid').style.display = '';
          document.getElementById('media-picker-upload').style.display = 'none';
          modal.querySelector('.media-picker-toolbar').style.display = '';
          currentPage = 1;
          loadMedia(true);
        }
      })
      .catch(function() {
        var dropzone = modal.querySelector('.media-picker-dropzone p');
        if (dropzone) {
          dropzone.textContent = 'Upload failed. Please try again.';
          setTimeout(function() {
            dropzone.textContent = 'Drag files here or click to upload';
          }, 3000);
        }
      });
    });
  }

  function close() {
    if (modal) {
      modal.style.display = 'none';
      callback = null;
    }
  }

  function escapeHtml(s) {
    var d = document.createElement('div');
    d.textContent = s;
    return d.innerHTML;
  }

  function escapeAttr(s) {
    return s.replace(/&/g, '&amp;')
            .replace(/"/g, '&quot;')
            .replace(/</g, '&lt;')
            .replace(/>/g, '&gt;');
  }

  // Public API
  window.MediaPicker = {
    open: function(opts) {
      opts = opts || {};
      createModal();
      callback = opts.onSelect || function() {};
      currentType = opts.type || '';
      currentSearch = '';
      currentPage = 1;

      // Reset UI state
      modal.querySelector('.media-picker-type-filter').value = currentType;
      modal.querySelector('.media-picker-search').value = '';
      modal.querySelector('.media-picker-insert').disabled = true;
      modal.querySelector('.media-picker-selected-info').textContent = '';
      modal.querySelectorAll('.media-picker-tab').forEach(function(t) {
        t.classList.remove('media-picker-tab--active');
      });
      modal.querySelector('[data-tab="browse"]').classList.add('media-picker-tab--active');
      document.getElementById('media-picker-grid').style.display = '';
      document.getElementById('media-picker-upload').style.display = 'none';
      modal.querySelector('.media-picker-toolbar').style.display = '';

      modal.style.display = '';
      loadMedia(true);
    },
    close: close,
  };

  // Auto-bind "Browse media" buttons in file upload widgets
  function initBrowseButtons() {
    document.querySelectorAll('.media-picker-browse').forEach(function(btn) {
      btn.addEventListener('click', function() {
        var targetId = btn.dataset.target;
        var widget = btn.closest('.file-upload-widget');
        var hiddenInput = widget ? widget.querySelector('input[type="hidden"]') : null;
        var previewDiv = widget ? widget.querySelector('.file-upload-widget__preview') : null;
        var inputDiv = widget ? widget.querySelector('.file-upload-widget__input') : null;

        window.MediaPicker.open({
          onSelect: function(media) {
            if (hiddenInput) hiddenInput.value = media.id;
            if (previewDiv) {
              var nameSpan = previewDiv.querySelector('.file-upload-widget__filename');
              if (nameSpan) nameSpan.textContent = media.filename;

              // Show image preview for images
              var existingImg = previewDiv.querySelector('.file-upload-widget__image');
              if (existingImg) existingImg.remove();
              if (media.mime_type && media.mime_type.indexOf('image/') === 0) {
                var img = document.createElement('img');
                img.src = media.thumbnail_url || media.url;
                img.alt = media.filename;
                img.className = 'file-upload-widget__image';
                previewDiv.insertBefore(img, previewDiv.firstChild);
              }

              previewDiv.style.display = '';
            }
            if (inputDiv) inputDiv.style.display = 'none';
          },
        });
      });
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', initBrowseButtons);
  } else {
    initBrowseButtons();
  }
})();
