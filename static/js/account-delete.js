// Passkey step-up for account deletion.
//
// A WebAuthn assertion cannot be driven by a plain form, so this is the one part
// of deleting an account that needs JavaScript. Everything else on the flow is a
// form POST, and a password account never loads this file.
(function () {
  "use strict";

  var button = document.getElementById("delete-passkey-verify");
  var status = document.getElementById("delete-passkey-status");
  if (!button) { return; }

  function show(message) { if (status) { status.textContent = message; } }

  if (!window.PublicKeyCredential) {
    button.disabled = true;
    show("This browser does not support passkeys.");
    return;
  }

  var meta = document.querySelector('meta[name="csrf-token"]');
  var csrf = meta ? meta.getAttribute("content") : "";

  // WebAuthn speaks ArrayBuffers; the JSON wire format uses base64url.
  function b64urlToBuffer(value) {
    var padded = value.replace(/-/g, "+").replace(/_/g, "/");
    while (padded.length % 4) { padded += "="; }
    var binary = window.atob(padded);
    var bytes = new Uint8Array(binary.length);
    for (var i = 0; i < binary.length; i++) { bytes[i] = binary.charCodeAt(i); }
    return bytes.buffer;
  }

  function bufferToB64url(buffer) {
    var bytes = new Uint8Array(buffer);
    var binary = "";
    for (var i = 0; i < bytes.length; i++) { binary += String.fromCharCode(bytes[i]); }
    return window.btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
  }

  function post(url, body) {
    return window.fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: {
        "Content-Type": "application/json",
        "X-CSRF-Token": csrf
      },
      body: body ? JSON.stringify(body) : "{}"
    });
  }

  button.addEventListener("click", function () {
    button.disabled = true;
    show("Follow your device's prompt…");

    post("/user/delete/passkey/start")
      .then(function (response) {
        if (!response.ok) { throw new Error("Could not start verification."); }
        return response.json();
      })
      .then(function (started) {
        var pk = started.publicKey;
        pk.challenge = b64urlToBuffer(pk.challenge);
        if (pk.allowCredentials) {
          pk.allowCredentials = pk.allowCredentials.map(function (c) {
            return { id: b64urlToBuffer(c.id), type: c.type, transports: c.transports };
          });
        }
        return navigator.credentials.get({ publicKey: pk });
      })
      .then(function (assertion) {
        return post("/user/delete/passkey/finish", {
          credential: {
            id: assertion.id,
            rawId: bufferToB64url(assertion.rawId),
            type: assertion.type,
            extensions: assertion.getClientExtensionResults(),
            response: {
              authenticatorData: bufferToB64url(assertion.response.authenticatorData),
              clientDataJSON: bufferToB64url(assertion.response.clientDataJSON),
              signature: bufferToB64url(assertion.response.signature),
              userHandle: assertion.response.userHandle
                ? bufferToB64url(assertion.response.userHandle)
                : null
            }
          }
        });
      })
      .then(function (response) {
        if (!response.ok) { throw new Error("That passkey was not accepted."); }
        window.location.href = "/user/delete/confirm";
      })
      .catch(function (err) {
        show(err.message || "Verification failed.");
        button.disabled = false;
      });
  });
})();
