Plugins had no asymmetric signing and no way to hold an operator secret: the
only place a plugin could keep a signing key was a site variable or an item
field, readable by anything that reads content or config, which made an APNs
style provider token (and anything else that needs a header and claims signed
with a private key) impossible to build as a plugin.

`crypto-api` gained `sign-jwt-es256`: a plugin names a key by a short `key_ref`
and the host signs a JWT with ES256 using the PEM PKCS#8 P-256 private key the
operator mapped to it in the `crypto_signing_keys` site config entry, never a
raw environment variable or file path of the plugin's own choosing. The key is
parsed and cached in the host and never crosses into WASM memory, never
appears in a log, and never appears in an error. The host always signs with
`alg: ES256`; a header that names a different algorithm is refused rather than
silently overridden, and a `key_ref` nothing maps for the calling plugin
answers the same refusal as one mapped to a different plugin, so it cannot be
used to probe who a key belongs to.
