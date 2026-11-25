A `SITE_URL` that names a bare IP address (`http://10.20.250.7`, LAN deployments
without DNS) no longer stops the kernel from starting. At startup `AppState`
built its WebAuthn relying party straight from `SITE_URL` and propagated any
failure with `?`; `webauthn-rs` refuses an IP address as a relying party id
("rp_id is not an effective_domain of rp_origin"), so an IP `SITE_URL` aborted
the whole process before a single route could serve. Browsers do not allow
WebAuthn on an IP origin either, so refusing to start was never buying
anything. The WebAuthn relying party is now optional: when `SITE_URL` cannot
be one, the kernel logs a warning naming the reason and starts with passkeys
disabled, rather than failing to start. The passkey registration and login
endpoints answer a plain "Passkeys are not available on this site" instead of
reaching into a relying party that does not exist, the login and passkey
management pages say the same in place of the registration/sign-in controls,
and password sign-in is unaffected. Every other absolute URL `SITE_URL` feeds
— feed and sitemap links, canonical links, reset and verification mail — was
already built by plain string formatting and never depended on WebAuthn, so
those already carried the IP address correctly; only startup was the problem.
A hostname `SITE_URL` configures WebAuthn exactly as before.
