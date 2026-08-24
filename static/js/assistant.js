/* AI Assistant conversation page: progressive enhancement.
 *
 * The page works without this file. Proposal cards and Start over are plain
 * forms with a `_token` field, and the server redirects back to the page. What
 * this adds is the one thing a form cannot do: consume the SSE stream a turn
 * produces, so a tool call and a proposal appear as they happen rather than
 * after a reload.
 *
 * Vanilla, no build step, no dependencies, and it degrades to the server path
 * on any failure. */
(function () {
    'use strict';

    var root = document.querySelector('.assistant');
    if (!root) {
        return;
    }

    var conversationId = root.getAttribute('data-conversation-id');
    var csrfToken = root.getAttribute('data-csrf-token');
    var transcript = document.getElementById('assistant-transcript');
    var composer = document.getElementById('assistant-composer');
    var textarea = document.getElementById('assistant-message');

    function el(tag, className, text) {
        var node = document.createElement(tag);
        if (className) {
            node.className = className;
        }
        if (text !== undefined && text !== null) {
            node.textContent = text;
        }
        return node;
    }

    function appendEntry(node) {
        transcript.appendChild(node);
        node.scrollIntoView({ block: 'nearest' });
    }

    function addMessage(who, text, variant) {
        var item = el('li', 'assistant-entry assistant-entry--' + variant);
        item.appendChild(el('span', 'assistant-entry__who', who));
        item.appendChild(el('div', 'assistant-entry__text', text));
        appendEntry(item);
        return item;
    }

    function addToolResult(event) {
        var item = el('li', 'assistant-entry assistant-entry--tool' +
            (event.ok ? '' : ' assistant-entry--tool-failed'));
        item.appendChild(el('code', 'assistant-entry__tool', event.tool));
        item.appendChild(el('span', 'assistant-entry__summary', event.summary || ''));
        appendEntry(item);
    }

    function addNote(text) {
        appendEntry(el('li', 'assistant-entry assistant-entry--note', text));
    }

    /* A proposal card, built to match what the server renders, so a reload
     * shows the same thing this did. */
    function addProposal(event) {
        var item = el('li', 'assistant-entry assistant-entry--proposal');
        var card = el('div', 'assistant-proposal assistant-proposal--' + event.risk +
            ' assistant-proposal--proposed');
        card.id = 'proposal-' + event.proposal_id;

        card.appendChild(el('p', 'assistant-proposal__description', event.description));

        var meta = el('p', 'assistant-proposal__meta');
        meta.appendChild(el('code', null, event.tool));
        meta.appendChild(el('span', 'assistant-proposal__risk', event.risk + ' risk'));
        meta.appendChild(el('span', 'assistant-proposal__status', 'proposed'));
        card.appendChild(meta);

        var actions = el('div', 'assistant-proposal__actions');
        actions.appendChild(proposalButton(event.proposal_id, 'apply', 'Apply', 'button button--primary'));
        actions.appendChild(proposalButton(event.proposal_id, 'discard', 'Discard', 'button'));
        card.appendChild(actions);

        item.appendChild(card);
        appendEntry(item);
    }

    function proposalButton(proposalId, action, label, className) {
        var form = document.createElement('form');
        form.method = 'post';
        form.action = '/api/v1/assistant/' + conversationId + '/proposals/' + proposalId + '/' + action;
        var token = document.createElement('input');
        token.type = 'hidden';
        token.name = '_token';
        token.value = csrfToken;
        form.appendChild(token);
        var button = el('button', className, label);
        button.type = 'submit';
        form.appendChild(button);
        enhanceProposalForm(form);
        return form;
    }

    /* Turn a proposal form into a fetch that updates the card in place.
     * On any failure it falls back to submitting the form for real, which is
     * the server path the page ships with. */
    function enhanceProposalForm(form) {
        form.addEventListener('submit', function (submitEvent) {
            /* Once the fetch path has failed, this flag lets the next submit
             * through to the browser, which posts the form the server way. */
            if (form.dataset.fallback === 'yes') {
                return;
            }
            submitEvent.preventDefault();
            var card = form.closest('.assistant-proposal');
            setBusy(form, true);

            fetch(form.action, {
                method: 'POST',
                headers: { 'X-CSRF-Token': csrfToken },
                credentials: 'same-origin'
            })
                .then(function (response) {
                    if (!response.ok) {
                        throw new Error('proposal action failed');
                    }
                    return response.json();
                })
                .then(function (body) {
                    updateCard(card, body);
                    setBusy(form, false);
                })
                .catch(function () {
                    /* Let the browser do it the plain way. */
                    form.dataset.fallback = 'yes';
                    setBusy(form, false);
                    form.submit();
                });
        });
    }

    function updateCard(card, body) {
        if (!card || !body || !body.proposal) {
            return;
        }
        var status = body.proposal.status;
        card.className = card.className
            .replace(/assistant-proposal--(proposed|applied|discarded|failed)/, '')
            .trim() + ' assistant-proposal--' + status;

        var label = card.querySelector('.assistant-proposal__status');
        if (label) {
            label.textContent = status;
        }
        var actions = card.querySelector('.assistant-proposal__actions');
        if (actions) {
            actions.remove();
        }
        if (body.proposal.result) {
            card.appendChild(el('p', 'assistant-proposal__result', body.proposal.result));
        }
        if (body.note) {
            addNote(body.note);
        }
    }

    function setBusy(form, busy) {
        var buttons = form.querySelectorAll('button');
        for (var i = 0; i < buttons.length; i += 1) {
            buttons[i].disabled = busy;
        }
    }

    /* Consume one turn's SSE stream.
     *
     * fetch + a reader rather than EventSource, because EventSource cannot send
     * a POST body or a CSRF header. */
    function sendMessage(text) {
        if (!text.trim()) {
            return;
        }
        addMessage('You', text, 'user');
        textarea.value = '';
        root.classList.add('assistant--busy');
        setComposerEnabled(false);

        fetch('/api/v1/assistant/' + conversationId + '/message', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'X-CSRF-Token': csrfToken
            },
            credentials: 'same-origin',
            body: JSON.stringify({ message: text })
        })
            .then(function (response) {
                if (!response.ok || !response.body) {
                    throw new Error('the assistant could not be reached');
                }
                return readStream(response.body.getReader(), handleEvent);
            })
            .catch(function () {
                addNote('Something went wrong sending that message.');
            })
            .then(function () {
                root.classList.remove('assistant--busy');
                setComposerEnabled(true);
                textarea.focus();
            });

        function handleEvent(event) {
            switch (event.type) {
                case 'assistant':
                    addMessage('Assistant', event.text, 'assistant');
                    break;
                case 'tool_result':
                    addToolResult(event);
                    break;
                case 'proposal':
                    addProposal(event);
                    break;
                case 'note':
                    addNote(event.text);
                    break;
                case 'error':
                    addNote(event.message);
                    break;
                default:
                    /* turn_start, tool_call and done need nothing drawn. */
                    break;
            }
        }
    }

    /* Parse an SSE byte stream into events. Only `data:` lines carry payload;
     * a keep-alive comment starts with ':' and is skipped. */
    function readStream(reader, onEvent) {
        var decoder = new TextDecoder();
        var buffer = '';

        function pump() {
            return reader.read().then(function (result) {
                if (result.done) {
                    return;
                }
                buffer += decoder.decode(result.value, { stream: true });
                var lines = buffer.split('\n');
                buffer = lines.pop();
                for (var i = 0; i < lines.length; i += 1) {
                    var line = lines[i];
                    if (line.indexOf('data:') !== 0) {
                        continue;
                    }
                    try {
                        onEvent(JSON.parse(line.slice(5).trim()));
                    } catch (e) {
                        /* A partial or unreadable event is not worth ending the
                         * stream over. */
                    }
                }
                return pump();
            });
        }

        return pump();
    }

    function setComposerEnabled(enabled) {
        if (!composer) {
            return;
        }
        textarea.disabled = !enabled;
        var buttons = composer.querySelectorAll('button');
        for (var i = 0; i < buttons.length; i += 1) {
            buttons[i].disabled = !enabled;
        }
    }

    if (composer) {
        composer.addEventListener('submit', function (event) {
            event.preventDefault();
            sendMessage(textarea.value);
        });
    }

    var existing = document.querySelectorAll('.assistant-proposal__actions form');
    for (var i = 0; i < existing.length; i += 1) {
        enhanceProposalForm(existing[i]);
    }

    var suggestions = document.querySelectorAll('.assistant__suggestion');
    for (var s = 0; s < suggestions.length; s += 1) {
        suggestions[s].addEventListener('click', function (event) {
            var text = event.currentTarget.getAttribute('data-suggestion');
            if (textarea) {
                textarea.value = text;
                textarea.focus();
            }
        });
    }
})();
