/**
 * AI Assist — inline text operations for content editing forms.
 *
 * Attaches to `.ai-assist-btn` buttons injected by the trovato_ai plugin's
 * tap_form_alter. Each button shows a popover with operations (rewrite,
 * expand, shorten, translate, tone). The selected operation sends the field
 * value to POST /api/v1/ai/assist and shows a preview for accept/reject.
 */
(function () {
  'use strict';

  var OPERATIONS = [
    { id: 'rewrite',   label: 'Rewrite',   desc: 'Improve clarity and flow' },
    { id: 'expand',    label: 'Expand',     desc: 'Add more detail' },
    { id: 'shorten',   label: 'Shorten',    desc: 'Reduce to key points' },
    { id: 'translate', label: 'Translate',  desc: 'Translate to another language' },
    { id: 'tone',      label: 'Adjust Tone', desc: 'Change the writing tone' }
  ];

  var LANGUAGES = ['English', 'Spanish', 'French', 'German', 'Italian', 'Portuguese', 'Japanese', 'Chinese', 'Arabic', 'Korean'];
  var TONES = ['formal', 'casual', 'technical', 'friendly', 'professional', 'academic'];

  /** Get the CSRF token from the page meta tag or form. */
  function getCsrfToken() {
    var meta = document.querySelector('meta[name="csrf-token"]');
    if (meta) return meta.getAttribute('content');
    var input = document.querySelector('input[name="_token"]');
    if (input) return input.value;
    return '';
  }

  /** Create the popover element for a given field. */
  function createPopover(fieldName) {
    var popover = document.createElement('div');
    popover.className = 'ai-assist-popover';
    popover.setAttribute('data-field', fieldName);

    var header = document.createElement('div');
    header.className = 'ai-assist-header';
    header.textContent = 'AI Assist';

    var closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'ai-assist-close';
    closeBtn.textContent = '\u00d7';
    closeBtn.addEventListener('click', function () { popover.remove(); });
    header.appendChild(closeBtn);
    popover.appendChild(header);

    var opList = document.createElement('div');
    opList.className = 'ai-assist-operations';
    OPERATIONS.forEach(function (op) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'ai-assist-op';
      btn.setAttribute('data-op', op.id);
      btn.innerHTML = '<strong>' + op.label + '</strong><br><small>' + op.desc + '</small>';
      btn.addEventListener('click', function () {
        handleOperation(popover, fieldName, op.id);
      });
      opList.appendChild(btn);
    });
    popover.appendChild(opList);

    return popover;
  }

  /** Handle an operation selection. */
  function handleOperation(popover, fieldName, operation) {
    var field = document.querySelector('[name="' + fieldName + '"]');
    if (!field) return;

    var text = field.value;
    if (!text.trim()) {
      showError(popover, 'Field is empty — nothing to transform.');
      return;
    }

    // For translate and tone, show sub-options first
    if (operation === 'translate') {
      showSubOptions(popover, fieldName, operation, LANGUAGES, 'Select language:');
      return;
    }
    if (operation === 'tone') {
      showSubOptions(popover, fieldName, operation, TONES, 'Select tone:');
      return;
    }

    executeAssist(popover, fieldName, operation, {});
  }

  /** Show sub-option selector (language or tone). */
  function showSubOptions(popover, fieldName, operation, options, label) {
    var ops = popover.querySelector('.ai-assist-operations');
    if (ops) ops.style.display = 'none';

    var existing = popover.querySelector('.ai-assist-subopts');
    if (existing) existing.remove();

    var container = document.createElement('div');
    container.className = 'ai-assist-subopts';

    var title = document.createElement('div');
    title.className = 'ai-assist-subtitle';
    title.textContent = label;
    container.appendChild(title);

    options.forEach(function (opt) {
      var btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'ai-assist-subopt';
      btn.textContent = opt;
      btn.addEventListener('click', function () {
        var extra = {};
        if (operation === 'translate') extra.language = opt;
        if (operation === 'tone') extra.tone = opt;
        executeAssist(popover, fieldName, operation, extra);
      });
      container.appendChild(btn);
    });

    var back = document.createElement('button');
    back.type = 'button';
    back.className = 'ai-assist-back';
    back.textContent = '\u2190 Back';
    back.addEventListener('click', function () {
      container.remove();
      if (ops) ops.style.display = '';
    });
    container.appendChild(back);

    popover.appendChild(container);
  }

  /** Execute the AI assist request. */
  function executeAssist(popover, fieldName, operation, extra) {
    var field = document.querySelector('[name="' + fieldName + '"]');
    if (!field) return;

    showLoading(popover);

    var body = {
      text: field.value,
      operation: operation
    };
    if (extra.language) body.language = extra.language;
    if (extra.tone) body.tone = extra.tone;

    fetch('/api/v1/ai/assist', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-CSRF-Token': getCsrfToken()
      },
      body: JSON.stringify(body)
    })
      .then(function (resp) {
        if (!resp.ok) {
          return resp.json().then(function (err) {
            throw new Error(err.error || 'Request failed');
          });
        }
        return resp.json();
      })
      .then(function (data) {
        showPreview(popover, fieldName, data.result, field.value);
      })
      .catch(function (err) {
        showError(popover, err.message || 'AI request failed');
      });
  }

  /** Show loading indicator. */
  function showLoading(popover) {
    clearContent(popover);
    var loading = document.createElement('div');
    loading.className = 'ai-assist-loading';
    loading.textContent = 'Generating\u2026';
    popover.appendChild(loading);
  }

  /** Show the AI result with accept/reject buttons. */
  function showPreview(popover, fieldName, result, original) {
    clearContent(popover);

    var preview = document.createElement('div');
    preview.className = 'ai-assist-preview';

    var text = document.createElement('div');
    text.className = 'ai-assist-preview-text';
    text.textContent = result;
    preview.appendChild(text);

    var actions = document.createElement('div');
    actions.className = 'ai-assist-actions';

    var accept = document.createElement('button');
    accept.type = 'button';
    accept.className = 'ai-assist-accept';
    accept.textContent = 'Accept';
    accept.addEventListener('click', function () {
      var field = document.querySelector('[name="' + fieldName + '"]');
      if (field) {
        field.value = result;
        field.dispatchEvent(new Event('input', { bubbles: true }));
      }
      popover.remove();
    });

    var reject = document.createElement('button');
    reject.type = 'button';
    reject.className = 'ai-assist-reject';
    reject.textContent = 'Reject';
    reject.addEventListener('click', function () {
      popover.remove();
    });

    actions.appendChild(accept);
    actions.appendChild(reject);
    preview.appendChild(actions);
    popover.appendChild(preview);
  }

  /** Show an error message. */
  function showError(popover, message) {
    clearContent(popover);
    var err = document.createElement('div');
    err.className = 'ai-assist-error';
    err.textContent = message;

    var dismiss = document.createElement('button');
    dismiss.type = 'button';
    dismiss.className = 'ai-assist-dismiss';
    dismiss.textContent = 'Dismiss';
    dismiss.addEventListener('click', function () { popover.remove(); });

    popover.appendChild(err);
    popover.appendChild(dismiss);
  }

  /** Remove dynamic content from popover (keep header). */
  function clearContent(popover) {
    var children = Array.from(popover.children);
    children.forEach(function (child) {
      if (!child.classList.contains('ai-assist-header')) {
        child.remove();
      }
    });
  }

  /** Initialize: attach click handlers to all AI assist buttons. */
  function init() {
    document.addEventListener('click', function (e) {
      var btn = e.target.closest('.ai-assist-btn');
      if (!btn) return;

      e.preventDefault();

      // Remove any existing popover
      var existing = document.querySelector('.ai-assist-popover');
      if (existing) existing.remove();

      var fieldName = btn.getAttribute('data-field');
      var popover = createPopover(fieldName);

      // Position relative to the button
      btn.parentNode.style.position = 'relative';
      btn.parentNode.appendChild(popover);
    });

    // Close popover on Escape
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape') {
        var popover = document.querySelector('.ai-assist-popover');
        if (popover) popover.remove();
      }
    });
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
