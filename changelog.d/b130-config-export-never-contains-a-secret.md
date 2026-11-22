A config export never contains a secret, so the directory can be committed.

PR 201 (B92) exported each plugin config row as Postgres returned it, with no
notion of a secret column, so `webhook.<pk>.yml` carried the webhook signing
secret in the clear and `oauth_client.<pk>.yml` the client secret's Argon2 hash;
the variable `plugin.trovato_captcha.turnstile_secret_key` went out the same way.
A manifest now names its secret columns under `[config] secret_columns`
(additive; plugin API 0.107), and `trovato_webhooks` and `trovato_oauth2`
declare theirs. Export writes `env:TROVATO_SECRET_<TABLE>_<PK>_<COLUMN>` in
place of each secret, or nothing when it is empty; secret `site_config` keys
(`SECRET_VARIABLE_KEYS`) follow the same rule. Import keeps a secret the target
already holds, else reads the named variable (hashing it for a `_hash` column),
else refuses the file and writes nothing; a secret in the clear is refused. A
config table column whose name says it is a credential is treated as secret
even when undeclared. A test exports a site with a webhook and an OAuth client
and fails if any file holds either secret or the hash.
