/**
 * Gather query builder — block-based UI for composing Gather queries.
 *
 * Manages filter, sort, and relationship blocks on the gather admin
 * form, syncing state to hidden JSON inputs (#definition_json and
 * #display_json) for server-side persistence. Provides live preview
 * via AJAX POST to /api/v1/gather/query.
 *
 * Every word this script puts on the page, the option lists included, comes
 * from the translated <script type="application/json"> element with
 * id="gather-i18n" (B253): none of it is a literal here.
 */
(function () {
    'use strict';

    function readJson(id) {
        var el = document.getElementById(id);
        if (!el) return null;
        try { return JSON.parse(el.textContent); } catch (e) { return null; }
    }

    /** Replace each `{name}` in `template` with `values[name]`. */
    function fillTemplate(template, values) {
        return template.replace(/\{(\w+)\}/g, function (match, name) {
            return Object.prototype.hasOwnProperty.call(values, name) ? values[name] : match;
        });
    }

    // -------------------------------------------------------------------------
    // Constants
    // -------------------------------------------------------------------------

    var I18N = readJson('gather-i18n') || { operators: [], join_types: [], sort_directions: [], strings: {} };

    var OPERATORS = I18N.operators;

    var JOIN_TYPES = I18N.join_types;

    var SORT_DIRECTIONS = I18N.sort_directions;

    var MAX_RELATIONSHIPS = 3;

    var STAGE_AWARE_TABLES = { item: true };

    // Operators whose value is a list (one value per line or comma
    // separated), not a single value.
    var LIST_OPERATORS = { in: true, not_in: true, has_any_tag: true, has_all_tags: true };

    // Operators that take no value at all.
    var NO_VALUE_OPERATORS = { is_null: true, is_not_null: true };

    // The bare-string spellings `ContextualValue`'s unit variants serialize
    // as (`crates/kernel/src/gather/types.rs`). A fixed value typed as one of
    // these would silently read back as that contextual value, not as the
    // literal text, so the form refuses it (B279).
    var RESERVED_CONTEXTUAL_KEYWORDS = ['current_item', 'current_user', 'current_time', 'current_date'];

    // Refreshers the base-table listener calls so every filter row's operator
    // list drops (or restores) `semantic_similarity` when the table changes.
    var operatorSelectRefreshers = [];

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
        items_per_page: null,
        pager: { enabled: true, show_count: false, style: 'full', zero_based: false },
        empty_text: '',
        canonical_url: ''
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
    var canonicalUrlInput;
    var pagerEnabledCheckbox;
    var pagerShowCountCheckbox;
    var pagerZeroBasedCheckbox;
    var feedSettings;
    var feedItemsInput;
    var feedFullContentCheckbox;

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
        canonicalUrlInput      = document.getElementById('canonical_url');
        pagerEnabledCheckbox   = document.getElementById('pager_enabled');
        pagerShowCountCheckbox = document.getElementById('pager_show_count');
        pagerZeroBasedCheckbox = document.getElementById('pager_zero_based');
        feedSettings           = document.getElementById('feed-settings');
        feedItemsInput         = document.getElementById('feed_items');
        feedFullContentCheckbox = document.getElementById('feed_full_content');

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
                // Not edited here, but kept: dropping it on save would make a
                // gather that requires a permission public.
                if (parsed.access) { definition.access = parsed.access; }
                // Not shown here either, but carried, so the preview runs the
                // whole definition. The server refuses to save one carrying
                // them, because this form cannot show what it would save.
                ['filter_groups', 'group_by', 'aggregates', 'having', 'distinct'].forEach(function (key) {
                    if (parsed[key] !== undefined) { definition[key] = parsed[key]; }
                });
            }
        } catch (e) {
            // Keep defaults on parse error
        }

        try {
            var dispVal = displayInput.value;
            if (dispVal && dispVal !== '{}') {
                var parsed = JSON.parse(dispVal);
                display.format         = parsed.format         || display.format;
                display.items_per_page = parsed.items_per_page || null;
                display.empty_text     = parsed.empty_text !== undefined ? parsed.empty_text : display.empty_text;
                display.canonical_url  = parsed.canonical_url !== undefined ? parsed.canonical_url : display.canonical_url;
                if (parsed.pager) {
                    display.pager.enabled    = parsed.pager.enabled !== undefined ? parsed.pager.enabled : display.pager.enabled;
                    display.pager.show_count = parsed.pager.show_count !== undefined ? parsed.pager.show_count : display.pager.show_count;
                    display.pager.style      = parsed.pager.style || display.pager.style;
                    display.pager.zero_based = parsed.pager.zero_based === true;
                }
                // The feed, when the gather has one: its item count and full text
                // are edited here, the rest kept as stored.
                if (parsed.feed) {
                    display.feed = parsed.feed;
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
        if (itemsPerPageInput)      itemsPerPageInput.value      = display.items_per_page || '';
        if (emptyTextInput)         emptyTextInput.value         = display.empty_text;
        if (canonicalUrlInput)      canonicalUrlInput.value      = display.canonical_url;
        if (pagerEnabledCheckbox)   pagerEnabledCheckbox.checked = display.pager.enabled;
        if (pagerShowCountCheckbox) pagerShowCountCheckbox.checked = display.pager.show_count;
        if (pagerZeroBasedCheckbox) pagerZeroBasedCheckbox.checked = display.pager.zero_based;
        if (feedSettings) {
            feedSettings.hidden = !display.feed;
            if (display.feed) {
                if (feedItemsInput) feedItemsInput.value = display.feed.items || 20;
                if (feedFullContentCheckbox) feedFullContentCheckbox.checked = display.feed.full_content === true;
            }
        }
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

    /** `true` when `v` is a JSON object (not null, not an array). */
    function isPlainObject(v) {
        return v !== null && typeof v === 'object' && !Array.isArray(v);
    }

    /**
     * Read a stored `FilterValue` into the shape the value-source UI edits:
     * `{source, text, list, field, arg}`. `source` is one of `fixed`,
     * `current_item`, `current_item_field`, `url_arg`, `current_user`,
     * `current_time`, `current_date`, or `unknown` for an object shape this
     * builder does not recognize (kept verbatim like an unknown operator).
     */
    function parseValueShape(value) {
        if (value === null || value === undefined) {
            return { source: 'fixed', text: '', list: [] };
        }
        if (typeof value === 'string') {
            if (RESERVED_CONTEXTUAL_KEYWORDS.indexOf(value) !== -1) {
                return { source: value };
            }
            return { source: 'fixed', text: value, list: splitListInput(value) };
        }
        if (Array.isArray(value)) {
            var list = value.map(function (item) {
                return typeof item === 'string' ? item : JSON.stringify(item);
            });
            return { source: 'fixed', text: list.join(', '), list: list };
        }
        if (isPlainObject(value)) {
            if (typeof value.url_arg === 'string') return { source: 'url_arg', arg: value.url_arg };
            if (typeof value.current_item_field === 'string') {
                return { source: 'current_item_field', field: value.current_item_field };
            }
            return { source: 'unknown' };
        }
        // A number or boolean: shown as its text form, restored verbatim
        // (its JSON type kept) unless the user edits it.
        return { source: 'fixed', text: String(value), list: [value] };
    }

    /** One value per line, or comma separated (the hint states both). */
    function splitListInput(text) {
        return (text || '')
            .split(/[\n,]+/)
            .map(function (s) { return s.trim(); })
            .filter(function (s) { return s.length > 0; });
    }

    /** Build `<option>`s (grouped by `optgroup`) for one operator `<select>`. */
    function populateOperatorSelect(select, currentValue, unknownOperatorLabel) {
        select.innerHTML = '';
        if (unknownOperatorLabel !== null) {
            var unknownOpt = document.createElement('option');
            unknownOpt.value = '';
            unknownOpt.textContent = unknownOperatorLabel;
            unknownOpt.selected = true;
            select.appendChild(unknownOpt);
            select.disabled = true;
            return;
        }
        var group = null;
        var groupEl = null;
        OPERATORS.forEach(function (op) {
            if (op.item_only && definition.base_table !== 'item' && op.value !== currentValue) return;
            if (op.group !== group) {
                group = op.group;
                groupEl = document.createElement('optgroup');
                groupEl.label = group;
                select.appendChild(groupEl);
            }
            var opt = document.createElement('option');
            opt.value = op.value;
            opt.textContent = op.label;
            if (op.value === currentValue) opt.selected = true;
            groupEl.appendChild(opt);
        });
    }

    function createFilterRow(data, index, isNew) {
        var raw = data;
        var unknownOperator = typeof raw.operator !== 'string';
        var parsedValue = parseValueShape(raw.value);
        var unknown = unknownOperator || parsedValue.source === 'unknown';
        var valueTouched = !!isNew;

        var row = document.createElement('div');
        row.className = 'gather-block-row gather-block-row--filter';
        if (unknown) row.className += ' gather-filter-row--unknown';

        var main = document.createElement('div');
        main.className = 'gather-filter-row__main';

        // Field name
        var fieldInput = document.createElement('input');
        fieldInput.type = 'text';
        fieldInput.className = 'form-text';
        fieldInput.placeholder = I18N.strings.field_name;
        fieldInput.value = raw.field || '';
        fieldInput.setAttribute('aria-label', I18N.strings.filter_field_name);
        fieldInput.setAttribute('list', 'gather-item-columns');
        fieldInput.dataset.role = 'field-name';
        fieldInput.disabled = unknown;

        // Operator
        var opSelect = document.createElement('select');
        opSelect.className = 'form-select';
        opSelect.setAttribute('aria-label', I18N.strings.filter_operator);
        var unknownOperatorLabel = unknownOperator
            ? fillTemplate(I18N.strings.unknown_operator_option, {
                operator: raw.operator && raw.operator.custom ? raw.operator.custom : JSON.stringify(raw.operator),
            })
            : null;
        populateOperatorSelect(opSelect, unknownOperator ? '' : raw.operator, unknownOperatorLabel);
        operatorSelectRefreshers.push(function () {
            if (unknownOperator) return;
            populateOperatorSelect(opSelect, opSelect.value, null);
        });

        // Value source
        var sourceSelect = document.createElement('select');
        sourceSelect.className = 'form-select';
        sourceSelect.setAttribute('aria-label', I18N.strings.value_source);
        [
            ['fixed', I18N.strings.value_source_fixed],
            ['current_item', I18N.strings.value_source_current_item],
            ['current_item_field', I18N.strings.value_source_current_item_field],
            ['url_arg', I18N.strings.value_source_url_arg],
            ['current_user', I18N.strings.value_source_current_user],
            ['current_time', I18N.strings.value_source_current_time],
            ['current_date', I18N.strings.value_source_current_date],
        ].forEach(function (pair) {
            var opt = document.createElement('option');
            opt.value = pair[0];
            opt.textContent = pair[1];
            if (parsedValue.source === pair[0]) opt.selected = true;
            sourceSelect.appendChild(opt);
        });
        sourceSelect.disabled = unknown;

        // Fixed value: single text input
        var fixedInput = document.createElement('input');
        fixedInput.type = 'text';
        fixedInput.className = 'form-text';
        fixedInput.placeholder = I18N.strings.value;
        fixedInput.value = parsedValue.text || '';
        fixedInput.setAttribute('aria-label', I18N.strings.filter_value);
        fixedInput.dataset.role = 'value-fixed';
        fixedInput.disabled = unknown;

        // Fixed value: list (in / not_in / has_any_tag / has_all_tags)
        var listInput = document.createElement('textarea');
        listInput.className = 'form-text gather-filter-row__list';
        listInput.setAttribute('aria-label', I18N.strings.filter_value);
        listInput.value = (parsedValue.list || []).join('\n');
        listInput.dataset.role = 'value-list';
        listInput.disabled = unknown;

        // Value source: a field of this item
        var itemFieldInput = document.createElement('input');
        itemFieldInput.type = 'text';
        itemFieldInput.className = 'form-text';
        itemFieldInput.placeholder = I18N.strings.item_field_name_example;
        itemFieldInput.value = parsedValue.field || '';
        itemFieldInput.setAttribute('aria-label', I18N.strings.item_field_name);
        itemFieldInput.dataset.role = 'value-item-field';
        itemFieldInput.disabled = unknown;

        // Value source: a URL argument
        var urlArgInput = document.createElement('input');
        urlArgInput.type = 'text';
        urlArgInput.className = 'form-text';
        urlArgInput.placeholder = I18N.strings.url_arg_name_example;
        urlArgInput.value = parsedValue.arg || '';
        urlArgInput.setAttribute('aria-label', I18N.strings.url_arg_name);
        urlArgInput.dataset.role = 'value-url-arg';
        urlArgInput.disabled = unknown;

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = I18N.strings.remove;
        removeBtn.setAttribute('aria-label', I18N.strings.remove_filter);

        var hint = document.createElement('p');
        hint.className = 'gather-filter-row__hint description';
        hint.textContent = unknown ? I18N.strings.unknown_filter_note : I18N.strings.fixed_value_hint;

        var error = document.createElement('p');
        error.className = 'gather-filter-row__error';
        error.setAttribute('role', 'alert');
        error.hidden = true;

        /** Show or hide the value controls that match the current operator and source. */
        function updateValueControls() {
            var op = opSelect.value;
            var noValue = NO_VALUE_OPERATORS[op] === true;
            var isList = LIST_OPERATORS[op] === true;
            var source = sourceSelect.value;

            sourceSelect.hidden = noValue;
            fixedInput.hidden = noValue || source !== 'fixed' || isList;
            listInput.hidden = noValue || source !== 'fixed' || !isList;
            itemFieldInput.hidden = noValue || source !== 'current_item_field';
            urlArgInput.hidden = noValue || source !== 'url_arg';
            hint.hidden = unknown ? false : (noValue || source !== 'fixed');
        }
        updateValueControls();

        function markTouched() {
            valueTouched = true;
            error.hidden = true;
            readFiltersFromDOM();
            syncToHiddenInputs();
        }

        fieldInput.addEventListener('input', function () {
            readFiltersFromDOM();
            syncToHiddenInputs();
        });
        opSelect.addEventListener('change', function () {
            // Changing the operator alone does not touch the value: a
            // number, boolean or UUID fixed value still keeps its JSON type
            // on save unless the value itself is also edited.
            updateValueControls();
            error.hidden = true;
            readFiltersFromDOM();
            syncToHiddenInputs();
        });
        sourceSelect.addEventListener('change', markTouched);
        fixedInput.addEventListener('input', markTouched);
        listInput.addEventListener('input', markTouched);
        itemFieldInput.addEventListener('input', markTouched);
        urlArgInput.addEventListener('input', markTouched);
        removeBtn.addEventListener('click', function () {
            row.remove();
            readFiltersFromDOM();
            syncToHiddenInputs();
        });

        /** This row's current value as a `FilterValue` JSON shape. */
        function currentValue() {
            var op = opSelect.value;
            if (NO_VALUE_OPERATORS[op]) return null;
            if (!valueTouched) return raw.value !== undefined ? raw.value : null;
            switch (sourceSelect.value) {
                case 'current_item': return 'current_item';
                case 'current_user': return 'current_user';
                case 'current_time': return 'current_time';
                case 'current_date': return 'current_date';
                case 'url_arg': return { url_arg: urlArgInput.value.trim() };
                case 'current_item_field': return { current_item_field: itemFieldInput.value.trim() };
                default:
                    return LIST_OPERATORS[op] ? splitListInput(listInput.value) : fixedInput.value;
            }
        }

        /**
         * `null`, or an error message when a fixed value is written exactly
         * as a reserved contextual keyword (B279): the engine would read it
         * back as that contextual value, never as the literal text.
         */
        function validate() {
            if (unknown) return null;
            var op = opSelect.value;
            if (NO_VALUE_OPERATORS[op] || sourceSelect.value !== 'fixed') return null;
            var candidates = LIST_OPERATORS[op] ? splitListInput(listInput.value) : [fixedInput.value];
            var hit = candidates.filter(function (c) {
                return RESERVED_CONTEXTUAL_KEYWORDS.indexOf(c.trim()) !== -1;
            })[0];
            if (!hit) return null;
            return {
                message: fillTemplate(I18N.strings.reserved_keyword_error, { keyword: hit.trim() }),
                input: LIST_OPERATORS[op] ? listInput : fixedInput,
            };
        }

        /** This row's filter, exactly as stored when nothing in it changed. */
        function readFilter() {
            if (unknown) return raw;
            var filter = {
                field: fieldInput.value.trim(),
                operator: opSelect.value,
                value: currentValue(),
            };
            if (raw.exposed) filter.exposed = raw.exposed;
            if (raw.exposed_label !== undefined) filter.exposed_label = raw.exposed_label;
            if (raw.widget !== undefined) filter.widget = raw.widget;
            return filter;
        }

        row.readFilter = readFilter;
        row.validateFilter = function () {
            var problem = validate();
            if (problem) {
                error.textContent = problem.message;
                error.hidden = false;
            }
            return problem;
        };

        main.appendChild(fieldInput);
        main.appendChild(opSelect);
        main.appendChild(sourceSelect);
        main.appendChild(fixedInput);
        main.appendChild(listInput);
        main.appendChild(itemFieldInput);
        main.appendChild(urlArgInput);
        main.appendChild(removeBtn);

        row.appendChild(main);
        row.appendChild(hint);
        row.appendChild(error);

        return row;
    }

    function readFiltersFromDOM() {
        var rows = filtersContainer.querySelectorAll('.gather-block-row--filter');
        definition.filters = [];
        rows.forEach(function (row) {
            definition.filters.push(row.readFilter());
        });
    }

    /**
     * `null`, or the first filter row's validation problem: a fixed value
     * written exactly as a reserved contextual keyword (B279).
     */
    function validateFiltersInDOM() {
        var rows = filtersContainer.querySelectorAll('.gather-block-row--filter');
        for (var i = 0; i < rows.length; i++) {
            var problem = rows[i].validateFilter();
            if (problem) return problem;
        }
        return null;
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
        fieldInput.placeholder = I18N.strings.field_name;
        fieldInput.value = data.field || '';
        fieldInput.setAttribute('aria-label', I18N.strings.sort_field_name);
        fieldInput.setAttribute('list', 'gather-item-columns');
        fieldInput.dataset.role = 'field-name';

        // Direction
        var dirSelect = document.createElement('select');
        dirSelect.className = 'form-select';
        dirSelect.setAttribute('aria-label', I18N.strings.sort_direction);
        SORT_DIRECTIONS.forEach(function (d) {
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
        removeBtn.textContent = I18N.strings.remove;
        removeBtn.setAttribute('aria-label', I18N.strings.remove_sort);

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
            var field     = row.querySelector('[data-role="field-name"]').value.trim();
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
        joinSelect.setAttribute('aria-label', I18N.strings.join_type);
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
        targetInput.placeholder = I18N.strings.target_table;
        targetInput.value = data.target_table || '';
        targetInput.setAttribute('aria-label', I18N.strings.target_table);

        // Local field
        var localInput = document.createElement('input');
        localInput.type = 'text';
        localInput.className = 'form-text';
        localInput.placeholder = I18N.strings.local_field;
        localInput.value = data.local_field || '';
        localInput.setAttribute('aria-label', I18N.strings.local_field);

        // Foreign field
        var foreignInput = document.createElement('input');
        foreignInput.type = 'text';
        foreignInput.className = 'form-text';
        foreignInput.placeholder = I18N.strings.foreign_field;
        foreignInput.value = data.foreign_field || '';
        foreignInput.setAttribute('aria-label', I18N.strings.foreign_field);

        // Name (used as table alias in joins)
        var nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.className = 'form-text';
        nameInput.placeholder = I18N.strings.name_alias;
        nameInput.value = data.name || '';
        nameInput.setAttribute('aria-label', I18N.strings.relationship_name);

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = I18N.strings.remove;
        removeBtn.setAttribute('aria-label', I18N.strings.remove_relationship);

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
            addRelationshipBtn.title = fillTemplate(I18N.strings.max_relationships_reached, { max: MAX_RELATIONSHIPS });
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
        nameInput.placeholder = I18N.strings.include_name;
        nameInput.value = name || '';
        nameInput.setAttribute('aria-label', I18N.strings.include_name);
        nameInput.dataset.role = 'include-name';

        // Parent field
        var parentInput = document.createElement('input');
        parentInput.type = 'text';
        parentInput.className = 'form-text';
        parentInput.placeholder = I18N.strings.parent_field_example;
        parentInput.value = data.parent_field || '';
        parentInput.setAttribute('aria-label', I18N.strings.parent_field);
        parentInput.dataset.role = 'parent-field';

        // Child field
        var childInput = document.createElement('input');
        childInput.type = 'text';
        childInput.className = 'form-text';
        childInput.placeholder = I18N.strings.child_field_example;
        childInput.value = data.child_field || '';
        childInput.setAttribute('aria-label', I18N.strings.child_field);
        childInput.dataset.role = 'child-field';

        // Child base table
        var childTableInput = document.createElement('input');
        childTableInput.type = 'text';
        childTableInput.className = 'form-text';
        childTableInput.placeholder = I18N.strings.child_base_table;
        childTableInput.value = (data.definition && data.definition.base_table) || 'item';
        childTableInput.setAttribute('aria-label', I18N.strings.child_base_table);
        childTableInput.dataset.role = 'child-table';

        // Child item type
        var childTypeInput = document.createElement('input');
        childTypeInput.type = 'text';
        childTypeInput.className = 'form-text';
        childTypeInput.placeholder = I18N.strings.child_item_type_optional;
        childTypeInput.value = (data.definition && data.definition.item_type) || '';
        childTypeInput.setAttribute('aria-label', I18N.strings.child_item_type);
        childTypeInput.dataset.role = 'child-type';

        // Singular checkbox
        var singularLabel = document.createElement('label');
        singularLabel.style.cssText = 'display: inline-flex; align-items: center; gap: 0.25rem;';
        var singularCheckbox = document.createElement('input');
        singularCheckbox.type = 'checkbox';
        singularCheckbox.checked = data.singular || false;
        singularCheckbox.dataset.role = 'singular';
        singularLabel.appendChild(singularCheckbox);
        singularLabel.appendChild(document.createTextNode(' ' + I18N.strings.singular_label));

        // Limit
        var limitInput = document.createElement('input');
        limitInput.type = 'number';
        limitInput.className = 'form-text';
        limitInput.placeholder = I18N.strings.limit;
        limitInput.value = (data.display && data.display.items_per_page) || '';
        limitInput.setAttribute('aria-label', I18N.strings.include_limit);
        limitInput.dataset.role = 'include-limit';
        limitInput.style.width = '120px';

        // Remove button
        var removeBtn = document.createElement('button');
        removeBtn.type = 'button';
        removeBtn.className = 'button button--secondary gather-block-row__remove';
        removeBtn.textContent = I18N.strings.remove;
        removeBtn.setAttribute('aria-label', I18N.strings.remove_include);

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
                filtersContainer.appendChild(createFilterRow(newFilter, definition.filters.length - 1, true));
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
                // Blank: the site's items per page (/admin/config/site).
                display.items_per_page = (!isNaN(val) && val > 0) ? val : null;
                syncToHiddenInputs();
            });
        }

        if (emptyTextInput) {
            emptyTextInput.addEventListener('input', function () {
                display.empty_text = emptyTextInput.value;
                syncToHiddenInputs();
            });
        }

        if (canonicalUrlInput) {
            canonicalUrlInput.addEventListener('input', function () {
                display.canonical_url = canonicalUrlInput.value;
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

        if (pagerZeroBasedCheckbox) {
            pagerZeroBasedCheckbox.addEventListener('change', function () {
                display.pager.zero_based = pagerZeroBasedCheckbox.checked;
                syncToHiddenInputs();
            });
        }

        if (feedItemsInput) {
            feedItemsInput.addEventListener('input', function () {
                var val = parseInt(feedItemsInput.value, 10);
                if (display.feed && !isNaN(val) && val > 0) {
                    display.feed.items = val;
                    syncToHiddenInputs();
                }
            });
        }

        if (feedFullContentCheckbox) {
            feedFullContentCheckbox.addEventListener('change', function () {
                if (display.feed) {
                    display.feed.full_content = feedFullContentCheckbox.checked;
                    syncToHiddenInputs();
                }
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
            // `semantic_similarity` only applies to `item`: drop it from (or
            // restore it to) every filter row's operator list.
            operatorSelectRefreshers.forEach(function (refresh) { refresh(); });
            readFiltersFromDOM();
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
        previewBtn.textContent = I18N.strings.loading;

        // Clear previous results
        previewResults.innerHTML = '';
        var loadingMsg = document.createElement('p');
        loadingMsg.textContent = I18N.strings.loading_preview;
        loadingMsg.className = 'gather-preview__loading';
        previewResults.appendChild(loadingMsg);

        var payload = JSON.stringify({
            definition: definition,
            display: display
        });

        var xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/v1/gather/query', true);
        xhr.setRequestHeader('Content-Type', 'application/json');

        xhr.addEventListener('load', function () {
            previewBtn.disabled = false;
            previewBtn.textContent = I18N.strings.load_preview;
            previewResults.innerHTML = '';

            if (xhr.status === 200) {
                try {
                    var response = JSON.parse(xhr.responseText);
                    renderPreviewResults(response);
                } catch (e) {
                    showPreviewError(I18N.strings.invalid_response);
                }
            } else {
                try {
                    var errResponse = JSON.parse(xhr.responseText);
                    showPreviewError(errResponse.message || fillTemplate(I18N.strings.preview_failed, { status: xhr.status }));
                } catch (e) {
                    showPreviewError(fillTemplate(I18N.strings.preview_failed, { status: xhr.status }));
                }
            }
        });

        xhr.addEventListener('error', function () {
            previewBtn.disabled = false;
            previewBtn.textContent = I18N.strings.load_preview;
            previewResults.innerHTML = '';
            showPreviewError(I18N.strings.network_error);
        });

        xhr.send(payload);
    }

    function renderPreviewResults(response) {
        var rows = response.rows || response.results || response.data || [];

        if (!rows.length) {
            var empty = document.createElement('p');
            empty.className = 'gather-preview__empty';
            empty.textContent = display.empty_text || I18N.strings.no_results_found;
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
            summary.textContent = fillTemplate(I18N.strings.showing_results, { shown: rows.length, total: response.total });
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

        form.addEventListener('submit', function (event) {
            // A fixed value spelled exactly like a contextual keyword
            // (B279): block the submit rather than silently storing a
            // filter that reads back as something the user did not intend.
            var problem = validateFiltersInDOM();
            if (problem) {
                event.preventDefault();
                problem.input.focus();
                return;
            }

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
