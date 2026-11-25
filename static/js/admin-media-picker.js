/**
 * The media picker on admin pages: the same window.MediaPicker.open(opts)
 * that media-picker.js offers (and the file widget's "Browse media" button
 * calls), built as an accessible modal dialog. The front end keeps
 * media-picker.js as it is; this file is loaded by the admin shell only and
 * replaces the picker there.
 *
 * - A native <dialog> opened with showModal(): the rest of the page is inert,
 *   focus stays inside, Escape closes it, and focus returns to the button
 *   that opened it.
 * - Every control is labelled: the type filter and the search have visible
 *   labels, each file is a button named by its file name whose pressed state
 *   says it is the one chosen, and the result count is announced politely.
 * - Upload is an ordinary, labelled file input (dragging files onto it works
 *   too, but nothing needs a drag).
 * - Words come from the shell's translated table (window.trovatoText).
 */
(function () {
    'use strict';

    if (!document.body || !document.body.classList.contains('admin')) return;
    if (typeof HTMLDialogElement === 'undefined') return;

    var tr = window.trovatoText || function (s, a) {
        return s.replace(/\{([a-z_]+)\}/g, function (m, n) { return a && n in a ? a[n] : m; });
    };

    var dialog = null;
    var opener = null;
    var callback = null;
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
        dialog = el('dialog', { class: 'admin-picker', 'aria-labelledby': 'admin-picker-title' });

        var head = el('div', { class: 'admin-picker__head' });
        head.appendChild(el('h2', { id: 'admin-picker-title', class: 'admin-picker__title' }, tr('Choose media')));
        var close = el('button', { type: 'button', class: 'button button--secondary admin-picker__close' }, tr('Close'));
        close.addEventListener('click', function () { dialog.close(); });
        head.appendChild(close);
        dialog.appendChild(head);

        var tools = el('div', { class: 'admin-picker__tools' });
        var typeItem = el('div', { class: 'form-item form-item--select' });
        typeItem.appendChild(el('label', { class: 'form-item__label', for: 'admin-picker-type' }, tr('Type')));
        var select = el('select', { id: 'admin-picker-type', class: 'form-select' });
        [['', 'All types'], ['image', 'Images'], ['document', 'Documents']].forEach(function (o) {
            select.appendChild(el('option', { value: o[0] }, tr(o[1])));
        });
        select.addEventListener('change', function () { type = select.value; page = 1; load(true); });
        typeItem.appendChild(select);
        tools.appendChild(typeItem);

        var searchItem = el('div', { class: 'form-item form-item--search' });
        searchItem.appendChild(el('label', { class: 'form-item__label', for: 'admin-picker-q' }, tr('Search files')));
        var search = el('input', { id: 'admin-picker-q', type: 'search', class: 'form-text form-text--w20', autocomplete: 'off' });
        var wait;
        search.addEventListener('input', function () {
            clearTimeout(wait);
            wait = setTimeout(function () { query = search.value; page = 1; load(true); }, 300);
        });
        searchItem.appendChild(search);
        tools.appendChild(searchItem);

        var uploadItem = el('div', { class: 'form-item form-item--file' });
        uploadItem.appendChild(el('label', { class: 'form-item__label', for: 'admin-picker-upload' }, tr('Choose files to upload')));
        var file = el('input', { id: 'admin-picker-upload', type: 'file', class: 'form-file', multiple: '' });
        file.addEventListener('change', function () { upload(file.files); file.value = ''; });
        uploadItem.appendChild(file);
        tools.appendChild(uploadItem);
        dialog.appendChild(tools);

        dialog.appendChild(el('p', { class: 'visually-hidden', id: 'admin-picker-status', role: 'status' }));
        var grid = el('ul', { class: 'admin-picker__grid', id: 'admin-picker-grid', 'aria-labelledby': 'admin-picker-title' });
        dialog.appendChild(grid);

        var foot = el('div', { class: 'admin-picker__foot' });
        var more = el('button', { type: 'button', class: 'button button--secondary admin-picker__more', hidden: '' }, tr('Load more'));
        more.addEventListener('click', function () { page += 1; load(false); });
        foot.appendChild(more);
        foot.appendChild(el('p', { class: 'admin-picker__chosen', id: 'admin-picker-chosen' }));
        var insert = el('button', { type: 'button', class: 'button button--primary admin-picker__insert', disabled: '' }, tr('Insert'));
        insert.addEventListener('click', function () {
            if (chosen && callback) callback(chosen);
            dialog.close();
        });
        foot.appendChild(insert);
        dialog.appendChild(foot);

        // Files dropped anywhere on the dialog upload too.
        dialog.addEventListener('dragover', function (e) { e.preventDefault(); });
        dialog.addEventListener('drop', function (e) { e.preventDefault(); upload(e.dataTransfer.files); });

        dialog.addEventListener('close', function () {
            callback = null;
            if (opener && document.body.contains(opener)) opener.focus();
        });
        document.body.appendChild(dialog);
    }

    function status(text) {
        dialog.querySelector('#admin-picker-status').textContent = text;
    }

    function choose(button, item) {
        dialog.querySelectorAll('.admin-picker__item[aria-pressed="true"]').forEach(function (b) {
            b.setAttribute('aria-pressed', 'false');
        });
        button.setAttribute('aria-pressed', 'true');
        chosen = {
            id: item.id,
            url: item.url,
            filename: item.filename,
            thumbnail_url: item.thumbnail_url || item.url,
            mime_type: item.mime_type
        };
        dialog.querySelector('.admin-picker__insert').disabled = false;
        dialog.querySelector('#admin-picker-chosen').textContent = tr('Selected: {name}', { name: item.filename });
    }

    function load(replace) {
        var grid = dialog.querySelector('#admin-picker-grid');
        if (replace) {
            grid.textContent = '';
            status(tr('Loading…'));
        }
        var params = new URLSearchParams();
        params.set('page', page);
        params.set('per_page', '24');
        if (type) params.set('type', type);
        if (query) params.set('q', query);
        fetch('/api/v1/media/browse?' + params.toString(), { credentials: 'same-origin' })
            .then(function (r) { return r.json(); })
            .then(function (data) {
                var items = (data && data.data) || [];
                items.forEach(function (item) {
                    var li = el('li', { class: 'admin-picker__cell' });
                    var button = el('button', { type: 'button', class: 'admin-picker__item', 'aria-pressed': 'false' });
                    if (item.mime_type && item.mime_type.indexOf('image/') === 0 && item.thumbnail_url) {
                        // The button is named by the file name; the image
                        // is decoration beside it.
                        button.appendChild(el('img', { src: item.thumbnail_url, alt: '', loading: 'lazy' }));
                    } else {
                        button.appendChild(el('span', { class: 'admin-picker__ext', 'aria-hidden': 'true' },
                            String(item.filename.split('.').pop() || '').toUpperCase()));
                    }
                    button.appendChild(el('span', { class: 'admin-picker__name' }, item.filename));
                    button.addEventListener('click', function () { choose(button, item); });
                    li.appendChild(button);
                    grid.appendChild(li);
                });
                var shown = grid.children.length;
                status(shown ? tr('{count} files shown.', { count: shown }) : tr('No media found.'));
                dialog.querySelector('.admin-picker__more').hidden = items.length < 24;
            })
            .catch(function () {
                status(tr('The media could not be loaded.'));
            });
    }

    function upload(files) {
        if (!files || !files.length) return;
        var headers = {};
        var token = csrf();
        if (token) headers['X-CSRF-Token'] = token;
        Array.prototype.forEach.call(files, function (f) {
            var body = new FormData();
            body.append('file', f);
            fetch('/file/upload', { method: 'POST', headers: headers, body: body, credentials: 'same-origin' })
                .then(function (r) { return r.json(); })
                .then(function (data) {
                    if (data && data.success) {
                        page = 1;
                        load(true);
                        status(tr('Uploaded. Choose it from the list.'));
                    } else {
                        status(tr('Upload failed. Please try again.'));
                    }
                })
                .catch(function () { status(tr('Upload failed. Please try again.')); });
        });
    }

    var previous = window.MediaPicker || {};
    window.MediaPicker = {
        open: function (opts) {
            opts = opts || {};
            if (!dialog) build();
            opener = document.activeElement;
            callback = opts.onSelect || function () {};
            type = opts.type || '';
            query = '';
            page = 1;
            chosen = null;
            dialog.querySelector('#admin-picker-type').value = type;
            dialog.querySelector('#admin-picker-q').value = '';
            dialog.querySelector('.admin-picker__insert').disabled = true;
            dialog.querySelector('#admin-picker-chosen').textContent = '';
            dialog.showModal();
            load(true);
        },
        close: function () {
            if (dialog && dialog.open) dialog.close();
        },
        previous: previous
    };
})();
