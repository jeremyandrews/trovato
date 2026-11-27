/*
 * The account recovery page (templates/user/recover.html): posts each step of
 * the flow as JSON with the page's CSRF token in X-CSRF-Token, and takes the
 * next token from each answer, since tokens are single use.
 *
 * A file rather than an inline script: the site's Content-Security-Policy
 * allows no inline script, so the inline copy this replaces never ran.
 */
(function () {
  "use strict";

  var meta = document.querySelector('meta[name="csrf-token"]');
  var CSRF = meta ? meta.getAttribute("content") : "";
  var messageEl = document.getElementById("recover-message");

  function show(text, ok) {
    messageEl.textContent = text;
    messageEl.style.display = "block";
    messageEl.style.background = ok ? "#efe" : "#fee";
    messageEl.style.border = "1px solid " + (ok ? "#0a0" : "#c00");
  }

  function step(id) {
    ["step-identify", "step-choose", "step-verify", "step-reset"].forEach(function (s) {
      document.getElementById(s).style.display = (s === id) ? "block" : "none";
    });
  }

  function post(url, body) {
    return fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "X-CSRF-Token": CSRF },
      body: JSON.stringify(body || {})
    }).then(function (response) {
      /* Tokens are single use; each answer carries the next one. */
      var fresh = response.headers.get("X-CSRF-Token");
      if (fresh) { CSRF = fresh; }
      return response.json().then(function (parsed) {
        if (!response.ok) { throw new Error(parsed.message || "That did not work."); }
        return parsed;
      });
    });
  }

  /* The first step is the form `user_recover`; its submission is taken here
     and posted as the JSON step, never natively. */
  document.getElementById("user_recover").addEventListener("submit", function (event) {
    event.preventDefault();
    post("/user/recover/start", { identifier: document.getElementById("identifier").value })
      .then(function (body) {
        show(body.message, true);
        var list = document.getElementById("method-list");
        list.textContent = "";
        if (!body.methods || body.methods.length === 0) { return; }
        body.methods.forEach(function (method) {
          var button = document.createElement("button");
          button.type = "button";
          button.textContent = method.display_name;
          button.style.cssText = "display:block;margin:0.5rem 0;padding:0.6rem 1.2rem;cursor:pointer;";
          button.addEventListener("click", function () {
            post("/user/recover/choose", { method_id: method.method_id })
              .then(function (chosen) {
                document.getElementById("challenge-hint").textContent = chosen.challenge_hint;
                step("step-verify");
              })
              .catch(function (err) { show(err.message, false); });
          });
          list.appendChild(button);
        });
        step("step-choose");
      })
      .catch(function (err) { show(err.message, false); });
  });

  document.getElementById("verify").addEventListener("click", function () {
    post("/user/recover/verify", { response: document.getElementById("response").value })
      .then(function (body) {
        if (body.next === "reset") {
          show(body.message, true);
          step("step-reset");
        } else {
          show(body.message, true);
        }
      })
      .catch(function (err) { show(err.message, false); });
  });

  document.getElementById("reset").addEventListener("click", function () {
    post("/user/recover/reset", { new_password: document.getElementById("new-password").value })
      .then(function () { window.location.href = "/"; })
      .catch(function (err) { show(err.message, false); });
  });
})();
