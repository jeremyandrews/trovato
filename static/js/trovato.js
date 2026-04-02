// Trovato AJAX framework
window.Trovato = window.Trovato || {};

// Announce a message to screen readers via the live region.
// Clears after 5 seconds to allow re-announcement of the same message.
Trovato.announce = function(message) {
    var el = document.getElementById('trovato-announcements');
    if (el) {
        el.textContent = message;
        setTimeout(function() { el.textContent = ''; }, 5000);
    }
};

Trovato.ajax = {
    // Execute an array of AJAX commands
    executeCommands: function(commands) {
        commands.forEach(function(cmd) {
            Trovato.ajax.executeCommand(cmd);
        });
    },

    // Execute a single AJAX command
    executeCommand: function(cmd) {
        var el;
        switch (cmd.command) {
            case 'replace':
                el = document.querySelector(cmd.selector);
                if (el) el.innerHTML = cmd.html;
                Trovato.announce(cmd.announcement || 'Content updated');
                break;

            case 'append':
                el = document.querySelector(cmd.selector);
                if (el) el.insertAdjacentHTML('beforeend', cmd.html);
                Trovato.announce(cmd.announcement || 'Item added');
                break;

            case 'prepend':
                el = document.querySelector(cmd.selector);
                if (el) el.insertAdjacentHTML('afterbegin', cmd.html);
                Trovato.announce(cmd.announcement || 'Item added');
                break;

            case 'remove':
                el = document.querySelector(cmd.selector);
                if (el) el.remove();
                Trovato.announce(cmd.announcement || 'Item removed');
                break;

            case 'alert':
                alert(cmd.message);
                break;

            case 'redirect':
                window.location.href = cmd.url;
                break;

            case 'add_class':
                el = document.querySelector(cmd.selector);
                if (el) el.classList.add(cmd.class);
                break;

            case 'remove_class':
                el = document.querySelector(cmd.selector);
                if (el) el.classList.remove(cmd.class);
                break;

            case 'set_attribute':
                el = document.querySelector(cmd.selector);
                if (el) el.setAttribute(cmd.name, cmd.value);
                break;

            case 'focus':
                el = document.querySelector(cmd.selector);
                if (el) el.focus();
                break;

            case 'scroll_to':
                el = document.querySelector(cmd.selector);
                if (el) el.scrollIntoView({ behavior: 'smooth' });
                break;

            case 'invoke_callback':
                // Resolve nested callback path (e.g., "Trovato.updateFieldDelta")
                var parts = cmd.callback.split('.');
                var fn = window;
                for (var i = 0; i < parts.length; i++) {
                    fn = fn[parts[i]];
                    if (!fn) break;
                }
                if (typeof fn === 'function') {
                    fn(cmd.args);
                }
                break;

            default:
                console.warn('Unknown AJAX command:', cmd.command);
        }
    },

    // Serialize form to JSON object.
    // Handles both flat keys ("title") and array notation ("tags[0]", "field[1][value]").
    serializeForm: function(form) {
        var values = {};
        var formData = new FormData(form);
        formData.forEach(function(value, key) {
            if (key.indexOf('[') === -1) {
                // Simple key — flat assignment
                values[key] = value;
                return;
            }

            // Array/nested notation: split "field[0][value]" into ["field", "0", "value"]
            var base = key.substring(0, key.indexOf('['));
            var segments = [];
            var rest = key.substring(key.indexOf('['));
            var match;
            var re = /\[([^\]]*)\]/g;
            while ((match = re.exec(rest)) !== null) {
                segments.push(match[1]);
            }

            // Build the nested structure
            var target = values;
            if (!(base in target)) {
                // First segment determines array vs object
                target[base] = (/^\d+$/.test(segments[0])) ? [] : {};
            }
            target = target[base];

            for (var i = 0; i < segments.length - 1; i++) {
                var seg = segments[i];
                var nextSeg = segments[i + 1];
                if (!(seg in target) && !Array.isArray(target[seg])) {
                    target[seg] = (/^\d+$/.test(nextSeg)) ? [] : {};
                }
                target = target[seg];
            }

            var lastSeg = segments[segments.length - 1];
            if (lastSeg === '') {
                // "field[]" push notation
                if (!Array.isArray(values[base])) {
                    values[base] = [];
                }
                values[base].push(value);
            } else {
                target[lastSeg] = value;
            }
        });
        return values;
    },

    // Submit an AJAX request
    submit: function(form, trigger) {
        var formBuildId = form.querySelector('input[name="_form_build_id"]');
        if (!formBuildId) {
            console.error('Form missing _form_build_id');
            return;
        }

        var payload = {
            form_build_id: formBuildId.value,
            trigger: trigger,
            values: Trovato.ajax.serializeForm(form)
        };

        fetch('/system/ajax', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            },
            body: JSON.stringify(payload),
            credentials: 'same-origin'
        })
        .then(function(response) {
            if (!response.ok) {
                throw new Error('AJAX request failed: ' + response.status);
            }
            return response.json();
        })
        .then(function(data) {
            if (data.commands && data.commands.length > 0) {
                Trovato.ajax.executeCommands(data.commands);
            }
        })
        .catch(function(error) {
            console.error('AJAX error:', error);
            alert('An error occurred. Please try again.');
        });
    }
};

// Event delegation for AJAX triggers
document.addEventListener('click', function(e) {
    var trigger = e.target.closest('[data-ajax-trigger]');
    if (!trigger) return;

    e.preventDefault();

    var form = trigger.closest('form');
    if (!form) {
        console.error('AJAX trigger not inside a form');
        return;
    }

    var triggerName = trigger.getAttribute('data-ajax-trigger');
    Trovato.ajax.submit(form, triggerName);
});

// Update multi-value field metadata after add/remove operations.
// Called via invoke_callback from the kernel's AJAX response.
// Args: { field: "field_name", count: N }
Trovato.updateFieldDelta = function(args) {
    if (!args || !args.field) return;

    // Update the hidden count input if it exists
    var countInput = document.querySelector(
        'input[name="' + args.field + '_count"]'
    );
    if (countInput) {
        countInput.value = args.count;
    }

    // Update data-count attribute on the wrapper
    var wrapper = document.getElementById(args.field + '-wrapper');
    if (wrapper) {
        wrapper.setAttribute('data-count', args.count);
    }

    // Toggle "Add another" button visibility (hide if at max)
    var maxItems = wrapper && wrapper.getAttribute('data-max-items');
    if (maxItems) {
        var addBtn = wrapper.parentElement.querySelector(
            '[data-ajax-trigger="add_' + args.field + '"]'
        );
        if (addBtn) {
            addBtn.style.display =
                args.count >= parseInt(maxItems, 10) ? 'none' : '';
        }
    }
};

// Reset the add field form after successful submission
Trovato.resetAddFieldForm = function() {
    var form = document.getElementById('add-field-form');
    if (form) {
        form.reset();
        // Clear the userEdited flag so auto-generation works again
        var nameInput = document.getElementById('new_field_name');
        if (nameInput) {
            delete nameInput.dataset.userEdited;
        }
    }
};
