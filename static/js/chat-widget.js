/**
 * Chat widget — SSE streaming chat with the AI chatbot.
 *
 * Initializes on any element with class "chat-widget". Reads configuration
 * from data attributes:
 *   data-machine-name: widget identifier for DOM element IDs
 *
 * CSRF token is read from a meta tag or hidden form input.
 */
(function () {
  'use strict';

  function initWidget(widget) {
    var wid = widget.getAttribute('data-machine-name');
    if (!wid) return;

    var form = document.getElementById('chat-form-' + wid);
    var input = document.getElementById('chat-input-' + wid);
    var messages = document.getElementById('chat-messages-' + wid);
    if (!form || !input || !messages) return;

    var csrfToken = '';
    var csrfMeta = document.querySelector('meta[name="csrf-token"]');
    if (csrfMeta) csrfToken = csrfMeta.content;
    if (!csrfToken) {
      var csrfInput = document.querySelector('input[name="_token"]');
      if (csrfInput) csrfToken = csrfInput.value;
    }

    if (!csrfToken) {
      var notice = document.createElement('p');
      notice.className = 'chat-login-notice';
      notice.textContent = 'Please log in to use the chat.';
      form.style.display = 'none';
      messages.parentNode.insertBefore(notice, messages);
      return;
    }

    function appendMsg(role, text) {
      var div = document.createElement('div');
      div.className = 'chat-message chat-message--' + role;
      div.textContent = text;
      messages.appendChild(div);
      messages.scrollTop = messages.scrollHeight;
      return div;
    }

    form.addEventListener('submit', async function (e) {
      e.preventDefault();
      var text = input.value.trim();
      if (!text) return;
      input.value = '';
      appendMsg('user', text);

      var assistantDiv = appendMsg('assistant', '');
      var assistantText = '';

      try {
        var response = await fetch('/api/v1/chat', {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-CSRF-Token': csrfToken
          },
          body: JSON.stringify({ message: text })
        });

        if (!response.ok) {
          var err = await response.json().catch(function () {
            return { error: 'Request failed' };
          });
          assistantDiv.textContent = 'Error: ' + (err.error || 'Unknown error');
          assistantDiv.classList.add('chat-message--error');
          return;
        }

        var reader = response.body.getReader();
        var decoder = new TextDecoder();
        var buffer = '';

        while (true) {
          var result = await reader.read();
          if (result.done) break;
          buffer += decoder.decode(result.value, { stream: true });
          var lines = buffer.split('\n');
          buffer = lines.pop() || '';
          for (var i = 0; i < lines.length; i++) {
            var line = lines[i].trim();
            if (line.startsWith('data: ')) {
              try {
                var data = JSON.parse(line.substring(6));
                if (data.type === 'token') {
                  assistantText += data.text;
                  assistantDiv.textContent = assistantText;
                  messages.scrollTop = messages.scrollHeight;
                } else if (data.type === 'error') {
                  assistantDiv.textContent = 'Error: ' + data.message;
                  assistantDiv.classList.add('chat-message--error');
                }
              } catch (parseErr) {
                // Ignore malformed SSE lines
              }
            }
          }
        }
      } catch (fetchErr) {
        assistantDiv.textContent = 'Connection error. Please try again.';
        assistantDiv.classList.add('chat-message--error');
      }
    });
  }

  function init() {
    var widgets = document.querySelectorAll('.chat-widget');
    widgets.forEach(initWidget);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
