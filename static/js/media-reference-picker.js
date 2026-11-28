/**
 * The media library picker for a `RecordReference("media")` field
 * (BL-115 / B290): an accessible modal dialog, built the same way as
 * static/js/admin-media-picker.js —
 *
 * - A native <dialog> opened with showModal(): the rest of the page is
 *   inert, focus stays inside, Escape closes it, and focus returns to the
 *   button that opened it.
 * - Every control is labelled; each result is a button named by its title
 *   whose pressed state says it is the one chosen.
 * - "Upload new" is an ordinary, labelled form inside the same dialog.
 * - Progressive enhancement: the field's no-JS `<select>` posts under the
 *   field's own name until this script runs, at which point it is disabled
 *   (so it stops posting) and the hidden input beside it (enabled here) takes
 *   over — see templates/form/widget--media_reference.html.
 */
(function () {
    'use strict';

    if (typeof HTMLDialogElement === 'undefined') return;

    var tr = window.trovatoText || function (s, a) {
        return s.replace(/\{([a-z_]+)\}/g, function (m, n) { return a && n in a ? a[n] : m; });
    };

    var MEDIA_TYPES = [
        ['media_file', 'File'],
        ['media_image', 'Image'],
        ['media_remote_video', 'Remote video'],
        ['media_remote_embed', 'Remote embed']
    ];

    var dialog = null;
    var opener = null;
    var active = null; // the field wrapper currently being edited
    var page = 1;
    var type = '';
    var query = '';
    var chosen = null;

    function csrf() {
        var meta = document.querySelector('meta[name="csrf-token"]');
        if (meta) return meta.content;
        var input = document.querySelector('input[name="_token"]');
        return input ? input.value : '';
    }

    function el(tag, attrs, text) {
        var node = document.createElement(tag);
        Object.keys(attrs || {}).forEach(function (k) { node.setAttribute(k, attrs[k]); });
        if (text !== undefined) node.textContent = text;
        return node;
    }

    function build() {
        dialog = el('dialog', { class: 'media-ref-picker', 'aria-labelledby': 'media-ref-picker-title' });

        var head = el('div', { class: 'media-ref-picker__head' });
        head.appendChild(el('h2', { id: 'media-ref-picker-title', class: 'media-ref-picker__title' }, tr('Choose media')));
        var close = el('button', { type: 'button', class: 'button button--secondary media-ref-picker__close' }, tr('Close'));
        close.addEventListener('click', function () { dialog.close(); });
        head.appendChild(close);
        dialog.appendChild(head);

        var tools = el('div', { class: 'media-ref-picker__tools' });
        var typeItem = el('div', { class: 'form-item form-item--select' });
        typeItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-type' }, tr('Media type')));
        var select = el('select', { id: 'media-ref-picker-type', class: 'form-select' });
        select.appendChild(el('option', { value: '' }, tr('All types')));
        MEDIA_TYPES.forEach(function (o) { select.appendChild(el('option', { value: o[0] }, tr(o[1]))); });
        select.addEventListener('change', function () { type = select.value; page = 1; load(true); });
        typeItem.appendChild(select);
        tools.appendChild(typeItem);

        var searchItem = el('div', { class: 'form-item form-item--search' });
        searchItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-q' }, tr('Search media')));
        var search = el('input', { id: 'media-ref-picker-q', type: 'search', class: 'form-text form-text--w20', autocomplete: 'off' });
        var wait;
        search.addEventListener('input', function () {
            clearTimeout(wait);
            wait = setTimeout(function () { query = search.value; page = 1; load(true); }, 300);
        });
        searchItem.appendChild(search);
        tools.appendChild(searchItem);
        dialog.appendChild(tools);

        dialog.appendChild(el('p', { class: 'visually-hidden', id: 'media-ref-picker-status', role: 'status' }));
        var grid = el('ul', { class: 'media-ref-picker__grid', id: 'media-ref-picker-grid', 'aria-labelledby': 'media-ref-picker-title' });
        dialog.appendChild(grid);

        var more = el('button', { type: 'button', class: 'button button--secondary media-ref-picker__more', hidden: '' }, tr('Load more'));
        more.addEventListener('click', function () { page += 1; load(false); });
        dialog.appendChild(more);

        // Upload new, in the same dialog.
        var uploadSection = el('details', { class: 'media-ref-picker__upload' });
        uploadSection.appendChild(el('summary', {}, tr('Upload new media')));
        var uploadType = el('select', { id: 'media-ref-picker-upload-type', class: 'form-select' });
        MEDIA_TYPES.forEach(function (o) { uploadType.appendChild(el('option', { value: o[0] }, tr(o[1]))); });
        var uploadTypeItem = el('div', { class: 'form-item form-item--select' });
        uploadTypeItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-upload-type' }, tr('New media type')));
        uploadTypeItem.appendChild(uploadType);
        uploadSection.appendChild(uploadTypeItem);

        var titleItem = el('div', { class: 'form-item form-item--text' });
        titleItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-upload-title' }, tr('Name')));
        var uploadTitle = el('input', { id: 'media-ref-picker-upload-title', type: 'text', class: 'form-text' });
        titleItem.appendChild(uploadTitle);
        uploadSection.appendChild(titleItem);

        var fileItem = el('div', { class: 'form-item form-item--file', id: 'media-ref-picker-file-item' });
        fileItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-upload-file' }, tr('File')));
        var uploadFile = el('input', { id: 'media-ref-picker-upload-file', type: 'file', class: 'form-file' });
        fileItem.appendChild(uploadFile);
        uploadSection.appendChild(fileItem);

        var urlItem = el('div', { class: 'form-item form-item--text', id: 'media-ref-picker-url-item', hidden: '' });
        urlItem.appendChild(el('label', { class: 'form-item__label', for: 'media-ref-picker-upload-url' }, tr('URL')));
        var uploadUrl = el('input', { id: 'media-ref-picker-upload-url', type: 'url', class: 'form-text' });
        urlItem.appendChild(uploadUrl);
        uploadSection.appendChild(urlItem);

        function syncUploadFields() {
            var remote = uploadType.value === 'media_remote_video' || uploadType.value === 'media_remote_embed';
            fileItem.hidden = remote;
            urlItem.hidden = !remote;
        }
        uploadType.addEventListener('change', syncUploadFields);
        syncUploadFields();

        dialog.appendChild(el('p', { class: 'visually-hidden', id: 'media-ref-picker-upload-status', role: 'status' }));
        var uploadButton = el('button', { type: 'button', class: 'button button--secondary' }, tr('Add media'));
        uploadButton.addEventListener('click', function () { upload(uploadType.value, uploadTitle.value, uploadFile.files[0], uploadUrl.value); });
        uploadSection.appendChild(uploadButton);
        dialog.appendChild(uploadSection);

        var foot = el('div', { class: 'media-ref-picker__foot' });
        foot.appendChild(el('p', { class: 'media-ref-picker__chosen', id: 'media-ref-picker-chosen' }));
        var insert = el('button', { type: 'button', class: 'button button--primary media-ref-picker__insert', disabled: '' }, tr('Choose'));
        insert.addEventListener('click', function () {
            if (chosen) apply(chosen);
            dialog.close();
        });
        foot.appendChild(insert);
        dialog.appendChild(foot);

        dialog.addEventListener('close', function () {
            if (opener && document.body.contains(opener)) opener.focus();
        });
        document.body.appendChild(dialog);
    }

    function status(text) {
        dialog.querySelector('#media-ref-picker-status').textContent = text;
    }

    function choose(button, item) {
        dialog.querySelectorAll('.media-ref-picker__item[aria-pressed="true"]').forEach(function (b) {
            b.setAttribute('aria-pressed', 'false');
        });
        button.setAttribute('aria-pressed', 'true');
        chosen = { id: item.id, label: item.label };
        dialog.querySelector('.media-ref-picker__insert').disabled = false;
        dialog.querySelector('#media-ref-picker-chosen').textContent = tr('Selected: {name}', { name: item.label });
    }

    function load(replace) {
        var grid = dialog.querySelector('#media-ref-picker-grid');
        if (replace) {
            grid.textContent = '';
            status(tr('Loading…'));
        }
        var params = new URLSearchParams();
        params.set('page', page);
        if (type) params.set('type', type);
        if (query) params.set('q', query);
        fetch('/admin/content/media/picker?' + params.toString(), { credentials: 'same-origin' })
            .then(function (r) { return r.json(); })
            .then(function (data) {
                var items = (data && data.items) || [];
                items.forEach(function (item) {
                    var li = el('li', { class: 'media-ref-picker__cell' });
                    var button = el('button', { type: 'button', class: 'media-ref-picker__item', 'aria-pressed': 'false' });
                    if (item.thumbnail_url) {
                        button.appendChild(el('img', { src: item.thumbnail_url, alt: '', loading: 'lazy' }));
                    } else {
                        button.appendChild(el('span', { class: 'media-ref-picker__ext', 'aria-hidden': 'true' }, item.type_label || '?'));
                    }
                    button.appendChild(el('span', { class: 'media-ref-picker__name' }, item.label));
                    button.addEventListener('click', function () { choose(button, item); });
                    li.appendChild(button);
                    grid.appendChild(li);
                });
                var shown = grid.children.length;
                status(shown ? tr('{count} media items shown.', { count: shown }) : tr('No media found.'));
                dialog.querySelector('.media-ref-picker__more').hidden = items.length < 24;
            })
            .catch(function () { status(tr('The media could not be loaded.')); });
    }

    function upload(mediaType, title, file, url) {
        var headers = {};
        var token = csrf();
        if (token) headers['X-CSRF-Token'] = token;
        var body = new FormData();
        body.append('media_type', mediaType);
        body.append('title', title || '');
        if (file) body.append('file', file);
        if (url) body.append('remote_url', url);
        dialog.querySelector('#media-ref-picker-upload-status').textContent = tr('Uploading…');
        fetch('/admin/content/media/picker/upload', { method: 'POST', headers: headers, body: body, credentials: 'same-origin' })
            .then(function (r) { return r.json(); })
            .then(function (data) {
                if (data && data.success && data.item) {
                    apply({ id: data.item.id, label: data.item.label });
                    dialog.querySelector('#media-ref-picker-upload-status').textContent = tr('Added and selected.');
                    dialog.close();
                } else {
                    dialog.querySelector('#media-ref-picker-upload-status').textContent = (data && data.error) || tr('Upload failed. Please try again.');
                }
            })
            .catch(function () {
                dialog.querySelector('#media-ref-picker-upload-status').textContent = tr('Upload failed. Please try again.');
            });
    }

    function apply(item) {
        if (!active) return;
        var hidden = active.querySelector('.media-reference-field__value');
        var preview = active.querySelector('.media-reference-field__preview-label');
        var clear = active.querySelector('.media-reference-field__clear');
        hidden.value = item.id;
        preview.textContent = item.label || '';
        if (clear) clear.hidden = !item.id;
    }

    function openFor(field) {
        active = field;
        if (!dialog) build();
        opener = document.activeElement;
        type = '';
        query = '';
        page = 1;
        chosen = null;
        dialog.querySelector('#media-ref-picker-type').value = '';
        dialog.querySelector('#media-ref-picker-q').value = '';
        dialog.querySelector('.media-ref-picker__insert').disabled = true;
        dialog.querySelector('#media-ref-picker-chosen').textContent = '';
        dialog.querySelector('#media-ref-picker-upload-status').textContent = '';
        dialog.showModal();
        load(true);
    }

    function enhance(field) {
        if (field.hasAttribute('data-media-ref-enhanced')) return;
        field.setAttribute('data-media-ref-enhanced', '1');
        var hidden = field.querySelector('.media-reference-field__value');
        var select = field.querySelector('.media-reference-field__fallback');
        var preview = field.querySelector('.media-reference-field__preview');
        var actions = field.querySelector('.media-reference-field__actions');
        var chooseButton = field.querySelector('.media-reference-field__choose');
        var clearButton = field.querySelector('.media-reference-field__clear');
        var noscriptHint = field.querySelector('.media-reference-field__noscript-hint');
        var noscriptLink = field.querySelector('.media-reference-field__noscript-link');
        // The select posted the value until now; the hidden input takes over.
        hidden.disabled = false;
        if (select) {
            select.disabled = true;
            select.hidden = true;
            if (select.value) hidden.value = select.value;
        }
        if (noscriptHint) noscriptHint.hidden = true;
        if (noscriptLink) noscriptLink.hidden = true;
        preview.hidden = false;
        actions.hidden = false;
        if (clearButton) clearButton.hidden = !hidden.value;
        chooseButton.addEventListener('click', function () { openFor(field); });
        if (clearButton) {
            clearButton.addEventListener('click', function () {
                hidden.value = '';
                clearButton.hidden = true;
                var preview = field.querySelector('.media-reference-field__preview-label');
                preview.textContent = tr('No media selected');
            });
        }
    }

    function enhanceAll() {
        document.querySelectorAll('.media-reference-field').forEach(enhance);
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', enhanceAll);
    } else {
        enhanceAll();
    }
})();
