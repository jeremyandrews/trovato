// The API token page, /user/tokens.
//
// Creating and revoking call the same JSON endpoints an API client does
// (POST /api/v1/tokens, DELETE /api/v1/tokens/{id}) with the page's CSRF token from
// the csrf-token meta tag. The raw token is in the create response only: it is
// shown once, here, and the server never renders it.
//
// A CSRF token is single-use, so the page's one token is spent by the first
// action. A revoke reloads the page (fresh list, fresh token); a create shows
// the new token and disables the rest of the page until it is reloaded.
(function () {
  "use strict";

  var meta = document.querySelector('meta[name="csrf-token"]');
  var csrf = meta ? meta.getAttribute("content") : "";
  var message = document.getElementById("api-token-message");
  var form = document.getElementById("api-token-create");
  var created = document.getElementById("api-token-created");
  var value = document.getElementById("api-token-value");
  var done = document.getElementById("api-token-done");

  function show(text) {
    message.textContent = text;
    message.style.display = "block";
    message.style.background = "#fee";
    message.style.border = "1px solid #c00";
  }

  function failure(response) {
    return response.json().then(
      function (body) { throw new Error(body.message || "That did not work."); },
      function () { throw new Error("That did not work."); }
    );
  }

  function disablePage() {
    Array.prototype.forEach.call(document.querySelectorAll(".api-token-revoke"), function (el) {
      el.disabled = true;
    });
    if (form) {
      Array.prototype.forEach.call(form.elements, function (el) { el.disabled = true; });
    }
  }

  Array.prototype.forEach.call(document.querySelectorAll(".api-token-revoke"), function (el) {
    el.addEventListener("click", function () {
      if (!window.confirm("Revoke “" + el.dataset.name + "”? Programs using it stop working at once.")) {
        return;
      }
      el.disabled = true;
      window.fetch("/api/v1/tokens/" + encodeURIComponent(el.dataset.id), {
        method: "DELETE",
        credentials: "same-origin",
        headers: { "X-CSRF-Token": csrf }
      })
        .then(function (response) {
          if (!response.ok) { return failure(response); }
          window.location.reload();
        })
        .catch(function (err) {
          show(err.message + " Reload the page and try again.");
        });
    });
  });

  if (form) {
    form.addEventListener("submit", function (event) {
      event.preventDefault();
      var body = { name: form.elements.name.value };
      var days = form.elements.expires_in_days.value;
      if (days !== "") { body.expires_in_days = parseInt(days, 10); }
      disablePage();

      window.fetch("/api/v1/tokens", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-CSRF-Token": csrf },
        body: JSON.stringify(body)
      })
        .then(function (response) {
          if (!response.ok) { return failure(response); }
          return response.json();
        })
        .then(function (token) {
          value.value = token.token;
          created.hidden = false;
          value.focus();
          value.select();
        })
        .catch(function (err) {
          show(err.message + " Reload the page and try again.");
        });
    });
  }

  if (done) {
    done.addEventListener("click", function () {
      // Drop the token from the page before reloading it.
      value.value = "";
      window.location.reload();
    });
  }
})();
