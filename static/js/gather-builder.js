/**
 * Gather query builder — block-based UI for composing Gather queries.
 *
 * Manages filter, sort, and relationship blocks on the gather admin
 * form, syncing state to hidden JSON inputs (#definition_json and
 * #display_json) for server-side persistence. Provides live preview
 * via AJAX POST to /api/gather/query.
 */
(function () {
    'use strict';

    // -------------------------------------------------------------------------
    // Constants
    // -------------------------------------------------------------------------

    var OPERATORS = [
        { value: 'equals',       label: 'Equals' },
        { value: 'not_equals',   label: 'Not equals' },
        { value: 'contains',     label: 'Contains' },
        { value: 'not_contains', label: 'Not contains' },
        { value: 'greater_than', label: 'Greater than' },
        { value: 'less_than',    label: 'Less than' },
        { value: 'in',           label: 'In' },
        { value: 'not_in',       label: 'Not in' },
        { value: 'is_null',           label: 'Is null' },
        { value: 'is_not_null',        label: 'Is not null' },
        { value: 'full_text_search',   label: 'Full-text search' }
    ];

    var JOIN_TYPES = [
        { value: 'inner', label: 'Inner join' },
        { value: 'left',  label: 'Left join' },
        { value: 'right', label: 'Right join' }
    ];

    var MAX_RELATIONSHIPS = 3;

    var STAGE_AWARE_TABLES = { item: true };

    // -------------------------------------------------------------------------
    // State
    // -------------------------------------------------------------------------

    var definition = {
        base_table: 'item',
        filters: [],
        sorts: [],
        relationships: [],
        includes: {},
        fields: [],
        stage_aware: true
    };

    var display = {
        format: 'table',
        items_per_page: 10,
        pager: { enabled: true, show_count: false, style: 'full' },
        empty_text: ''
    };

    // DOM references (populated in init)
    var definitionInput;
    var displayInput;
    var filtersContainer;
    var sortsContainer;
    var relationshipsContainer;
    var includesContainer;
    var addFilterBtn;
    var addSortBtn;
    var addRelationshipBtn;
    var addIncludeBtn;
    var previewBtn;
    var previewResults;
    var baseTableSelect;
    var displayFormatSelect;
    var itemsPerPageInput;
    var emptyTextInput;
    var pagerEnabledCheckbox;
    var pagerShowCountCheckbox;

    // Preview debounce timer
    var previewTimer = null;

    // -------------------------------------------------------------------------
    // Initialization
    // -------------------------------------------------------------------------

    function init() {
        // Locate DOM elements
        definitionInput        = document.getElementById('definition_json');
        displayInput           = document.getElementById('display_json');
        filtersContainer       = document.getElementById('filters-container');
        sortsContainer         = document.getElementById('sorts-container');
        relationshipsContainer = document.getElementById('relationships-container');
        includesContainer      = document.getElementById('includes-container');
        addFilterBtn           = document.getElementById('add-filter');
        addSortBtn             = document.getElementById('add-sort');
        addRelationshipBtn     = document.getElementById('add-relationship');
        addIncludeBtn          = document.getElementById('add-include');
        previewBtn             = document.getElementById('preview-btn');
        previewResults         = document.getElementById('preview-results');
        baseTableSelect        = document.getElementById('base_table');
        displayFormatSelect    = document.getElementById('display_format');
        itemsPerPageInput      = document.getElementById('items_per_page');
        emptyTextInput         = document.getElementById('empty_text');
        pagerEnabledCheckbox   = document.getElementById('pager_enabled');
        pagerShowCountCheckbox = document.getElementById('pager_show_count');

        // Bail if the form is not present on this page
        if (!definitionInput || !displayInput) return;

        // Hydrate state from hidden inputs
        hydrateState();

        // Render existing blocks
        renderAllBlocks();

        // Sync display fields from state into their inputs
        syncDisplayFieldsFromState();

        // Bind event listeners
        bindAddButtons();
        bindDisplayListeners();
        bindBaseTableListener();
        bindPreviewButton();
        bindFormSubmit();
    }

    function hydrateState() {
        try {
            var defVal = definitionInput.value;
            if (defVal && defVal !== '{}') {
                var parsed = JSON.parse(defVal);
                definition.base_table    = parsed.base_table    || definition.base_table;
                definition.filters       = parsed.filters       || [];
                definition.sorts         = parsed.sorts         || [];
                definition.relationships = parsed.relationships || [];
                definition.includes      = parsed.includes      || {};
                definition.fields        = parsed.fields        || [];
                definition.stage_aware   = parsed.stage_aware !== undefined ? parsed.stage_aware : (STAGE_AWARE_TABLES[definition.base_table] || false);
            }
        } catch (e) {
            // Keep defaults on parse error
        }

        try {
            var dispVal = displayInput.value;
            if (dispVal && dispVal !== '{}') {
                var parsed = JSON.parse(dispVal);
                display.format         = parsed.format         || display.format;
                display.items_per_page = parsed.items_per_page || display.items_per_page;
                display.empty_text     = parsed.empty_text !== undefined ? parsed.empty_text : display.empty_text;
                if (parsed.pager) {
                    display.pager.enabled    = parsed.pager.enabled !== undefined ? parsed.pager.enabled : display.pager.enabled;
                    display.pager.show_count = parsed.pager.show_count !== undefined ? parsed.pager.show_count : display.pager.show_count;
                    display.pager.style      = parsed.pager.style || display.pager.style;
                }
            }
        } catch (e) {
            // Keep defaults on parse error
        }
    }

    /**
     * Push current display state values into the visible form controls
     * so the UI matches whatever was persisted in the hidden input.
     */
    function syncDisplayFieldsFromState() {
        if (displayFormatSelect)    displayFormatSelect.value    = display.format;
        if (itemsPerPageInput)      itemsPerPageInput.value      = display.items_per_page;
        if (emptyTextInput)         emptyTextInput.value         = display.empty_text;
        if (pagerEnabledCheckbox)   pagerEnabledCheckbox.checked = display.pager.enabled;
        if (pagerShowCountCheckbox) pagerShowCountCheckbox.checked = display.pager.show_count;
        if (baseTableSelect)        baseTableSelect.value        = definition.base_table;
    }

    // -------------------------------------------------------------------------
    // Rendering
    // -------------------------------------------------------------------------

    function renderAllBlocks() {
        // Filters
        filtersContainer.innerHTML = '';
        definition.filters.forEach(function (f, i) {
            filtersContainer.appendChild(createFilterRow(f, i));
        });

        // Sorts
        sortsContainer.innerHTML = '';
        definition.sorts.forEach(function (s, i) {
            sortsContainer.appendChild(createSortRow(s, i));
        });

        // Relationships
        relationshipsContainer.innerHTML = '';
        definition.relationships.forEach(function (r, i) {
            relationshipsContainer.appendChild(createRelationshipRow(r, i));
        });

        updateRelationshipButton();

        // Includes
        if (includesContainer) {
            includesContainer.innerHTML = '';
            var names = Object.keys(definition.includes || {});
            names.forEach(function (name) {
                includesContainer.appendChild(createIncludeRow(name, definition.includes[name]));
            });
        }
    }

    // -------------------------------------------------------------------------
    // Filter blocks
    // -------------------------------------------------------------------------

    function createFilterRow(data, index) {
        var row = document.createElement('div');
        row.className = 'gather-block-row gather-block-row--filter';

        // Field name
        var fieldInput = document.createElement('input');
        fieldInput.type = 'text';
        fieldInput.className = 'form-text';
        fieldInput.placeholder = 'Field name';
        fieldInput.value = data.field || '';
        fieldInput.setAttribute('aria-label', 'Filter field name');

        // Operator
        var opSelect = document.createElement('select');
        opSelect.className = 'form-select';
        opSelect.setAttribute('aria-label', 'Filter operator');
        OPERATORS.forEach(function (op) {
            var opt = document.createElement('option');
            opt.value = op.value;
            opt.textContent = op.label;
            if (data.operator === op.value) opt.selected = true;
            opSelect.appendChild(opt);
        });

        // Value
        var valueInput = document.createElement('input');
        valueInput.type = 'text';
        valueInput.className = 'form-text';
        valueInput.placeholder = 'Value';
        valueInput.value = data.value !== undefined && data.value !== null ? data.value : '';
        valueInput.setAttribute('aria-label', 'Filter value');

        // Disable value for null operators
        function updateValueState() {
            var op = opSelect.value;
            var isNullOp = (op === 'is_null' || op === 'is_not_null');
            valueInput.disabled = isNullOp;
            if (isNullOp) valueInput.value = '';
        }
        updateValueState();

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = 'Remove';
        removeBtn.setAttribute('aria-label', 'Remove filter');

        // Events
        fieldInput.addEventListener('input', function () {
            readFiltersFromDOM();
            syncToHiddenInputs();
        });
        opSelect.addEventListener('change', function () {
            updateValueState();
            readFiltersFromDOM();
            syncToHiddenInputs();
        });
        valueInput.addEventListener('input', function () {
            readFiltersFromDOM();
            syncToHiddenInputs();
        });
        removeBtn.addEventListener('click', function () {
            row.remove();
            readFiltersFromDOM();
            syncToHiddenInputs();
        });

        row.appendChild(fieldInput);
        row.appendChild(opSelect);
        row.appendChild(valueInput);
        row.appendChild(removeBtn);

        return row;
    }

    function readFiltersFromDOM() {
        var rows = filtersContainer.querySelectorAll('.gather-block-row--filter');
        definition.filters = [];
        rows.forEach(function (row) {
            var field    = row.querySelector('input[placeholder="Field name"]').value.trim();
            var operator = row.querySelector('select').value;
            var value    = row.querySelector('input[placeholder="Value"]').value;
            var filter   = { field: field, operator: operator };
            if (operator !== 'is_null' && operator !== 'is_not_null') {
                filter.value = value;
            }
            definition.filters.push(filter);
        });
    }

    // -------------------------------------------------------------------------
    // Sort blocks
    // -------------------------------------------------------------------------

    function createSortRow(data, index) {
        var row = document.createElement('div');
        row.className = 'gather-block-row gather-block-row--sort';

        // Field name
        var fieldInput = document.createElement('input');
        fieldInput.type = 'text';
        fieldInput.className = 'form-text';
        fieldInput.placeholder = 'Field name';
        fieldInput.value = data.field || '';
        fieldInput.setAttribute('aria-label', 'Sort field name');

        // Direction
        var dirSelect = document.createElement('select');
        dirSelect.className = 'form-select';
        dirSelect.setAttribute('aria-label', 'Sort direction');
        [{ value: 'asc', label: 'Ascending' }, { value: 'desc', label: 'Descending' }].forEach(function (d) {
            var opt = document.createElement('option');
            opt.value = d.value;
            opt.textContent = d.label;
            if (data.direction === d.value) opt.selected = true;
            dirSelect.appendChild(opt);
        });

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = 'Remove';
        removeBtn.setAttribute('aria-label', 'Remove sort');

        // Events
        fieldInput.addEventListener('input', function () {
            readSortsFromDOM();
            syncToHiddenInputs();
        });
        dirSelect.addEventListener('change', function () {
            readSortsFromDOM();
            syncToHiddenInputs();
        });
        removeBtn.addEventListener('click', function () {
            row.remove();
            readSortsFromDOM();
            syncToHiddenInputs();
        });

        row.appendChild(fieldInput);
        row.appendChild(dirSelect);
        row.appendChild(removeBtn);

        return row;
    }

    function readSortsFromDOM() {
        var rows = sortsContainer.querySelectorAll('.gather-block-row--sort');
        definition.sorts = [];
        rows.forEach(function (row) {
            var field     = row.querySelector('input[placeholder="Field name"]').value.trim();
            var direction = row.querySelector('select').value;
            definition.sorts.push({ field: field, direction: direction });
        });
    }

    // -------------------------------------------------------------------------
    // Relationship blocks
    // -------------------------------------------------------------------------

    function createRelationshipRow(data, index) {
        var row = document.createElement('div');
        row.className = 'gather-block-row gather-block-row--relationship';

        // Join type
        var joinSelect = document.createElement('select');
        joinSelect.className = 'form-select';
        joinSelect.setAttribute('aria-label', 'Join type');
        JOIN_TYPES.forEach(function (jt) {
            var opt = document.createElement('option');
            opt.value = jt.value;
            opt.textContent = jt.label;
            if (data.join_type === jt.value) opt.selected = true;
            joinSelect.appendChild(opt);
        });

        // Target table
        var targetInput = document.createElement('input');
        targetInput.type = 'text';
        targetInput.className = 'form-text';
        targetInput.placeholder = 'Target table';
        targetInput.value = data.target_table || '';
        targetInput.setAttribute('aria-label', 'Target table');

        // Local field
        var localInput = document.createElement('input');
        localInput.type = 'text';
        localInput.className = 'form-text';
        localInput.placeholder = 'Local field';
        localInput.value = data.local_field || '';
        localInput.setAttribute('aria-label', 'Local field');

        // Foreign field
        var foreignInput = document.createElement('input');
        foreignInput.type = 'text';
        foreignInput.className = 'form-text';
        foreignInput.placeholder = 'Foreign field';
        foreignInput.value = data.foreign_field || '';
        foreignInput.setAttribute('aria-label', 'Foreign field');

        // Name (used as table alias in joins)
        var nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'form-text';
        nameInput.placeholder = 'Name (alias)';
        nameInput.value = data.name || '';
        nameInput.setAttribute('aria-label', 'Relationship name');

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = 'Remove';
        removeBtn.setAttribute('aria-label', 'Remove relationship');

        // Change handler shared by all inputs in this row
        function onChange() {
            readRelationshipsFromDOM();
            syncToHiddenInputs();
        }

        joinSelect.addEventListener('change', onChange);
        targetInput.addEventListener('input', onChange);
        localInput.addEventListener('input', onChange);
        foreignInput.addEventListener('input', onChange);
        nameInput.addEventListener('input', onChange);
        removeBtn.addEventListener('click', function () {
            row.remove();
            readRelationshipsFromDOM();
            syncToHiddenInputs();
            updateRelationshipButton();
        });

        row.appendChild(nameInput);
        row.appendChild(joinSelect);
        row.appendChild(targetInput);
        row.appendChild(localInput);
        row.appendChild(foreignInput);
        row.appendChild(removeBtn);

        return row;
    }

    function readRelationshipsFromDOM() {
        var rows = relationshipsContainer.querySelectorAll('.gather-block-row--relationship');
        definition.relationships = [];
        rows.forEach(function (row) {
            var inputs = row.querySelectorAll('input');
            var select = row.querySelector('select');
            var rel = {
                name:          inputs[0].value.trim(),
                join_type:     select.value,
                target_table:  inputs[1].value.trim(),
                local_field:   inputs[2].value.trim(),
                foreign_field: inputs[3].value.trim()
            };
            definition.relationships.push(rel);
        });
    }

    function updateRelationshipButton() {
        if (!addRelationshipBtn) return;
        var count = relationshipsContainer.querySelectorAll('.gather-block-row--relationship').length;
        if (count >= MAX_RELATIONSHIPS) {
            addRelationshipBtn.disabled = true;
            addRelationshipBtn.title = 'Maximum of ' + MAX_RELATIONSHIPS + ' relationships reached';
        } else {
            addRelationshipBtn.disabled = false;
            addRelationshipBtn.title = '';
        }
    }

    // -------------------------------------------------------------------------
    // Include blocks (Story 23.10)
    // -------------------------------------------------------------------------

    function createIncludeRow(name, data) {
        var row = document.createElement('div');
        row.className = 'gather-block-row gather-block-row--include';
        row.style.cssText = 'border: 1px solid var(--gray-300, #ccc); padding: 0.75rem; border-radius: 0.25rem; margin-bottom: 0.5rem;';

        // Name
        var nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'form-text';
        nameInput.placeholder = 'Include name';
        nameInput.value = name || '';
        nameInput.setAttribute('aria-label', 'Include name');
        nameInput.dataset.role = 'include-name';

        // Parent field
        var parentInput = document.createElement('input');
        parentInput.type = 'text';
        parentInput.className = 'form-text';
        parentInput.placeholder = 'Parent field (e.g., id)';
        parentInput.value = data.parent_field || '';
        parentInput.setAttribute('aria-label', 'Parent field');
        parentInput.dataset.role = 'parent-field';

        // Child field
        var childInput = document.createElement('input');
        childInput.type = 'text';
        childInput.className = 'form-text';
        childInput.placeholder = 'Child field (e.g., fields.story_id)';
        childInput.value = data.child_field || '';
        childInput.setAttribute('aria-label', 'Child field');
        childInput.dataset.role = 'child-field';

        // Child base table
        var childTableInput = document.createElement('input');
        childTableInput.type = 'text';
        childTableInput.className = 'form-text';
        childTableInput.placeholder = 'Child base table';
        childTableInput.value = (data.definition && data.definition.base_table) || 'item';
        childTableInput.setAttribute('aria-label', 'Child base table');
        childTableInput.dataset.role = 'child-table';

        // Child item type
        var childTypeInput = document.createElement('input');
        childTypeInput.type = 'text';
        childTypeInput.className = 'form-text';
        childTypeInput.placeholder = 'Child item type (optional)';
        childTypeInput.value = (data.definition && data.definition.item_type) || '';
        childTypeInput.setAttribute('aria-label', 'Child item type');
        childTypeInput.dataset.role = 'child-type';

        // Singular checkbox
        var singularLabel = document.createElement('label');
        singularLabel.style.cssText = 'display: inline-flex; align-items: center; gap: 0.25rem;';
        var singularCheckbox = document.createElement('input');
        singularCheckbox.type = 'checkbox';
        singularCheckbox.checked = data.singular || false;
        singularCheckbox.dataset.role = 'singular';
        singularLabel.appendChild(singularCheckbox);
        singularLabel.appendChild(document.createTextNode(' Singular (single object, not array)'));

        // Limit
        var limitInput = document.createElement('input');
        limitInput.type = 'number';
        limitInput.className = 'form-text';
        limitInput.placeholder = 'Limit (default: 1000)';
        limitInput.value = (data.display && data.display.items_per_page) || '';
        limitInput.setAttribute('aria-label', 'Include limit');
        limitInput.dataset.role = 'include-limit';
        limitInput.style.width = '120px';

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = 'Remove';
        removeBtn.setAttribute('aria-label', 'Remove include');

        // Events
        function onChange() {
            readIncludesFromDOM();
            syncToHiddenInputs();
        }

        nameInput.addEventListener('input', onChange);
        parentInput.addEventListener('input', onChange);
        childInput.addEventListener('input', onChange);
        childTableInput.addEventListener('input', onChange);
        childTypeInput.addEventListener('input', onChange);
        singularCheckbox.addEventListener('change', onChange);
        limitInput.addEventListener('input', onChange);
        removeBtn.addEventListener('click', function () {
            row.remove();
            readIncludesFromDOM();
            syncToHiddenInputs();
        });

        // Layout
        var grid = document.createElement('div');
        grid.style.cssText = 'display: grid; grid-template-columns: 1fr 1fr; gap: 0.5rem; margin-bottom: 0.5rem;';
        grid.appendChild(nameInput);
        grid.appendChild(childTableInput);
        grid.appendChild(parentInput);
        grid.appendChild(childInput);
        grid.appendChild(childTypeInput);
        grid.appendChild(limitInput);

        row.appendChild(grid);
        row.appendChild(singularLabel);
        row.appendChild(removeBtn);

        return row;
    }

    function readIncludesFromDOM() {
        if (!includesContainer) return;
        var rows = includesContainer.querySelectorAll('.gather-block-row--include');
        definition.includes = {};
        rows.forEach(function (row) {
            var name = row.querySelector('[data-role="include-name"]').value.trim();
            if (!name) return;

            var parentField = row.querySelector('[data-role="parent-field"]').value.trim();
            var childField = row.querySelector('[data-role="child-field"]').value.trim();
            var childTable = row.querySelector('[data-role="child-table"]').value.trim() || 'item';
            var childType = row.querySelector('[data-role="child-type"]').value.trim();
            var singular = row.querySelector('[data-role="singular"]').checked;
            var limitVal = parseInt(row.querySelector('[data-role="include-limit"]').value, 10);

            var childDef = { base_table: childTable };
            if (childType) childDef.item_type = childType;

            var include = {
                definition: childDef,
                parent_field: parentField,
                child_field: childField,
                singular: singular
            };

            if (!isNaN(limitVal) && limitVal > 0) {
                include.display = { items_per_page: limitVal };
            }

            definition.includes[name] = include;
        });
    }

    // -------------------------------------------------------------------------
    // Add buttons
    // -------------------------------------------------------------------------

    function bindAddButtons() {
        if (addFilterBtn) {
            addFilterBtn.addEventListener('click', function () {
                var newFilter = { field: '', operator: 'equals', value: '' };
                definition.filters.push(newFilter);
                filtersContainer.appendChild(createFilterRow(newFilter, definition.filters.length - 1));
                syncToHiddenInputs();
            });
        }

        if (addSortBtn) {
            addSortBtn.addEventListener('click', function () {
                var newSort = { field: '', direction: 'asc' };
                definition.sorts.push(newSort);
                sortsContainer.appendChild(createSortRow(newSort, definition.sorts.length - 1));
                syncToHiddenInputs();
            });
        }

        if (addRelationshipBtn) {
            addRelationshipBtn.addEventListener('click', function () {
                if (definition.relationships.length >= MAX_RELATIONSHIPS) return;
                var newRel = { name: '', join_type: 'inner', target_table: '', local_field: '', foreign_field: '' };
                definition.relationships.push(newRel);
                relationshipsContainer.appendChild(createRelationshipRow(newRel, definition.relationships.length - 1));
                syncToHiddenInputs();
                updateRelationshipButton();
            });
        }

        if (addIncludeBtn) {
            addIncludeBtn.addEventListener('click', function () {
                var newInclude = {
                    definition: { base_table: 'item' },
                    parent_field: 'id',
                    child_field: '',
                    singular: false
                };
                var name = 'include_' + Date.now();
                definition.includes[name] = newInclude;
                if (includesContainer) {
                    includesContainer.appendChild(createIncludeRow(name, newInclude));
                }
                syncToHiddenInputs();
            });
        }
    }

    // -------------------------------------------------------------------------
    // Display config sync
    // -------------------------------------------------------------------------

    function bindDisplayListeners() {
        if (displayFormatSelect) {
            displayFormatSelect.addEventListener('change', function () {
                display.format = displayFormatSelect.value;
                syncToHiddenInputs();
            });
        }

        if (itemsPerPageInput) {
            itemsPerPageInput.addEventListener('input', function () {
                var val = parseInt(itemsPerPageInput.value, 10);
                if (!isNaN(val) && val > 0) {
                    display.items_per_page = val;
                }
                syncToHiddenInputs();
            });
        }

        if (emptyTextInput) {
            emptyTextInput.addEventListener('input', function () {
                display.empty_text = emptyTextInput.value;
                syncToHiddenInputs();
            });
        }

        if (pagerEnabledCheckbox) {
            pagerEnabledCheckbox.addEventListener('change', function () {
                display.pager.enabled = pagerEnabledCheckbox.checked;
                syncToHiddenInputs();
            });
        }

        if (pagerShowCountCheckbox) {
            pagerShowCountCheckbox.addEventListener('change', function () {
                display.pager.show_count = pagerShowCountCheckbox.checked;
                syncToHiddenInputs();
            });
        }
    }

    // -------------------------------------------------------------------------
    // Base table sync
    // -------------------------------------------------------------------------

    function bindBaseTableListener() {
        if (!baseTableSelect) return;

        baseTableSelect.addEventListener('change', function () {
            definition.base_table = baseTableSelect.value;
            definition.stage_aware = !!STAGE_AWARE_TABLES[definition.base_table];
            syncToHiddenInputs();
        });
    }

    // -------------------------------------------------------------------------
    // Serialization
    // -------------------------------------------------------------------------

    function syncToHiddenInputs() {
        definitionInput.value = JSON.stringify(definition);
        displayInput.value = JSON.stringify(display);
    }

    // -------------------------------------------------------------------------
    // Live preview (Story 23.3)
    // -------------------------------------------------------------------------

    function bindPreviewButton() {
        if (!previewBtn) return;

        previewBtn.addEventListener('click', function () {
            loadPreview();
        });
    }

    function loadPreview() {
        // Ensure latest state is serialized
        syncToHiddenInputs();

        // Disable button during request
        previewBtn.disabled = true;
        previewBtn.textContent = 'Loading...';

        // Clear previous results
        previewResults.innerHTML = '';
        var loadingMsg = document.createElement('p');
        loadingMsg.textContent = 'Loading preview...';
        loadingMsg.className = 'gather-preview__loading';
        previewResults.appendChild(loadingMsg);

        var payload = JSON.stringify({
            definition: definition,
            display: display
        });

        var xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/gather/query', true);
        xhr.setRequestHeader('Content-Type', 'application/json');

        xhr.addEventListener('load', function () {
            previewBtn.disabled = false;
            previewBtn.textContent = 'Load preview';
            previewResults.innerHTML = '';

            if (xhr.status === 200) {
                try {
                    var response = JSON.parse(xhr.responseText);
                    renderPreviewResults(response);
                } catch (e) {
                    showPreviewError('Invalid response from server.');
                }
            } else {
                try {
                    var errResponse = JSON.parse(xhr.responseText);
                    showPreviewError(errResponse.error || 'Preview failed (' + xhr.status + ')');
                } catch (e) {
                    showPreviewError('Preview failed (' + xhr.status + ')');
                }
            }
        });

        xhr.addEventListener('error', function () {
            previewBtn.disabled = false;
            previewBtn.textContent = 'Load preview';
            previewResults.innerHTML = '';
            showPreviewError('Network error. Please check your connection.');
        });

        xhr.send(payload);
    }

    function renderPreviewResults(response) {
        var rows = response.rows || response.results || response.data || [];

        if (!rows.length) {
            var empty = document.createElement('p');
            empty.className = 'gather-preview__empty';
            empty.textContent = display.empty_text || 'No results found.';
            previewResults.appendChild(empty);
            return;
        }

        // Build an HTML table from the result rows
        var table = document.createElement('table');
        table.className = 'gather-preview__table';

        // Header row: extract keys from the first result
        var keys = Object.keys(rows[0]);
        var thead = document.createElement('thead');
        var headerRow = document.createElement('tr');
        keys.forEach(function (key) {
            var th = document.createElement('th');
            th.textContent = key;
            headerRow.appendChild(th);
        });
        thead.appendChild(headerRow);
        table.appendChild(thead);

        // Data rows
        var tbody = document.createElement('tbody');
        rows.forEach(function (row) {
            var tr = document.createElement('tr');
            keys.forEach(function (key) {
                var td = document.createElement('td');
                var val = row[key];
                // Safely render as text
                td.textContent = (val !== null && val !== undefined) ? String(val) : '';
                tr.appendChild(td);
            });
            tbody.appendChild(tr);
        });
        table.appendChild(tbody);

        previewResults.appendChild(table);

        // Row count summary
        if (response.total !== undefined) {
            var summary = document.createElement('p');
            summary.className = 'gather-preview__summary';
            summary.textContent = 'Showing ' + rows.length + ' of ' + response.total + ' results.';
            previewResults.appendChild(summary);
        }
    }

    function showPreviewError(message) {
        var err = document.createElement('div');
        err.className = 'gather-preview__error';
        err.textContent = message;
        previewResults.appendChild(err);
    }

    /**
     * Debounced preview — reserved for future auto-preview on change.
     * Currently only the manual button trigger is wired up.
     */
    function debouncedPreview() {
        if (previewTimer) clearTimeout(previewTimer);
        previewTimer = setTimeout(function () {
            loadPreview();
        }, 500);
    }

    // -------------------------------------------------------------------------
    // Form submit
    // -------------------------------------------------------------------------

    function bindFormSubmit() {
        var form = document.getElementById('gather-form');
        if (!form) return;

        form.addEventListener('submit', function () {
            // Final sync before the form data is sent
            readFiltersFromDOM();
            readSortsFromDOM();
            readRelationshipsFromDOM();
            readIncludesFromDOM();
            syncToHiddenInputs();
        });
    }

    // -------------------------------------------------------------------------
    // Bootstrap
    // -------------------------------------------------------------------------

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }

    // Expose for potential re-initialization after AJAX page loads
    window.initGatherBuilder = init;
})();
