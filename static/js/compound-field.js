/**
 * Compound field widget — add/remove/reorder typed sections.
 *
 * Finds all .compound-field containers on DOMContentLoaded,
 * reads data-config for section schemas, and provides AJAX
 * interactions serialised to a hidden JSON input on submit.
 */
(function () {
    'use strict';

    function initCompoundFields() {
        document.querySelectorAll('.compound-field').forEach(function (el) {
            // Guard against double-init (if script loaded multiple times)
            if (el.dataset.compoundInit) return;
            el.dataset.compoundInit = '1';
            initField(el);
        });
    }

    function initField(container) {
        var fieldName = container.dataset.field;
        var config = JSON.parse(container.dataset.config || '{}');
        var sectionTypesRaw = container.dataset.sectionTypes;
        var sectionsEl = container.querySelector('.compound-field__sections');
        var hiddenInput = container.querySelector('.compound-field__value');
        var addBtn = container.querySelector('.compound-field__add');

        var allowedTypes = config.allowed_types || [];
        var maxItems = (config.max_items !== undefined && config.max_items !== null) ? config.max_items : null;
        var minItems = (config.min_items !== undefined && config.min_items !== null) ? config.min_items : null;
        // Use the label element text as human-readable name for validation messages
        var fieldLabel = (function () {
            var labelEl = container.parentNode && container.parentNode.querySelector('label[for="' + fieldName + '"]');
            return (labelEl && labelEl.textContent) ? labelEl.textContent.trim().replace(/\s*\*$/, '') : fieldName;
        })();

        // Build section schemas map
        var sectionSchemas = {};
        var sectionTypesArr = [];
        try {
            sectionTypesArr = sectionTypesRaw ? JSON.parse(sectionTypesRaw) : [];
        } catch (e) {
            sectionTypesArr = [];
        }
        sectionTypesArr.forEach(function (st) {
            sectionSchemas[st.machine_name] = st;
        });

        // Hydrate existing sections from hidden input
        try {
            var existing = JSON.parse(hiddenInput.value || '{"sections":[]}');
            var sorted = (existing.sections || []).slice();
            sorted.sort(function (a, b) { return (a.weight || 0) - (b.weight || 0); });
            sorted.forEach(function (sec) {
                addSectionDOM(sec.type, sec.data, sec.weight);
            });
        } catch (e) {
            // ignore parse errors on initial load
        }

        // Update add button state based on max_items
        function updateAddButton() {
            if (maxItems !== null && sectionsEl.children.length >= maxItems) {
                addBtn.disabled = true;
                addBtn.title = 'Maximum of ' + maxItems + ' section(s) reached';
            } else {
                addBtn.disabled = false;
                addBtn.title = '';
            }
        }
        updateAddButton();

        // Add section button — shows dropdown if multiple types
        addBtn.addEventListener('click', function () {
            if (addBtn.disabled) return;
            if (allowedTypes.length === 1) {
                addSectionDOM(allowedTypes[0], {}, nextWeight());
                sync();
                updateAddButton();
            } else {
                showTypeDropdown();
            }
        });

        function showTypeDropdown() {
            // Remove any existing dropdown
            var old = container.querySelector('.compound-field__dropdown');
            if (old) { old.remove(); addBtn.setAttribute('aria-expanded', 'false'); return; }

            var dropdown = document.createElement('div');
            dropdown.className = 'compound-field__dropdown';
            dropdown.setAttribute('role', 'menu');
            addBtn.setAttribute('aria-expanded', 'true');

            function closeDropdown() {
                dropdown.remove();
                addBtn.setAttribute('aria-expanded', 'false');
                document.removeEventListener('click', outsideClickHandler);
                document.removeEventListener('keydown', keyHandler);
                addBtn.focus();
            }

            allowedTypes.forEach(function (typeName) {
                var schema = sectionSchemas[typeName];
                var btn = document.createElement('button');
                btn.type = 'button';
                btn.className = 'compound-field__dropdown-item';
                btn.setAttribute('role', 'menuitem');
                btn.textContent = schema ? schema.label : typeName;
                btn.addEventListener('click', function () {
                    addSectionDOM(typeName, {}, nextWeight());
                    closeDropdown();
                    sync();
                    updateAddButton();
                });
                dropdown.appendChild(btn);
            });
            addBtn.parentNode.insertBefore(dropdown, addBtn.nextSibling);

            // Focus first item
            var firstItem = dropdown.querySelector('.compound-field__dropdown-item');
            if (firstItem) firstItem.focus();

            // Keyboard navigation: Escape closes, ArrowDown/ArrowUp move focus
            function keyHandler(e) {
                if (e.key === 'Escape') {
                    closeDropdown();
                    return;
                }
                if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
                    e.preventDefault();
                    var items = dropdown.querySelectorAll('.compound-field__dropdown-item');
                    if (!items.length) return;
                    var current = dropdown.querySelector('.compound-field__dropdown-item:focus');
                    var idx = Array.prototype.indexOf.call(items, current);
                    if (e.key === 'ArrowDown') {
                        idx = (idx + 1) % items.length;
                    } else {
                        idx = (idx - 1 + items.length) % items.length;
                    }
                    items[idx].focus();
                }
            }
            document.addEventListener('keydown', keyHandler);

            // Close dropdown on outside click
            function outsideClickHandler(e) {
                if (!dropdown.contains(e.target) && e.target !== addBtn) {
                    closeDropdown();
                }
            }
            setTimeout(function () {
                document.addEventListener('click', outsideClickHandler);
            }, 0);
        }

        function nextWeight() {
            return sectionsEl.children.length;
        }

        // Counter for generating unique input IDs
        var inputIdCounter = 0;

        function addSectionDOM(typeName, data, weight) {
            var schema = sectionSchemas[typeName];
            var section = document.createElement('div');
            section.className = 'compound-section-editor';
            section.dataset.type = typeName;
            section.draggable = true;

            // Header with drag handle, reorder buttons, type label, and remove button
            var header = document.createElement('div');
            header.className = 'compound-section-editor__header';
            header.innerHTML =
                '<span class="compound-section-editor__drag" role="img" aria-label="Drag to reorder" title="Drag to reorder">&#x2630;</span>' +
                '<button type="button" class="compound-section-editor__move-up" title="Move up" aria-label="Move section up">&#x25B2;</button>' +
                '<button type="button" class="compound-section-editor__move-down" title="Move down" aria-label="Move section down">&#x25BC;</button>' +
                '<span class="compound-section-editor__label">' + escapeHtml(schema ? schema.label : typeName) + '</span>' +
                '<button type="button" class="compound-section-editor__remove" title="Remove section" aria-label="Remove section">&times;</button>';
            section.appendChild(header);

            // Move up handler
            header.querySelector('.compound-section-editor__move-up').addEventListener('click', function () {
                var prev = section.previousElementSibling;
                if (prev) {
                    sectionsEl.insertBefore(section, prev);
                    sync();
                }
            });

            // Move down handler
            header.querySelector('.compound-section-editor__move-down').addEventListener('click', function () {
                var next = section.nextElementSibling;
                if (next) {
                    sectionsEl.insertBefore(section, next.nextSibling);
                    sync();
                }
            });

            // Remove handler
            header.querySelector('.compound-section-editor__remove').addEventListener('click', function () {
                if (confirm('Remove this section?')) {
                    section.remove();
                    sync();
                    updateAddButton();
                }
            });

            // Fields
            var fieldsContainer = document.createElement('div');
            fieldsContainer.className = 'compound-section-editor__fields';

            if (schema && schema.fields) {
                schema.fields.forEach(function (fieldSchema) {
                    var wrapper = document.createElement('div');
                    wrapper.className = 'compound-section-editor__field';

                    inputIdCounter++;
                    var inputId = fieldName + '-' + typeName + '-' + fieldSchema.field_name + '-' + inputIdCounter;

                    var label = document.createElement('label');
                    label.textContent = fieldSchema.label + (fieldSchema.required ? ' *' : '');
                    label.setAttribute('for', inputId);
                    wrapper.appendChild(label);

                    var rawValue = data[fieldSchema.field_name];
                    var input = createInput(fieldSchema, rawValue);
                    input.id = inputId;
                    input.dataset.subfield = fieldSchema.field_name;
                    // Preserve original structured value for types that aren't text-editable
                    if (rawValue && typeof rawValue === 'object' && !rawValue.format && rawValue.value === undefined) {
                        input.dataset.structuredValue = JSON.stringify(rawValue);
                        // Clear structured value if user edits the field, so their
                        // text input takes precedence over the original object
                        input.addEventListener('input', function () {
                            delete input.dataset.structuredValue;
                        });
                    }
                    wrapper.appendChild(input);

                    fieldsContainer.appendChild(wrapper);
                });
            }

            section.appendChild(fieldsContainer);

            // Drag and drop
            section.addEventListener('dragstart', function (e) {
                e.dataTransfer.effectAllowed = 'move';
                section.classList.add('compound-section-editor--dragging');
            });
            section.addEventListener('dragend', function () {
                section.classList.remove('compound-section-editor--dragging');
                sync();
            });
            section.addEventListener('dragover', function (e) {
                e.preventDefault();
                e.dataTransfer.dropEffect = 'move';
                var dragging = sectionsEl.querySelector('.compound-section-editor--dragging');
                if (dragging && dragging !== section) {
                    var rect = section.getBoundingClientRect();
                    var midY = rect.top + rect.height / 2;
                    if (e.clientY < midY) {
                        // Only move if not already in position (avoids excessive reflows)
                        if (section.previousElementSibling !== dragging) {
                            sectionsEl.insertBefore(dragging, section);
                        }
                    } else {
                        if (section.nextElementSibling !== dragging) {
                            sectionsEl.insertBefore(dragging, section.nextSibling);
                        }
                    }
                }
            });
            section.addEventListener('drop', function (e) {
                e.preventDefault();
            });

            sectionsEl.appendChild(section);
        }

        function createInput(fieldSchema, value) {
            var ft = fieldSchema.field_type;
            // ft can be a string or an object like {Text: {max_length: 255}}
            var typeName = typeof ft === 'string' ? ft : Object.keys(ft)[0];

            // Preserve format metadata for text fields (e.g. {value: "...", format: "filtered_html"})
            var format = (value && typeof value === 'object' && value.format) ? value.format : null;

            if (typeName === 'TextLong') {
                var ta = document.createElement('textarea');
                ta.className = 'form-control compound-section-editor__input';
                ta.rows = 5;
                ta.value = extractValue(value);
                if (format) ta.dataset.format = format;
                if (fieldSchema.required) ta.required = true;
                return ta;
            }

            if (typeName === 'Boolean') {
                var cb = document.createElement('input');
                cb.type = 'checkbox';
                cb.className = 'compound-section-editor__input';
                cb.checked = !!extractValue(value);
                return cb;
            }

            if (typeName === 'Integer') {
                var num = document.createElement('input');
                num.type = 'number';
                num.className = 'form-control compound-section-editor__input';
                var numVal = extractValue(value);
                num.value = (numVal !== '' && numVal !== null && numVal !== undefined) ? numVal : '';
                if (fieldSchema.required) num.required = true;
                return num;
            }

            if (typeName === 'Float') {
                var fl = document.createElement('input');
                fl.type = 'number';
                fl.step = 'any';
                fl.className = 'form-control compound-section-editor__input';
                var flVal = extractValue(value);
                fl.value = (flVal !== '' && flVal !== null && flVal !== undefined) ? flVal : '';
                if (fieldSchema.required) fl.required = true;
                return fl;
            }

            if (typeName === 'Date') {
                var dt = document.createElement('input');
                dt.type = 'date';
                dt.className = 'form-control compound-section-editor__input';
                var dtVal = extractValue(value);
                dt.value = (dtVal !== '' && dtVal !== null && dtVal !== undefined) ? dtVal : '';
                if (fieldSchema.required) dt.required = true;
                return dt;
            }

            if (typeName === 'Email') {
                var em = document.createElement('input');
                em.type = 'email';
                em.className = 'form-control compound-section-editor__input';
                var emVal = extractValue(value);
                em.value = (emVal !== '' && emVal !== null && emVal !== undefined) ? emVal : '';
                if (fieldSchema.required) em.required = true;
                return em;
            }

            // Default: text input
            var inp = document.createElement('input');
            inp.type = 'text';
            inp.className = 'form-control compound-section-editor__input';
            inp.value = extractValue(value);
            if (fieldSchema.required) inp.required = true;
            if (typeName === 'Text' && ft.Text && ft.Text.max_length) {
                inp.maxLength = ft.Text.max_length;
            }
            return inp;
        }

        function extractValue(val) {
            if (val === null || val === undefined) return '';
            if (typeof val === 'object' && val.value !== undefined) return val.value;
            return val;
        }

        function sync() {
            var sections = [];
            var items = sectionsEl.querySelectorAll('.compound-section-editor');
            items.forEach(function (el, idx) {
                var data = {};
                el.querySelectorAll('[data-subfield]').forEach(function (input) {
                    var name = input.dataset.subfield;
                    if (input.type === 'checkbox') {
                        data[name] = input.checked;
                    } else if (input.dataset.format) {
                        // Preserve {value, format} structure for text fields
                        data[name] = { value: input.value, format: input.dataset.format };
                    } else if (input.dataset.structuredValue) {
                        // Preserve original structured object (e.g. RecordReference)
                        try {
                            data[name] = JSON.parse(input.dataset.structuredValue);
                        } catch (e) {
                            data[name] = input.value;
                        }
                    } else {
                        data[name] = input.value;
                    }
                });
                sections.push({
                    type: el.dataset.type,
                    weight: idx,
                    data: data
                });
            });
            hiddenInput.value = JSON.stringify({ sections: sections });
        }

        // Sync on form submit (single authoritative sync point)
        var form = container.closest('form');
        if (form) {
            form.addEventListener('submit', function (e) {
                sync();
                // Enforce min_items on submit
                if (minItems !== null && sectionsEl.children.length < minItems) {
                    e.preventDefault();
                    alert(fieldLabel + ' requires at least ' + minItems + ' section(s).');
                }
                // Enforce max_items on submit
                if (maxItems !== null && sectionsEl.children.length > maxItems) {
                    e.preventDefault();
                    alert(fieldLabel + ' allows at most ' + maxItems + ' section(s).');
                }
            });
        }
    }

    function escapeHtml(text) {
        var div = document.createElement('div');
        div.textContent = text;
        return div.innerHTML;
    }

    // Initialize on DOM ready
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initCompoundFields);
    } else {
        initCompoundFields();
    }

    window.initCompoundFields = initCompoundFields;
})();
