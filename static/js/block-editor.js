/**
 * Block editor field widget — integrates Editor.js with Trovato's
 * compound field system.
 *
 * Maps between Trovato's block format ({type, weight, data}) and
 * Editor.js's format ({id, type, data}). Serializes to a hidden
 * input on save.
 *
 * Loaded only for fields with widget: "block_editor".
 */
(function () {
    'use strict';

    // -------------------------------------------------------------------------
    // Configuration
    // -------------------------------------------------------------------------

    var UPLOAD_ENDPOINT = '/api/block-editor/upload';
    var PREVIEW_ENDPOINT = '/api/block-editor/preview';

    // -------------------------------------------------------------------------
    // Trovato <-> Editor.js format mapping
    // -------------------------------------------------------------------------

    /**
     * Convert Trovato block array to Editor.js blocks.
     * Trovato: [{ type: "paragraph", weight: 0, data: { text: "..." } }]
     * Editor.js: { blocks: [{ id: "...", type: "paragraph", data: { text: "..." } }] }
     */
    function trovatoToEditorJs(blocks) {
        if (!blocks || !Array.isArray(blocks)) return { blocks: [] };

        // Sort by weight
        var sorted = blocks.slice().sort(function (a, b) {
            return (a.weight || 0) - (b.weight || 0);
        });

        return {
            blocks: sorted.map(function (block) {
                return {
                    id: block.id || generateId(),
                    type: block.type,
                    data: block.data || {}
                };
            })
        };
    }

    /**
     * Convert Editor.js output back to Trovato block array.
     */
    function editorJsToTrovato(editorData) {
        if (!editorData || !editorData.blocks) return [];

        return editorData.blocks.map(function (block, index) {
            return {
                type: block.type,
                weight: index,
                data: block.data || {}
            };
        });
    }

    /**
     * Generate a simple random ID for blocks.
     */
    function generateId() {
        return Math.random().toString(36).substring(2, 10);
    }

    // -------------------------------------------------------------------------
    // Editor initialization
    // -------------------------------------------------------------------------

    /**
     * Initialize a block editor instance on a container element.
     *
     * @param {HTMLElement} container - The element to host the editor
     * @param {HTMLInputElement} hiddenInput - Hidden input for serialized data
     * @param {string[]} allowedTypes - Allowed block types
     * @param {boolean} readOnly - Whether the editor is read-only
     */
    function initBlockEditor(container, hiddenInput, allowedTypes, readOnly) {
        // Parse existing data
        var existingBlocks = [];
        try {
            if (hiddenInput.value) {
                existingBlocks = JSON.parse(hiddenInput.value);
            }
        } catch (e) {
            // Start fresh on parse error
        }

        var editorData = trovatoToEditorJs(existingBlocks);

        // Check if Editor.js is available
        if (typeof EditorJS === 'undefined') {
            container.innerHTML = '<p style="color: var(--gray-500, #999);">Editor.js not loaded. Falling back to JSON editor.</p>';
            renderFallbackEditor(container, hiddenInput, existingBlocks);
            return;
        }

        // Build tool config based on allowed types
        var tools = buildToolConfig(allowedTypes || []);

        var editor = new EditorJS({
            holder: container,
            data: editorData,
            readOnly: readOnly || false,
            tools: tools,
            placeholder: 'Start writing or click + to add a block...',
            onChange: function () {
                // Debounce save
                if (editor._saveTimer) clearTimeout(editor._saveTimer);
                editor._saveTimer = setTimeout(function () {
                    saveEditorData(editor, hiddenInput);
                }, 300);
            }
        });

        // Store reference for form submission
        container._editorInstance = editor;
    }

    /**
     * Build Editor.js tool configuration from allowed block types.
     */
    function buildToolConfig(allowedTypes) {
        var tools = {};
        var all = !allowedTypes.length; // Empty = allow all

        if (all || allowedTypes.indexOf('heading') >= 0) {
            if (typeof Header !== 'undefined') {
                tools.header = {
                    class: Header,
                    config: { levels: [2, 3, 4], defaultLevel: 2 }
                };
            }
        }

        if (all || allowedTypes.indexOf('list') >= 0) {
            if (typeof List !== 'undefined') {
                tools.list = { class: List, inlineToolbar: true };
            }
        }

        if (all || allowedTypes.indexOf('quote') >= 0) {
            if (typeof Quote !== 'undefined') {
                tools.quote = { class: Quote, inlineToolbar: true };
            }
        }

        if (all || allowedTypes.indexOf('code') >= 0) {
            if (typeof CodeTool !== 'undefined') {
                tools.code = { class: CodeTool };
            }
        }

        if (all || allowedTypes.indexOf('image') >= 0) {
            if (typeof ImageTool !== 'undefined') {
                tools.image = {
                    class: ImageTool,
                    config: {
                        endpoints: { byFile: UPLOAD_ENDPOINT },
                        field: 'image'
                    }
                };
            }
        }

        if (all || allowedTypes.indexOf('delimiter') >= 0) {
            if (typeof Delimiter !== 'undefined') {
                tools.delimiter = { class: Delimiter };
            }
        }

        if (all || allowedTypes.indexOf('embed') >= 0) {
            if (typeof Embed !== 'undefined') {
                tools.embed = {
                    class: Embed,
                    config: {
                        services: {
                            youtube: true,
                            vimeo: true
                        }
                    }
                };
            }
        }

        return tools;
    }

    /**
     * Save editor data to hidden input.
     */
    function saveEditorData(editor, hiddenInput) {
        editor.save().then(function (outputData) {
            var blocks = editorJsToTrovato(outputData);
            hiddenInput.value = JSON.stringify(blocks);
        }).catch(function (err) {
            console.error('Block editor save failed:', err);
        });
    }

    // -------------------------------------------------------------------------
    // Fallback editor (when Editor.js is not loaded)
    // -------------------------------------------------------------------------

    function renderFallbackEditor(container, hiddenInput, blocks) {
        var textarea = document.createElement('textarea');
        textarea.className = 'form-textarea';
        textarea.rows = 12;
        textarea.style.fontFamily = 'monospace';
        textarea.style.fontSize = '0.875rem';
        textarea.value = JSON.stringify(blocks, null, 2);

        textarea.addEventListener('input', function () {
            try {
                JSON.parse(textarea.value);
                textarea.style.borderColor = '';
                hiddenInput.value = textarea.value;
            } catch (e) {
                textarea.style.borderColor = 'red';
            }
        });

        container.appendChild(textarea);
    }

    // -------------------------------------------------------------------------
    // Preview (Story 24.8)
    // -------------------------------------------------------------------------

    /**
     * Load a server-side preview of the current block content.
     */
    function loadPreview(editor, previewContainer) {
        if (!editor) return;

        previewContainer.innerHTML = '<p>Loading preview...</p>';

        editor.save().then(function (outputData) {
            var blocks = editorJsToTrovato(outputData);

            var xhr = new XMLHttpRequest();
            xhr.open('POST', PREVIEW_ENDPOINT, true);
            xhr.setRequestHeader('Content-Type', 'application/json');

            xhr.addEventListener('load', function () {
                if (xhr.status === 200) {
                    try {
                        var response = JSON.parse(xhr.responseText);
                        // Server-side rendering uses ammonia sanitization, but
                        // use a sandboxed iframe for defense-in-depth.
                        var html = response.html || '<p>No content.</p>';
                        var iframe = document.createElement('iframe');
                        iframe.sandbox = 'allow-same-origin';
                        iframe.style.width = '100%';
                        iframe.style.border = 'none';
                        previewContainer.innerHTML = '';
                        previewContainer.appendChild(iframe);
                        var doc = iframe.contentDocument || iframe.contentWindow.document;
                        doc.open();
                        doc.write('<!DOCTYPE html><html><body>' + html + '</body></html>');
                        doc.close();
                        // Auto-resize iframe to content height
                        iframe.style.height = doc.body.scrollHeight + 'px';
                    } catch (e) {
                        previewContainer.textContent = 'Preview error.';
                    }
                } else {
                    previewContainer.textContent = 'Preview failed.';
                }
            });

            xhr.addEventListener('error', function () {
                previewContainer.innerHTML = '<p>Network error.</p>';
            });

            xhr.send(JSON.stringify({ blocks: blocks }));
        });
    }

    // -------------------------------------------------------------------------
    // Auto-initialization
    // -------------------------------------------------------------------------

    function initAll() {
        // Find all block editor containers
        var containers = document.querySelectorAll('[data-block-editor]');
        containers.forEach(function (container) {
            var inputName = container.dataset.blockEditorInput;
            var hiddenInput = inputName
                ? document.querySelector('input[name="' + inputName + '"]')
                : container.previousElementSibling;

            if (!hiddenInput) return;

            var allowedTypes = (container.dataset.blockTypes || '').split(',').filter(Boolean);
            var readOnly = container.dataset.readOnly === 'true';

            initBlockEditor(container, hiddenInput, allowedTypes, readOnly);

            // Preview button
            var previewBtn = container.parentElement
                ? container.parentElement.querySelector('[data-block-preview]')
                : null;
            if (previewBtn) {
                var previewContainer = document.createElement('div');
                previewContainer.className = 'block-editor-preview';
                previewBtn.parentElement.appendChild(previewContainer);

                previewBtn.addEventListener('click', function () {
                    loadPreview(container._editorInstance, previewContainer);
                });
            }
        });
    }

    // Bind form submit to save all editors before submitting
    function bindFormSave() {
        var forms = document.querySelectorAll('form');
        forms.forEach(function (form) {
            form.addEventListener('submit', function (e) {
                var containers = form.querySelectorAll('[data-block-editor]');
                var editors = [];
                containers.forEach(function (container) {
                    var editor = container._editorInstance;
                    if (editor && editor.save) {
                        var inputName = container.dataset.blockEditorInput;
                        var hiddenInput = inputName
                            ? form.querySelector('input[name="' + inputName + '"]')
                            : container.previousElementSibling;

                        if (hiddenInput) {
                            editors.push({ editor: editor, input: hiddenInput });
                        }
                    }
                });

                if (editors.length > 0) {
                    // Prevent default submission, await all saves, then re-submit
                    e.preventDefault();
                    Promise.all(editors.map(function (entry) {
                        return entry.editor.save().then(function (outputData) {
                            var blocks = editorJsToTrovato(outputData);
                            entry.input.value = JSON.stringify(blocks);
                        });
                    })).then(function () {
                        // Submit without re-triggering this handler
                        form.removeEventListener('submit', arguments.callee);
                        form.submit();
                    }).catch(function (err) {
                        console.error('Block editor save failed on submit:', err);
                        // Submit anyway with last known data
                        form.submit();
                    });
                }
            });
        });
    }

    // Bootstrap
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', function () {
            initAll();
            bindFormSave();
        });
    } else {
        initAll();
        bindFormSave();
    }

    // Expose for external use
    window.BlockEditor = {
        init: initBlockEditor,
        trovatoToEditorJs: trovatoToEditorJs,
        editorJsToTrovato: editorJsToTrovato,
        loadPreview: loadPreview
    };
})();
