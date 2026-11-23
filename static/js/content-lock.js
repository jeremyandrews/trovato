// Content locking on the item edit form (trovato_content_locking).
//
// The form page takes the edit lock when it renders. While the viewer holds
// it, this keeps it alive with a heartbeat to /api/v1/lock/heartbeat once a
// minute; the lock lapses 15 minutes after the last one, so a closed tab frees
// the item on its own. When another editor holds it, a viewer with
// `break content lock` gets a button that breaks it (/api/v1/lock/break) and
// reloads the form, which then takes the lock for them.
//
// CSRF tokens are single use: the first request sends the page's token, and
// each answer carries the token for the next.
(function () {
  "use strict";

  var meta = document.querySelector('meta[name="csrf-token"]');
  var csrf = meta ? meta.getAttribute("content") : "";

  function post(path, el) {
    return window
      .fetch(path, {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify({
          entity_type: el.dataset.entityType,
          entity_id: el.dataset.entityId,
        }),
      })
      .then(function (r) {
        return r.json().then(function (body) {
          if (body && body.csrf_token) csrf = body.csrf_token;
          return { ok: r.ok, body: body };
        });
      });
  }

  var held = document.querySelector("[data-content-lock]");
  if (held) {
    var timer = window.setInterval(function () {
      post("/api/v1/lock/heartbeat", held).then(function (answer) {
        if (!answer.ok) window.clearInterval(timer);
      }).catch(function () {});
    }, 60000);
  }

  var other = document.querySelector("[data-content-lock-held]");
  var button = other && other.querySelector("[data-content-lock-break]");
  if (button) {
    button.addEventListener("click", function () {
      button.disabled = true;
      post("/api/v1/lock/break", other).then(function (answer) {
        if (answer.ok) {
          window.location.reload();
        } else {
          button.disabled = false;
          window.alert((answer.body && answer.body.message) || "The lock could not be broken.");
        }
      }).catch(function () { button.disabled = false; });
    });
  }
})();
