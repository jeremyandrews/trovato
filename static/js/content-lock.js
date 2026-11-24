// Content locking on the item edit form (trovato_content_locking).
//
// The form page takes the edit lock when it renders. While the viewer holds
// it, this keeps it alive with a heartbeat to /api/v1/lock/heartbeat once a
// minute, plus one extra heartbeat whenever the tab becomes visible again (a
// background tab's timers can be throttled or frozen, so this catches the
// lock up without waiting for the next tick); the lock lapses 3 minutes after
// the last one, so a closed tab, a navigated-away tab or a crashed browser
// frees the item on its own within that lease. On `pagehide` — closing the
// tab or navigating away — it also beacons a release to /api/v1/lock/release
// so a well-behaved exit frees the item at once rather than waiting out the
// lease; a browser process killed outright gets no chance to send this, which
// is why the lease above still has to be short. When another editor holds it,
// a viewer with `break content lock` gets a button that breaks it
// (/api/v1/lock/break) and reloads the form, which then takes the lock for
// them.
//
// CSRF tokens are single use: the first request sends the page's token, and
// each answer carries the token for the next. `navigator.sendBeacon` cannot
// set a custom header, so the release beacon carries its token in a
// form-urlencoded `_token` field instead of the `X-CSRF-Token` header the
// other two requests use; the server accepts either.
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
    function heartbeat() {
      post("/api/v1/lock/heartbeat", held).then(function (answer) {
        if (!answer.ok) window.clearInterval(timer);
      }).catch(function () {});
    }

    var timer = window.setInterval(heartbeat, 60000);

    document.addEventListener("visibilitychange", function () {
      if (document.visibilityState === "visible") heartbeat();
    });

    window.addEventListener("pagehide", function () {
      var body = "entity_type=" + encodeURIComponent(held.dataset.entityType) +
        "&entity_id=" + encodeURIComponent(held.dataset.entityId) +
        "&_token=" + encodeURIComponent(csrf);
      navigator.sendBeacon(
        "/api/v1/lock/release",
        new Blob([body], { type: "application/x-www-form-urlencoded" })
      );
    });
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
